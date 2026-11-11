//! Track ratings and Love, stored in the MP3 the way Aurora and MusicBee
//! store them: the rating as a POPM frame owned by "MusicBee", Love as a
//! `LOVE RATING` TXXX frame ("L" loved, "B" banned). Aurora NG only sets
//! whole stars, one to five, or clears the rating, and toggles Love.
//!
//! A save never touches the MP3 in place. It writes a hidden working copy in
//! the same folder, checks that the new values read back, that every other
//! frame and the audio itself are unchanged, and then swaps it in with one
//! atomic replace. Changes saved but not yet in Music Library's catalog are
//! kept in `%LOCALAPPDATA%\aurora_ng\pending-ratings.json` until it has them.

use crate::id3_write::write_tag_preserving_frames;
use id3::{
    Tag, TagLike, Version,
    frame::{Content, ExtendedText, Frame, Unknown},
    no_tag_ok,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MUSICBEE_OWNER: &str = "MusicBee";
/// Windows and some taggers write a 5-step rating under this owner. Aurora
/// reads it when there is no MusicBee rating and drops it when it saves.
const LEGACY_DEFAULT_OWNER: &str = "Default";

/// MusicBee's POPM byte for every half star, as 0–100 (20 per star). Files
/// rated elsewhere can hold half stars, so all are read; only whole ones are
/// ever written.
const MUSICBEE_BYTES: [(i64, u8); 10] = [
    (10, 13),
    (20, 1),
    (30, 54),
    (40, 64),
    (50, 118),
    (60, 128),
    (70, 186),
    (80, 196),
    (90, 242),
    (100, 255),
];
const LEGACY_DEFAULT_BYTES: [(i64, u8); 5] =
    [(20, 51), (40, 102), (60, 153), (80, 204), (100, 255)];
/// MusicBee's Love field; read in any case, written like this.
const LOVE_DESCRIPTION: &str = "LOVE RATING";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Love {
    Neutral,
    Loved,
    Banned,
}

/// The values a save can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Values {
    /// 0–100, or `None` when there is no rating.
    pub rating: Option<i64>,
    pub love: Love,
}

/// What a save changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Whole stars, or `None` to clear the rating.
    Rating(Option<Stars>),
    /// Loved becomes neutral; neutral or banned becomes loved, as in Aurora.
    ToggleLove,
}

impl Change {
    fn apply(self, values: Values) -> Values {
        match self {
            Self::Rating(stars) => Values {
                rating: stars.map(Stars::points),
                ..values
            },
            Self::ToggleLove => Values {
                love: if values.love == Love::Loved {
                    Love::Neutral
                } else {
                    Love::Loved
                },
                ..values
            },
        }
    }
}

/// A rating that can be set: one to five whole stars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stars(u8);

impl Stars {
    pub fn new(stars: u8) -> Option<Self> {
        (1..=5).contains(&stars).then_some(Self(stars))
    }

    /// 0–100, the scale of the catalog's `normalized_rating`.
    pub fn points(self) -> i64 {
        i64::from(self.0) * 20
    }

    fn musicbee_byte(self) -> u8 {
        let points = self.points();
        MUSICBEE_BYTES
            .iter()
            .find(|(value, _)| *value == points)
            .map(|(_, byte)| *byte)
            .expect("every whole star has a MusicBee byte")
    }
}

