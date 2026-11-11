//! Tells Music Library that an MP3 changed, so its catalog picks up the new
//! rating, just as Aurora does. Aurora NG never writes the catalog itself:
//! Music Library re-reads the file and updates the track, its album's rating
//! and Album Score, and everything else that depends on them.
//!
//! This goes through Music Library's bridge: `music-library.exe
//! --aurora-bridge <request.json> <response.json>`, where the request is
//! `{"protocolVersion": 1, "operation": ..., "payload": ...}`.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROTOCOL_VERSION: u32 = 1;
/// Points at another `music-library.exe`, the same override Aurora uses.
const EXECUTABLE_ENV: &str = "AURORA_MUSIC_LIBRARY_EXE";
const CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(30);
const SYNC_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

static EXCHANGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Makes Music Library re-read one MP3 in its album folder.
pub fn sync_file(file: &Path) -> Result<(), String> {
    let folder = file
        .parent()
        .ok_or_else(|| "The MP3 has no folder.".to_owned())?
        .to_string_lossy()
        .into_owned();
    let executable = executable()?;

    let capabilities: Capabilities =
        invoke(&executable, "capabilities", Empty {}, CAPABILITIES_TIMEOUT)?;
    let supports = &capabilities.supports;
    if capabilities.bridge_version != PROTOCOL_VERSION
        || !supports.sync_existing_folders
        || !supports.default_popm_rating_fallback
        || !supports.bounded_existing_folder_sync
    {
        return Err(
            "Music Library is too old to pick up ratings; update it (0.145.14 or newer)."
                .to_owned(),
        );
    }

    let changed = [file.to_string_lossy().into_owned()];
    let result: SyncResult = invoke(
        &executable,
        "syncExistingFolders",
        SyncPayload {
            folder_paths: std::slice::from_ref(&folder),
            changed_file_paths: &changed,
        },
        SYNC_TIMEOUT,
    )?;
    let named = result.folders.len() == 1
        && same_folder(&result.folders[0].folder_path, &folder)
        && result.synced_folder_count == 1;
    if !named {
        return Err("Music Library answered for a different folder.".to_owned());
    }
    Ok(())
}

/// Whether a failed sync is worth trying again soon: Music Library was busy
/// or not reachable, rather than refusing the file.
pub fn is_temporary(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "already running",
        "being used by another process",
        "could not start music library",
        "did not answer",
        "took too long",
        "database is locked",
        "interrupted",
        "busy",
    ]
    .iter()
    .any(|fragment| error.contains(fragment))
}

fn same_folder(a: &str, b: &str) -> bool {
    let normalize = |path: &str| {
        path.trim()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    normalize(a) == normalize(b)
}

/// `%LOCALAPPDATA%\Music Library\music-library.exe`, where Music Library
/// installs itself.
fn executable() -> Result<PathBuf, String> {
    let path = match std::env::var_os(EXECUTABLE_ENV) {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(
            std::env::var_os("LOCALAPPDATA")
                .ok_or_else(|| "The LOCALAPPDATA folder is not set.".to_owned())?,
        )
        .join("Music Library")
        .join("music-library.exe"),
    };
    if !path.is_file() {
        return Err(format!(
            "Music Library was not found at {}; install it so ratings reach its catalog.",
            path.display()
        ));
    }
    Ok(path)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<T> {
    protocol_version: u32,
    operation: &'static str,
    payload: T,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response<T> {
    protocol_version: u32,
    ok: bool,
    result: Option<T>,
    error: Option<ResponseError>,
}

#[derive(Deserialize)]
struct ResponseError {
    code: String,
    message: String,
}

#[derive(Serialize)]
struct Empty {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    bridge_version: u32,
    supports: Supports,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Supports {
    #[serde(default)]
    sync_existing_folders: bool,
    #[serde(default)]
    default_popm_rating_fallback: bool,
    #[serde(default)]
    bounded_existing_folder_sync: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncPayload<'a> {
    folder_paths: &'a [String],
    changed_file_paths: &'a [String],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncResult {
    synced_folder_count: usize,
    folders: Vec<FolderReceipt>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FolderReceipt {
    folder_path: String,
}

/// The request and response files, removed when done.
struct Exchange {
    request: PathBuf,
    response: PathBuf,
    progress: PathBuf,
}

impl Exchange {
    fn new() -> Result<Self, String> {
        let dir = crate::settings::data_dir()
            .ok_or_else(|| "The LOCALAPPDATA folder is not set.".to_owned())?
            .join("music-library-bridge");
        fs::create_dir_all(&dir)
            .map_err(|error| format!("Could not prepare the Music Library request: {error}"))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = EXCHANGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let stem = format!("exchange-{}-{nanos}-{sequence}", std::process::id());
        Ok(Self {
            request: dir.join(format!("{stem}.request.json")),
            response: dir.join(format!("{stem}.response.json")),
            // Music Library may report progress next to the request.
            progress: dir.join(format!("{stem}.request.progress.json")),
        })
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.request);
        let _ = fs::remove_file(&self.response);
        let _ = fs::remove_file(&self.progress);
    }
}

fn invoke<T: Serialize, R: DeserializeOwned>(
    executable: &Path,
    operation: &'static str,
    payload: T,
    timeout: Duration,
) -> Result<R, String> {
    let exchange = Exchange::new()?;
    let request = serde_json::to_vec(&Request {
        protocol_version: PROTOCOL_VERSION,
        operation,
        payload,
    })
    .map_err(|error| format!("Could not write the Music Library request: {error}"))?;
    fs::write(&exchange.request, request)
        .map_err(|error| format!("Could not write the Music Library request: {error}"))?;

    let mut command = Command::new(executable);
    command
        .arg("--aurora-bridge")
        .arg(&exchange.request)
        .arg(&exchange.response)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start Music Library: {error}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Music Library took too long to {operation}."));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Could not wait for Music Library: {error}"));
            }
        }
    };

    let response = read_response(&exchange.response, operation);
    match (status.success(), response) {
        (_, Err(error)) => Err(error),
        (true, Ok(result)) => Ok(result),
        (false, Ok(_)) => Err(format!(
            "Music Library stopped before it finished {operation} (exit code {}).",
            status
                .code()
                .map_or_else(|| "unknown".to_owned(), |code| code.to_string())
        )),
    }
}

