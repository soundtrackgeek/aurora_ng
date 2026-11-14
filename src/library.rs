//! Read-only access to Music Library's catalog, the same SQLite database Aurora reads.

use rusqlite::{Connection, InterruptHandle, OpenFlags, functions::FunctionFlags};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub struct Album {
    pub id: String,
    pub title: String,
    /// The original year, not the release year of a reissue.
    pub year: Option<i64>,
    pub publisher: Option<String>,
    pub genre: Option<String>,
    pub total_tracks: Option<i64>,
    pub total_seconds: Option<i64>,
    /// 0–100, like track ratings: the explicit album rating, or the mean of
    /// the tracks once every one is rated.
    pub rating: Option<i64>,
    /// Music Library's Album Score: an open-ended ranking number, not stars.
    pub score: Option<f64>,
    /// The folder the album's files are in, like `D:\MUSIC\Kittie - Oracle (2001)`.
    pub folder: Option<String>,
    /// Filled in from the covers folder after loading.
    pub cover: Option<PathBuf>,
    /// Best album chart positions: Billboard, Official UK, VG Lista.
    pub charts: Vec<ChartPeak>,
}

/// The charts Music Library matches the library against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chart {
    Billboard,
    OfficialUk,
    VgLista,
    TiISkuddet,
    Norsktoppen,
}

impl Chart {
    /// Track charts, in the order Aurora shows them.
    const SINGLES: [Chart; 5] = [
        Chart::Billboard,
        Chart::OfficialUk,
        Chart::VgLista,
        Chart::TiISkuddet,
        Chart::Norsktoppen,
    ];
    /// Album charts, in the order Aurora shows them.
    const ALBUMS: [Chart; 3] = [Chart::Billboard, Chart::OfficialUk, Chart::VgLista];

    /// "BB", as Aurora labels a track's chart.
    pub fn short_label(self) -> &'static str {
        match self {
            Chart::Billboard => "BB",
            Chart::OfficialUk => "UK",
            Chart::VgLista => "VG",
            Chart::TiISkuddet => "TI",
            Chart::Norsktoppen => "NT",
        }
    }

    /// The flag Aurora shows for an album chart.
    pub fn flag(self) -> &'static str {
        match self {
            Chart::Billboard => "flags/us.svg",
            Chart::OfficialUk => "flags/gb.svg",
            Chart::VgLista | Chart::TiISkuddet | Chart::Norsktoppen => "flags/no.svg",
        }
    }
}

/// An album's or track's best position on one chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChartPeak {
    pub chart: Chart,
    pub rank: i64,
}

/// The ranks Music Library stored, one column per chart, without the charts
/// the album or track never reached.
fn chart_peaks(charts: &[Chart], ranks: &[Option<i64>]) -> Vec<ChartPeak> {
    charts
        .iter()
        .zip(ranks)
        .filter_map(|(&chart, &rank)| {
            rank.filter(|&rank| rank > 0)
                .map(|rank| ChartPeak { chart, rank })
        })
        .collect()
}

#[derive(Clone)]
pub struct Artist {
    pub name: String,
    /// Lowercase name, used for sorting and type-to-find.
    pub sort_key: String,
    /// The flag of the country MusicBrainz says the artist is from, like
    /// "flags/gb.svg".
    pub flag: Option<String>,
    pub albums: Vec<Album>,
}

