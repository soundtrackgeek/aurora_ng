//! Listening history, kept the way Aurora keeps it so that Aurora, Aurora NG
//! and Music Library all see the same plays.
//!
//! Aurora NG writes its own history database with Aurora's schema
//! (`%LOCALAPPDATA%\aurora_ng\aurora-history.sqlite3`) under its own device ID,
//! and publishes a copy as `aurora-history-{device}.sqlite3` in
//! `OneDrive\_musicbackup`, where Aurora picks up every device's history.
//! It reads the other histories the same way, never writing them: Aurora's
//! own database on this computer, Music Library's player (Tonehavn), and the
//! other devices' OneDrive copies. Its registered plays also go to Music
//! Library's listening history through the `recordPlays` bridge operation.
//!
//! A play registers once 30 seconds have been listened to (or the whole
//! track, when it is shorter). Only forward playback counts: a seek moves the
//! baseline, so skipping ahead adds nothing.

use crate::{library::Track, music_library};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Aurora's history schema version; other versions are not read.
const SCHEMA_VERSION: i64 = 1;
const DEFAULT_THRESHOLD_SECONDS: u32 = 30;
/// The listened time is saved every 30 seconds, as in Aurora.
const CHECKPOINT_SECONDS: f64 = 30.0;
/// The OneDrive copy is refreshed at most this often while playing.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);
/// Plays go to Music Library in batches, at most this often.
const LIBRARY_INTERVAL: Duration = Duration::from_secs(120);
const LIBRARY_RETRY_AFTER_ERROR: Duration = Duration::from_secs(15 * 60);
const LIBRARY_BATCH: usize = 1_000;
const TONEHAVN_FILES: [&str; 2] = [
    "aurora-history-tonehavn-local.sqlite3",
    "aurora-history-tonehavn-ios.sqlite3",
];

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS history_meta (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  play_threshold_seconds INTEGER NOT NULL DEFAULT 30
    CHECK (play_threshold_seconds BETWEEN 1 AND 3600),
  content_revision INTEGER NOT NULL DEFAULT 0 CHECK (content_revision >= 0),
  mirrored_revision INTEGER NOT NULL DEFAULT 0 CHECK (mirrored_revision >= 0),
  last_synced_at_ms INTEGER
);
CREATE TABLE IF NOT EXISTS listening_sessions (
  session_id TEXT PRIMARY KEY,
  track_key TEXT NOT NULL,
  title TEXT NOT NULL,
  artist TEXT NOT NULL,
  album TEXT NOT NULL,
  genre TEXT,
  directory TEXT NOT NULL,
  filename TEXT NOT NULL,
  duration_seconds INTEGER,
  started_at_ms INTEGER NOT NULL,
  ended_at_ms INTEGER,
  listened_seconds REAL NOT NULL DEFAULT 0 CHECK (listened_seconds >= 0),
  registered_play INTEGER NOT NULL DEFAULT 0 CHECK (registered_play IN (0, 1)),
  registered_at_ms INTEGER,
  threshold_seconds INTEGER NOT NULL CHECK (threshold_seconds BETWEEN 1 AND 3600),
  outcome TEXT NOT NULL DEFAULT 'active'
    CHECK (outcome IN ('active', 'completed', 'skipped', 'interrupted'))
);
CREATE INDEX IF NOT EXISTS idx_history_started
  ON listening_sessions(started_at_ms DESC, session_id DESC);
CREATE INDEX IF NOT EXISTS idx_history_track_plays
  ON listening_sessions(track_key, registered_play, started_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_history_outcome
  ON listening_sessions(outcome, started_at_ms DESC);
"#;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as i64)
}

/// Aurora's stable track identity: the lowercased Windows path, so a play
/// is still the same track after Music Library renumbers its catalog.
pub fn track_key(directory: &str, filename: &str) -> String {
    let directory = directory.trim().replace('/', "\\");
    let directory = directory.strip_prefix("\\\\?\\").unwrap_or(&directory);
    format!("{}\\{}", directory.trim_end_matches('\\'), filename.trim()).to_lowercase()
}

/// What a session remembers about its track, so the history still reads
/// well after the track is renamed or removed.
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: Option<String>,
    pub directory: String,
    pub filename: String,
    pub duration_seconds: Option<i64>,
}

impl TrackInfo {
    pub fn new(track: &Track, album: &str, genre: Option<&str>) -> Self {
        let directory = track
            .path
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        let filename = track
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            key: track_key(&directory, &filename),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: album.to_owned(),
            genre: genre.map(str::to_owned),
            directory,
            filename,
            duration_seconds: track.duration_seconds,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Active,
    /// The track played to its end.
    Completed,
    /// Something else was played before the end.
    Skipped,
    /// The app closed during the track.
    Interrupted,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Active => "active",
            Outcome::Completed => "completed",
            Outcome::Skipped => "skipped",
            Outcome::Interrupted => "interrupted",
        }
    }
}