fn read_response<R: DeserializeOwned>(path: &Path, operation: &str) -> Result<R, String> {
    let length = fs::metadata(path)
        .map_err(|_| format!("Music Library did not answer {operation}."))?
        .len();
    if length > MAX_RESPONSE_BYTES {
        return Err("Music Library sent an unexpectedly large answer.".to_owned());
    }
    let bytes = fs::read(path).map_err(|_| format!("Music Library did not answer {operation}."))?;
    let response: Response<R> = serde_json::from_slice(&bytes).map_err(|error| {
        format!("Music Library sent an answer Aurora NG does not understand: {error}")
    })?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "Music Library speaks bridge version {}, not {PROTOCOL_VERSION}; update it.",
            response.protocol_version
        ));
    }
    if !response.ok {
        return Err(match response.error {
            Some(error) => format!("Music Library: {} ({})", error.message, error.code),
            None => format!("Music Library could not {operation}."),
        });
    }
    response
        .result
        .ok_or_else(|| format!("Music Library did not answer {operation}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_request_aurora_sends() {
        let folder = vec![r"D:\MUSIC\Kittie - Oracle (2001)".to_owned()];
        let file = vec![r"D:\MUSIC\Kittie - Oracle (2001)\01 Oracle.mp3".to_owned()];
        let request = Request {
            protocol_version: PROTOCOL_VERSION,
            operation: "syncExistingFolders",
            payload: SyncPayload {
                folder_paths: &folder,
                changed_file_paths: &file,
            },
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "protocolVersion": 1,
                "operation": "syncExistingFolders",
                "payload": {
                    "folderPaths": [r"D:\MUSIC\Kittie - Oracle (2001)"],
                    "changedFilePaths": [r"D:\MUSIC\Kittie - Oracle (2001)\01 Oracle.mp3"],
                }
            })
        );
    }

    #[test]
    fn reads_answers_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("response.json");
        fs::write(
            &path,
            r#"{"ok":true,"protocolVersion":1,"result":{"syncedFolderCount":1,"updatedFolderCount":1,
               "folders":[{"folderPath":"D:\\MUSIC\\A","status":"updated"}]}}"#,
        )
        .unwrap();
        let result: SyncResult = read_response(&path, "sync").unwrap();
        assert_eq!(result.synced_folder_count, 1);
        assert!(same_folder(&result.folders[0].folder_path, r"d:/music/a\"));

        fs::write(
            &path,
            r#"{"ok":false,"protocolVersion":1,"error":{"code":"busy","message":"Another Aurora album intake is already running"}}"#,
        )
        .unwrap();
        let error = read_response::<SyncResult>(&path, "sync").err().unwrap();
        assert!(is_temporary(&error), "{error}");
    }

    /// Asks the real Music Library to re-read an MP3 that has not changed,
    /// which it should answer without updating anything. Set
    /// `AURORA_NG_SYNC_TEST_MP3` to a file in the library to run it.
    #[test]
    #[ignore = "runs the installed Music Library"]
    fn live_sync_of_an_unchanged_file() {
        let path = std::env::var_os("AURORA_NG_SYNC_TEST_MP3").expect("AURORA_NG_SYNC_TEST_MP3");
        sync_file(Path::new(&path)).unwrap();
    }
}
