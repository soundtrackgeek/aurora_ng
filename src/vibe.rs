//! `vibe:` and `sort:vibe`: albums that sound like a description.
//!
//! Aurora 0.66.0 and newer keep an average CLAP embedding per album in their
//! sound descriptor file (`sound_album_vectors`), and the embedding of every
//! description searched for (`sound_queries`). Aurora NG reads both through
//! the attached `sound` database. A description Aurora has not seen yet goes
//! to Music Library's CLAP worker, which turns it into an embedding in about
//! a second. An album matches when it is in the best-fitting 2%.

use base64::Engine;
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags};
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// The share of albums `vibe:` keeps, as in Aurora.
const SHARE: f64 = 0.02;
const MAX_CHARS: usize = 200;
const DIMENSIONS: usize = 512;
/// Scores are kept as integers, like Aurora's.
const SCALE: f32 = 1_000_000.0;
/// The CLAP model Aurora's vectors and saved descriptions come from.
const CLAP_MODEL: &str = "clap-lmas-e9fd5ac-3x10s-v1";
const MODEL_FOLDER: &str = "clap-model/larger_clap_music_and_speech-e9fd5ac";
const WORKER_TIMEOUT: Duration = Duration::from_secs(120);
const TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CACHED: usize = 8;

/// Album id to score, for the best share of albums.
pub type Scores = Arc<HashMap<String, i64>>;

/// Lowercase, single spaces, 1 to 200 characters: as Aurora writes it.
pub fn normalize(value: &str) -> Result<String, String> {
    let text = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if text.is_empty() {
        return Err("Describe the sound after vibe:, like vibe:\"dark ambient drone\".".to_owned());
    }
    if text.chars().count() > MAX_CHARS {
        return Err(format!(
            "A vibe: description can be at most {MAX_CHARS} characters."
        ));
    }
    Ok(text)
}

/// SQL for an album's score, NULL outside the best share. The scores must be
/// registered on the connection first (`register`).
pub fn album_sql(text: &str, album_id: &str) -> String {
    format!("ng_vibe('{}', {album_id})", text.replace('\'', "''"))
}

/// Makes `ng_vibe(text, album_id)` answer from these scores.
pub fn register(connection: &Connection, scores: HashMap<String, Scores>) -> Result<(), String> {
    connection
        .create_scalar_function(
            "ng_vibe",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| {
                let text = context.get_raw(0).as_str()?;
                let id = context.get_raw(1).as_str_or_null()?;
                Ok(id.and_then(|id| scores.get(text)?.get(id).copied()))
            },
        )
        .map_err(|error| format!("Could not search: {error}"))
}

fn score_cache() -> &'static Mutex<HashMap<String, (Instant, Scores)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Scores)>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// The best share of albums for a normalized description.
pub fn scores(connection: &Connection, text: &str) -> Result<Scores, String> {
    if let Some((_, scores)) = score_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(text)
        .filter(|(at, _)| at.elapsed() < TTL)
    {
        return Ok(scores.clone());
    }
    if !has_vectors(connection)? {
        return Err(not_ready());
    }
    let query = text_embedding(connection, text)?;
    let scores = Arc::new(score_albums(connection, &query)?);
    let mut cache = score_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if cache.len() >= MAX_CACHED
        && let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(key, _)| key.clone())
    {
        cache.remove(&oldest);
    }
    cache.insert(text.to_owned(), (Instant::now(), scores.clone()));
    Ok(scores)
}

fn not_ready() -> String {
    "vibe: needs Aurora 0.66.0 or newer, which averages each album's sound a few minutes after it starts.".to_owned()
}

fn has_vectors(connection: &Connection) -> Result<bool, String> {
    if !crate::sound::attached(connection)? {
        return Ok(false);
    }
    connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sound.sqlite_master WHERE type = 'table' AND name = 'sound_album_vectors')
             AND EXISTS (SELECT 1 FROM sound.sound_album_vectors)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not search: {error}"))
}

fn score_albums(connection: &Connection, query: &[f32]) -> Result<HashMap<String, i64>, String> {
    let failed = |error: rusqlite::Error| format!("Could not read album sounds: {error}");
    let mut statement = connection
        .prepare("SELECT album_id, vector FROM sound.sound_album_vectors")
        .map_err(failed)?;
    let mut rows = statement.query([]).map_err(failed)?;
    let mut scored = Vec::new();
    while let Some(row) = rows.next().map_err(failed)? {
        let bytes = row
            .get_ref(1)
            .and_then(|value| value.as_blob().map_err(Into::into))
            .map_err(failed)?;
        if let Some(score) = dot_f16(bytes, query) {
            scored.push((row.get::<_, String>(0).map_err(failed)?, score));
        }
    }
    if scored.is_empty() {
        return Err(not_ready());
    }
    Ok(best_share(scored).into_iter().collect())
}

