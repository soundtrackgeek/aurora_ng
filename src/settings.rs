//! What the app looks like when it closes, so it opens the same way next time:
//! the view, the cover size, the selected album, the search and the window.
//!
//! Saved as JSON in `%LOCALAPPDATA%\aurora_ng\settings.json`. A missing or
//! unreadable file gives the defaults.

use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

/// The cover size on first start.
pub const DEFAULT_COVER_SIZE: f32 = 500.;
/// How many recent search queries to remember.
pub const MAX_SEARCH_HISTORY: usize = 20;

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
    /// The search the albums were filtered by; empty for none.
    pub search: String,
    /// Recent searches, newest first.
    pub search_history: Vec<String>,
    /// The draw `sort:random` showed, so it comes back the same.
    pub shuffle_seed: Option<u64>,
    pub window: Option<SavedWindow>,
    /// What colours the waveform in the seek bar.
    pub seek_bar_colour: SeekBarColour,
}

/// Records `query` at the front of `history`, deduplicating and capping its
/// length. Blank queries are ignored.
pub fn record_search_history(history: &mut Vec<String>, query: &str) {
    let query = query.trim();
    if query.is_empty() {
        return;
    }
    history.retain(|entry| entry != query);
    history.insert(0, query.to_owned());
    history.truncate(MAX_SEARCH_HISTORY);
}

/// What the seek bar's waveform is coloured by, from Music Library's
/// per-second timeline. Without a timeline it is always plain.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SeekBarColour {
    /// Blue where the sound is dull or dark, gold where it is bright.
    #[default]
    Brightness,
    /// Orange where it is close to mono, cyan where it is wide.
    Width,
    /// The gradient from before 0.23.0.
    Plain,
}

impl SeekBarColour {
    pub fn next(self) -> Self {
        match self {
            Self::Brightness => Self::Width,
            Self::Width => Self::Plain,
            Self::Plain => Self::Brightness,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Brightness => "Brightness",
            Self::Width => "Stereo width",
            Self::Plain => "Plain",
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cover_wall: true,
            cover_size: DEFAULT_COVER_SIZE,
            album_id: None,
            details_open: false,
            track_id: None,
            search: String::new(),
            search_history: Vec::new(),
            shuffle_seed: None,
            window: None,
            seek_bar_colour: SeekBarColour::default(),
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

/// `%LOCALAPPDATA%\aurora_ng`, where the app keeps its own files.
pub fn data_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("aurora_ng"))
}

fn settings_path() -> Option<PathBuf> {
    Some(data_dir()?.join("settings.json"))
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
            search: "aartist:\"bon jovi\" sort:random".to_owned(),
            search_history: vec![
                "aartist:\"bon jovi\" sort:random".to_owned(),
                "genre:scores".to_owned(),
            ],
            shuffle_seed: Some(42),
            window: Some(SavedWindow {
                x: 10.,
                y: 20.,
                width: 1280.,
                height: 800.,
                maximized: true,
            }),
            seek_bar_colour: SeekBarColour::Width,
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
        assert!(settings.search_history.is_empty());
        assert_eq!(settings.seek_bar_colour, SeekBarColour::Brightness);
    }

    #[test]
    fn records_search_history_newest_first_without_duplicates() {
        let mut history = Vec::new();
        record_search_history(&mut history, "   ");
        assert!(history.is_empty());

        record_search_history(&mut history, "genre:scores");
        record_search_history(&mut history, " year:1985 ");
        record_search_history(&mut history, "genre:scores");
        assert_eq!(history, vec!["genre:scores", "year:1985"]);

        for i in 0..25 {
            record_search_history(&mut history, &format!("year:{}", 1980 + i));
        }
        assert_eq!(history.len(), MAX_SEARCH_HISTORY);
        assert_eq!(history[0], "year:2004");
        assert_eq!(history[MAX_SEARCH_HISTORY - 1], "year:1985");
    }

    #[test]
    fn seek_bar_colours_cycle() {
        let colour = SeekBarColour::Brightness;
        assert_eq!(colour.next(), SeekBarColour::Width);
        assert_eq!(colour.next().next(), SeekBarColour::Plain);
        assert_eq!(colour.next().next().next(), colour);
    }
}