/// One listen to a track, from when it starts until something else plays.
#[derive(Clone, Debug)]
pub struct Session {
    id: String,
    track: TrackInfo,
    started_at_ms: i64,
    listened_seconds: f64,
    last_position: f64,
    threshold_seconds: u32,
    /// The threshold, or the track's length when that is shorter.
    needed_seconds: f64,
    registered_at_ms: Option<i64>,
    checkpoint_bucket: u64,
}

/// Everything a session knows, to be saved; later checkpoints of the same
/// session replace earlier ones.
#[derive(Clone, Debug)]
pub struct Checkpoint {
    session_id: String,
    track: TrackInfo,
    started_at_ms: i64,
    ended_at_ms: Option<i64>,
    listened_seconds: f64,
    registered_at_ms: Option<i64>,
    threshold_seconds: u32,
    outcome: Outcome,
}

impl Session {
    fn new(device_id: &str, track: TrackInfo, threshold_seconds: u32) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let started_at_ms = now_ms();
        let needed_seconds = match track.duration_seconds {
            Some(duration) if duration > 0 => (threshold_seconds as f64).min(duration as f64),
            _ => threshold_seconds as f64,
        }
        .max(1.0);
        Self {
            id: format!(
                "session-{device_id}-{started_at_ms}-{}",
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            track,
            started_at_ms,
            listened_seconds: 0.0,
            last_position: 0.0,
            threshold_seconds,
            needed_seconds,
            registered_at_ms: None,
            checkpoint_bucket: 0,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn track_key(&self) -> &str {
        &self.track.key
    }

    pub fn listened_seconds(&self) -> f64 {
        self.listened_seconds
    }

    pub fn registered(&self) -> bool {
        self.registered_at_ms.is_some()
    }

    /// Counts playback up to `position`. Returns a checkpoint to save when
    /// the play just registered or another 30 seconds have been listened to.
    pub fn observe(&mut self, position: f64) -> Option<Checkpoint> {
        let position = position.max(0.0);
        let delta = position - self.last_position;
        self.last_position = position;
        if delta <= 0.0 {
            return None;
        }
        self.listened_seconds += delta;
        let registered = self.register_if_played();
        let bucket = (self.listened_seconds / CHECKPOINT_SECONDS).floor() as u64;
        if !registered && bucket == self.checkpoint_bucket {
            return None;
        }
        self.checkpoint_bucket = bucket;
        Some(self.checkpoint(Outcome::Active))
    }

    /// After a seek: playback continues from `position`, and the distance
    /// jumped is not counted.
    pub fn reset_position(&mut self, position: f64) {
        self.last_position = position.max(0.0);
    }

    /// The track played to its end: the stretch since the last observation
    /// counts too.
    pub fn complete(&mut self) -> Checkpoint {
        if let Some(duration) = self.track.duration_seconds.filter(|&d| d > 0) {
            let rest = duration as f64 - self.last_position;
            if rest > 0.0 {
                self.listened_seconds += rest;
                self.last_position = duration as f64;
            }
        }
        self.register_if_played();
        let mut checkpoint = self.checkpoint(Outcome::Completed);
        checkpoint.ended_at_ms = Some(now_ms());
        checkpoint
    }

    pub fn finish(&self, outcome: Outcome) -> Checkpoint {
        let mut checkpoint = self.checkpoint(outcome);
        checkpoint.ended_at_ms = Some(now_ms());
        checkpoint
    }

    fn register_if_played(&mut self) -> bool {
        if self.registered_at_ms.is_none()
            && self.listened_seconds + f64::EPSILON >= self.needed_seconds
        {
            self.registered_at_ms = Some(now_ms());
            true
        } else {
            false
        }
    }

    fn checkpoint(&self, outcome: Outcome) -> Checkpoint {
        Checkpoint {
            session_id: self.id.clone(),
            track: self.track.clone(),
            started_at_ms: self.started_at_ms,
            ended_at_ms: None,
            listened_seconds: self.listened_seconds,
            registered_at_ms: self.registered_at_ms,
            threshold_seconds: self.threshold_seconds,
            outcome,
        }
    }
}

/// A track's plays and listening time across every history.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub plays: i64,
    pub listened_seconds: f64,
}

impl Stats {
    fn add(&mut self, plays: i64, listened_seconds: f64) {
        self.plays = self.plays.saturating_add(plays);
        self.listened_seconds += listened_seconds.max(0.0);
    }
}

/// Where the histories are.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Aurora NG's own history.
    pub local: PathBuf,
    /// Aurora NG's folder, for the Music Library cursor and staging files.
    pub work: PathBuf,
    /// `OneDrive\_musicbackup`, where every device publishes its history.
    pub remote: Option<PathBuf>,
    /// Aurora's history on this computer.
    pub aurora: Option<PathBuf>,
    /// Music Library's player (Tonehavn) histories on this computer.
    pub tonehavn: Option<PathBuf>,
    /// Whether registered plays are sent to Music Library.
    pub send_to_library: bool,
}