/// The best `SHARE` of `scored`, at least one, as integer scores.
fn best_share<K>(mut scored: Vec<(K, f32)>) -> Vec<(K, i64)> {
    if scored.is_empty() {
        return Vec::new();
    }
    let keep = ((scored.len() as f64 * SHARE).ceil() as usize).clamp(1, scored.len());
    scored.select_nth_unstable_by(keep - 1, |a, b| b.1.total_cmp(&a.1));
    scored.truncate(keep);
    scored
        .into_iter()
        .map(|(key, score)| (key, (score * SCALE).round() as i64))
        .collect()
}

/// A half float as f32.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let fraction = u32::from(bits & 0x3ff);
    let value = match exponent {
        0 if fraction == 0 => sign,
        // Subnormal: fraction × 2^-24.
        0 => {
            let magnitude = fraction as f32 * 2f32.powi(-24);
            return if sign == 0 { magnitude } else { -magnitude };
        }
        0x1f => sign | 0x7f80_0000 | (fraction << 13),
        _ => sign | ((exponent + 127 - 15) << 23) | (fraction << 13),
    };
    f32::from_bits(value)
}

fn half_table() -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| (0..=u16::MAX).map(f16_to_f32).collect())
}

/// The dot product of a little-endian half-float vector with `query`.
fn dot_f16(bytes: &[u8], query: &[f32]) -> Option<f32> {
    if bytes.len() != query.len() * 2 {
        return None;
    }
    let table = half_table();
    Some(
        bytes
            .chunks_exact(2)
            .zip(query)
            .map(|(pair, value)| table[usize::from(u16::from_le_bytes([pair[0], pair[1]]))] * value)
            .sum(),
    )
}

fn text_cache() -> &'static Mutex<HashMap<String, Vec<f32>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<f32>>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// The description's CLAP embedding: remembered, saved by Aurora, or from
/// Music Library's worker.
fn text_embedding(connection: &Connection, text: &str) -> Result<Vec<f32>, String> {
    let remembered = || {
        text_cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(text)
            .cloned()
    };
    if let Some(found) = remembered() {
        return Ok(found);
    }
    // One worker at a time; a search waiting here may find its answer.
    static WORKER: Mutex<()> = Mutex::new(());
    let _running = WORKER.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(found) = remembered() {
        return Ok(found);
    }
    let embedding = match saved_query(connection, text)? {
        Some(saved) => saved,
        None => run_worker(text)?,
    };
    text_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(text.to_owned(), embedding.clone());
    Ok(embedding)
}

/// A description Aurora already turned into an embedding.
fn saved_query(connection: &Connection, text: &str) -> Result<Option<Vec<f32>>, String> {
    let present: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sound.sqlite_master WHERE type = 'table' AND name = 'sound_queries')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not search: {error}"))?;
    if !present {
        return Ok(None);
    }
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT embedding FROM sound.sound_queries WHERE query = ?1 AND model = ?2",
            [text, CLAP_MODEL],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("Could not search: {error}"))?;
    Ok(bytes
        .filter(|bytes| bytes.len() == DIMENSIONS * 2)
        .map(|bytes| {
            bytes
                .chunks_exact(2)
                .map(|pair| f16_to_f32(u16::from_le_bytes([pair[0], pair[1]])))
                .collect()
        }))
}

/// `%LOCALAPPDATA%\Music Library\music-clap-worker.exe`, next to Music Library.
fn worker_path() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("Music Library")
            .join("music-clap-worker.exe"),
    )
}

