//! The waveform drawn in the seek bar: how loud each moment of a track is,
//! read from the audio file itself.

use rodio::{Decoder, Source};
use std::{fs::File, path::Path};

/// How much audio each loudness value covers, in seconds.
const BLOCK_SECONDS: f32 = 0.01;

/// The loudness of a track, one value per 10 ms block.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Waveform {
    /// The RMS level of each block, 0.0 to 1.0.
    blocks: Vec<f32>,
}

impl Waveform {
    /// Decodes the whole file and measures how loud each block is. Meant
    /// for a background thread: a long track takes a moment.
    pub fn analyze(path: &Path) -> Result<Self, String> {
        let file = File::open(path)
            .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
        let decoder = Decoder::try_from(file)
            .map_err(|error| format!("Could not decode {}: {error}", path.display()))?;
        let samples_per_block =
            (decoder.sample_rate().get() as f32 * decoder.channels().get() as f32 * BLOCK_SECONDS)
                .round()
                .max(1.) as usize;
        Ok(Self::from_samples(decoder, samples_per_block))
    }

    /// Measures interleaved samples, `samples_per_block` at a time.
    fn from_samples(samples: impl Iterator<Item = f32>, samples_per_block: usize) -> Self {
        let mut blocks = Vec::new();
        let mut sum = 0f32;
        let mut count = 0;
        for sample in samples {
            sum += sample * sample;
            count += 1;
            if count == samples_per_block {
                blocks.push((sum / count as f32).sqrt());
                sum = 0.;
                count = 0;
            }
        }
        if count > 0 {
            blocks.push((sum / count as f32).sqrt());
        }
        Self { blocks }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The track squeezed into `count` bars, each 0.0 to 1.0, with the
    /// loudest bar at 1.0 so quiet masters still fill the seek bar.
    pub fn bars(&self, count: usize) -> Vec<f32> {
        if count == 0 || self.blocks.is_empty() {
            return Vec::new();
        }
        let len = self.blocks.len();
        let mut bars: Vec<f32> = (0..count)
            .map(|bar| {
                // With more bars than blocks, a bar shows the block under it.
                let start = (bar * len / count).min(len - 1);
                let end = ((bar + 1) * len / count).clamp(start + 1, len);
                let blocks = &self.blocks[start..end];
                blocks.iter().sum::<f32>() / blocks.len() as f32
            })
            .collect();
        let loudest = bars.iter().copied().fold(0f32, f32::max);
        if loudest > 0. {
            for bar in &mut bars {
                *bar /= loudest;
            }
        }
        bars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_each_block() {
        let samples = [0.5, -0.5, 0.5, -0.5, 0., 0., 0., 0., 1., -1.];
        let waveform = Waveform::from_samples(samples.into_iter(), 4);
        assert_eq!(waveform.blocks, vec![0.5, 0., 1.]);
    }

    #[test]
    fn bars_are_scaled_to_the_loudest() {
        let waveform = Waveform {
            blocks: vec![0.1, 0.1, 0.2, 0.2, 0.4, 0.4],
        };
        assert_eq!(waveform.bars(3), vec![0.25, 0.5, 1.]);
    }

    #[test]
    fn more_bars_than_blocks_repeats_blocks() {
        let waveform = Waveform {
            blocks: vec![0.5, 1.],
        };
        assert_eq!(waveform.bars(4), vec![0.5, 0.5, 1., 1.]);
    }

    #[test]
    fn silence_and_nothing_give_flat_bars() {
        let silent = Waveform {
            blocks: vec![0.; 10],
        };
        assert_eq!(silent.bars(2), vec![0., 0.]);
        assert!(Waveform::default().bars(5).is_empty());
        assert!(silent.bars(0).is_empty());
    }
}