/// Makes `change` to the MP3, starting from the values it has now, and
/// returns the values it has after. Does nothing when nothing would change.
pub fn save(path: &Path, change: Change) -> Result<Values, String> {
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"))
    {
        return Err("Only MP3 files can be rated or loved.".to_owned());
    }
    let original = Fingerprint::read(path)?;
    let (mut tag, version) = read_tag(path)?;
    let before = read_values(&tag)?;
    let desired = change.apply(before);
    if desired == before {
        return Ok(before);
    }
    let preserved = other_frames(&tag);
    let audio = audio_payload_hash(path)?;
    match change {
        Change::Rating(stars) => set_rating(&mut tag, version, stars),
        Change::ToggleLove => set_love(&mut tag, desired.love),
    }

    let working = sibling_path(path, "working.tmp")?;
    let backup = sibling_path(path, "original.backup")?;
    let prepared = (|| {
        fs::copy(path, &working)
            .map_err(|error| format!("Could not copy the MP3 to save its rating: {error}"))?;
        write_tag_preserving_frames(&tag, &working, version)
            .map_err(|error| format!("Could not write the rating: {error}"))?;
        File::options()
            .write(true)
            .open(&working)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("Could not flush the changed copy: {error}"))?;
        verify(&working, desired, &preserved, &audio)?;
        // Keeps other programs from writing to the MP3 until it is replaced.
        let _exclusion = open_write_exclusion(path)?;
        if Fingerprint::read(path)? != original {
            return Err("The MP3 changed while it was saved; it was left as it was.".to_owned());
        }
        replace_file(path, &working, Some(&backup))
    })();
    if let Err(error) = prepared {
        let _ = fs::remove_file(&working);
        return Err(error);
    }

    if let Err(error) = verify(path, desired, &preserved, &audio) {
        return match replace_file(path, &backup, None) {
            Ok(()) => Err(format!(
                "The saved MP3 could not be verified, so the original was put back: {error}"
            )),
            Err(restore) => Err(format!(
                "The saved MP3 could not be verified ({error}), and the original could not be put back; it is kept at {}: {restore}",
                backup.display()
            )),
        };
    }
    let _ = fs::remove_file(&backup);
    Ok(desired)
}

#[cfg(test)]
fn read_values_from_path(path: &Path) -> Result<Values, String> {
    read_values(&read_tag(path)?.0)
}

fn read_values(tag: &Tag) -> Result<Values, String> {
    Ok(Values {
        rating: read_rating(tag)?,
        love: read_love(tag)?,
    })
}

fn is_love_text(text: &ExtendedText) -> bool {
    text.description.eq_ignore_ascii_case(LOVE_DESCRIPTION)
}

/// Love the way Aurora reads it: one `LOVE RATING` field, "L" or "B".
fn read_love(tag: &Tag) -> Result<Love, String> {
    let values: Vec<&str> = tag
        .extended_texts()
        .filter(|text| is_love_text(text))
        .map(|text| {
            text.value
                .trim_start()
                .trim_end_matches(|character: char| character == '\0' || character.is_whitespace())
        })
        .collect();
    if values.len() > 1 {
        return Err("This MP3 has more than one Love field; it was left as it was.".to_owned());
    }
    match values.first().copied().unwrap_or_default() {
        "" => Ok(Love::Neutral),
        "L" => Ok(Love::Loved),
        "B" => Ok(Love::Banned),
        _ => {
            Err("This MP3 has a Love value Aurora does not know; it was left as it was.".to_owned())
        }
    }
}

/// Replaces the Love field, keeping every other TXXX frame.
fn set_love(tag: &mut Tag, love: Love) {
    let kept: Vec<Frame> = tag
        .remove("TXXX")
        .into_iter()
        .filter(|frame| !is_love_frame(frame))
        .collect();
    for frame in kept {
        tag.add_frame(frame);
    }
    let value = match love {
        Love::Neutral => return,
        Love::Loved => "L",
        Love::Banned => "B",
    };
    tag.add_frame(ExtendedText {
        description: LOVE_DESCRIPTION.to_owned(),
        value: value.to_owned(),
    });
}

fn is_love_frame(frame: &Frame) -> bool {
    frame.content().extended_text().is_some_and(is_love_text)
}

fn read_tag(path: &Path) -> Result<(Tag, Version), String> {
    let tag = no_tag_ok(Tag::read_from_path(path))
        .map_err(|error| format!("Could not read the MP3's tags: {error}"))?;
    Ok(match tag {
        Some(tag) => {
            let version = tag.version();
            (tag, version)
        }
        None => (Tag::with_version(Version::Id3v23), Version::Id3v23),
    })
}

