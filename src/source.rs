pub mod converted;
pub mod empty;
pub mod file;
pub mod mapped;
pub mod mixed;
pub mod resampled;
pub mod synth;

// -------------------------------------------------------------------------------------------------

/// Timing info for `AudioSource` buffers.
#[derive(Debug, Copy, Clone)]
pub struct AudioSourceTime {
    /// Time in absolute sample frames.
    pub pos_in_frames: u64,
}

// -------------------------------------------------------------------------------------------------

/// AudioSource types produce audio samples in `f32` format and can be `Send` and `Sync`ed
/// across threads.
///
/// The output buffer is a raw interleaved buffer, which is going to be written by the source
/// in the specified `channel_count` and `sample_rate` specs. Specs may not change during runtime,
/// so following sources don't have to adapt to new specs.
///
/// `write` is called in the realtime audio thread, so it must not block!
pub trait AudioSource: Send + Sync + 'static {
    /// Write at most of `output.len()` samples into the interleaved `output`
    /// The given [`AudioSourceTime`] parameter specifies which absolute time this buffer in the
    /// final output stream refers to. It can be used to schedule and apply real-time events.
    /// Returns the number of written **samples** (not frames).
    fn write(&mut self, output: &mut [f32], time: &AudioSourceTime) -> usize;
    /// The source's output channel count.
    fn channel_count(&self) -> usize;
    /// The source's output sample rate.
    fn sample_rate(&self) -> u32;
    /// returns if the source finished playback. Exhausted sources should only return 0 on `write`
    /// and can be removed from a source render graph.
    fn is_exhausted(&self) -> bool;
}
