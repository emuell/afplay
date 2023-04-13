use std::sync::Arc;

use crossbeam_channel::Sender;
use crossbeam_queue::ArrayQueue;

use crate::{
    player::{AudioSourceDropEvent, PlaybackMessageSender},
    source::{AudioSource, AudioSourceTime},
    AudioFilePlaybackId,
};

// -------------------------------------------------------------------------------------------------

/// Mixer internal struct to keep track of currently playing sources.
struct MixedPlayingSource {
    is_active: bool,
    playback_id: AudioFilePlaybackId,
    playback_message_queue: PlaybackMessageSender,
    source: Arc<dyn AudioSource>,
    start_time: u64,
    stop_time: Option<u64>,
}

// -------------------------------------------------------------------------------------------------

/// Messages send from player to mixer to start or stop playing sources.
pub enum MixedSourceMsg {
    AddSource {
        playback_id: AudioFilePlaybackId,
        playback_message_queue: PlaybackMessageSender,
        source: Arc<dyn AudioSource>,
        sample_time: u64,
    },
    StopSource {
        playback_id: AudioFilePlaybackId,
        sample_time: u64,
    },
    RemoveAllSources,
    RemoveAllPendingSources,
}

// -------------------------------------------------------------------------------------------------

/// A source which converts and mixes other sources together.
pub struct MixedSource {
    playing_sources: Vec<MixedPlayingSource>,
    event_queue: Arc<ArrayQueue<MixedSourceMsg>>,
    drop_send: Sender<AudioSourceDropEvent>,
    channel_count: usize,
    sample_rate: u32,
    temp_out: Vec<f32>,
}

impl MixedSource {
    /// Create a new mixer source with the given signal specs.
    /// Param `sample_time` is the intial sample frame time that we start to run with.
    /// This usually will be the audio outputs playback pos.
    pub fn new(
        channel_count: usize,
        sample_rate: u32,
        drop_send: Sender<AudioSourceDropEvent>,
    ) -> Self {
        // assume that we'll never add more than on event per sample with a delay/buffer of a second
        // even if we exceed this size, this won't panic, but will skip older events...
        let event_queue_size = sample_rate as usize * 2;
        let event_queue = Arc::new(ArrayQueue::new(event_queue_size));
        // temp mix buffer size
        const BUFFER_SIZE: usize = 8 * 1024;
        // avoid allocs in real-time threads
        const PLAYING_EVENTS_CAPACITY: usize = 1024;
        Self {
            playing_sources: Vec::with_capacity(PLAYING_EVENTS_CAPACITY),
            event_queue,
            drop_send,
            channel_count,
            sample_rate,
            temp_out: vec![0.0; BUFFER_SIZE],
        }
    }

    /// Allows controlling the mixer by pushing messages into this event queue.
    /// NB: When adding new sources, ensure they match the mixers sample rate and channel layout
    pub(crate) fn event_queue(&self) -> Arc<ArrayQueue<MixedSourceMsg>> {
        self.event_queue.clone()
    }

    /// remove all entries from self.playing_sources which match the given filter function.
    fn remove_matching_sources<F>(&mut self, match_fn: F)
    where
        F: Fn(&MixedPlayingSource) -> bool,
    {
        let drop_send = self.drop_send.clone();
        self.playing_sources.retain(move |p| {
            if match_fn(p) {
                // drop it in the player's main thread if it has no other refs
                if let Err(err) = drop_send.try_send(AudioSourceDropEvent::new(p.source.clone())) {
                    log::warn!("failed to send drop source event: {}", err)
                }
                false // remove
            } else {
                true // keep
            }
        });
    }

    /// remove all entries from self.playing_sources.
    fn remove_all_sources(&mut self) {
        let drop_send = self.drop_send.clone();
        for p in self.playing_sources.iter() {
            // drop it in the player's main thread if it has no other refs
            if let Err(err) = drop_send.try_send(AudioSourceDropEvent::new(p.source.clone())) {
                log::warn!("failed to send drop source event: {}", err)
            }
        }
        self.playing_sources.clear();
    }
}

