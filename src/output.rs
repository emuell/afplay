#[cfg(feature = "cpal")]
pub mod cpal;
#[cfg(feature = "cubeb")]
pub mod cubeb;

/// The enabled audio output type: cpal or cubeb
#[cfg(feature = "cpal")]
pub type DefaultAudioOutput = cpal::CpalOutput;
#[cfg(feature = "cubeb")]
pub type DefaultAudioOutput = cubeb::CubebOutput;

/// The enabled audio output sink type: cpal or cubeb
pub type DefaultAudioSink = <DefaultAudioOutput as AudioOutput>::Sink;

use super::source::AudioSource;

// -------------------------------------------------------------------------------------------------

/// AudioOutput controller
pub trait AudioSink {
    fn channel_count(&self) -> usize;
    fn sample_rate(&self) -> u32;

    fn set_volume(&self, volume: f32);

    fn play(&self, source: impl AudioSource);
    fn pause(&self);
    fn resume(&self);
    fn stop(&self);

    // release audio device
    fn close(&self);
}

// -------------------------------------------------------------------------------------------------

/// AudioOutput implementation: provides a sink controller.
pub trait AudioOutput {
    type Sink: AudioSink;
    fn sink(&self) -> Self::Sink;
}