impl Paths {
    pub fn default_paths() -> Option<Self> {
        let work = crate::settings::data_dir()?;
        let app_data = std::env::var_os("APPDATA").map(PathBuf::from);
        Some(Self {
            local: work.join("aurora-history.sqlite3"),
            work,
            remote: std::env::var_os("USERPROFILE")
                .map(|profile| PathBuf::from(profile).join("OneDrive").join("_musicbackup")),
            aurora: app_data.as_ref().map(|dir| {
                dir.join("com.soundtrackgeek.aurora")
                    .join("aurora-history.sqlite3")
            }),
            tonehavn: app_data
                .map(|dir| dir.join("com.local.musiclibrary").join("tonehavn-history")),
            send_to_library: true,
        })
    }
}

enum Message {
    Save(Box<Checkpoint>),
    /// A track's plays, leaving out one session; answered after every save
    /// sent before it.
    Stats {
        track_key: String,
        except: Option<String>,
        reply: async_channel::Sender<Stats>,
    },
    /// Publish one last time, then answer and stop.
    Shutdown(mpsc::Sender<()>),
}

/// Aurora NG's listening history: saving happens on its own thread, in order.
pub struct History {
    device_id: String,
    threshold_seconds: u32,
    sender: mpsc::Sender<Message>,
    worker: Option<thread::JoinHandle<()>>,
}

impl History {
    pub fn open() -> Result<Self, String> {
        let paths = Paths::default_paths()
            .ok_or_else(|| "The LOCALAPPDATA folder is not set.".to_owned())?;
        Self::open_at(paths)
    }

    pub fn open_at(paths: Paths) -> Result<Self, String> {
        fs::create_dir_all(&paths.work)
            .map_err(|error| format!("Could not create Aurora NG's folder: {error}"))?;
        let device_id = match read_device_id(&paths.local) {
            Some(id) => id,
            None => {
                let id = new_device_id();
                restore_from_remote(&paths, &id);
                id
            }
        };
        let connection = open_local(&paths.local)?;
        migrate(&connection, &device_id, &device_name())?;
        recover_interrupted(&connection)?;
        let threshold_seconds = connection
            .query_row(
                "SELECT play_threshold_seconds FROM history_meta WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .ok()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| (1..=3600).contains(value))
            .unwrap_or(DEFAULT_THRESHOLD_SECONDS);

        let (sender, receiver) = mpsc::channel();
        let mut worker = Worker::new(connection, paths, device_id.clone());
        let worker = thread::Builder::new()
            .name("listening-history".to_owned())
            .spawn(move || worker.run(receiver))
            .map_err(|error| format!("Could not start the listening history: {error}"))?;
        Ok(Self {
            device_id,
            threshold_seconds,
            sender,
            worker: Some(worker),
        })
    }

    pub fn start_session(&self, track: TrackInfo) -> Session {
        Session::new(&self.device_id, track, self.threshold_seconds)
    }

    pub fn save(&self, checkpoint: Checkpoint) {
        let _ = self.sender.send(Message::Save(Box::new(checkpoint)));
    }

    /// A track's plays and listening time across every history, leaving out
    /// the session playing now (`except`), which the caller counts live.
    pub fn track_stats(
        &self,
        track_key: String,
        except: Option<String>,
    ) -> async_channel::Receiver<Stats> {
        let (reply, answer) = async_channel::bounded(1);
        let _ = self.sender.send(Message::Stats {
            track_key,
            except,
            reply,
        });
        answer
    }

    /// Saves what is left and refreshes the OneDrive copy, waiting at most
    /// `timeout` so closing the app never hangs on OneDrive.
    pub fn shutdown(&mut self, timeout: Duration) {
        let (done, finished) = mpsc::channel();
        if self.sender.send(Message::Shutdown(done)).is_ok() {
            let _ = finished.recv_timeout(timeout);
        }
        self.worker.take();
    }
}

// ── Saving ──────────────────────────────────────────────────────────────

struct Worker {
    connection: Connection,
    paths: Paths,
    device_id: String,
    peers: PeerIndex,
    last_publish: Option<Instant>,
    /// When to send plays to Music Library next.
    next_library_send: Instant,
    library_send: Option<thread::JoinHandle<Result<(), String>>>,
}

impl Worker {
    fn new(connection: Connection, paths: Paths, device_id: String) -> Self {
        Self {
            connection,
            paths,
            device_id,
            peers: PeerIndex::default(),
            last_publish: None,
            // Plays left from last time go out once the app has settled.
            next_library_send: Instant::now() + Duration::from_secs(20),
            library_send: None,
        }
    }

    fn run(&mut self, receiver: mpsc::Receiver<Message>) {
        loop {
            match receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(Message::Save(checkpoint)) => {
                    let _ = persist(&self.connection, &checkpoint);
                }
                Ok(Message::Stats {
                    track_key,
                    except,
                    reply,
                }) => {
                    let _ = reply.try_send(self.track_stats(&track_key, except.as_deref()));
                    continue;
                }
                Ok(Message::Shutdown(done)) => {
                    let _ = self.publish();
                    let _ = done.send(());
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = self.publish();
                    return;
                }
            }
            if self
                .last_publish
                .is_none_or(|last| last.elapsed() >= PUBLISH_INTERVAL)
            {
                self.last_publish = Some(Instant::now());
                let _ = self.publish();
            }
            self.send_to_library();
        }
    }