fn run_worker(text: &str) -> Result<Vec<f32>, String> {
    let missing_worker = "vibe: needs Music Library's sound search worker, which comes with Music Library 0.216.0 or newer.";
    let worker = worker_path()
        .filter(|path| path.is_file())
        .ok_or(missing_worker)?;
    let model = crate::library::catalog_path()?
        .parent()
        .ok_or("Music Library's data folder is missing.")?
        .join(MODEL_FOLDER);
    if !model.is_dir() {
        return Err("vibe: needs Music Library's sound search model. Run audio analysis in Music Library once so it downloads it.".to_owned());
    }
    let mut command = Command::new(&worker);
    command
        .arg("--text")
        .arg(&model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start Music Library's sound search worker: {error}"))?;
    child
        .stdin
        .take()
        .ok_or("The sound search worker has no input")?
        .write_all(text.as_bytes())
        .map_err(|error| format!("Could not ask the sound search worker: {error}"))?;
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(child.wait_with_output());
    });
    let output = receive
        .recv_timeout(WORKER_TIMEOUT)
        .map_err(|_| "The sound search worker did not answer within two minutes.".to_owned())?
        .map_err(|error| format!("The sound search worker failed: {error}"))?;
    if !output.status.success() {
        let message: String = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(500)
            .collect();
        return Err(format!("The sound search worker failed: {message}"));
    }
    parse_worker_reply(&output.stdout)
}

/// `{"embedding": "<base64 little-endian f32>"}`.
fn parse_worker_reply(stdout: &[u8]) -> Result<Vec<f32>, String> {
    let unreadable = || "The sound search worker sent an unreadable answer.".to_owned();
    let reply: serde_json::Value = serde_json::from_slice(stdout).map_err(|_| unreadable())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(reply["embedding"].as_str().ok_or_else(unreadable)?)
        .map_err(|_| unreadable())?;
    if bytes.len() != DIMENSIONS * 4 {
        return Err(unreadable());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Half floats for values that are exact in them.
    fn f16_bytes(values: &[f32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| {
                let bits = (0..=u16::MAX)
                    .find(|&bits| f16_to_f32(bits) == *value)
                    .expect("exact in half precision");
                bits.to_le_bytes()
            })
            .collect()
    }

    fn axis(value: f32) -> Vec<f32> {
        let mut vector = vec![0.0; DIMENSIONS];
        vector[0] = value;
        vector
    }

    /// Adds album vectors and a saved description to a cache from
    /// `sound::tests::write_cache`. Album `i` of `albums` points along the
    /// description with strength `(i + 1) / 128`, so the last fits best.
    pub fn write_vectors(path: &std::path::Path, text: &str, albums: &[&str]) {
        let cache = Connection::open(path).unwrap();
        cache
            .execute_batch(
                "CREATE TABLE sound_album_vectors(album_id TEXT PRIMARY KEY, vector BLOB NOT NULL);
                 CREATE TABLE sound_queries(query TEXT NOT NULL, model TEXT NOT NULL, embedding BLOB NOT NULL, PRIMARY KEY(query, model));",
            )
            .unwrap();
        for (index, album) in albums.iter().enumerate() {
            cache
                .execute(
                    "INSERT INTO sound_album_vectors VALUES (?1, ?2)",
                    rusqlite::params![album, f16_bytes(&axis((index + 1) as f32 / 128.0))],
                )
                .unwrap();
        }
        cache
            .execute(
                "INSERT INTO sound_queries VALUES (?1, ?2, ?3)",
                rusqlite::params![text, CLAP_MODEL, f16_bytes(&axis(1.0))],
            )
            .unwrap();
    }

    #[test]
    fn normalizes_descriptions() {
        assert_eq!(
            normalize("  Dark   AMBIENT\tdrone "),
            Ok("dark ambient drone".to_owned())
        );
        assert!(normalize("   ").is_err());
        assert!(normalize(&"x".repeat(201)).is_err());
        assert!(normalize(&"ø".repeat(200)).is_ok());
    }

    #[test]
    fn keeps_the_best_share() {
        let scored: Vec<(u32, f32)> = (0..100).map(|i| (i, i as f32 / 100.0)).collect();
        let mut kept: Vec<u32> = best_share(scored).into_iter().map(|(key, _)| key).collect();
        kept.sort();
        assert_eq!(kept, [98, 99]);
        assert_eq!(best_share(vec![("only", 0.5)]), [("only", 500_000)]);
    }

    #[test]
    fn reads_the_workers_embedding() {
        let values: Vec<f32> = (0..DIMENSIONS).map(|i| i as f32 / 512.0).collect();
        let bytes: Vec<u8> = values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let reply = format!(
            "{{\"embedding\":\"{}\"}}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        assert_eq!(parse_worker_reply(reply.as_bytes()), Ok(values));
        assert!(parse_worker_reply(b"{\"embedding\":\"AAAA\"}").is_err());
        assert!(parse_worker_reply(b"not json").is_err());
    }

    #[test]
    fn half_floats_read_back() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert!(f16_to_f32(0x7c00).is_infinite());
    }
}
