//! Audio output: one rodio player that plays a single file at a time.

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};
use std::{fs::File, path::Path, time::Duration};

pub struct Audio {
    // The device sink must stay alive for as long as anything plays.
    _sink: MixerDeviceSink,
    player: Player,
}

impl Audio {
    pub fn new() -> Result<Self, String> {
        let mut sink = DeviceSinkBuilder::open_default_sink()
            .map_err(|error| format!("Could not open the audio device: {error}"))?;
        sink.log_on_drop(false);
        let player = Player::connect_new(sink.mixer());
        Ok(Self {
            _sink: sink,
            player,
        })
    }

    /// Replaces whatever is playing with `path` and starts it.
    pub fn play_file(&self, path: &Path) -> Result<(), String> {
        let file = File::open(path)
            .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
        let decoder = Decoder::try_from(file)
            .map_err(|error| format!("Could not decode {}: {error}", path.display()))?;
        self.player.clear();
        self.player.append(decoder);
        self.player.play();
        Ok(())
    }

    pub fn stop(&self) {
        self.player.clear();
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn resume(&self) {
        self.player.play();
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    /// True once the current file has played to its end.
    pub fn is_finished(&self) -> bool {
        self.player.empty()
    }

    pub fn position(&self) -> Duration {
        self.player.get_pos()
    }

    pub fn seek(&self, position: Duration) {
        // A failed seek just leaves playback where it was.
        let _ = self.player.try_seek(position);
    }
}