    /// Music Library takes a few seconds to start, so the plays go out on a
    /// thread of their own while saving and lookups carry on.
    fn send_to_library(&mut self) {
        if self
            .library_send
            .as_ref()
            .is_some_and(|send| send.is_finished())
        {
            let result = self.library_send.take().map(thread::JoinHandle::join);
            self.next_library_send = Instant::now()
                + match result {
                    Some(Ok(Ok(()))) => LIBRARY_INTERVAL,
                    _ => LIBRARY_RETRY_AFTER_ERROR,
                };
        }
        if !self.paths.send_to_library
            || self.library_send.is_some()
            || Instant::now() < self.next_library_send
        {
            return;
        }
        let (local, work) = (self.paths.local.clone(), self.paths.work.clone());
        self.library_send = thread::Builder::new()
            .name("music-library-plays".to_owned())
            .spawn(move || {
                let connection = open_local(&local)?;
                send_plays(&connection, &work, music_library::record_plays)
            })
            .ok();
    }

    fn publish(&self) -> Result<(), String> {
        match &self.paths.remote {
            Some(remote) => publish(&self.connection, &self.paths.work, remote, &self.device_id),
            None => Ok(()),
        }
    }
}

fn open_local(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(path)
        .map_err(|error| format!("Could not open the listening history: {error}"))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .and_then(|()| connection.pragma_update(None, "journal_mode", "WAL"))
        .map_err(|error| format!("Could not set up the listening history: {error}"))?;
    Ok(connection)
}

fn migrate(connection: &Connection, device_id: &str, device_name: &str) -> Result<(), String> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| format!("Could not read the listening history: {error}"))?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "The listening history uses a newer format ({version}); update Aurora NG."
        ));
    }
    connection
        .execute_batch(SCHEMA)
        .map_err(|error| format!("Could not set up the listening history: {error}"))?;
    let existing: Option<String> = connection
        .query_row(
            "SELECT device_id FROM history_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("Could not read the listening history: {error}"))?;
    match existing {
        Some(existing) if existing != device_id => {
            return Err("The listening history belongs to another device.".to_owned());
        }
        Some(_) => connection.execute(
            "UPDATE history_meta SET device_name = ?1 WHERE singleton = 1",
            [device_name],
        ),
        None => connection.execute(
            "INSERT INTO history_meta (singleton, device_id, device_name, play_threshold_seconds)
             VALUES (1, ?1, ?2, ?3)",
            params![device_id, device_name, DEFAULT_THRESHOLD_SECONDS],
        ),
    }
    .map_err(|error| format!("Could not set up the listening history: {error}"))?;
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| format!("Could not set up the listening history: {error}"))
}

/// A session still active from last time was cut off when the app stopped.
fn recover_interrupted(connection: &Connection) -> Result<(), String> {
    let changed = connection
        .execute(
            "UPDATE listening_sessions SET ended_at_ms = ?1, outcome = 'interrupted'
             WHERE outcome = 'active'",
            [now_ms()],
        )
        .map_err(|error| format!("Could not tidy the listening history: {error}"))?;
    if changed > 0 {
        bump_revision(connection)?;
    }
    Ok(())
}

fn bump_revision(connection: &Connection) -> Result<(), String> {
    connection
        .execute(
            "UPDATE history_meta SET content_revision = content_revision + 1 WHERE singleton = 1",
            [],
        )
        .map(|_| ())
        .map_err(|error| format!("Could not save the listening history: {error}"))
}

/// Saves a checkpoint exactly as Aurora does: values only grow, and a
/// finished session is never reopened.
fn persist(connection: &Connection, checkpoint: &Checkpoint) -> Result<(), String> {
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
            .map_err(|error| format!("Could not save the listening history: {error}"))?;
    let track = &checkpoint.track;
    let changed = transaction
        .execute(
            r#"
            INSERT INTO listening_sessions(
              session_id, track_key, title, artist, album, genre, directory, filename,
              duration_seconds, started_at_ms, ended_at_ms, listened_seconds,
              registered_play, registered_at_ms, threshold_seconds, outcome
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(session_id) DO UPDATE SET
              ended_at_ms = excluded.ended_at_ms,
              listened_seconds = MAX(listening_sessions.listened_seconds, excluded.listened_seconds),
              registered_play = MAX(listening_sessions.registered_play, excluded.registered_play),
              registered_at_ms = COALESCE(listening_sessions.registered_at_ms, excluded.registered_at_ms),
              threshold_seconds = excluded.threshold_seconds,
              outcome = excluded.outcome
            WHERE listening_sessions.outcome = 'active'
            "#,
            params![
                checkpoint.session_id,
                track.key,
                track.title,
                track.artist,
                track.album,
                track.genre,
                track.directory,
                track.filename,
                track.duration_seconds,
                checkpoint.started_at_ms,
                checkpoint.ended_at_ms,
                checkpoint.listened_seconds,
                i64::from(checkpoint.registered_at_ms.is_some()),
                checkpoint.registered_at_ms,
                i64::from(checkpoint.threshold_seconds),
                checkpoint.outcome.as_str(),
            ],
        )
        .map_err(|error| format!("Could not save the listening history: {error}"))?;
    if changed > 0 {
        bump_revision(&transaction)?;
    }
    transaction
        .commit()
        .map_err(|error| format!("Could not save the listening history: {error}"))
}

// ── Device identity ─────────────────────────────────────────────────────

fn device_name() -> String {
    let computer = std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.trim().is_empty() && !name.chars().any(char::is_control))
        .unwrap_or_else(|| "This computer".to_owned());
    let name: String = computer.chars().take(60).collect();
    format!("{name} (Aurora NG)")
}

