use std::{
    ops::Range,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use crossbeam_channel::Sender;
use rb::{Consumer, Producer, RbConsumer, RbProducer, SpscRb, RB};
use symphonia::core::{
    audio::{SampleBuffer, SignalSpec},
    units::TimeBase,
};

use super::{FilePlaybackMessage, FilePlaybackOptions, FileSource};
use crate::{
    error::Error,
    source::playback::{PlaybackId, PlaybackStatusEvent},
    source::AudioSource,
    utils::{
        actor::{Act, Actor, ActorHandle},
        decoder::AudioDecoder,
        fader::{FaderState, VolumeFader},
        unique_usize_id,
    },
};

// -------------------------------------------------------------------------------------------------

/// A source which streams & decodes an audio file asynchromiously in a worker thread
pub struct StreamedFileSource {
    actor: ActorHandle<FilePlaybackMessage>,
    file_id: usize,
    file_path: String,
    volume: f32,
    stop_fader: VolumeFader,
    consumer: Consumer<f32>,
    worker_state: SharedFileWorkerState,
    event_send: Option<Sender<PlaybackStatusEvent>>,
    signal_spec: SignalSpec,
    time_base: TimeBase,
    report_precision: u64,
    reported_pos: Option<u64>,
    playback_finished: bool,
}

impl StreamedFileSource {
    pub(crate) const REPORT_PRECISION: Duration = Duration::from_millis(500);

    pub(crate) fn total_samples(&self) -> Option<u64> {
        let total = self.worker_state.total_samples.load(Ordering::Relaxed);
        if total == u64::MAX {
            None
        } else {
            Some(total)
        }
    }

    pub(crate) fn written_samples(&self, position: u64) -> u64 {
        self.worker_state
            .position
            .fetch_add(position, Ordering::Relaxed)
            + position
    }

    fn should_report_pos(&self, pos: u64) -> bool {
        if let Some(reported) = self.reported_pos {
            reported > pos || pos - reported >= self.report_precision
        } else {
            true
        }
    }

    fn samples_to_duration(&self, samples: u64) -> Duration {
        let frames = samples / self.signal_spec.channels.count() as u64;
        let time = self.time_base.calc_time(frames);
        Duration::from_secs(time.seconds) + Duration::from_secs_f64(time.frac)
    }
}

impl FileSource for StreamedFileSource {
    fn new(
        file_path: &str,
        event_send: Option<Sender<PlaybackStatusEvent>>,
        options: FilePlaybackOptions,
    ) -> Result<Self, Error> {
        // create decoder
        let decoder = AudioDecoder::new(file_path.to_string())?;
        // Gather the source signal parameters and compute how often we should report
        // the play-head position.
        let signal_spec = decoder.signal_spec();
        let time_base = decoder.codec_params().time_base.unwrap();
        let report_precision = (signal_spec.rate as f64
            * signal_spec.channels.count() as f64
            * Self::REPORT_PRECISION.as_secs_f64()) as u64;
        let reported_pos = None;

        // Create a ring-buffer for the decoded samples. Worker thread is producing,
        // we are consuming in the `AudioSource` impl.
        let buffer = StreamedFileWorker::default_buffer();
        let consumer = buffer.consumer();

        let worker_state = SharedFileWorkerState {
            // We keep track of the current play-head position by sharing an atomic sample
            // counter with the decoding worker.  Worker is setting this on seek, we are
            // incrementing on reading from the ring-buffer.
            position: Arc::new(AtomicU64::new(0)),
            // Because the `n_frames` count that Symphonia gives us can be a bit unreliable,
            // we track the total number of samples in this stream in this atomic, set when
            // the underlying decoder returns EOF.
            total_samples: Arc::new(AtomicU64::new(u64::MAX)),
            // True when worker reached EOF
            end_of_file: Arc::new(AtomicBool::new(false)),
            // False, when worked received a stop event
            is_playing: Arc::new(AtomicBool::new(true)),
            // True when the worker received a fadeout stop
            is_fading_out: Arc::new(AtomicBool::new(false)),
            // When fading out, the requested fade_out duration in ms
            fade_out_duration_ms: Arc::new(AtomicU64::new(0)),
        };

        let playback_finished = false;

        // Spawn the worker and kick-start the decoding. The buffer will start filling now.
        let actor = StreamedFileWorker::spawn_with_default_cap("audio_decoding", {
            let shared_state = worker_state.clone();
            let repeat = options.repeat;
            move |this| StreamedFileWorker::new(this, decoder, buffer, shared_state, repeat)
        });
        actor.send(FilePlaybackMessage::Read)?;

        Ok(Self {
            actor,
            file_id: unique_usize_id(),
            file_path: file_path.to_string(),
            volume: options.volume,
            stop_fader: VolumeFader::new(signal_spec.channels.count(), signal_spec.rate),
            consumer,
            event_send,
            signal_spec,
            time_base,
            worker_state,
            playback_finished,
            report_precision,
            reported_pos,
        })
    }

    fn playback_message_sender(&self) -> Sender<FilePlaybackMessage> {
        self.actor.sender()
    }

    fn playback_id(&self) -> PlaybackId {
        self.file_id
    }

    fn current_frame_position(&self) -> u64 {
        self.worker_state.position.load(Ordering::Relaxed) / self.channel_count() as u64
    }

    fn total_frames(&self) -> Option<u64> {
        self.total_samples()
            .map(|samples| samples / self.channel_count() as u64)
    }

    fn end_of_track(&self) -> bool {
        self.playback_finished && self.worker_state.end_of_file.load(Ordering::Relaxed)
    }
}

impl AudioSource for StreamedFileSource {
    fn write(&mut self, output: &mut [f32]) -> usize {
        // return empty handed when playback finished
        if self.playback_finished {
            return 0;
        }
        // consume output from our ring-buffer
        let written = self.consumer.read(output).unwrap_or(0);
        let position = self.written_samples(written as u64);

        // apply volume parameter
        if (1.0f32 - self.volume).abs() > 0.0001 {
            for o in output[0..written].as_mut() {
                *o *= self.volume;
            }
        }

        // apply fade-out stop
        let is_stopping = self.worker_state.is_fading_out.load(Ordering::Relaxed);
        if is_stopping {
            if self.stop_fader.state() == FaderState::Stopped {
                let duration = Duration::from_millis(
                    self.worker_state
                        .fade_out_duration_ms
                        .load(Ordering::Relaxed),
                );
                self.stop_fader.start(duration);
            }
            self.stop_fader.process(&mut output[0..written]);
        }

        // send position change events
        if let Some(event_send) = &self.event_send {
            if self.should_report_pos(position) {
                self.reported_pos = Some(position);
                // NB: try_send: we want to ignore full channels on playback pos events and don't want to block
                if let Err(err) = event_send.try_send(PlaybackStatusEvent::Position {
                    id: self.file_id,
                    path: self.file_path.clone(),
                    position: self.samples_to_duration(position),
                }) {
                    log::warn!("failed to send playback event: {}", err)
                }
            }
        }

        // check if playback finished and send Stopped events
        let is_playing = self.worker_state.is_playing.load(Ordering::Relaxed);
        let is_exhausted = written == 0 && self.worker_state.end_of_file.load(Ordering::Relaxed);
        let fadeout_completed = is_stopping && self.stop_fader.state() == FaderState::Finished;
        if !is_playing || is_exhausted || fadeout_completed {
            // we're reached end of file or got stopped: send stop message
            if let Some(event_send) = &self.event_send {
                if let Err(err) = event_send.try_send(PlaybackStatusEvent::Stopped {
                    id: self.file_id,
                    path: self.file_path.clone(),
                    exhausted: is_exhausted,
                }) {
                    log::warn!("failed to send playback event: {}", err)
                }
            }
            // stop our worker
            self.worker_state.is_playing.store(false, Ordering::Relaxed);
            // and stop processing
            self.playback_finished = true;
        }

        // return dirty output len
        written
    }

    fn channel_count(&self) -> usize {
        self.signal_spec.channels.count()
    }

    fn sample_rate(&self) -> u32 {
        self.signal_spec.rate
    }

    fn is_exhausted(&self) -> bool {
        self.playback_finished
    }
}

impl Drop for StreamedFileSource {
    fn drop(&mut self) {
        // ignore error: channel maybe already is disconnected
        let _ = self.actor.send(FilePlaybackMessage::Stop(Duration::ZERO));
    }
}

// -------------------------------------------------------------------------------------------------

#[derive(Clone)]
struct SharedFileWorkerState {
    /// Current position. We update this on seek and EOF only.
    position: Arc<AtomicU64>,
    /// Total number of samples. We set this on EOF.
    total_samples: Arc<AtomicU64>,
    /// Is the worker thread not stopped?
    is_playing: Arc<AtomicBool>,
    /// Did the worker thread played until the end of the file?
    end_of_file: Arc<AtomicBool>,
    /// True when a stop fadeout was requested.
    is_fading_out: Arc<AtomicBool>,
    /// Stop fadeout duration in ms
    fade_out_duration_ms: Arc<AtomicU64>,
}

// -------------------------------------------------------------------------------------------------

struct StreamedFileWorker {
    /// Sending part of our own actor channel.
    this: Sender<FilePlaybackMessage>,
    /// Decoder we are reading packets/samples from.
    input: AudioDecoder,
    /// Audio properties of the decoded signal.
    input_spec: SignalSpec,
    /// Sample buffer containing samples read in the last packet.
    input_packet: SampleBuffer<f32>,
    /// Ring-buffer for the output signal.
    output: SpscRb<f32>,
    /// Producing part of the output ring-buffer.
    output_producer: Producer<f32>,
    // Shared state with StreamedFileSource
    shared_state: SharedFileWorkerState,
    /// Range of samples in `resampled` that are awaiting flush into `output`.
    samples_to_write: Range<usize>,
    /// Number of samples written into the output channel.
    samples_written: u64,
    /// Are we in the middle of automatic read loop?
    is_reading: bool,
    /// Number of times we should repeat the source
    repeat: usize,
}

impl StreamedFileWorker {
    fn default_buffer() -> SpscRb<f32> {
        const DEFAULT_BUFFER_SIZE: usize = 128 * 1024;
        SpscRb::new(DEFAULT_BUFFER_SIZE)
    }

    fn new(
        this: Sender<FilePlaybackMessage>,
        input: AudioDecoder,
        output: SpscRb<f32>,
        shared_state: SharedFileWorkerState,
        repeat: usize,
    ) -> Self {
        const DEFAULT_MAX_FRAMES: u64 = 8 * 1024;

        let max_input_frames = input
            .codec_params()
            .max_frames_per_packet
            .unwrap_or(DEFAULT_MAX_FRAMES);

        // Promote the worker thread to audio priority to prevent buffer under-runs on high CPU usage.
        if let Err(err) =
            audio_thread_priority::promote_current_thread_to_real_time(0, input.signal_spec().rate)
        {
            log::warn!(
                "failed to set file worker thread's priority to real-time: {}",
                err
            );
        }

        Self {
            output_producer: output.producer(),
            input_packet: SampleBuffer::new(max_input_frames, input.signal_spec()),
            input_spec: input.signal_spec(),
            input,
            this,
            output,
            shared_state,
            samples_written: 0,
            samples_to_write: 0..0,
            is_reading: false,
            repeat,
        }
    }
}

impl Actor for StreamedFileWorker {
    type Message = FilePlaybackMessage;
    type Error = Error;

    fn handle(&mut self, msg: FilePlaybackMessage) -> Result<Act<Self>, Self::Error> {
        match msg {
            FilePlaybackMessage::Seek(time) => self.on_seek(time),
            FilePlaybackMessage::Read => self.on_read(),
            FilePlaybackMessage::Stop(fadeout) => self.on_stop(fadeout),
        }
    }
}

impl StreamedFileWorker {
    fn on_stop(&mut self, fadeout: Duration) -> Result<Act<Self>, Error> {
        if fadeout.is_zero() {
            // immediately stop reading
            self.is_reading = false;
            self.shared_state.is_playing.store(false, Ordering::Relaxed);
            Ok(Act::Shutdown)
        } else {
            // duration and fade out state will be picked up by our parent source
            self.shared_state
                .fade_out_duration_ms
                .store(fadeout.as_millis() as u64, Ordering::Relaxed);
            self.shared_state
                .is_fading_out
                .store(true, Ordering::Relaxed);
            // keep running until fade-out completed
            Ok(Act::Continue)
        }
    }

    fn on_seek(&mut self, time: Duration) -> Result<Act<Self>, Error> {
        match self.input.seek(time) {
            Ok(timestamp) => {
                if self.is_reading {
                    self.samples_to_write = 0..0;
                } else {
                    self.this.send(FilePlaybackMessage::Read)?;
                }
                let position = timestamp * self.input_spec.channels.count() as u64;
                self.samples_written = position;
                self.shared_state
                    .position
                    .store(position, Ordering::Relaxed);
                self.output.clear();
            }
            Err(err) => {
                log::error!("failed to seek: {}", err);
            }
        }
        Ok(Act::Continue)
    }

    fn on_read(&mut self) -> Result<Act<Self>, Error> {
        // check if we no longer need to run the worker
        if !self.shared_state.is_playing.load(Ordering::Relaxed) {
            return Ok(Act::Shutdown);
        }
        // check if we need to fetch more input samples
        if !self.samples_to_write.is_empty() {
            let input = &self.input_packet.samples()[self.samples_to_write.clone()];
            // TODO: self.output_fader.process(&mut input_mut.borrow_mut());
            if let Ok(written) = self.output_producer.write(input) {
                self.samples_written += written as u64;
                self.samples_to_write.start += written;
                self.is_reading = true;
                self.this.send(FilePlaybackMessage::Read)?;
                Ok(Act::Continue)
            } else {
                // Buffer is full.  Wait a bit a try again.  We also have to indicate that the
                // read loop is not running at the moment (if we receive a `Seek` while waiting,
                // we need it to explicitly kickstart reading again).
                self.is_reading = false;
                Ok(Act::WaitOr {
                    timeout: Duration::from_millis(500),
                    timeout_msg: FilePlaybackMessage::Read,
                })
            }
        } else {
            // fetch more input samples
            match self.input.read_packet(&mut self.input_packet) {
                Some(_) => {
                    // continue reading
                    self.samples_to_write = 0..self.input_packet.samples().len();
                    self.is_reading = true;
                    self.this.send(FilePlaybackMessage::Read)?;
                }
                None => {
                    // reached EOF
                    if self.repeat > 0 {
                        if self.repeat != usize::MAX {
                            self.repeat -= 1;
                        }
                        // seek to start and continue reading
                        self.input.seek(Duration::ZERO)?;
                        self.samples_written = 0;
                        self.samples_to_write = 0..0;
                        self.shared_state.position.store(0, Ordering::Relaxed);
                        self.is_reading = true;
                        self.this.send(FilePlaybackMessage::Read)?;
                    } else {
                        // stop reading and mark as exhausted
                        self.is_reading = false;
                        self.shared_state.end_of_file.store(true, Ordering::Relaxed);
                        self.shared_state
                            .total_samples
                            .store(self.samples_written, Ordering::Relaxed);
                    }
                }
            }
            Ok(Act::Continue)
        }
    }
}
