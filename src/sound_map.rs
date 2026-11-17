//! The coloured seek bar, as in Aurora 0.71.0: brightness and stereo width
//! over a track, the places where its sound changes, and where the main part
//! starts after a long quiet intro.
//!
//! Everything comes from Music Library's per-second timelines (`timeline-v1`
//! in `music-timelines.sqlite3`, one 29-value half-float row per second of
//! audio), found through its analysis store by the track's path. Aurora NG
//! only reads them. A track whose file changed since analysis gets no map.
//! The rules and numbers are Aurora's (`src-tauri/src/sound_map.rs`), so both
//! apps mark the same sections.

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{path::Path, time::Duration};

/// Music Library's analysis profile, which keys its tracks.
const PROFILE: &str = "bliss-0.13.0-symphonia-0.6.1-v2-full-mp3";
/// Bars in a map, like Aurora's waveform; the seek bar picks the bar under each of its own.
pub const BINS: usize = 640;
const FILE: &str = "music-timelines.sqlite3";
const VERSION: &str = "timeline-v1";
const COLUMNS: usize = 29;
/// Three hours, Music Library's limit.
const MAX_ROWS: usize = 3 * 60 * 60;
const CENTROID: usize = 12;
const LEVEL: usize = 13;
const WIDTH: usize = 14;

/// Seconds quieter than this have no colour: their brightness and width
/// describe noise, not music.
const QUIET_DB: f32 = -50.0;
/// Spectral centroids at the 5th and 95th percentile of a sample of the
/// library's seconds; brightness runs from 0 to 1 between them on a log scale.
const DULL_HZ: f32 = 600.0;
const BRIGHT_HZ: f32 = 3_400.0;
/// Side/Mid energy: about the 5th percentile (nearly mono) and the 95th
/// (very wide) of the library's seconds.
const NARROW_DB: f32 = -20.0;
const WIDE_DB: f32 = 0.0;

/// Seconds compared before and after each moment to find a change.
const NOVELTY_WINDOW: usize = 5;
/// Changes must stand this many standard deviations above the track's mean.
const SECTION_SIGMA: f32 = 1.0;
/// Sections are at least this many seconds apart.
const SECTION_GAP: usize = 10;

/// Only tracks this long can skip an intro.
const INTRO_TRACK_MIN: usize = 120;
/// An intro is at least this long ...
const INTRO_MIN: usize = 30;
/// ... at most this share of the track ...
const INTRO_MAX_SHARE: f32 = 0.5;
/// ... and this many dB quieter than the track's median second.
const INTRO_DROP_DB: f32 = 6.0;
/// The main part starts on a section boundary this close before or after the
/// level rises, when there is one.
const SNAP_BEFORE: usize = 8;
const SNAP_AFTER: usize = 2;

/// What the seek bar shows for one track.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundMap {
    /// Seconds of audio the timeline covers.
    pub seconds: usize,
    /// `BINS` values from 0 (dull) to 1 (bright). None where silent.
    pub brightness: Vec<Option<f32>>,
    /// `BINS` values from 0 (narrow) to 1 (wide). None where silent or mono.
    pub width: Vec<Option<f32>>,
    /// Seconds where the sound changes clearly.
    pub sections: Vec<usize>,
    /// Where the main part starts, after a long quiet intro.
    pub main_start: Option<usize>,
}

impl SoundMap {
    /// The map of the track at `path`, from the Music Library folder holding
    /// `catalog`. None when it has no current timeline. Meant for a
    /// background thread.
    pub fn for_track(catalog: &Path, path: &Path) -> Option<Self> {
        let folder = catalog.parent()?;
        let directory = path.parent()?.to_str()?;
        let filename = path.file_name()?.to_str()?;
        let analysis = open_read_only(&folder.join("music-analysis.sqlite3"))?;
        let (hash, size, modified) = analysis
            .query_row(
                "SELECT audio_hash,size,modified FROM sonic_tracks WHERE track_key=?1 AND profile=?2",
                params![crate::history::track_key(directory, filename), PROFILE],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .ok()??;
        if !file_is_current(path, size, &modified) {
            return None;
        }
        let timelines = open_read_only(&folder.join(FILE))?;
        let (rows, data) = timelines
            .query_row(
                "SELECT rows,data FROM timelines WHERE audio_hash=?1 AND version=?2 AND data IS NOT NULL",
                params![hash, VERSION],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .ok()??;
        Some(build(&decode(rows, &data)?))
    }
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

/// The size and modified time Music Library saw when it analyzed the file;
/// the time is nanoseconds since 1970, as text.
fn file_is_current(path: &Path, size: i64, modified: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && i64::try_from(metadata.len()).is_ok_and(|len| len == size)
            && metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|since| since.as_nanos().to_string() == modified)
    })
}