/// `aurora-ng-{computer}`: the same on every run, so a reinstall picks up
/// this computer's OneDrive history again.
fn new_device_id() -> String {
    let slug: String = std::env::var("COMPUTERNAME")
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .map(|c| c.to_ascii_lowercase())
        .take(48)
        .collect();
    format!(
        "aurora-ng-{}",
        if slug.is_empty() { "computer" } else { &slug }
    )
}

fn read_device_id(local: &Path) -> Option<String> {
    if !local.is_file() {
        return None;
    }
    let connection = Connection::open_with_flags(local, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    read_meta(&connection).ok().map(|meta| meta.device_id)
}

/// A fresh install on a computer that played before: start from its copy
/// in OneDrive rather than from nothing.
fn restore_from_remote(paths: &Paths, device_id: &str) {
    let Some(remote) = &paths.remote else {
        return;
    };
    let snapshot = remote.join(format!("aurora-history-{device_id}.sqlite3"));
    if paths.local.exists() || !snapshot.is_file() {
        return;
    }
    let staging = paths.work.join("aurora-history.restore.tmp");
    let restored = fs::copy(&snapshot, &staging).is_ok()
        && validate(&staging, device_id).is_ok()
        && fs::rename(&staging, &paths.local).is_ok();
    if !restored {
        let _ = fs::remove_file(&staging);
    }
}

// ── OneDrive copy ───────────────────────────────────────────────────────

struct Meta {
    device_id: String,
    content_revision: i64,
    mirrored_revision: i64,
}

fn read_meta(connection: &Connection) -> rusqlite::Result<Meta> {
    connection.query_row(
        "SELECT device_id, content_revision, mirrored_revision FROM history_meta WHERE singleton = 1",
        [],
        |row| {
            Ok(Meta {
                device_id: row.get(0)?,
                content_revision: row.get(1)?,
                mirrored_revision: row.get(2)?,
            })
        },
    )
}

/// Checks that a copy is a sound history of `device_id` that Aurora can read.
fn validate(path: &Path, device_id: &str) -> Result<(), String> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("Could not open the history copy: {error}"))?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    let meta = read_meta(&connection).map_err(|error| error.to_string())?;
    let check: String = connection
        .pragma_query_value(None, "quick_check", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if version != SCHEMA_VERSION || meta.device_id != device_id || check != "ok" {
        return Err("The history copy did not check out.".to_owned());
    }
    Ok(())
}

