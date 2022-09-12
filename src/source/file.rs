pub mod preloaded;
pub mod streamed;

use crossbeam_channel::Sender;
use std::time::Duration;

use super::{
    playback::{PlaybackId, PlaybackStatusEvent},
    AudioSource,
};
use crate::utils::db_to_linear;

// -------------------------------------------------------------------------------------------------

/// Options to control playback of a FileSource
#[derive(Clone, Copy)]
pub struct FilePlaybackOptions {
    /// By default false: when true, the file will be decoded and streamed on the fly.
    /// This should be enabled for very long files only, especiall when a lot of files are
    /// going to be played at once.
    pub stream: bool,
    /// By default 1.0f32. Customize to lower or raise the volume of the file.
    pub volume: f32,
    /// By default 1.0f64. Customize to pitch the playback speed up or down.
    pub speed: f64,
    /// By default 0: when > 0 the number of times the file should be looped.
    /// Set to usize::MAX to repeat forever.
    pub repeat: usize,
}

impl Default for FilePlaybackOptions {
    fn default() -> Self {
        Self {
            stream: false,
            volume: 1.0,
            speed: 1.0,
            repeat: 0,
        }
    }
}

impl FilePlaybackOptions {
    pub fn preloaded(mut self) -> Self {
        self.stream = false;
        self
    }
    pub fn streamed(mut self) -> Self {
        self.stream = true;
        self
    }

    pub fn with_volume(mut self, volume: f32) -> Self {
        self.volume = volume;
        self
    }
    pub fn with_volume_db(mut self, volume_db: f32) -> Self {
        self.volume = db_to_linear(volume_db);
        self
    }

    pub fn with_speed(mut self, speed: f64) -> Self {
        self.speed = speed;
        self
    }

    pub fn repeat(mut self, count: usize) -> Self {
        self.repeat = count;
        self
    }
    pub fn repeat_forever(mut self) -> Self {
        self.repeat = usize::MAX;
        self
    }
}

// -------------------------------------------------------------------------------------------------

/// Events to control playback of a FileSource
pub enum FilePlaybackMessage {
    /// Seek the file source to a new position
    Seek(Duration),
    /// Start reading streamed sources (internally used only)
    Read,
    /// Stop the source with the given fade-out duration
    Stop(Duration),
}

// -------------------------------------------------------------------------------------------------

/// A source which decodes an audio file
pub trait FileSource: AudioSource + Sized {
    /// Channel to control file playback.
    fn playback_message_sender(&self) -> Sender<FilePlaybackMessage>;
    /// A unique ID, which can be used to identify sources in `PlaybackStatusEvent`s.
    fn playback_id(&self) -> PlaybackId;

    /// Total number of sample frames in the decoded file: may not be known before playback finished.
    fn total_frames(&self) -> Option<u64>;
    /// Current playback pos in frames
    fn current_frame_position(&self) -> u64;

    /// True when the source played through the entire file, else false.
    fn end_of_track(&self) -> bool;
}
