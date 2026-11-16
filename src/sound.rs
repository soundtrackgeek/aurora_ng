//! Sound descriptors (`sound:` and `group:sound`), read from Aurora.
//!
//! Aurora 0.65.0 and newer describe what each track sounds like (orchestral,
//! choir, synth, chiptune, …) from Music Library's CLAP analysis, and keep the
//! results in `%APPDATA%\com.soundtrackgeek.aurora\aurora-sound-descriptors.sqlite3`.
//! Aurora NG only reads that file's album table: an album carries a
//! descriptor when at least a third of its tracks do, and its most common
//! descriptor is its `group:sound` heading. Without Aurora's file, `sound:`
//! finds nothing.

use rusqlite::{Connection, OptionalExtension};
use std::{collections::HashMap, path::Path, path::PathBuf};

const FILE_NAME: &str = "aurora-sound-descriptors.sqlite3";

/// The descriptor model this list of labels belongs to. A cache Aurora built
/// with other labels is not used.
const MODEL: &str = "sound-v1|clap-lmas-e9fd5ac-3x10s-v1";

/// Label ids and names, in the order of the bits in Aurora's masks.
const LABELS: [(&str, &str); 12] = [
    ("orchestral", "Orchestral"),
    ("choir", "Choir"),
    ("strings", "Strings"),
    ("piano", "Piano"),
    ("guitar", "Guitar"),
    ("synth", "Synth"),
    ("hybrid", "Hybrid"),
    ("ambient", "Ambient"),
    ("percussion", "Percussion"),
    ("chiptune", "Chiptune"),
    ("jazz", "Jazz"),
    ("vocals", "Vocals"),
];

const ALIASES: [(&str, &str); 9] = [
    ("orchestra", "orchestral"),
    ("choral", "choir"),
    ("string", "strings"),
    ("synthesizer", "synth"),
    ("synths", "synth"),
    ("drums", "percussion"),
    ("chip", "chiptune"),
    ("vocal", "vocals"),
    ("voice", "vocals"),
];

/// The label index for a `sound:` value: a label, or an alias like `choral`.
pub fn parse_label(value: &str) -> Result<u8, String> {
    let wanted = value.trim().to_lowercase();
    let wanted = ALIASES
        .iter()
        .find(|(alias, _)| *alias == wanted)
        .map_or(wanted.as_str(), |(_, id)| id);
    LABELS
        .iter()
        .position(|(id, _)| *id == wanted)
        .map(|index| index as u8)
        .ok_or_else(|| {
            let names: Vec<&str> = LABELS.iter().map(|(id, _)| *id).collect();
            format!(
                "Unknown sound \"{}\". Use sound: with {}.",
                value.trim(),
                names.join(", ")
            )
        })
}

/// "Choir" for label 1.
pub fn label_name(index: u8) -> Option<&'static str> {
    LABELS.get(usize::from(index)).map(|(_, name)| *name)
}

/// Aurora's descriptor file, unless `AURORA_NG_SOUND_CACHE` points somewhere else.
pub fn cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AURORA_NG_SOUND_CACHE") {
        return Some(PathBuf::from(path));
    }
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join("com.soundtrackgeek.aurora")
            .join(FILE_NAME),
    )
}

/// Attaches Aurora's descriptors to a catalog connection as `sound`, when the
/// file is there and was built with these labels. Read-only, like the catalog.
pub fn attach(connection: &Connection, path: &Path) -> bool {
    if !path.is_file()
        || connection
            .execute(
                "ATTACH DATABASE ?1 AS sound",
                [path.to_string_lossy().as_ref()],
            )
            .is_err()
    {
        return false;
    }
    let current = connection
        .query_row(
            "SELECT value FROM sound.sound_meta WHERE key = 'model'
             AND EXISTS (SELECT 1 FROM sound.sqlite_master WHERE type = 'table' AND name = 'sound_albums')",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .as_deref()
        == Some(MODEL);
    if !current {
        connection.execute_batch("DETACH DATABASE sound").ok();
    }
    current
}

/// Whether `attach` found usable descriptors on this connection.
pub fn attached(connection: &Connection) -> Result<bool, String> {
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_database_list WHERE name = 'sound')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not search: {error}"))
}

/// Every album's `group:sound` label index; albums without one are left out.
pub fn album_tops(connection: &Connection) -> Result<HashMap<String, u8>, String> {
    if !attached(connection)? {
        return Ok(HashMap::new());
    }
    let mut statement = connection
        .prepare("SELECT album_id, top FROM sound.sound_albums WHERE top IS NOT NULL")
        .map_err(|error| format!("Could not read sound descriptors: {error}"))?;
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|error| format!("Could not read sound descriptors: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("Could not read sound descriptors: {error}"))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Writes a cache like Aurora's: (album id, mask, top).
    pub fn write_cache(path: &Path, model: &str, albums: &[(&str, u32, Option<u8>)]) {
        let cache = Connection::open(path).unwrap();
        cache
            .execute_batch(
                "CREATE TABLE sound_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE sound_audio(audio_hash TEXT PRIMARY KEY, mask INTEGER NOT NULL, top INTEGER) WITHOUT ROWID;
                 CREATE TABLE sound_albums(album_id TEXT PRIMARY KEY, tracks INTEGER NOT NULL, mask INTEGER NOT NULL, top INTEGER) WITHOUT ROWID;",
            )
            .unwrap();
        cache
            .execute("INSERT INTO sound_meta VALUES ('model', ?1)", [model])
            .unwrap();
        for (id, mask, top) in albums {
            cache
                .execute(
                    "INSERT INTO sound_albums VALUES (?1, 10, ?2, ?3)",
                    rusqlite::params![id, mask, top],
                )
                .unwrap();
        }
    }

    pub const MODEL_FOR_TESTS: &str = MODEL;

    #[test]
    fn parses_labels_and_aliases() {
        assert_eq!(parse_label("orchestral"), Ok(0));
        assert_eq!(parse_label(" Choral "), Ok(1));
        assert_eq!(parse_label("SYNTHESIZER"), Ok(5));
        assert_eq!(parse_label("chip"), Ok(9));
        assert_eq!(parse_label("voice"), Ok(11));
        let error = parse_label("kazoo").unwrap_err();
        assert!(error.starts_with("Unknown sound \"kazoo\". Use sound: with orchestral, choir,"));
        assert_eq!(label_name(5), Some("Synth"));
        assert_eq!(label_name(12), None);
    }

    #[test]
    fn reads_only_a_cache_with_these_labels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let open = || {
            let connection = Connection::open_in_memory().unwrap();
            let usable = attach(&connection, &path);
            (connection, usable)
        };
        // No file yet.
        let (connection, usable) = open();
        assert!(!usable);
        assert!(album_tops(&connection).unwrap().is_empty());

        write_cache(&path, "sound-v0|other", &[("a", 1, Some(0))]);
        let (connection, usable) = open();
        assert!(!usable, "another model's labels mean something else");
        assert!(!attached(&connection).unwrap());

        std::fs::remove_file(&path).unwrap();
        write_cache(&path, MODEL, &[("a", 0b10_0001, Some(5)), ("b", 0, None)]);
        let (connection, usable) = open();
        assert!(usable);
        assert_eq!(
            album_tops(&connection).unwrap(),
            HashMap::from([("a".to_owned(), 5)])
        );
    }
}