/// Replaces this device's copy in OneDrive when the history has changed:
/// a consistent copy is made and checked locally first, then swapped in, so
/// Aurora never reads a half-written file.
fn publish(
    connection: &Connection,
    work: &Path,
    remote: &Path,
    device_id: &str,
) -> Result<(), String> {
    if !remote.is_dir() {
        return Ok(());
    }
    let target = remote.join(format!("aurora-history-{device_id}.sqlite3"));
    let meta = read_meta(connection).map_err(|error| error.to_string())?;
    if meta.content_revision == meta.mirrored_revision && target.is_file() {
        return Ok(());
    }
    let now = now_ms();
    let staging = work.join("aurora-history.publish.tmp");
    let _ = fs::remove_file(&staging);
    // Not named aurora-history-*.sqlite3, so no one reads it half-copied.
    let upload = remote.join(format!(".{device_id}.upload.tmp"));
    let result = (|| {
        connection
            .execute("VACUUM INTO ?1", [staging.to_string_lossy()])
            .map_err(|error| format!("Could not copy the listening history: {error}"))?;
        {
            let copy = Connection::open(&staging).map_err(|error| error.to_string())?;
            // One self-contained file, with no -wal or -shm beside it.
            copy.pragma_update(None, "journal_mode", "DELETE")
                .map_err(|error| error.to_string())?;
            copy.execute(
                "UPDATE history_meta SET mirrored_revision = content_revision, last_synced_at_ms = ?1
                 WHERE singleton = 1",
                [now],
            )
            .map_err(|error| error.to_string())?;
        }
        validate(&staging, device_id)?;
        fs::copy(&staging, &upload).map_err(|error| {
            format!("Could not copy the listening history to OneDrive: {error}")
        })?;
        validate(&upload, device_id)?;
        fs::rename(&upload, &target)
            .map_err(|error| format!("Could not publish the listening history: {error}"))
    })();
    let _ = fs::remove_file(&staging);
    if result.is_err() {
        let _ = fs::remove_file(&upload);
    }
    result?;
    connection
        .execute(
            "UPDATE history_meta SET mirrored_revision = ?1, last_synced_at_ms = ?2
             WHERE singleton = 1 AND content_revision = ?1",
            params![meta.content_revision, now],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

// ── Music Library ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryCursor {
    /// The start of the newest play Music Library has accepted.
    sent_until_ms: i64,
}

/// Sends this device's registered plays that Music Library has not had,
/// oldest first. The cursor only moves once Music Library has accepted a
/// batch; it ignores plays it already has, so sending twice is harmless.
fn send_plays(
    connection: &Connection,
    work: &Path,
    mut send: impl FnMut(&[music_library::LibraryPlay]) -> Result<(), String>,
) -> Result<(), String> {
    let path = work.join("library-plays.json");
    let mut cursor: LibraryCursor = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut statement = connection
        .prepare_cached(
            "SELECT started_at_ms, artist, title, album, directory, filename
             FROM listening_sessions
             WHERE registered_play = 1 AND started_at_ms > ?1
             ORDER BY started_at_ms LIMIT ?2",
        )
        .map_err(|error| error.to_string())?;
    loop {
        let plays = statement
            .query_map(params![cursor.sent_until_ms, LIBRARY_BATCH as i64], |row| {
                let started: i64 = row.get(0)?;
                let album: String = row.get(3)?;
                let directory: String = row.get(4)?;
                let filename: String = row.get(5)?;
                Ok((
                    started,
                    music_library::LibraryPlay {
                        artist: row.get(1)?,
                        title: row.get(2)?,
                        album: (!album.trim().is_empty()).then_some(album),
                        // Seconds, as Aurora sends them, so Music Library
                        // sees Aurora's copy of this play as the same play.
                        played_at: started.div_euclid(1_000),
                        file_path: (!directory.is_empty() && !filename.is_empty()).then(|| {
                            format!("{}\\{filename}", directory.trim_end_matches(['\\', '/']))
                        }),
                    },
                ))
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|error| error.to_string())?;
        let Some(newest) = plays.iter().map(|(started, _)| *started).max() else {
            return Ok(());
        };
        let batch: Vec<_> = plays.into_iter().map(|(_, play)| play).collect();
        send(&batch)?;
        cursor.sent_until_ms = newest;
        let bytes = serde_json::to_vec(&cursor).map_err(|error| error.to_string())?;
        fs::write(&path, bytes).map_err(|error| error.to_string())?;
        if batch.len() < LIBRARY_BATCH {
            return Ok(());
        }
    }
}

// ── Reading ─────────────────────────────────────────────────────────────

/// Plays and listening time per track in the other histories, worked out
/// once and again only when one of their files changes.
#[derive(Default)]
struct PeerIndex {
    fingerprint: Vec<(PathBuf, u64, Option<SystemTime>)>,
    tracks: HashMap<String, Stats>,
}

impl Worker {
    /// Answered on the saving thread, so the sessions saved before the
    /// question are always counted.
    fn track_stats(&mut self, track_key: &str, except: Option<&str>) -> Stats {
        let mut stats = self.local_stats(track_key, except).unwrap_or_default();
        self.refresh_peers();
        if let Some(peer) = self.peers.tracks.get(track_key) {
            stats.add(peer.plays, peer.listened_seconds);
        }
        stats
    }

    fn local_stats(&self, track_key: &str, except: Option<&str>) -> Result<Stats, String> {
        self.connection
            .query_row(
                "SELECT COALESCE(SUM(registered_play), 0), COALESCE(SUM(listened_seconds), 0)
                 FROM listening_sessions WHERE track_key = ?1 AND session_id IS NOT ?2",
                params![track_key, except],
                |row| {
                    Ok(Stats {
                        plays: row.get(0)?,
                        listened_seconds: row.get(1)?,
                    })
                },
            )
            .map_err(|error| error.to_string())
    }

    /// The other histories, the live ones on this computer first: a
    /// device's OneDrive copy is only read when its live file is not here.
    fn peer_sources(&self) -> Vec<(PathBuf, bool)> {
        let mut sources = Vec::new();
        if let Some(aurora) = &self.paths.aurora {
            sources.push((aurora.clone(), true));
        }
        if let Some(tonehavn) = &self.paths.tonehavn {
            sources.extend(
                TONEHAVN_FILES
                    .iter()
                    .map(|name| (tonehavn.join(name), true)),
            );
        }
        if let Some(remote) = &self.paths.remote {
            let mut snapshots: Vec<PathBuf> = fs::read_dir(remote)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("aurora-history-") && name.ends_with(".sqlite3")
                        })
                })
                .collect();
            snapshots.sort();
            sources.extend(snapshots.into_iter().map(|path| (path, false)));
            let backups = remote.join("tonehavn-history");
            sources.extend(
                TONEHAVN_FILES
                    .iter()
                    .map(|name| (backups.join(name), false)),
            );
        }
        sources.retain(|(path, _)| path.is_file());
        sources
    }

    fn refresh_peers(&mut self) {
        let sources = self.peer_sources();
        // A live history changes in its -wal file first.
        let fingerprint: Vec<_> = sources
            .iter()
            .flat_map(|(path, _)| {
                let mut wal = path.clone().into_os_string();
                wal.push("-wal");
                [path.clone(), PathBuf::from(wal)]
            })
            .filter_map(|path| {
                let metadata = fs::metadata(&path).ok()?;
                Some((path, metadata.len(), metadata.modified().ok()))
            })
            .collect();
        if fingerprint == self.peers.fingerprint {
            return;
        }
        let mut seen = HashSet::from([self.device_id.clone()]);
        let mut tracks: HashMap<String, Stats> = HashMap::new();
        for (path, live) in sources {
            let _ = read_peer(&path, live, &mut seen, &mut tracks);
        }
        self.peers = PeerIndex {
            fingerprint,
            tracks,
        };
    }
}