/// The rating the way Aurora and Music Library read it: the MusicBee frame,
/// else the legacy Default one.
fn read_rating(tag: &Tag) -> Result<Option<i64>, String> {
    let bytes_for = |owner: &str| {
        tag.frames()
            .filter(|frame| frame.id() == "POPM")
            .filter_map(|frame| frame.content().popularimeter())
            .filter(|popularimeter| popularimeter.user == owner)
            .map(|popularimeter| popularimeter.rating)
            .collect::<Vec<_>>()
    };
    let musicbee = bytes_for(MUSICBEE_OWNER);
    let legacy = bytes_for(LEGACY_DEFAULT_OWNER);
    if musicbee.len() > 1 || legacy.len() > 1 {
        return Err("This MP3 has more than one rating frame; it was left as it was.".to_owned());
    }
    if let Some(&byte) = musicbee.first() {
        // Some taggers write the 5-step scale (153 = 3 stars) into both
        // frames; a matching Default frame says which scale the byte is on.
        let five_step = legacy.first() == Some(&byte)
            && !MUSICBEE_BYTES.iter().any(|(_, value)| *value == byte);
        if !five_step {
            return points_for(byte, &MUSICBEE_BYTES);
        }
    }
    match legacy.first() {
        Some(&byte) => points_for(byte, &LEGACY_DEFAULT_BYTES),
        None => Ok(None),
    }
}

fn points_for(byte: u8, scale: &[(i64, u8)]) -> Result<Option<i64>, String> {
    if byte == 0 {
        return Ok(None);
    }
    scale
        .iter()
        .find(|(_, value)| *value == byte)
        .map(|(points, _)| Some(*points))
        .ok_or_else(|| {
            format!(
                "This MP3 has a rating byte ({byte}) Aurora does not know; it was left as it was."
            )
        })
}

/// Replaces the MusicBee and Default rating frames with one MusicBee frame,
/// keeping other players' POPM frames.
fn set_rating(tag: &mut Tag, version: Version, stars: Option<Stars>) {
    let kept: Vec<Frame> = tag
        .remove("POPM")
        .into_iter()
        .filter(|frame| !is_rating_frame(frame))
        .collect();
    for frame in kept {
        tag.add_frame(frame);
    }
    if let Some(stars) = stars {
        // The exact bytes MusicBee and Aurora write: owner, rating, and a
        // four-byte play counter of zero.
        let mut data = Vec::with_capacity(MUSICBEE_OWNER.len() + 6);
        data.extend_from_slice(MUSICBEE_OWNER.as_bytes());
        data.push(0);
        data.push(stars.musicbee_byte());
        data.extend_from_slice(&[0, 0, 0, 0]);
        tag.add_frame(Frame::with_content(
            "POPM",
            Content::Unknown(Unknown { data, version }),
        ));
    }
}

fn is_rating_frame(frame: &Frame) -> bool {
    frame.id() == "POPM"
        && frame
            .content()
            .popularimeter()
            .is_some_and(|value| value.user == MUSICBEE_OWNER || value.user == LEGACY_DEFAULT_OWNER)
}

fn other_frames(tag: &Tag) -> Vec<Frame> {
    tag.frames()
        .filter(|frame| !is_rating_frame(frame) && !is_love_frame(frame))
        .cloned()
        .collect()
}

/// The written file has the new values, every other frame as before, and
/// the same audio.
fn verify(
    path: &Path,
    expected: Values,
    preserved: &[Frame],
    audio: &[u8; 32],
) -> Result<(), String> {
    let (tag, _) = read_tag(path)?;
    if read_values(&tag)? != expected {
        return Err("the new values did not read back".to_owned());
    }
    if !same_frames(&other_frames(&tag), preserved) {
        return Err("another tag changed".to_owned());
    }
    if &audio_payload_hash(path)? != audio {
        return Err("the audio changed".to_owned());
    }
    Ok(())
}

/// The same frames, in any order.
fn same_frames(actual: &[Frame], expected: &[Frame]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    let mut unmatched = actual.to_vec();
    for frame in expected {
        let Some(ix) = unmatched.iter().position(|candidate| candidate == frame) else {
            return false;
        };
        unmatched.remove(ix);
    }
    true
}