fn decode(rows: i64, data: &[u8]) -> Option<Vec<f32>> {
    let rows = usize::try_from(rows).ok()?;
    if rows == 0 || rows > MAX_ROWS || data.len() != rows * COLUMNS * 2 {
        return None;
    }
    Some(
        data.chunks_exact(2)
            .map(|pair| f16_to_f32(u16::from_le_bytes([pair[0], pair[1]])))
            .collect(),
    )
}

/// An IEEE half float as f32.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exponent = u32::from((bits >> 10) & 0x1f);
    let fraction = u32::from(bits & 0x3ff);
    let value = match exponent {
        0 if fraction == 0 => sign,
        0 => {
            let magnitude = fraction as f32 * 2f32.powi(-24);
            return if sign == 0 { magnitude } else { -magnitude };
        }
        0x1f => sign | 0x7f80_0000 | (fraction << 13),
        _ => sign | ((exponent + 127 - 15) << 23) | (fraction << 13),
    };
    f32::from_bits(value)
}

/// The map of a decoded timeline, `rows * COLUMNS` values.
pub fn build(values: &[f32]) -> SoundMap {
    let rows = values.len() / COLUMNS;
    let row = |second: usize| &values[second * COLUMNS..(second + 1) * COLUMNS];
    let brightness: Vec<Option<f32>> = (0..rows)
        .map(|second| {
            let row = row(second);
            (row[LEVEL] > QUIET_DB && row[CENTROID] > 0.0)
                .then(|| (row[CENTROID] / DULL_HZ).ln() / (BRIGHT_HZ / DULL_HZ).ln())
        })
        .collect();
    let width: Vec<Option<f32>> = (0..rows)
        .map(|second| {
            let row = row(second);
            (row[LEVEL] > QUIET_DB && row[WIDTH].is_finite())
                .then(|| (row[WIDTH] - NARROW_DB) / (WIDE_DB - NARROW_DB))
        })
        .collect();
    let levels: Vec<f32> = (0..rows).map(|second| row(second)[LEVEL]).collect();
    let sections = sections(&novelty(values, rows));
    let main_start = main_start(&levels, &sections);
    SoundMap {
        seconds: rows,
        brightness: bins(&brightness),
        width: bins(&width),
        sections,
        main_start,
    }
}

/// Seconds spread over `BINS` bins as averages, clamped to 0..1. With
/// fewer seconds than bins, a bin shows the second under it.
fn bins(seconds: &[Option<f32>]) -> Vec<Option<f32>> {
    let len = seconds.len();
    if len == 0 {
        return vec![None; BINS];
    }
    (0..BINS)
        .map(|bin| {
            let start = (bin * len / BINS).min(len - 1);
            let end = ((bin + 1) * len / BINS).clamp(start + 1, len);
            let (sum, count) = seconds[start..end]
                .iter()
                .flatten()
                .fold((0.0, 0), |(sum, count), value| (sum + value, count + 1));
            (count > 0).then(|| (sum / count as f32).clamp(0.0, 1.0))
        })
        .collect()
}