/// Adds one history's plays per track, unless its device was already read.
fn read_peer(
    path: &Path,
    live: bool,
    seen: &mut HashSet<String>,
    tracks: &mut HashMap<String, Stats>,
) -> Result<(), String> {
    // A live history may be in use, so it is read normally; a OneDrive copy
    // is never written to, so it is read without any locking.
    let connection = if live {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    } else {
        let mut uri = String::from("file:");
        uri.push_str(
            &path
                .to_string_lossy()
                .replace('\\', "/")
                .replace('?', "%3f")
                .replace('#', "%23"),
        );
        uri.push_str("?immutable=1");
        Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
    }
    .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|error| error.to_string())?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if version != SCHEMA_VERSION {
        return Ok(());
    }
    let meta = read_meta(&connection).map_err(|error| error.to_string())?;
    if !seen.insert(meta.device_id) {
        return Ok(());
    }
    let mut statement = connection
        .prepare(
            "SELECT track_key, SUM(registered_play), SUM(listened_seconds)
             FROM listening_sessions GROUP BY track_key",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (key, plays, seconds) = row.map_err(|error| error.to_string())?;
        tracks.entry(key).or_default().add(plays, seconds);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(key: &str, duration: Option<i64>) -> TrackInfo {
        TrackInfo {
            key: key.to_owned(),
            title: "Song".to_owned(),
            artist: "Artist".to_owned(),
            album: "Album".to_owned(),
            genre: Some("Rock".to_owned()),
            directory: "D:\\MUSIC\\Artist - Album".to_owned(),
            filename: "01 Song.mp3".to_owned(),
            duration_seconds: duration,
        }
    }

    fn paths(dir: &Path) -> Paths {
        let remote = dir.join("remote");
        fs::create_dir_all(&remote).unwrap();
        Paths {
            local: dir.join("ng").join("aurora-history.sqlite3"),
            work: dir.join("ng"),
            remote: Some(remote),
            aurora: Some(dir.join("aurora").join("aurora-history.sqlite3")),
            tonehavn: Some(dir.join("tonehavn")),
            send_to_library: false,
        }
    }

    /// An Aurora-style history of another device with one session.
    fn peer(path: &Path, device: &str, key: &str, plays: i64, seconds: f64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let connection = Connection::open(path).unwrap();
        migrate(&connection, device, "Peer").unwrap();
        for play in 0..plays {
            connection
                .execute(
                    "INSERT INTO listening_sessions (session_id, track_key, title, artist, album,
                       directory, filename, started_at_ms, listened_seconds, registered_play,
                       threshold_seconds, outcome)
                     VALUES (?1, ?2, 't', 'a', 'b', 'd', 'f', 1, ?3, 1, 30, 'completed')",
                    params![
                        format!("s-{device}-{key}-{play}"),
                        key,
                        seconds / plays as f64
                    ],
                )
                .unwrap();
        }
    }

    #[test]
    fn track_keys_match_aurora() {
        assert_eq!(
            track_key(
                "\\\\?\\D:/MUSIC/Kittie - Oracle (2001)\\",
                " 01 Oracle.MP3 "
            ),
            "d:\\music\\kittie - oracle (2001)\\01 oracle.mp3"
        );
    }

    #[test]
    fn plays_register_after_30_seconds_of_forward_listening() {
        let mut session = Session::new("aurora-ng-test", info("k", Some(200)), 30);
        assert!(session.observe(10.0).is_none());
        // Seeking ahead counts nothing.
        session.reset_position(150.0);
        assert!(session.observe(160.0).is_none());
        assert!(!session.registered());
        let checkpoint = session.observe(171.0).expect("registers");
        assert!(session.registered());
        assert!(checkpoint.registered_at_ms.is_some());
        assert!((session.listened_seconds() - 31.0).abs() < 1e-9);
        // Going back counts nothing either.
        assert!(session.observe(5.0).is_none());
        assert!((session.listened_seconds() - 31.0).abs() < 1e-9);
    }

    #[test]
    fn short_tracks_register_when_they_finish() {
        let mut session = Session::new("aurora-ng-test", info("k", Some(12)), 30);
        session.observe(11.5);
        assert!(!session.registered());
        let checkpoint = session.complete();
        assert!(session.registered());
        assert_eq!(checkpoint.outcome, Outcome::Completed);
        assert!((checkpoint.listened_seconds - 12.0).abs() < 1e-9);
    }

    #[test]
    fn saved_sessions_only_grow_and_stay_finished() {
        let dir = tempfile::tempdir().unwrap();
        let connection = Connection::open(dir.path().join("h.sqlite3")).unwrap();
        migrate(&connection, "aurora-ng-test", "Test").unwrap();
        let mut session = Session::new("aurora-ng-test", info("k", Some(300)), 30);
        let early = session.observe(40.0).unwrap();
        persist(&connection, &session.finish(Outcome::Skipped)).unwrap();
        // A late checkpoint does not reopen or shrink the session.
        persist(&connection, &early).unwrap();
        let (outcome, plays): (String, i64) = connection
            .query_row(
                "SELECT outcome, registered_play FROM listening_sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((outcome.as_str(), plays), ("skipped", 1));
    }

    #[test]
    fn combines_every_history_once_and_publishes_one_aurora_can_read() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let remote = paths.remote.clone().unwrap();
        // Aurora on this computer, and its older OneDrive copy (not counted twice).
        peer(paths.aurora.as_ref().unwrap(), "device-pc-1", "k", 2, 100.0);
        peer(
            &remote.join("aurora-history-device-pc-1.sqlite3"),
            "device-pc-1",
            "k",
            1,
            50.0,
        );
        // Another computer, and Music Library's player.
        peer(
            &remote.join("aurora-history-device-laptop-2.sqlite3"),
            "device-laptop-2",
            "k",
            3,
            90.0,
        );
        peer(
            &paths.tonehavn.as_ref().unwrap().join(TONEHAVN_FILES[0]),
            "tonehavn-local",
            "k",
            1,
            40.0,
        );

        let stats_now = |history: &History, except: &str| {
            history
                .track_stats("k".to_owned(), Some(except.to_owned()))
                .recv_blocking()
                .unwrap()
        };
        let mut history = History::open_at(paths.clone()).unwrap();
        let mut session = history.start_session(info("k", Some(200)));
        session.observe(45.0);
        history.save(session.finish(Outcome::Skipped));
        let current = history.start_session(info("k", Some(200)));
        history.save(current.checkpoint(Outcome::Active));
        // Answered after the saves sent before it.
        let stats = stats_now(&history, current.id());
        assert_eq!(stats.plays, 2 + 3 + 1 + 1);
        assert!((stats.listened_seconds - (100.0 + 90.0 + 40.0 + 45.0)).abs() < 1e-6);
        let device_id = history.device_id.clone();
        history.shutdown(Duration::from_secs(10));

        // The OneDrive copy is a sound Aurora history of this device, and
        // reading it as a peer does not count Aurora NG twice.
        let published = remote.join(format!("aurora-history-{device_id}.sqlite3"));
        validate(&published, &device_id).unwrap();
        let mut history = History::open_at(paths).unwrap();
        assert_eq!(stats_now(&history, current.id()), stats);
        history.shutdown(Duration::from_secs(10));
    }

    #[test]
    fn sends_new_plays_to_music_library_once() {
        let dir = tempfile::tempdir().unwrap();
        let connection = Connection::open(dir.path().join("h.sqlite3")).unwrap();
        migrate(&connection, "aurora-ng-test", "Test").unwrap();
        for (ix, seconds) in [40.0, 5.0, 60.0].into_iter().enumerate() {
            let mut checkpoint = Session::new("aurora-ng-test", info("k", Some(300)), 30)
                .checkpoint(Outcome::Completed);
            checkpoint.started_at_ms = 1_000_000 + ix as i64 * 1_000;
            checkpoint.listened_seconds = seconds;
            checkpoint.registered_at_ms = (seconds >= 30.0).then_some(1);
            persist(&connection, &checkpoint).unwrap();
        }
        let mut sent = Vec::new();
        send_plays(&connection, dir.path(), |plays| {
            sent.extend(plays.iter().map(|play| play.played_at));
            Ok(())
        })
        .unwrap();
        assert_eq!(sent, [1_000, 1_002]);
        send_plays(&connection, dir.path(), |_| panic!("nothing new to send")).unwrap();
    }

    /// Reads this computer's real histories; run with `--ignored` to check.
    #[test]
    #[ignore]
    fn live_histories_on_this_computer() {
        let mut paths = Paths::default_paths().unwrap();
        paths.send_to_library = false;
        let connection = Connection::open_in_memory().unwrap();
        migrate(&connection, "aurora-ng-live-test", "Test").unwrap();
        let mut worker = Worker::new(connection, paths, "aurora-ng-live-test".to_owned());
        let started = Instant::now();
        worker.refresh_peers();
        let tracks = &worker.peers.tracks;
        let plays: i64 = tracks.values().map(|stats| stats.plays).sum();
        println!(
            "{} tracks, {plays} plays, read in {:?}",
            tracks.len(),
            started.elapsed()
        );
        assert!(!tracks.is_empty());
    }
}