#[derive(Clone)]
pub struct Track {
    pub id: i64,
    pub album_id: String,
    pub title: String,
    pub artist: String,
    pub disc: Option<i64>,
    pub number: Option<i64>,
    pub duration_seconds: Option<i64>,
    /// 0–100: 20 per star, so 70 is three and a half stars.
    pub rating: Option<i64>,
    pub loved: bool,
    pub path: PathBuf,
    /// Best single chart positions: Billboard, Official UK, VG Lista, Ti i
    /// Skuddet, Norsktoppen.
    pub charts: Vec<ChartPeak>,
    /// Released as an official single, as MusicBrainz confirms it.
    pub single: bool,
    /// 1, 2 or 3 for the album's most-listened tracks on Last.fm, shown
    /// with a flame. Filled in after loading.
    pub popular: Option<u8>,
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
    /// Music Library 0.210.0 and newer list official singles.
    has_singles: bool,
    /// Music Library looks up where artists are from on MusicBrainz.
    has_origins: bool,
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
        register_functions(&connection)?;
        Self::with_connection(connection)
    }

    fn with_connection(connection: Connection) -> Result<Self, String> {
        let has_table = |name: &str| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    [name],
                    |row| row.get(0),
                )
                .map_err(|error| format!("Could not read the catalog: {error}"))
        };
        let has_singles = has_table("track_singles")?;
        let has_origins = has_table("musicbrainz_artist_origin_countries")?;
        Ok(Self {
            connection,
            has_singles,
            has_origins,
        })
    }

    /// Stops a query running on this connection from another thread.
    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.connection.get_interrupt_handle()
    }

    /// The ids of the albums a search finds.
    pub fn search_album_ids(
        &self,
        search: &crate::search::Search,
    ) -> Result<HashSet<String>, String> {
        crate::search::album_ids(&self.connection, search)
    }

    /// Every album artist with their albums, sorted by name, then by year.
    pub fn artists(&self) -> Result<Vec<Artist>, String> {
        let country = if self.has_origins {
            format!(
                "(SELECT o.country_code FROM musicbrainz_artist_origin_countries o
                  WHERE o.local_artist_key = {})",
                crate::search::artist_key_sql("a.album_artist_display")
            )
        } else {
            "NULL".to_owned()
        };
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT a.id, a.album, a.album_artist_display, a.year,
                        (SELECT file_path FROM tracks t WHERE t.album_id = a.id LIMIT 1),
                        a.publisher, a.total_tracks, a.total_seconds,
                        a.effective_album_rating, a.album_score, a.canonical_genre,
                        a.billboard_rank, a.official_uk_rank, a.vg_lista_rank, {country}
                 FROM albums a"
            ))
            .map_err(|error| format!("Could not read albums: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    chart_peaks(&Chart::ALBUMS, &[row.get(11)?, row.get(12)?, row.get(13)?]),
                    row.get::<_, Option<String>>(14)?,
                ))
            })
            .map_err(|error| format!("Could not read albums: {error}"))?;

        let mut artists: BTreeMap<String, Artist> = BTreeMap::new();
        for row in rows {
            let (
                id,
                title,
                artist,
                year,
                folder,
                publisher,
                total_tracks,
                total_seconds,
                rating,
                score,
                genre,
                charts,
                country,
            ) = row.map_err(|error| format!("Could not read an album: {error}"))?;
            let name = non_empty(artist).unwrap_or_else(|| "Unknown Artist".to_owned());
            let sort_key = name.to_lowercase();
            let artist = artists.entry(sort_key.clone()).or_insert_with(|| Artist {
                name,
                sort_key,
                flag: None,
                albums: Vec::new(),
            });
            if artist.flag.is_none() {
                artist.flag = country.as_deref().and_then(crate::flags::path);
            }
            artist.albums.push(Album {
                id,
                title: non_empty(title).unwrap_or_else(|| "Unknown Album".to_owned()),
                year,
                publisher: non_empty(publisher),
                genre: non_empty(genre),
                total_tracks,
                total_seconds,
                rating,
                score,
                folder: non_empty(folder),
                cover: None,
                charts,
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
        // Music Library keys singles by `lower(trim(...))` of the album artist
        // and title. Built here in SQLite too, the keys match byte for byte:
        // SQLite's `lower` folds ASCII only, unlike Rust's.
        let single = if self.has_singles {
            "EXISTS(SELECT 1 FROM track_singles s
                    WHERE s.artist_key = lower(trim(t.album_artist_display))
                      AND s.track_key = lower(trim(t.title))
                      AND s.confidence = 'confirmed')"
        } else {
            "0"
        };
        let mut statement = self
            .connection
            .prepare_cached(&format!(
                "SELECT t.id, t.title,
                        COALESCE(NULLIF(TRIM(t.display_artist), ''), t.album_artist_display),
                        t.disc_number, t.track_number, t.time_seconds, t.file_path, t.filename,
                        t.normalized_rating, t.love,
                        t.billboard_single_rank, t.official_uk_rank, t.vg_lista_rank,
                        t.ti_i_skuddet_rank, t.norsktoppen_rank, {single}
                 FROM tracks t
                 WHERE t.album_id = ?1
                 ORDER BY COALESCE(t.disc_number, 1), t.track_number, t.filename"
            ))
            .map_err(|error| format!("Could not read tracks: {error}"))?;
        statement
            .query_map([album_id], |row| {
                let directory: String = row.get(6)?;
                let filename: String = row.get(7)?;
                Ok(Track {
                    id: row.get(0)?,
                    album_id: album_id.to_owned(),
                    title: non_empty(row.get(1)?).unwrap_or_else(|| filename.clone()),
                    artist: non_empty(row.get(2)?).unwrap_or_else(|| "Unknown Artist".to_owned()),
                    disc: row.get(3)?,
                    number: row.get(4)?,
                    duration_seconds: row.get(5)?,
                    rating: row.get::<_, Option<i64>>(8)?.filter(|&rating| rating > 0),
                    loved: row.get::<_, Option<String>>(9)?.as_deref() == Some("L"),
                    path: Path::new(&directory).join(&filename),
                    charts: chart_peaks(
                        &Chart::SINGLES,
                        &[
                            row.get(10)?,
                            row.get(11)?,
                            row.get(12)?,
                            row.get(13)?,
                            row.get(14)?,
                        ],
                    ),
                    single: row.get(15)?,
                    popular: None,
                })
            })
            .map_err(|error| format!("Could not read tracks: {error}"))?
            .collect::<Result<_, _>>()
            .map_err(|error| format!("Could not read a track: {error}"))
    }
}

