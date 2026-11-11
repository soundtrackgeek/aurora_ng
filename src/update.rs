//! Self-update from GitHub Releases.
//!
//! The release workflow publishes the Windows build as `ASSET_NAME` on each
//! `vX.Y.Z` release. The app compares that tag with its own version, and can
//! download the new exe, swap it in for the running one, and restart.

use semver::Version;
use serde::Deserialize;
use std::{fs::File, io, path::PathBuf, process::Command, time::Duration};

const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/soundtrackgeek/aurora_ng/releases/latest";
/// The file name the release workflow uploads.
pub const ASSET_NAME: &str = "aurora_ng-windows-x64.exe";
pub const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const USER_AGENT: &str = concat!("aurora_ng/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    pub download_url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a valid version")
}

/// Asks GitHub for the latest release. Returns it only if it is newer than
/// this build and has a Windows exe attached. Blocks, so call it off the UI thread.
pub fn check() -> Result<Option<Release>, String> {
    let response = ureq::get(LATEST_RELEASE_URL)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call();
    let release: GithubRelease = match response {
        Ok(mut response) => response
            .body_mut()
            .read_json()
            .map_err(|error| format!("Could not read the release info: {error}"))?,
        // No releases published yet.
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(error) => return Err(format!("Could not reach GitHub: {error}")),
    };
    Ok(newer_release(&current_version(), release))
}

fn newer_release(current: &Version, release: GithubRelease) -> Option<Release> {
    let version = Version::parse(release.tag_name.trim_start_matches('v')).ok()?;
    if version <= *current {
        return None;
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == ASSET_NAME)?;
    Some(Release {
        version,
        download_url: asset.browser_download_url,
    })
}

/// Downloads the release and replaces the running exe with it. Blocks.
pub fn install(release: &Release) -> Result<(), String> {
    let download = std::env::temp_dir().join(format!("aurora_ng-{}.exe", release.version));
    let result = download_to(&release.download_url, &download).and_then(|()| {
        self_replace::self_replace(&download)
            .map_err(|error| format!("Could not replace the app: {error}"))
    });
    std::fs::remove_file(&download).ok();
    result
}

fn download_to(url: &str, path: &PathBuf) -> Result<(), String> {
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| format!("Could not download the update: {error}"))?;
    let mut file =
        File::create(path).map_err(|error| format!("Could not save the update: {error}"))?;
    let bytes = io::copy(&mut response.body_mut().as_reader(), &mut file)
        .map_err(|error| format!("Could not download the update: {error}"))?;
    if bytes == 0 {
        return Err("The downloaded update was empty".to_owned());
    }
    Ok(())
}

/// Starts the (now updated) exe again. The caller quits the current one.
pub fn relaunch() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    Command::new(exe)
        .spawn()
        .map(drop)
        .map_err(|error| format!("Updated, but could not restart: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, asset: &str) -> GithubRelease {
        GithubRelease {
            tag_name: tag.to_owned(),
            assets: vec![GithubAsset {
                name: asset.to_owned(),
                browser_download_url: format!("https://example.com/{asset}"),
            }],
        }
    }

    #[test]
    fn finds_newer_release() {
        let current = Version::new(0, 2, 0);
        let found = newer_release(&current, release("v0.3.0", ASSET_NAME)).unwrap();
        assert_eq!(found.version, Version::new(0, 3, 0));
        assert!(found.download_url.ends_with(ASSET_NAME));
    }

    #[test]
    fn ignores_same_or_older_release() {
        let current = Version::new(0, 2, 0);
        assert!(newer_release(&current, release("v0.2.0", ASSET_NAME)).is_none());
        assert!(newer_release(&current, release("0.1.5", ASSET_NAME)).is_none());
    }

    #[test]
    fn ignores_release_without_windows_exe() {
        let current = Version::new(0, 2, 0);
        assert!(newer_release(&current, release("v0.3.0", "notes.txt")).is_none());
        assert!(newer_release(&current, release("not-a-version", ASSET_NAME)).is_none());
    }
}
