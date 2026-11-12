//! Last.fm popularity: the 🔥 on an album's three most-listened tracks.
//!
//! Counts come from three places, best first: Music Library's catalog
//! (`lastfm_track_popularity`), Aurora's own Last.fm cache, and Aurora NG's
//! cache (`%LOCALAPPDATA%\aurora_ng\aurora-lastfm-cache.sqlite3`, the same
//! layout as Aurora's). Tracks none of them know, or only knew a while ago,
//! are looked up with Last.fm's `track.getInfo` and saved to Aurora NG's cache.

use rusqlite::{Connection, OpenFlags};
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const API_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const USER_AGENT: &str = concat!("aurora_ng/", env!("CARGO_PKG_VERSION"));
/// Last.fm asks for at most five requests a second.
const REQUEST_GAP: Duration = Duration::from_millis(210);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a count is trusted before it is looked up again.
const AVAILABLE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
/// How long "Last.fm does not know this track" is trusted.
const UNAVAILABLE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
/// How many tracks get a flame.
const TOP: usize = 3;

/// Where Aurora saved its Last.fm API key with the `keyring` crate, then
/// where Music Library saved its own.
#[cfg(windows)]
const SAVED_KEYS: [&str; 2] = [
    "Last.fm API key.Aurora",
    "api-key.com.local.musiclibrary.lastfm",
];

/// A track to rank: its title, and the artist Last.fm knows it by.
#[derive(Clone, Debug)]
pub struct Lookup {
    pub title: String,
    pub artist: String,
}

#[derive(Clone, Debug)]
struct Record {
    track_key: String,
    track_name: String,
    listeners: Option<i64>,
    play_count: Option<i64>,
    available: bool,
    fresh: bool,
}

/// The files the counts are read from and saved to.
#[derive(Clone, Debug)]
pub struct Sources {
    catalog: Option<PathBuf>,
    aurora: Option<PathBuf>,
    own: Option<PathBuf>,
}

impl Sources {
    pub fn new(catalog: Option<PathBuf>) -> Self {
        let aurora = std::env::var_os("APPDATA").map(|dir| {
            PathBuf::from(dir)
                .join("com.soundtrackgeek.aurora")
                .join("aurora-lastfm-cache.sqlite3")
        });
        let own = std::env::var_os("LOCALAPPDATA").map(|dir| {
            PathBuf::from(dir)
                .join("aurora_ng")
                .join("aurora-lastfm-cache.sqlite3")
        });
        Self {
            catalog,
            aurora,
            own,
        }
    }

    /// Every count saved for the album artist's tracks. The catalog wins,
    /// then Aurora's cache, then ours, but a fresh count beats a stale one.
    fn records(&self, artist: &str) -> Vec<Record> {
        let mut records = self
            .catalog
            .as_deref()
            .map(|path| catalog_records(path, artist))
            .unwrap_or_default();
        for path in [&self.aurora, &self.own].into_iter().flatten() {
            for record in cache_records(path, artist) {
                match records
                    .iter_mut()
                    .find(|known| known.track_key == record.track_key)
                {
                    Some(known) if !known.fresh && record.fresh => *known = record,
                    Some(_) => {}
                    None => records.push(record),
                }
            }
        }
        records
    }

    /// The flames from saved counts only: 1, 2 or 3 for the top tracks,
    /// in the order of `tracks`.
    pub fn cached_ranks(&self, album_artist: &str, tracks: &[Lookup]) -> Vec<Option<u8>> {
        rank(tracks, &self.records(album_artist))
    }

    /// Whether some track has no fresh count yet, and a key to look it up.
    pub fn needs_refresh(&self, album_artist: &str, tracks: &[Lookup]) -> bool {
        let records = self.records(album_artist);
        api_key().is_some()
            && tracks
                .iter()
                .any(|track| !matching(&records, &track.title).is_some_and(|record| record.fresh))
    }

    /// Looks up the tracks without a fresh count, saves what Last.fm says,
    /// and ranks the album again. Gives up early when `generation` is no
    /// longer the latest from [`next_generation`], so a quickly browsed
    /// album does not keep Last.fm busy.
    pub fn refresh(
        &self,
        album_artist: &str,
        tracks: &[Lookup],
        generation: u64,
    ) -> Result<Vec<Option<u8>>, String> {
        static RUNNING: Mutex<()> = Mutex::new(());
        let _running = RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let key = api_key().ok_or("No Last.fm API key was found.")?;
        let own = self
            .own
            .as_deref()
            .ok_or("The LOCALAPPDATA folder is not set.")?;
        let cache = open_own_cache(own)?;
        let mut records = self.records(album_artist);
        let mut first = true;
        for track in tracks {
            if GENERATION.load(Ordering::Relaxed) != generation {
                return Err("Another album was opened.".to_owned());
            }
            if matching(&records, &track.title).is_some_and(|record| record.fresh) {
                continue;
            }
            if !first {
                std::thread::sleep(REQUEST_GAP);
            }
            first = false;
            let artist = if track.artist.trim().is_empty() {
                album_artist
            } else {
                &track.artist
            };
            let record = fetch(artist, &track.title, &key)?;
            save(&cache, album_artist, &record);
            records.retain(|known| known.track_key != record.track_key);
            records.push(record);
        }
        Ok(rank(tracks, &records))
    }
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// A new number for the album being opened, which stops older lookups.
pub fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed) + 1
}

