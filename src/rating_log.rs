//! A log of every rating and Love change Aurora NG saves, kept in
//! `%LOCALAPPDATA%\aurora_ng\rating-log.sqlite3` so other programs (like
//! Watchme) can see what was rated, when, and from what to what. Music
//! Library only keeps the album's totals.
//!
//! One row per saved change, appended right after the MP3 is saved and
//! checked. Rows are never changed or removed; a change that could not be
//! saved is not logged. Ratings are 0–100, 20 per star, as in the MP3.

use crate::rating::Saved;
use rusqlite::{Connection, params};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// The schema version, in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 1;

/// What the log says about a track besides the values.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackNames {
    pub path: PathBuf,
    pub album_id: String,
    pub title: String,
    pub artist: String,
    /// The album's title and artist, when the app knows them.
    pub album: Option<String>,
    pub album_artist: Option<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

pub fn default_path() -> Option<PathBuf> {
    Some(crate::settings::data_dir()?.join("rating-log.sqlite3"))
}

/// Appends one change to the log at `log`. A save that changed nothing is
/// not logged.
pub fn append(
    log: &Path,
    track: &TrackNames,
    saved: Saved,
    changed_at_ms: i64,
) -> Result<(), String> {
    if saved.before == saved.after {
        return Ok(());
    }
    let connection = open(log)?;
    let directory = track
        .path
        .parent()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let filename = track
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    connection
        .execute(
            "INSERT INTO rating_changes (
                changed_at_ms, track_key, path, album_id, title, artist, album, album_artist,
                rating_before, rating_after, love_before, love_after
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                changed_at_ms,
                crate::history::track_key(&directory, &filename),
                track.path.to_string_lossy(),
                track.album_id,
                track.title,
                track.artist,
                track.album,
                track.album_artist,
                saved.before.rating,
                saved.after.rating,
                saved.before.love.as_str(),
                saved.after.love.as_str(),
            ],
        )
        .map_err(|error| format!("Could not add to the rating log: {error}"))?;
    Ok(())
}

fn open(path: &Path) -> Result<Connection, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("Could not create the rating log's folder: {error}"))?;
    }
    let connection = Connection::open(path)
        .map_err(|error| format!("Could not open the rating log: {error}"))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .and_then(|()| connection.pragma_update(None, "journal_mode", "WAL"))
        .map_err(|error| format!("Could not open the rating log: {error}"))?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("Could not read the rating log: {error}"))?;
    if version < SCHEMA_VERSION {
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS rating_changes (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    changed_at_ms INTEGER NOT NULL,
                    track_key TEXT NOT NULL,
                    path TEXT NOT NULL,
                    album_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    artist TEXT NOT NULL,
                    album TEXT,
                    album_artist TEXT,
                    rating_before INTEGER,
                    rating_after INTEGER,
                    love_before TEXT NOT NULL,
                    love_after TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS rating_changes_changed_at ON rating_changes(changed_at_ms);
                PRAGMA user_version = 1;",
            )
            .map_err(|error| format!("Could not create the rating log: {error}"))?;
    }
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rating::{Love, Values};

    fn track() -> TrackNames {
        TrackNames {
            path: PathBuf::from(r"D:\MUSIC\Tiësto - Just Be (2004)\02 Love Comes Again.mp3"),
            album_id: "mb:1".to_owned(),
            title: "Love Comes Again".to_owned(),
            artist: "Tiësto".to_owned(),
            album: Some("Just Be".to_owned()),
            album_artist: Some("Tiësto".to_owned()),
        }
    }

    fn values(rating: Option<i64>, love: Love) -> Values {
        Values { rating, love }
    }

    #[test]
    fn changes_are_appended_and_no_ops_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("logs").join("rating-log.sqlite3");
        let rated = Saved {
            before: values(None, Love::Neutral),
            after: values(Some(80), Love::Neutral),
        };
        let loved = Saved {
            before: values(Some(80), Love::Neutral),
            after: values(Some(80), Love::Loved),
        };
        let nothing = Saved {
            before: values(Some(80), Love::Loved),
            after: values(Some(80), Love::Loved),
        };
        append(&log, &track(), rated, 1_000).unwrap();
        append(&log, &track(), loved, 2_000).unwrap();
        append(&log, &track(), nothing, 3_000).unwrap();

        let connection = Connection::open(&log).unwrap();
        /// changed_at_ms, track_key, rating before/after, love before/after.
        type Row = (i64, String, Option<i64>, Option<i64>, String, String);
        let rows: Vec<Row> = connection
            .prepare("SELECT changed_at_ms, track_key, rating_before, rating_after, love_before, love_after FROM rating_changes ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let key = r"d:\music\tiësto - just be (2004)\02 love comes again.mp3".to_owned();
        assert_eq!(
            rows,
            [
                (
                    1_000,
                    key.clone(),
                    None,
                    Some(80),
                    "neutral".to_owned(),
                    "neutral".to_owned()
                ),
                (
                    2_000,
                    key,
                    Some(80),
                    Some(80),
                    "neutral".to_owned(),
                    "loved".to_owned()
                ),
            ]
        );
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