impl AudioSource for MixedSource {
    fn write(&mut self, output: &mut [f32], time: &AudioSourceTime) -> usize {
        // process events
        let mut got_new_sources = false;
        while let Some(event) = self.event_queue.pop() {
            match event {
                MixedSourceMsg::AddSource {
                    playback_id,
                    playback_message_queue,
                    source,
                    sample_time,
                } => {
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
                    got_new_sources = true;
                    self.playing_sources.push(MixedPlayingSource {
                        is_active: true,
                        playback_id,
                        playback_message_queue,
                        source,
                        start_time: sample_time,
                        stop_time: None,
                    });
                }
                MixedSourceMsg::StopSource {
                    playback_id,
                    sample_time,
                } => {
                    for source in self.playing_sources.iter_mut() {
                        if source.playback_id == playback_id {
                            source.stop_time = Some(sample_time);
                            break;
                        }
                    }
                }
                MixedSourceMsg::RemoveAllPendingSources => {
                    // remove all sources which are not yet playing
                    self.remove_matching_sources(|source| source.start_time > time.pos_in_frames);
                }
                MixedSourceMsg::RemoveAllSources => {
                    self.remove_all_sources();
                }
            }
        }
        // keep sources sorted by sample time: this makes batch processing easier
        if got_new_sources {
            self.playing_sources
                .sort_by(|a, b| a.start_time.cmp(&b.start_time));
        }

        // return empty handed when we have no sources
        let output_frame_count = output.len() / self.channel_count;
        if self.playing_sources.is_empty() {
            return 0;
        }
        // clear entire output first, as we're only adding below
        for o in output.iter_mut() {
            *o = 0.0;
        }
        // run and add all playing sources
        let mut max_written = 0;
        'all_sources: for playing_source in self.playing_sources.iter_mut() {
            let source = &mut playing_source.source;
            let mut total_written = 0;
            // check source's sample start time
            if playing_source.start_time > time.pos_in_frames {
                let frames_until_source_starts =
                    (playing_source.start_time - time.pos_in_frames) as usize;
                if frames_until_source_starts > 0 {
                    if frames_until_source_starts >= output_frame_count {
                        // playing_sources are sorted by sample time: all following sources will run
                        // after this source, and thus also can also be skipped...
                        break 'all_sources;
                    }
                    // move offset to the sample's start pos
                    total_written += frames_until_source_starts * self.channel_count;
                }
            }
            // We should be the only owner of the source. If not, we'll need to wrap source into a RefCell.
            let source = Arc::get_mut(source).expect(
                "Failed to access a source as mutable in the mixer. Is someone else holding a ref?",
            );
            // run and mix down the source
            'source: while total_written < output.len() {
                let source_time =
                    time.with_added_frames((total_written / self.channel_count) as u64);
                // check if there's a pending stop command for the source
                let mut samples_until_stop = u64::MAX;
                if let Some(stop_time_in_frames) = playing_source.stop_time {
                    if stop_time_in_frames >= source_time.pos_in_frames {
                        samples_until_stop = (stop_time_in_frames - source_time.pos_in_frames)
                            * self.channel_count as u64;
                    }
                }
                if samples_until_stop == 0 {
                    let sender = &playing_source.playback_message_queue;
                    if let Err(err) = sender.send_stop() {
                        log::warn!("failed to send stop event: {}", err)
                    }
                    samples_until_stop = u64::MAX;
                }
                // run source on temp_out until we've filled up the whole final output
                let remaining = (output.len() - total_written).min(samples_until_stop as usize);
                let to_write = remaining.min(self.temp_out.len());
                let written = source.write(&mut self.temp_out[..to_write], &source_time);
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
        // drop all sources which finished playing in this iteration
        self.remove_matching_sources(|s| !s.is_active);
        // return modified output len: we've cleared the entire output
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
