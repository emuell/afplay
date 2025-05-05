pub mod preloaded;
pub mod streamed;

use std::{time::Duration, sync::Arc};

use crossbeam_channel::Sender;
use crossbeam_queue::ArrayQueue;

use crate::{
    player::{AudioFilePlaybackId, AudioFilePlaybackStatusContext, AudioFilePlaybackStatusEvent},
    source::{resampled::ResamplingQuality, AudioSource},
    utils::db_to_linear,
    Error,
};

// -------------------------------------------------------------------------------------------------

/// Options to control playback of a FileSource.
#[derive(Clone, Copy)]
pub struct FilePlaybackOptions {
    /// By default false: when true, the file will be decoded and streamed on the fly.
    /// This should be enabled for very long files only, especiall when a lot of files are
    /// going to be played at once.
    pub stream: bool,

    /// By default 1.0f32. Customize to lower or raise the volume of the file.
    pub volume: f32,

    /// By default 0.0f32. Set in range -1.0..=1.0 to adjust panning position.
    pub panning: f32,

    /// By default 1.0f64. Customize to pitch the playback speed up or down.
    /// See also `resampling_quality` property.
    pub speed: f64,

    /// By default 0: when > 0 the number of times the file should be looped.
    /// Set to usize::MAX to repeat forever.
    pub repeat: usize,

    /// By default None: when set, the source should start playing at the given
    /// sample frame time in the audio output stream.
    pub start_time: Option<u64>,

    /// By default None: when set, the source's volume will fade in with the given
    /// amount when starting to play.
    pub fade_in_duration: Option<Duration>,
    /// By default 5ms: volume fade out duration, applied when the the source gets
    /// stopped before it finished playing.
    pub fade_out_duration: Option<Duration>,

    /// By default ResamplingQuality::Default: Quality mode of a applied resampler,
    /// either when the source is getting played back on a stream with a sample rate
    /// which does not match the file's sample rate or when pitching the playback up
    /// or down.
    pub resampling_quality: ResamplingQuality,

    /// Wallclock time rate of playback pos events, emited via AudioFilePlaybackStatusEvent
    /// in the player. By default one second to avoid unnecessary overhead.
    /// Set to e.g. Duration::from_secf32(1.0/30.0) to trigger events 30 times per second.
    pub playback_pos_emit_rate: Option<Duration>,
}

impl Default for FilePlaybackOptions {
    fn default() -> Self {
        Self {
            stream: false,
            volume: 1.0,
            panning: 0.0,
            speed: 1.0,
            repeat: 0,
            start_time: None,
            fade_in_duration: None,
            fade_out_duration: Some(Duration::from_millis(50)),
            resampling_quality: ResamplingQuality::Default,
            playback_pos_emit_rate: Some(Duration::from_secs(1)),
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

    pub fn volume(mut self, volume: f32) -> Self {
        self.volume = volume;
        self
    }
    pub fn volume_db(mut self, volume_db: f32) -> Self {
        self.volume = db_to_linear(volume_db);
        self
    }

     pub fn panning(mut self, panning: f32) -> Self {
        self.panning = panning;
        self
    }

    pub fn fade_in(mut self, duration: Duration) -> Self {
        self.fade_in_duration = Some(duration);
        self
    }
    pub fn fade_out(mut self, duration: Duration) -> Self {
        self.fade_out_duration = Some(duration);
        self
    }

    pub fn speed(mut self, speed: f64) -> Self {
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

    pub fn start_at_time(mut self, sample_time: u64) -> Self {
        self.start_time = Some(sample_time);
        self
    }

    pub fn playback_pos_emit_rate(mut self, duration: std::time::Duration) -> Self {
        self.playback_pos_emit_rate = Some(duration);
        self
    }

    pub fn resampling_quality(mut self, quality: ResamplingQuality) -> Self {
        self.resampling_quality = quality;
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
        if !(-1.0..=1.0).contains(&self.panning) || self.panning.is_nan() {
            return Err(Error::ParameterError(format!(
                "playback options 'panning' value is '{}'",
                self.panning
            )));
        }
        if self.speed < 0.0 || self.speed.is_nan() || self.speed.is_infinite() {
            return Err(Error::ParameterError(format!(
                "playback options 'speed' value is '{}'",
                self.speed
            )));
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------

/// Events to control playback of a FileSource
pub enum FilePlaybackMessage {
    /// Seek the file source to a new position
    Seek(Duration),
    /// Stop the source
    Stop,
}

// -------------------------------------------------------------------------------------------------

/// A source which decodes and plays back an audio file.
pub trait FileSource: AudioSource {
    /// A unique ID, which can be used to identify sources in `PlaybackStatusEvent`s.
    fn playback_id(&self) -> AudioFilePlaybackId;

    /// Message queue to control file playback.
    fn playback_message_queue(&self) -> Arc<ArrayQueue<FilePlaybackMessage>>;
    
    /// Channel to receive playback status from the file.
    fn playback_status_sender(&self) -> Option<Sender<AudioFilePlaybackStatusEvent>>;
    fn set_playback_status_sender(&mut self, sender: Option<Sender<AudioFilePlaybackStatusEvent>>);

    /// Optional context passed along with the playback status.
    fn playback_status_context(&self) -> Option<AudioFilePlaybackStatusContext>;
    fn set_playback_status_context(&mut self, context: Option<AudioFilePlaybackStatusContext>);

    /// Total number of sample frames in the decoded file: may not be known before playback finished.
    fn total_frames(&self) -> Option<u64>;
    /// Current playback pos in frames
    fn current_frame_position(&self) -> u64;

    /// True when the source played through the entire file, else false.
    fn end_of_track(&self) -> bool;
}
