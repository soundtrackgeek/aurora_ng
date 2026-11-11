//! What the app looks like when it closes, so it opens the same way next time:
//! the view, the cover size, the selected album and the window.
//!
//! Saved as JSON in `%LOCALAPPDATA%\aurora_ng\settings.json`. A missing or
//! unreadable file gives the defaults.

use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

/// The cover size on first start.
pub const DEFAULT_COVER_SIZE: f32 = 500.;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub cover_wall: bool,
    pub cover_size: f32,
    /// The selected album, by its catalog id, which stays the same when
    /// albums are added or removed.
    pub album_id: Option<String>,
    /// Whether the album's details were open on the cover wall.
    pub details_open: bool,
    /// The selected track, by its catalog id.
    pub track_id: Option<i64>,
    pub window: Option<SavedWindow>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cover_wall: true,
            cover_size: DEFAULT_COVER_SIZE,
            album_id: None,
            details_open: false,
            track_id: None,
            window: None,
        }
    }
}

/// The window's position and size; when maximized, the size it goes back to.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct SavedWindow {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

fn settings_path() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("aurora_ng")
            .join("settings.json"),
    )
}

impl Settings {
    pub fn load() -> Self {
        settings_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Saving is best effort: failing to save should never stop the app closing.
    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).ok();
        }
        // Write a copy first, so a crash halfway never leaves a broken file.
        let temp = path.with_extension("json.tmp");
        if fs::write(&temp, text).is_ok() {
            fs::rename(&temp, &path).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let settings = Settings {
            cover_wall: false,
            cover_size: 240.,
            album_id: Some("abc".to_owned()),
            details_open: true,
            track_id: Some(7),
            window: Some(SavedWindow {
                x: 10.,
                y: 20.,
                width: 1280.,
                height: 800.,
                maximized: true,
            }),
        };
        let text = serde_json::to_string(&settings).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap(), settings);
    }

    #[test]
    fn fills_missing_fields_with_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"cover_size": 300}"#).unwrap();
        assert_eq!(settings.cover_size, 300.);
        assert!(settings.cover_wall);
        assert_eq!(settings.album_id, None);
    }
}
