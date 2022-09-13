use crate::{converted::ConvertedSource, utils::resampler::InterpolationType};

use super::AudioSource;
use crossbeam_channel::{unbounded, Receiver, Sender};

// -------------------------------------------------------------------------------------------------

struct PlayingSource {
    is_active: bool,
    source: Box<dyn AudioSource>,
}

// -------------------------------------------------------------------------------------------------

pub enum MixedSourceMsg {
    AddSource { source: Box<dyn AudioSource> },
}

// -------------------------------------------------------------------------------------------------

/// A source which converts and mixes other sources together.
pub struct MixedSource {
    playing_sources: Vec<PlayingSource>,
    event_send: Sender<MixedSourceMsg>,
    event_recv: Receiver<MixedSourceMsg>,
    channel_count: usize,
    sample_rate: u32,
    temp_out: Vec<f32>,
}

impl MixedSource {
    /// Create a new mixer source with the given signal specs
    pub fn new(channel_count: usize, sample_rate: u32) -> Self {
        let (event_send, event_recv) = unbounded::<MixedSourceMsg>();
        const BUFFER_SIZE: usize = 8 * 1024;
        Self {
            playing_sources: Vec::new(),
            event_recv,
            event_send,
            channel_count,
            sample_rate,
            temp_out: vec![0.0; BUFFER_SIZE],
        }
    }

    /// Add a source to the mix
    pub fn add(&mut self, source: impl AudioSource, interpolation: InterpolationType) {
        let converted = Box::new(ConvertedSource::new(
            source,
            self.channel_count,
            self.sample_rate,
            interpolation,
        ));
        if let Err(err) = self
            .event_send
            .send(MixedSourceMsg::AddSource { source: converted })
        {
            log::error!("Failed to add mixer source: {}", { err });
        }
    }

    /// Allows controlling the mixer via a message channel.
    /// NB: When adding new sources, ensure they match the mixers sample rate and channel layout
    pub(crate) fn event_sender(&self) -> crossbeam_channel::Sender<MixedSourceMsg> {
        self.event_send.clone()
    }
}

impl AudioSource for MixedSource {
    fn write(&mut self, output: &mut [f32]) -> usize {
        // process events
        while let Ok(event) = self.event_recv.try_recv() {
            match event {
                MixedSourceMsg::AddSource { source } => {
                    debug_assert_eq!(
                        source.channel_count(),
                        self.channel_count,
                        "adjust source's channel layout before adding it"
                    );
                    debug_assert_eq!(
                        source.sample_rate(),
                        self.sample_rate,
                        "adjust source's sample rate before adding it"
                    );
                    self.playing_sources.push(PlayingSource {
                        is_active: true,
                        source,
                    });
                }
            }
        }
        // return empty handed when we have no sources
        if self.playing_sources.is_empty() {
            return 0;
        }
        // clear output as we're only adding below
        for o in output.iter_mut() {
            *o = 0_f32;
        }
        // run and add all playing sources
        let mut max_written = 0usize;
        for playing_source in self.playing_sources.iter_mut() {
            let source = &mut playing_source.source;
            let mut total_written = 0;
            'source: while total_written < output.len() {
                // run source on temp_out until we've filled up the whole final output
                let remaining = output.len() - total_written;
                let to_write = remaining.min(self.temp_out.len());
                let written = source.write(&mut self.temp_out[..to_write]);
                if source.is_exhausted() {
                    // source no longer is playing: mark it as inactive
                    playing_source.is_active = false;
                    break 'source;
                }
                // add output of the source to the final output
                let remaining_out = &mut output[total_written..];
                let written_out = &self.temp_out[..written];
                for (o, i) in remaining_out.iter_mut().zip(written_out) {
                    *o += *i;
                }
                total_written += written;
            }
            max_written = max_written.max(total_written);
        }
        // drain inactive sources
        self.playing_sources.retain(|s| s.is_active);
        // return modified output len (we've cleared output entirely)
        output.len()
    }

    fn channel_count(&self) -> usize {
        self.channel_count
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn is_exhausted(&self) -> bool {
        // mixer never is exhausted, as we may get new sources added
        false
    }
}