/// A hash of everything after the ID3v2 tag: the audio, and an ID3v1 tag if
/// there is one.
fn audio_payload_hash(path: &Path) -> Result<[u8; 32], String> {
    let mut file = File::open(path).map_err(|error| format!("Could not open the MP3: {error}"))?;
    let has_tag =
        Tag::skip(&mut file).map_err(|error| format!("Could not find the MP3's audio: {error}"))?;
    if !has_tag {
        // Looking for a tag reads ahead; untagged audio starts at byte zero.
        file.rewind()
            .map_err(|error| format!("Could not read the MP3's audio: {error}"))?;
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not read the MP3's audio: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

/// Tells whether a file was changed or replaced in the meantime.
#[derive(PartialEq, Eq)]
struct Fingerprint {
    length: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
}

impl Fingerprint {
    fn read(path: &Path) -> Result<Self, String> {
        let metadata =
            fs::metadata(path).map_err(|error| format!("Could not read the MP3: {error}"))?;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
        })
    }
}

/// A hidden file next to the MP3, like `.aurora-ng-1a2b…-4120-17….working.tmp`.
/// Not built from the MP3's name, which may already be as long as allowed.
fn sibling_path(target: &Path, suffix: &str) -> Result<PathBuf, String> {
    let parent = target
        .parent()
        .ok_or_else(|| "The MP3 has no folder.".to_owned())?;
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let digest = format!("{:x}", Sha256::digest(name.as_bytes()));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(
        ".aurora-ng-{}-{}-{nanos}.{suffix}",
        &digest[..16],
        std::process::id()
    )))
}

#[cfg(windows)]
fn open_write_exclusion(path: &Path) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_DELETE: u32 = 0x4;
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(path)
        .map_err(|error| {
            format!("The MP3 is open for writing in another program; try again later: {error}")
        })
}

#[cfg(not(windows))]
fn open_write_exclusion(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| format!("Could not open the MP3: {error}"))
}

/// Puts `replacement` in place of `target` in one step, keeping the old file
/// as `backup`. Windows keeps the original's creation time and attributes.
#[cfg(windows)]
fn replace_file(target: &Path, replacement: &Path, backup: Option<&Path>) -> Result<(), String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut c_void,
            reserved: *mut c_void,
        ) -> i32;
    }
    const REPLACEFILE_WRITE_THROUGH: u32 = 0x1;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }
    let target = wide(target);
    let replacement = wide(replacement);
    let backup = backup.map(wide);
    // SAFETY: every string is an owned, NUL-terminated UTF-16 buffer that
    // outlives the call.
    let ok = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            backup
                .as_ref()
                .map_or(std::ptr::null(), |backup| backup.as_ptr()),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(format!(
            "Windows could not replace the MP3: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(target: &Path, replacement: &Path, backup: Option<&Path>) -> Result<(), String> {
    if let Some(backup) = backup {
        fs::rename(target, backup).map_err(|error| error.to_string())?;
    }
    fs::rename(replacement, target).map_err(|error| error.to_string())
}

// ── Ratings Music Library does not have yet ─────────────────────────────

/// A rating or Love saved in an MP3 that Music Library's catalog does not
/// show yet.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PendingRating {
    pub path: PathBuf,
    pub album_id: String,
    /// 0–100, or `None` for a cleared rating.
    pub rating: Option<i64>,
    /// Missing in lists saved by 0.12.0, which only had ratings.
    #[serde(default)]
    pub loved: Option<bool>,
}

fn pending_path() -> Option<PathBuf> {
    Some(crate::settings::data_dir()?.join("pending-ratings.json"))
}

