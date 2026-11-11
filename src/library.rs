//! Read-only access to Music Library's catalog, the same SQLite database Aurora reads.

use rusqlite::{Connection, OpenFlags};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

pub struct Album {
    pub id: String,
    pub title: String,
    pub year: Option<i64>,
    /// The folder the album's files are in, like `D:\MUSIC\Kittie - Oracle (2001)`.
    pub folder: Option<String>,
    /// Filled in from the covers folder after loading.
    pub cover: Option<PathBuf>,
}

pub struct Artist {
    pub name: String,
    /// Lowercase name, used for sorting and type-to-find.
    pub sort_key: String,
    pub albums: Vec<Album>,
}

#[derive(Clone)]
pub struct Track {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub disc: Option<i64>,
    pub number: Option<i64>,
    pub duration_seconds: Option<i64>,
    pub path: PathBuf,
}

impl Track {
    /// "7" for single-disc albums, "2-07" when the album has several discs.
    pub fn position_label(&self, multi_disc: bool) -> String {
        match (self.disc, self.number) {
            (Some(disc), Some(number)) if multi_disc => format!("{disc}-{number:02}"),
            (_, Some(number)) => number.to_string(),
            _ => String::new(),
        }
    }
}

/// `%APPDATA%\com.local.musiclibrary\music-library.sqlite3`, unless
/// `AURORA_NG_CATALOG` points somewhere else.
pub fn catalog_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("AURORA_NG_CATALOG") {
        return Ok(PathBuf::from(path));
    }
    let app_data =
        std::env::var_os("APPDATA").ok_or_else(|| "The APPDATA folder is not set.".to_owned())?;
    Ok(PathBuf::from(app_data)
        .join("com.local.musiclibrary")
        .join("music-library.sqlite3"))
}

pub struct Catalog {
    connection: Connection,
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("Music catalog was not found at {}", path.display()));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let connection = Connection::open_with_flags(path, flags)
            .map_err(|error| format!("Could not open the music catalog: {error}"))?;
        // Music Library writes to the catalog while we read it.
        connection
            .busy_timeout(Duration::from_secs(2))
            .map_err(|error| format!("Could not configure the catalog: {error}"))?;
        connection
            .pragma_update(None, "query_only", true)
            .map_err(|error| format!("Could not make the catalog read-only: {error}"))?;
        Ok(Self { connection })
    }

    /// Every album artist with their albums, sorted by name, then by year.
    pub fn artists(&self) -> Result<Vec<Artist>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT a.id, a.album, a.album_artist_display, COALESCE(a.year, a.release_year),
                        (SELECT file_path FROM tracks t WHERE t.album_id = a.id LIMIT 1)
                 FROM albums a",
            )
            .map_err(|error| format!("Could not read albums: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|error| format!("Could not read albums: {error}"))?;

        let mut artists: BTreeMap<String, Artist> = BTreeMap::new();
        for row in rows {
            let (id, title, artist, year, folder) =
                row.map_err(|error| format!("Could not read an album: {error}"))?;
            let name = non_empty(artist).unwrap_or_else(|| "Unknown Artist".to_owned());
            let sort_key = name.to_lowercase();
            artists
                .entry(sort_key.clone())
                .or_insert_with(|| Artist {
                    name,
                    sort_key,
                    albums: Vec::new(),
                })
                .albums
                .push(Album {
                    id,
                    title: non_empty(title).unwrap_or_else(|| "Unknown Album".to_owned()),
                    year,
                    folder: non_empty(folder),
                    cover: None,
                });
        }

        let mut artists: Vec<Artist> = artists.into_values().collect();
        for artist in &mut artists {
            artist.albums.sort_by(|a, b| {
                // Albums without a year go last.
                (a.year.is_none(), a.year, a.title.to_lowercase()).cmp(&(
                    b.year.is_none(),
                    b.year,
                    b.title.to_lowercase(),
                ))
            });
        }
        Ok(artists)
    }

    /// The tracks of one album, in disc and track order.
    pub fn tracks(&self, album_id: &str) -> Result<Vec<Track>, String> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT id, title, COALESCE(NULLIF(TRIM(display_artist), ''), album_artist_display),
                        disc_number, track_number, time_seconds, file_path, filename
                 FROM tracks
                 WHERE album_id = ?1
                 ORDER BY COALESCE(disc_number, 1), track_number, filename",
            )
            .map_err(|error| format!("Could not read tracks: {error}"))?;
        statement
            .query_map([album_id], |row| {
                let directory: String = row.get(6)?;
                let filename: String = row.get(7)?;
                Ok(Track {
                    id: row.get(0)?,
                    title: non_empty(row.get(1)?).unwrap_or_else(|| filename.clone()),
                    artist: non_empty(row.get(2)?).unwrap_or_else(|| "Unknown Artist".to_owned()),
                    disc: row.get(3)?,
                    number: row.get(4)?,
                    duration_seconds: row.get(5)?,
                    path: Path::new(&directory).join(&filename),
                })
            })
            .map_err(|error| format!("Could not read tracks: {error}"))?
            .collect::<Result<_, _>>()
            .map_err(|error| format!("Could not read a track: {error}"))
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