impl Catalog {
    /// An album's rating (0–100) and Album Score, which Music Library works
    /// out again when a track's rating changes.
    pub fn album_rating(&self, album_id: &str) -> Result<(Option<i64>, Option<f64>), String> {
        self.connection
            .query_row(
                "SELECT effective_album_rating, album_score FROM albums WHERE id = ?1",
                [album_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| format!("Could not read the album's rating: {error}"))
    }
}

/// `unicode_lower`, which Music Library defines in its catalog and uses in
/// artist keys; SQLite's own `lower` only knows ASCII.
pub fn register_functions(connection: &Connection) -> Result<(), String> {
    connection
        .create_scalar_function(
            "unicode_lower",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                Ok(context
                    .get::<Option<String>>(0)?
                    .map(|text| text.to_lowercase()))
            },
        )
        .map_err(|error| format!("Could not prepare the catalog: {error}"))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE albums (id TEXT PRIMARY KEY, album TEXT, album_artist_display TEXT,
            year INTEGER, publisher TEXT, total_tracks INTEGER, total_seconds INTEGER,
            effective_album_rating INTEGER, album_score REAL, canonical_genre TEXT,
            billboard_rank INTEGER, official_uk_rank INTEGER, vg_lista_rank INTEGER);
        CREATE TABLE tracks (id INTEGER PRIMARY KEY, album_id TEXT, title TEXT,
            display_artist TEXT, album_artist_display TEXT, disc_number INTEGER,
            track_number INTEGER, time_seconds INTEGER, file_path TEXT, filename TEXT,
            normalized_rating INTEGER, love TEXT, billboard_single_rank INTEGER,
            official_uk_rank INTEGER, vg_lista_rank INTEGER, ti_i_skuddet_rank INTEGER,
            norsktoppen_rank INTEGER);
        INSERT INTO albums (id, album, album_artist_display, billboard_rank, official_uk_rank,
            vg_lista_rank) VALUES ('thriller', 'Thriller', 'Michael Jackson', 1, 1, 3),
            ('bad', 'Bad', 'Michael Jackson', NULL, 0, NULL);
        INSERT INTO tracks (id, album_id, title, album_artist_display, track_number, file_path,
            filename, billboard_single_rank, official_uk_rank, vg_lista_rank, ti_i_skuddet_rank)
            VALUES (1, 'thriller', 'Billie Jean', 'Michael Jackson', 6, 'D:\', '06.mp3', 2, 1, 5, 3),
                   (2, 'thriller', 'Baby Be Mine', 'Michael Jackson', 2, 'D:\', '02.mp3', NULL, NULL, NULL, NULL),
                   (3, 'thriller', ' Human Nature', 'Michael Jackson', 7, 'D:\', '07.mp3', 118, NULL, NULL, NULL);";

    fn catalog(singles: bool) -> Catalog {
        let connection = Connection::open_in_memory().unwrap();
        register_functions(&connection).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        if singles {
            connection
                .execute_batch(
                    "CREATE TABLE track_singles (artist_key TEXT, track_key TEXT,
                        confidence TEXT, PRIMARY KEY (artist_key, track_key));
                     INSERT INTO track_singles VALUES
                        ('michael jackson', 'billie jean', 'confirmed'),
                        ('michael jackson', 'human nature', 'confirmed'),
                        ('michael jackson', 'baby be mine', 'possible');",
                )
                .unwrap();
        }
        Catalog::with_connection(connection).unwrap()
    }

    #[test]
    fn albums_carry_the_charts_they_reached() {
        let artists = catalog(true).artists().unwrap();
        let albums = &artists[0].albums;
        let thriller = albums.iter().find(|album| album.id == "thriller").unwrap();
        assert_eq!(
            thriller.charts,
            [
                ChartPeak {
                    chart: Chart::Billboard,
                    rank: 1
                },
                ChartPeak {
                    chart: Chart::OfficialUk,
                    rank: 1
                },
                ChartPeak {
                    chart: Chart::VgLista,
                    rank: 3
                },
            ]
        );
        // A zero rank means the album never charted there.
        let bad = albums.iter().find(|album| album.id == "bad").unwrap();
        assert!(bad.charts.is_empty());
    }

    #[test]
    fn tracks_carry_chart_peaks_and_confirmed_singles() {
        let tracks = catalog(true).tracks("thriller").unwrap();
        let summary: Vec<_> = tracks
            .iter()
            .map(|track| {
                let labels: Vec<_> = track
                    .charts
                    .iter()
                    .map(|peak| (peak.chart.short_label(), peak.rank))
                    .collect();
                (track.title.as_str(), labels, track.single)
            })
            .collect();
        assert_eq!(
            summary,
            [
                // A possible single is not shown as one.
                ("Baby Be Mine", vec![], false),
                (
                    "Billie Jean",
                    vec![("BB", 2), ("UK", 1), ("VG", 5), ("TI", 3)],
                    true
                ),
                // Keys are trimmed, as Music Library trims them.
                ("Human Nature", vec![("BB", 118)], true),
            ]
        );
    }

    #[test]
    fn artists_carry_the_flag_of_their_country() {
        let catalog = catalog(true);
        // Without the origins table, older catalogs load without flags.
        assert_eq!(catalog.artists().unwrap()[0].flag, None);

        catalog
            .connection
            .execute_batch(
                "CREATE TABLE musicbrainz_artist_origin_countries (local_artist_key TEXT
                    PRIMARY KEY, country_code TEXT);
                 INSERT INTO musicbrainz_artist_origin_countries
                    VALUES ('michael jackson', 'US'), ('kino', 'SU');
                 INSERT INTO albums (id, album, album_artist_display)
                    VALUES ('gruppa', 'Gruppa krovi', 'Kino');",
            )
            .unwrap();
        let catalog = Catalog::with_connection(catalog.connection).unwrap();
        let flags: Vec<_> = catalog
            .artists()
            .unwrap()
            .into_iter()
            .map(|artist| (artist.name, artist.flag))
            .collect();
        assert_eq!(
            flags,
            [
                // The Soviet Union has no flag to show.
                ("Kino".to_owned(), None),
                (
                    "Michael Jackson".to_owned(),
                    Some("flags/us.svg".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn older_catalogs_without_singles_still_load_tracks() {
        let tracks = catalog(false).tracks("thriller").unwrap();
        assert_eq!(tracks.len(), 3);
        assert!(tracks.iter().all(|track| !track.single));
    }
}