pub fn load_pending() -> Vec<PendingRating> {
    pending_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Best effort; a list that fails to save only means a retry is lost if the
/// app closes before Music Library has the rating.
pub fn save_pending(pending: &[PendingRating]) {
    let Some(path) = pending_path() else {
        return;
    };
    if pending.is_empty() {
        let _ = fs::remove_file(path);
        return;
    }
    let Ok(text) = serde_json::to_string_pretty(pending) else {
        return;
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).ok();
    }
    let temp = path.with_extension("json.tmp");
    if fs::write(&temp, text).is_ok() {
        fs::rename(&temp, &path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use id3::frame::{Comment, Popularimeter};

    /// Fake audio: what matters is that it stays the same.
    const AUDIO: &[u8] = b"\xff\xfb\x90\x00 not really mpeg audio, but bytes that must survive";

    fn mp3(dir: &Path, version: Version, build: impl FnOnce(&mut Tag)) -> PathBuf {
        let path = dir.join("track.mp3");
        fs::write(&path, AUDIO).unwrap();
        let mut tag = Tag::with_version(version);
        tag.set_title("Song");
        build(&mut tag);
        tag.write_to_path(&path, version).unwrap();
        path
    }

    fn popm(user: &str, rating: u8, counter: u64) -> Popularimeter {
        Popularimeter {
            user: user.to_owned(),
            rating,
            counter,
        }
    }

    fn stars(n: u8) -> Option<Stars> {
        Some(Stars::new(n).unwrap())
    }

    #[test]
    fn only_whole_stars_from_one_to_five_exist() {
        assert_eq!(Stars::new(0), None);
        assert_eq!(Stars::new(6), None);
        let bytes: Vec<u8> = (1..=5)
            .map(|n| Stars::new(n).unwrap().musicbee_byte())
            .collect();
        assert_eq!(bytes, [1, 64, 128, 196, 255]);
    }

    #[test]
    fn writes_musicbees_exact_frame_and_keeps_everything_else() {
        for version in [Version::Id3v23, Version::Id3v24] {
            let dir = tempfile::tempdir().unwrap();
            let path = mp3(dir.path(), version, |tag| {
                tag.add_frame(popm("other-player", 77, 42));
                tag.add_frame(Comment {
                    lang: "eng".to_owned(),
                    description: String::new(),
                    text: "keep me".to_owned(),
                });
            });

            save(&path, Change::Rating(stars(4))).unwrap();

            let bytes = fs::read(&path).unwrap();
            let frame = [b"MusicBee\0".as_slice(), &[196, 0, 0, 0, 0]].concat();
            assert!(bytes.windows(frame.len()).any(|window| window == frame));
            assert!(bytes.ends_with(AUDIO));
            assert_eq!(read_values_from_path(&path).unwrap().rating, Some(80));
            let tag = Tag::read_from_path(&path).unwrap();
            assert!(tag.frames().any(|frame| {
                frame
                    .content()
                    .popularimeter()
                    .is_some_and(|value| *value == popm("other-player", 77, 42))
            }));
            assert_eq!(tag.comments().next().unwrap().text, "keep me");
            assert_eq!(tag.title(), Some("Song"));
            // No working copies or backups are left behind.
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn replaces_and_clears_the_rating() {
        let dir = tempfile::tempdir().unwrap();
        let path = mp3(dir.path(), Version::Id3v23, |tag| {
            tag.add_frame(popm(LEGACY_DEFAULT_OWNER, 153, 0));
        });
        assert_eq!(read_values_from_path(&path).unwrap().rating, Some(60));

        save(&path, Change::Rating(stars(5))).unwrap();
        let tag = Tag::read_from_path(&path).unwrap();
        let owners: Vec<_> = tag
            .frames()
            .filter_map(|frame| frame.content().popularimeter())
            .map(|value| (value.user.clone(), value.rating))
            .collect();
        assert_eq!(owners, [(MUSICBEE_OWNER.to_owned(), 255)]);

        save(&path, Change::Rating(None)).unwrap();
        assert_eq!(read_values_from_path(&path).unwrap().rating, None);
        let tag = Tag::read_from_path(&path).unwrap();
        assert_eq!(tag.frames().filter(|frame| frame.id() == "POPM").count(), 0);
        assert!(fs::read(&path).unwrap().ends_with(AUDIO));
    }

    #[test]
    fn rates_an_mp3_without_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare.mp3");
        fs::write(&path, AUDIO).unwrap();
        save(&path, Change::Rating(stars(1))).unwrap();
        assert_eq!(read_values_from_path(&path).unwrap().rating, Some(20));
        assert!(fs::read(&path).unwrap().ends_with(AUDIO));
    }

    #[test]
    fn reads_half_stars_rated_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let path = mp3(dir.path(), Version::Id3v23, |tag| {
            tag.add_frame(popm(MUSICBEE_OWNER, 186, 0));
        });
        assert_eq!(read_values_from_path(&path).unwrap().rating, Some(70));
        save(&path, Change::Rating(stars(3))).unwrap();
        assert_eq!(read_values_from_path(&path).unwrap().rating, Some(60));
    }

    #[test]
    fn leaves_unknown_ratings_and_other_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = mp3(dir.path(), Version::Id3v23, |tag| {
            tag.add_frame(popm(MUSICBEE_OWNER, 99, 0));
        });
        let before = fs::read(&path).unwrap();
        assert!(save(&path, Change::Rating(stars(2))).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);

        let flac = dir.path().join("track.flac");
        fs::write(&flac, AUDIO).unwrap();
        assert!(save(&flac, Change::Rating(stars(2))).is_err());
    }

    fn love_texts(path: &Path) -> Vec<(String, String)> {
        Tag::read_from_path(path)
            .unwrap()
            .extended_texts()
            .map(|text| (text.description.clone(), text.value.clone()))
            .collect()
    }

    #[test]
    fn toggles_love_like_musicbee_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = mp3(dir.path(), Version::Id3v24, |tag| {
            tag.add_frame(popm(MUSICBEE_OWNER, 196, 0));
            tag.add_frame(ExtendedText {
                description: "Other".to_owned(),
                value: "keep me".to_owned(),
            });
        });

        let loved = save(&path, Change::ToggleLove).unwrap();
        assert_eq!(
            loved,
            Values {
                rating: Some(80),
                love: Love::Loved
            }
        );
        let mut texts = love_texts(&path);
        texts.sort();
        assert_eq!(
            texts,
            [
                ("LOVE RATING".to_owned(), "L".to_owned()),
                ("Other".to_owned(), "keep me".to_owned())
            ]
        );
        assert!(fs::read(&path).unwrap().ends_with(AUDIO));

        // A second press reads the saved value and takes Love away again.
        assert_eq!(save(&path, Change::ToggleLove).unwrap().love, Love::Neutral);
        assert_eq!(
            love_texts(&path),
            [("Other".to_owned(), "keep me".to_owned())]
        );
        assert_eq!(read_values_from_path(&path).unwrap().rating, Some(80));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn banned_becomes_loved_and_a_rating_keeps_love() {
        let dir = tempfile::tempdir().unwrap();
        let path = mp3(dir.path(), Version::Id3v23, |tag| {
            tag.add_frame(ExtendedText {
                description: "love rating".to_owned(),
                value: "B".to_owned(),
            });
        });
        assert_eq!(read_values_from_path(&path).unwrap().love, Love::Banned);
        assert_eq!(save(&path, Change::ToggleLove).unwrap().love, Love::Loved);
        assert_eq!(
            love_texts(&path),
            [("LOVE RATING".to_owned(), "L".to_owned())]
        );

        let rated = save(&path, Change::Rating(stars(5))).unwrap();
        assert_eq!(
            rated,
            Values {
                rating: Some(100),
                love: Love::Loved
            }
        );
    }

    #[test]
    fn leaves_unknown_love_values_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = mp3(dir.path(), Version::Id3v23, |tag| {
            tag.add_frame(ExtendedText {
                description: LOVE_DESCRIPTION.to_owned(),
                value: "maybe".to_owned(),
            });
        });
        let before = fs::read(&path).unwrap();
        assert!(save(&path, Change::ToggleLove).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn reads_pending_lists_from_before_love() {
        let old = r#"[{"path":"D:\\a.mp3","album_id":"7","rating":60}]"#;
        let pending: Vec<PendingRating> = serde_json::from_str(old).unwrap();
        assert_eq!(pending[0].rating, Some(60));
        assert_eq!(pending[0].loved, None);
    }
}