/// How much the sound changes at each second: one minus the cosine between
/// the average standardized row of the seconds before and after it.
fn novelty(values: &[f32], rows: usize) -> Vec<f32> {
    let mut novelty = vec![0.0; rows];
    if rows < NOVELTY_WINDOW * 2 + 1 {
        return novelty;
    }
    // Standardize each column; missing values (mono width) count as average.
    let mut z = vec![0f64; rows * COLUMNS];
    for column in 0..COLUMNS {
        let finite = (0..rows)
            .map(|second| f64::from(values[second * COLUMNS + column]))
            .filter(|value| value.is_finite());
        let (sum, squares, count) =
            finite.fold((0.0, 0.0, 0usize), |(s, q, n), v| (s + v, q + v * v, n + 1));
        if count == 0 {
            continue;
        }
        let mean = sum / count as f64;
        let deviation = (squares / count as f64 - mean * mean).max(0.0).sqrt();
        let deviation = if deviation < 1e-6 { 1.0 } else { deviation };
        for second in 0..rows {
            let value = f64::from(values[second * COLUMNS + column]);
            if value.is_finite() {
                z[second * COLUMNS + column] = (value - mean) / deviation;
            }
        }
    }
    // Running sums make each window average a subtraction.
    let mut sums = vec![0f64; (rows + 1) * COLUMNS];
    for second in 0..rows {
        for column in 0..COLUMNS {
            sums[(second + 1) * COLUMNS + column] =
                sums[second * COLUMNS + column] + z[second * COLUMNS + column];
        }
    }
    let at = |second: usize, column: usize| sums[second * COLUMNS + column];
    for (second, value) in novelty
        .iter_mut()
        .enumerate()
        .take(rows - NOVELTY_WINDOW + 1)
        .skip(NOVELTY_WINDOW)
    {
        let (mut dot, mut before_norm, mut after_norm) = (0.0, 0.0, 0.0);
        for column in 0..COLUMNS {
            let before = at(second, column) - at(second - NOVELTY_WINDOW, column);
            let after = at(second + NOVELTY_WINDOW, column) - at(second, column);
            dot += before * after;
            before_norm += before * before;
            after_norm += after * after;
        }
        let denominator = (before_norm * after_norm).sqrt();
        if denominator > 0.0 {
            *value = (1.0 - dot / denominator) as f32;
        }
    }
    novelty
}

/// Peaks of `novelty` that stand out from the track, strongest first when
/// two are closer than `SECTION_GAP`, returned in time order.
fn sections(novelty: &[f32]) -> Vec<usize> {
    let rows = novelty.len();
    if rows < NOVELTY_WINDOW * 2 + 3 {
        return Vec::new();
    }
    let scored = &novelty[NOVELTY_WINDOW..=rows - NOVELTY_WINDOW];
    let mean = scored.iter().sum::<f32>() / scored.len() as f32;
    let deviation =
        (scored.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / scored.len() as f32).sqrt();
    let threshold = mean + SECTION_SIGMA * deviation;
    let mut candidates: Vec<usize> = (1..rows - 1)
        .filter(|&t| {
            novelty[t] > threshold && novelty[t] >= novelty[t - 1] && novelty[t] >= novelty[t + 1]
        })
        .collect();
    candidates.sort_by(|a, b| novelty[*b].total_cmp(&novelty[*a]).then(a.cmp(b)));
    let mut kept: Vec<usize> = Vec::new();
    for candidate in candidates {
        if kept
            .iter()
            .all(|second| second.abs_diff(candidate) >= SECTION_GAP)
        {
            kept.push(candidate);
        }
    }
    kept.sort_unstable();
    kept
}

