#[cfg(feature = "dasp")]
pub mod dasp;

use crossbeam_channel::Sender;
use std::time::Duration;

use crate::{player::AudioFilePlaybackId, source::AudioSource, utils::db_to_linear, Error};

// -------------------------------------------------------------------------------------------------

/// Options to control playback of a FileSource
#[derive(Clone, Copy)]
pub struct SynthPlaybackOptions {
    /// By default 1.0f32. Customize to lower or raise the volume of the file.
    pub volume: f32,
}

impl Default for SynthPlaybackOptions {
    fn default() -> Self {
        Self { volume: 1.0f32 }
    }
}

impl SynthPlaybackOptions {
    pub fn volume(mut self, volume: f32) -> Self {
        self.volume = volume;
        self
    }
    pub fn volume_db(mut self, volume_db: f32) -> Self {
        self.volume = db_to_linear(volume_db);
        self
    }

    /// Validate all parameters. Returns Error::ParameterError on errors.
    pub fn validate(&self) -> Result<(), Error> {
        if self.volume < 0.0 || self.volume.is_nan() {
            return Err(Error::ParameterError(format!(
                "playback options 'volume' value is '{}'",
                self.volume
            )));
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------

/// Events to control playback of a synth source
pub enum SynthPlaybackMessage {
    /// Stop the synth source
    Stop(Duration),
}

// -------------------------------------------------------------------------------------------------

/// A source which creates samples from a synthesized signal.
pub trait SynthSource: AudioSource + Sized {
    /// Channel sender to control this sources's playback
    fn playback_message_sender(&self) -> Sender<SynthPlaybackMessage>;
    /// A unique ID, which can be used to identify sources in `PlaybackStatusEvent`s
    fn playback_id(&self) -> AudioFilePlaybackId;
}