/// The top three tracks by listeners, then plays, then album order.
fn rank(tracks: &[Lookup], records: &[Record]) -> Vec<Option<u8>> {
    let mut ranked: Vec<(usize, i64, i64)> = tracks
        .iter()
        .enumerate()
        .filter_map(|(ix, track)| {
            let record = matching(records, &track.title)?;
            let listeners = record.listeners.unwrap_or(0);
            (record.available && listeners > 0).then_some((
                ix,
                listeners,
                record.play_count.unwrap_or(0),
            ))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
    let mut ranks = vec![None; tracks.len()];
    for (place, (ix, ..)) in ranked.into_iter().take(TOP).enumerate() {
        ranks[ix] = Some(place as u8 + 1);
    }
    ranks
}

/// Lowercase, single spaces, and plain dashes and apostrophes, as Aurora
/// keys its cache.
fn key(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2014}' | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' => '\'',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Letters and digits only, to match "Mr. Kadam" with "Mr Kadam".
fn loose_key(value: &str) -> String {
    key(value)
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The record for a title: an exact match, or a loose one when only one
/// record matches loosely.
fn matching<'a>(records: &'a [Record], title: &str) -> Option<&'a Record> {
    let exact = key(title);
    if let Some(record) = records.iter().find(|record| record.track_key == exact) {
        return Some(record);
    }
    let loose = loose_key(title);
    let mut matches = records
        .iter()
        .filter(|record| loose_key(&record.track_name) == loose);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn open_read_only(path: &Path) -> Option<Connection> {
    if !path.is_file() {
        return None;
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    connection.busy_timeout(Duration::from_secs(2)).ok()?;
    Some(connection)
}

/// Music Library's counts, which it keeps fresh itself.
fn catalog_records(path: &Path, artist: &str) -> Vec<Record> {
    let Some(connection) = open_read_only(path) else {
        return Vec::new();
    };
    // Music Library keys by SQLite's `lower(trim(...))`, so build the key there too.
    let Ok(mut statement) = connection.prepare(
        "SELECT track_key, track_name, listeners, play_count, state
         FROM lastfm_track_popularity WHERE artist_key = lower(trim(?1))",
    ) else {
        return Vec::new();
    };
    statement
        .query_map([artist], |row| {
            Ok(Record {
                track_key: key(&row.get::<_, String>(0)?),
                track_name: row.get(1)?,
                listeners: row.get(2)?,
                play_count: row.get(3)?,
                available: row.get::<_, String>(4)? == "available",
                fresh: true,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Aurora's or Aurora NG's cache.
fn cache_records(path: &Path, artist: &str) -> Vec<Record> {
    let Some(connection) = open_read_only(path) else {
        return Vec::new();
    };
    let Ok(mut statement) = connection.prepare(
        "SELECT track_key, track_name, listeners, play_count, state, expires_at_ms
         FROM track_popularity WHERE artist_key = ?1",
    ) else {
        return Vec::new();
    };
    let now = now_ms();
    statement
        .query_map([key(artist)], |row| {
            Ok(Record {
                track_key: row.get(0)?,
                track_name: row.get(1)?,
                listeners: row.get(2)?,
                play_count: row.get(3)?,
                available: row.get::<_, String>(4)? == "available",
                fresh: row.get::<_, i64>(5)? > now,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn open_own_cache(path: &Path) -> Result<Connection, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("Could not create {}: {error}", dir.display()))?;
    }
    let connection = Connection::open(path)
        .map_err(|error| format!("Could not open the Last.fm cache: {error}"))?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .and_then(|_| {
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS track_popularity (
                   artist_key TEXT NOT NULL,
                   track_key TEXT NOT NULL,
                   track_name TEXT NOT NULL,
                   listeners INTEGER,
                   play_count INTEGER,
                   state TEXT NOT NULL CHECK (state IN ('available', 'unavailable')),
                   expires_at_ms INTEGER NOT NULL,
                   PRIMARY KEY (artist_key, track_key)
                 );",
            )
        })
        .map_err(|error| format!("Could not prepare the Last.fm cache: {error}"))?;
    Ok(connection)
}

fn save(cache: &Connection, artist: &str, record: &Record) {
    let ttl = if record.available {
        AVAILABLE_TTL_MS
    } else {
        UNAVAILABLE_TTL_MS
    };
    // A cache that cannot be written only means asking Last.fm again later.
    let _ = cache.execute(
        "INSERT INTO track_popularity
           (artist_key, track_key, track_name, listeners, play_count, state, expires_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(artist_key, track_key) DO UPDATE SET
           track_name = excluded.track_name, listeners = excluded.listeners,
           play_count = excluded.play_count, state = excluded.state,
           expires_at_ms = excluded.expires_at_ms",
        rusqlite::params![
            key(artist),
            record.track_key,
            record.track_name,
            record.listeners,
            record.play_count,
            if record.available {
                "available"
            } else {
                "unavailable"
            },
            now_ms() + ttl,
        ],
    );
}

fn fetch(artist: &str, title: &str, api_key: &str) -> Result<Record, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        // Last.fm answers errors as JSON with a 4xx status.
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .get(API_URL)
        .header("User-Agent", USER_AGENT)
        .query("method", "track.getInfo")
        .query("artist", artist)
        .query("track", title)
        .query("autocorrect", "1")
        .query("api_key", api_key)
        .query("format", "json")
        .call()
        .map_err(|error| format!("Last.fm could not be reached: {error}"))?;
    let status = response.status().as_u16();
    let value: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|_| format!("Last.fm answered {status} without track details."))?;
    parse(title, &value)
}

/// Last.fm's answer as a record; "track not found" is a record too, so it
/// is not asked again for a while.
fn parse(title: &str, value: &serde_json::Value) -> Result<Record, String> {
    if let Some(error) = value.get("error") {
        if error.as_i64() == Some(6) {
            return Ok(Record {
                track_key: key(title),
                track_name: title.to_owned(),
                listeners: None,
                play_count: None,
                available: false,
                fresh: true,
            });
        }
        return Err(value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Last.fm turned the lookup down.")
            .to_owned());
    }
    let track = value.get("track").ok_or("Last.fm left out the track.")?;
    // Counts come as strings ("4982"), sometimes as numbers.
    let count = |name: &str| {
        track
            .get(name)
            .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
            .filter(|count| *count >= 0)
    };
    Ok(Record {
        track_key: key(title),
        track_name: track
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(title)
            .to_owned(),
        listeners: count("listeners"),
        play_count: count("playcount"),
        available: true,
        fresh: true,
    })
}

/// The Last.fm API key: `LAST_FM` in the environment, `.env.local` in the
/// source folder for development builds, or the key Aurora or Music
/// Library saved in Windows Credential Manager.
pub fn api_key() -> Option<String> {
    let usable = |value: String| {
        let value = value.trim().to_owned();
        (value.len() >= 16 && value.chars().all(|c| c.is_ascii_alphanumeric())).then_some(value)
    };
    if let Some(value) = std::env::var("LAST_FM").ok().and_then(usable) {
        return Some(value);
    }
    if cfg!(debug_assertions) {
        let env_file = Path::new(env!("CARGO_MANIFEST_DIR")).join(".env.local");
        if let Some(value) = std::fs::read_to_string(env_file)
            .ok()
            .and_then(|text| env_value(&text, "LAST_FM"))
            .and_then(usable)
        {
            return Some(value);
        }
    }
    #[cfg(windows)]
    for target in SAVED_KEYS {
        if let Some(value) = saved_credential(target).and_then(usable) {
            return Some(value);
        }
    }
    None
}

/// `NAME=value` from a `.env` file.
fn env_value(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (left, right) = line.split_once('=')?;
        (left.trim() == name).then(|| right.trim().trim_matches(['"', '\'']).to_owned())
    })
}

/// A generic credential's secret, which `keyring` saves as UTF-16.
#[cfg(windows)]
fn saved_credential(target: &str) -> Option<String> {
    use windows::{
        Win32::Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW},
        core::HSTRING,
    };
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: CredReadW fills `credential` on success; it is read once and
    // freed with CredFree.
    unsafe {
        CredReadW(
            &HSTRING::from(target),
            CRED_TYPE_GENERIC,
            None,
            &mut credential,
        )
        .ok()?;
        let blob = std::slice::from_raw_parts(
            (*credential).CredentialBlob,
            (*credential).CredentialBlobSize as usize,
        )
        .to_vec();
        CredFree(credential as *const _);
        decode_secret(&blob)
    }
}

/// UTF-16 when every other byte is zero, as `keyring` writes it; UTF-8 otherwise.
fn decode_secret(blob: &[u8]) -> Option<String> {
    if blob.len().is_multiple_of(2) && blob.iter().skip(1).step_by(2).all(|&byte| byte == 0) {
        let units: Vec<u16> = blob
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).ok()
    } else {
        String::from_utf8(blob.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(title: &str) -> Lookup {
        Lookup {
            title: title.to_owned(),
            artist: "A.R. Rahman".to_owned(),
        }
    }

    fn record(title: &str, listeners: Option<i64>, plays: Option<i64>) -> Record {
        Record {
            track_key: key(title),
            track_name: title.to_owned(),
            listeners,
            play_count: plays,
            available: listeners.is_some(),
            fresh: true,
        }
    }

    #[test]
    fn the_three_most_listened_tracks_get_flames() {
        let tracks: Vec<_> = ["Sajna", "Kurukuru Kan", "Suite", "NaNa", "Mr. Kadam"]
            .map(lookup)
            .to_vec();
        let records = [
            record("sajna", Some(725), Some(4982)),
            record("Kurukuru Kan", Some(519), Some(2501)),
            record("Suite", Some(261), Some(823)),
            record("NaNa", Some(601), Some(3711)),
            record("Mr. Kadam", None, None),
        ];
        assert_eq!(
            rank(&tracks, &records),
            [Some(1), Some(3), None, Some(2), None]
        );
    }

    #[test]
    fn ties_go_to_plays_then_album_order_and_unknowns_never_rank() {
        let tracks: Vec<_> = ["A", "B", "C", "D"].map(lookup).to_vec();
        let records = [
            record("A", Some(10), Some(5)),
            record("B", Some(10), Some(9)),
            record("C", Some(10), Some(5)),
            record("D", Some(0), Some(100)),
        ];
        assert_eq!(rank(&tracks, &records), [Some(2), Some(1), Some(3), None]);
    }

    #[test]
    fn titles_match_loosely_only_when_unambiguous() {
        let records = [record("Mr.. Kadam", Some(1), None)];
        assert!(matching(&records, "Mr Kadam").is_some());
        assert!(matching(&records, "Rock\u{2019}n Roll").is_none());
        let twins = [
            record("Hey-You", Some(1), None),
            record("Hey You", Some(2), None),
        ];
        assert!(matching(&twins, "Hey: You").is_none());
        assert_eq!(matching(&twins, "hey you").unwrap().listeners, Some(2));
    }

    #[test]
    fn reads_counts_and_not_found_answers() {
        let found: serde_json::Value = serde_json::json!({
            "track": { "name": "Sajna", "listeners": "725", "playcount": 4982 }
        });
        let record = parse("sajna", &found).unwrap();
        assert_eq!(
            (record.listeners, record.play_count),
            (Some(725), Some(4982))
        );
        assert!(record.available);

        let missing = serde_json::json!({ "error": 6, "message": "Track not found" });
        assert!(!parse("Nope", &missing).unwrap().available);

        let bad_key = serde_json::json!({ "error": 10, "message": "Invalid API key" });
        assert_eq!(parse("x", &bad_key).unwrap_err(), "Invalid API key");
    }

    #[test]
    fn caches_are_merged_and_saved_counts_rank_the_album() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("own.sqlite3");
        let cache = open_own_cache(&own).unwrap();
        save(
            &cache,
            "A.R. Rahman",
            &record("Sajna", Some(725), Some(4982)),
        );
        save(
            &cache,
            "A.R. Rahman",
            &record("Nana", Some(601), Some(3711)),
        );
        drop(cache);
        let sources = Sources {
            catalog: None,
            aurora: Some(dir.path().join("missing.sqlite3")),
            own: Some(own),
        };
        let tracks = [lookup("Sajna"), lookup("Itinerary"), lookup("Nana")];
        assert_eq!(
            sources.cached_ranks("a.r. rahman", &tracks),
            [Some(1), None, Some(2)]
        );
    }

    #[test]
    fn reads_keys_from_env_files_and_credential_blobs() {
        let text = "OTHER=1\nLAST_FM = \"abc123\"\nLAST_FM_SECRET=zzz\n";
        assert_eq!(env_value(text, "LAST_FM").as_deref(), Some("abc123"));
        let utf16: Vec<u8> = "abc1".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_secret(&utf16).as_deref(), Some("abc1"));
        assert_eq!(decode_secret(b"abc1").as_deref(), Some("abc1"));
    }

    /// Asks Last.fm with the key Aurora saved in Credential Manager.
    #[test]
    #[ignore = "talks to Last.fm"]
    #[cfg(windows)]
    fn live_lookup_with_auroras_saved_key() {
        let key = SAVED_KEYS
            .iter()
            .find_map(|target| saved_credential(target))
            .unwrap();
        let record = fetch("A.R. Rahman", "Sajna", &key).unwrap();
        assert!(
            record.available && record.listeners.unwrap() > 0,
            "{record:?}"
        );
    }
}