/// Where the main part starts: the end of a long opening stretch that is
/// clearly quieter than the rest of the track, moved onto a nearby change.
fn main_start(levels: &[f32], sections: &[usize]) -> Option<usize> {
    let rows = levels.len();
    if rows < INTRO_TRACK_MIN {
        return None;
    }
    let mut sorted = levels.to_vec();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[rows / 2];
    // Five seconds around each moment, averaged as power so a loud second
    // is not hidden by quiet ones.
    let smoothed = |second: usize| {
        let start = second.saturating_sub(2);
        let end = (second + 3).min(rows);
        let power = levels[start..end]
            .iter()
            .map(|db| 10f32.powf(db / 10.0))
            .sum::<f32>()
            / (end - start) as f32;
        10.0 * power.max(1e-12).log10()
    };
    let end = (0..rows).find(|second| smoothed(*second) >= median - INTRO_DROP_DB)?;
    if end < INTRO_MIN || end as f32 > rows as f32 * INTRO_MAX_SHARE {
        return None;
    }
    Some(
        sections
            .iter()
            .copied()
            .filter(|second| *second + SNAP_BEFORE >= end && *second <= end + SNAP_AFTER)
            .max()
            .unwrap_or(end),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A timeline of `levels`, with brightness and width per second.
    fn timeline(seconds: &[(f32, f32, f32)]) -> Vec<f32> {
        seconds
            .iter()
            .flat_map(|(level, centroid, width)| {
                let mut row = [0.0; COLUMNS];
                row[0] = 1.0;
                row[CENTROID] = *centroid;
                row[LEVEL] = *level;
                row[WIDTH] = *width;
                row[15] = 0.5;
                row
            })
            .collect()
    }

    #[test]
    fn colours_brightness_and_width_and_leaves_silence_blank() {
        let mut seconds = vec![(-15.0, DULL_HZ, NARROW_DB); 320];
        seconds.extend(vec![(-15.0, BRIGHT_HZ, WIDE_DB); 310]);
        seconds.extend(vec![(-90.0, 2_000.0, f32::NAN); 10]);
        let map = build(&timeline(&seconds));
        assert_eq!(map.seconds, 640);
        assert_eq!(map.brightness.len(), BINS);
        assert!(map.brightness[0].unwrap() < 0.01);
        assert!(map.brightness[400].unwrap() > 0.99);
        assert_eq!(map.width[0], Some(0.0));
        assert_eq!(map.width[400], Some(1.0));
        assert_eq!(map.brightness[635], None);
        assert_eq!(map.width[635], None);
    }

    #[test]
    fn short_tracks_repeat_seconds_across_bins() {
        let map = build(&timeline(&[(-15.0, 1_000.0, -10.0); 4]));
        assert_eq!(map.width.len(), BINS);
        assert!(map.width.iter().all(|value| *value == Some(0.5)));
        assert!(map.sections.is_empty());
        assert_eq!(map.main_start, None);
    }

    #[test]
    fn finds_the_change_between_two_sounds() {
        let mut seconds = vec![(-30.0, 800.0, -15.0); 60];
        seconds.extend(vec![(-12.0, 3_000.0, -3.0); 60]);
        // Small differences inside each half, so the change stands out.
        for (index, second) in seconds.iter_mut().enumerate() {
            second.1 += (index % 7) as f32 * 15.0;
        }
        let map = build(&timeline(&seconds));
        assert!(map.sections.contains(&60), "{:?}", map.sections);
        assert!(
            map.sections
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= SECTION_GAP)
        );
    }

    #[test]
    fn skips_a_long_quiet_intro_onto_its_change() {
        let mut seconds = vec![(-35.0, 700.0, -12.0); 45];
        seconds.extend(vec![(-12.0, 2_500.0, -4.0); 200]);
        for (index, second) in seconds.iter_mut().enumerate() {
            second.1 += (index % 5) as f32 * 20.0;
        }
        let map = build(&timeline(&seconds));
        assert_eq!(map.main_start, Some(45));
    }

    #[test]
    fn no_skip_for_short_intros_or_quiet_tracks() {
        let mut short = vec![(-35.0, 700.0, -12.0); 15];
        short.extend(vec![(-12.0, 2_500.0, -4.0); 200]);
        assert_eq!(build(&timeline(&short)).main_start, None);
        // Quiet for most of the track: that is the track, not an intro.
        let mut late = vec![(-35.0, 700.0, -12.0); 150];
        late.extend(vec![(-12.0, 2_500.0, -4.0); 100]);
        assert_eq!(build(&timeline(&late)).main_start, None);
    }

    #[test]
    fn rejects_timelines_of_the_wrong_size() {
        assert!(decode(2, &[0; COLUMNS * 4]).is_some());
        assert!(decode(2, &[0; COLUMNS * 4 - 2]).is_none());
        assert!(decode(0, &[]).is_none());
    }

    #[test]
    fn decodes_half_floats() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert!(f16_to_f32(0x7e00).is_nan());
    }

    /// Read-only proof on the real library, the same tracks as Aurora's:
    /// `cargo test sound_map::tests::real_library_proof -- --ignored --nocapture`
    #[test]
    #[ignore = "reads the real Music Library"]
    fn real_library_proof() {
        let catalog = crate::library::catalog_path().unwrap();
        let connection = open_read_only(&catalog).unwrap();
        let mut tracks = connection
            .prepare("SELECT file_path,filename,title FROM tracks WHERE title IN ('Imperium','Moving in Stereo') LIMIT 6")
            .unwrap();
        let rows: Vec<(String, String, String)> = tracks
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (directory, filename, title) in rows {
            let path = Path::new(&directory).join(&filename);
            match SoundMap::for_track(&catalog, &path) {
                Some(map) => println!(
                    "{title} ({} s): sections {:?}, main part {:?}",
                    map.seconds, map.sections, map.main_start
                ),
                None => println!("{title}: no current timeline"),
            }
        }
    }
}
