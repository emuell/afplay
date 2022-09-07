use std::{
    ops::Range,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
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

use crate::{
    error::Error,
    utils::{
        actor::{Act, Actor, ActorHandle},
        decoder::AudioDecoder,
    },
};

use super::{AudioSource, FileId, FilePlaybackMsg, FilePlaybackStatusMsg, FileSource};

// -------------------------------------------------------------------------------------------------

/// A source which streams & decodes an audio file asynchromiously in a worker thread
pub struct StreamedFileSource {
    actor: ActorHandle<FilePlaybackMsg>,
    file_id: usize,
    file_path: String,
    consumer: Consumer<f32>,
    event_send: Option<Sender<FilePlaybackStatusMsg>>,
    signal_spec: SignalSpec,
    time_base: TimeBase,
    total_samples: Arc<AtomicU64>,
    report_precision: u64,
    reported_pos: Option<u64>,
    position: Arc<AtomicU64>,
    end_of_track: bool,
}

impl StreamedFileSource {
    pub(crate) const REPORT_PRECISION: Duration = Duration::from_millis(500);

    pub(crate) fn total_samples(&self) -> Option<u64> {
        let total = self.total_samples.load(Ordering::Relaxed);
        if total == u64::MAX {
            None
        } else {
            Some(total)
        }
    }

    pub(crate) fn written_samples(&self, position: u64) -> u64 {
        self.position.fetch_add(position, Ordering::Relaxed) + position
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
        file_path: String,
        event_send: Option<Sender<FilePlaybackStatusMsg>>,
    ) -> Result<Self, Error> {
        // create decoder
        let decoder = AudioDecoder::new(file_path.clone())?;
        // Gather the source signal parameters and compute how often we should report
        // the play-head position.
        let signal_spec = decoder.signal_spec();
        let time_base = decoder.codec_params().time_base.unwrap();
        let report_precision = (signal_spec.rate as f64
            * signal_spec.channels.count() as f64
            * Self::REPORT_PRECISION.as_secs_f64()) as u64;
        let reported_pos = None;

        // Create a ring-buffer for the decoded samples.  Worker thread is producing,
        // we are consuming in the `AudioSource` impl.
        let buffer = StreamedFileWorker::default_buffer();
        let consumer = buffer.consumer();

        // We keep track of the current play-head position by sharing an atomic sample
        // counter with the decoding worker.  Worker is setting this on seek, we are
        // incrementing on reading from the ring-buffer.
        let position = Arc::new(AtomicU64::new(0));
        let end_of_track = false;

        // Because the `n_frames` count that Symphonia gives us can be a bit unreliable,
        // we track the total number of samples in this stream in this atomic, set when
        // the underlying decoder returns EOF.
        let total_samples = Arc::new(AtomicU64::new(u64::MAX));

        // Spawn the worker and kick-start the decoding.  The buffer will start filling
        // now.
        let actor = StreamedFileWorker::spawn_with_default_cap("audio_decoding", {
            let position = Arc::clone(&position);
            let total_samples = Arc::clone(&total_samples);
            move |this| StreamedFileWorker::new(this, decoder, buffer, position, total_samples)
        });
        actor.send(FilePlaybackMsg::Read)?;

        static FILE_ID_COUNTER: AtomicUsize = AtomicUsize::new(1);

        Ok(Self {
            actor,
            file_id: FILE_ID_COUNTER.fetch_add(1, Ordering::Relaxed),
            file_path,
            consumer,
            event_send,
            signal_spec,
            time_base,
            total_samples,
            position,
            end_of_track,
            report_precision,
            reported_pos,
        })
    }

    fn sender(&self) -> Sender<FilePlaybackMsg> {
        self.actor.sender()
    }

    fn file_id(&self) -> FileId {
        self.file_id
    }

    fn current_frame_position(&self) -> u64 {
        self.position.load(Ordering::Relaxed) / self.channel_count() as u64
    }

    fn total_frames(&self) -> Option<u64> {
        self.total_samples()
            .map(|samples| samples / self.channel_count() as u64)
    }

    fn end_of_track(&self) -> bool {
        self.end_of_track
    }
}

impl AudioSource for StreamedFileSource {
    fn write(&mut self, output: &mut [f32]) -> usize {
        if self.end_of_track {
            return 0;
        }
        let written = self.consumer.read(output).unwrap_or(0);
        let position = self.written_samples(written as u64);

        if let Some(event_send) = &self.event_send {
            if self.should_report_pos(position) {
                self.reported_pos = Some(position);
                // Send a position report, so the upper layers can visualize the playback
                // progress and preload the next track.  We cannot block here, so if the channel
                // is full, we just try the next time instead of waiting.
                if let Err(err) = event_send.try_send(FilePlaybackStatusMsg::Position {
                    file_id: self.file_id,
                    file_path: self.file_path.clone(),
                    position: self.samples_to_duration(position),
                }) {
                    log::warn!("failed to send playback event: {}", err)
                }
            }
        }

        let total_samples = self.total_samples.load(Ordering::Relaxed);
        if position >= total_samples {
            // After reading the total number of samples, we stop. Signal to the upper layer
            // this track is over and short-circuit all further reads from this source.
            if let Some(event_send) = &self.event_send {
                if let Err(err) = event_send.try_send(FilePlaybackStatusMsg::EndOfFile {
                    file_id: self.file_id,
                    file_path: self.file_path.clone(),
                }) {
                    log::warn!("failed to send playback event: {}", err)
                }
            }
            self.end_of_track = true;
        }

        written
    }

    fn channel_count(&self) -> usize {
        self.signal_spec.channels.count()
    }

    fn sample_rate(&self) -> u32 {
        self.signal_spec.rate
    }

    fn is_exhausted(&self) -> bool {
        self.end_of_track
    }
}

impl Drop for StreamedFileSource {
    fn drop(&mut self) {
        if let Err(err) = self.actor.send(FilePlaybackMsg::Stop) {
            log::warn!("failed to send playback event in drain: {}", err)
        }
    }
}

// -------------------------------------------------------------------------------------------------

pub struct StreamedFileWorker {
    /// Sending part of our own actor channel.
    this: Sender<FilePlaybackMsg>,
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
    /// Shared atomic position.  We update this on seek only.
    position: Arc<AtomicU64>,
    /// Shared atomic for total number of samples.  We set this on EOF.
    total_samples: Arc<AtomicU64>,
    /// Range of samples in `resampled` that are awaiting flush into `output`.
    samples_to_write: Range<usize>,
    /// Number of samples written into the output channel.
    samples_written: u64,
    /// Are we in the middle of automatic read loop?
    is_reading: bool,
}

impl StreamedFileWorker {
    fn default_buffer() -> SpscRb<f32> {
        const DEFAULT_BUFFER_SIZE: usize = 128 * 1024;

        SpscRb::new(DEFAULT_BUFFER_SIZE)
    }

    fn new(
        this: Sender<FilePlaybackMsg>,
        input: AudioDecoder,
        output: SpscRb<f32>,
        position: Arc<AtomicU64>,
        total_samples: Arc<AtomicU64>,
    ) -> Self {
        const DEFAULT_MAX_FRAMES: u64 = 8 * 1024;

        let max_input_frames = input
            .codec_params()
            .max_frames_per_packet
            .unwrap_or(DEFAULT_MAX_FRAMES);

        // Promote the worker thread to audio priority to prevent buffer under-runs on
        // high CPU usage.
        if let Err(err) =
            audio_thread_priority::promote_current_thread_to_real_time(0, input.signal_spec().rate)
        {
            log::warn!("failed to promote thread to audio priority: {}", err);
        }

        Self {
            output_producer: output.producer(),
            input_packet: SampleBuffer::new(max_input_frames, input.signal_spec()),
            input_spec: input.signal_spec(),
            input,
            this,
            output,
            position,
            total_samples,
            samples_written: 0,
            samples_to_write: 0..0, // Arbitrary empty range.
            is_reading: false,
        }
    }
}

impl Actor for StreamedFileWorker {
    type Message = FilePlaybackMsg;
    type Error = Error;

    fn handle(&mut self, msg: FilePlaybackMsg) -> Result<Act<Self>, Self::Error> {
        match msg {
            FilePlaybackMsg::Seek(time) => self.on_seek(time),
            FilePlaybackMsg::Read => self.on_read(),
            FilePlaybackMsg::Stop => Ok(Act::Shutdown),
        }
    }
}

impl StreamedFileWorker {
    fn on_seek(&mut self, time: Duration) -> Result<Act<Self>, Error> {
        match self.input.seek(time) {
            Ok(timestamp) => {
                if self.is_reading {
                    self.samples_to_write = 0..0;
                } else {
                    self.this.send(FilePlaybackMsg::Read)?;
                }
                let position = timestamp * self.input_spec.channels.count() as u64;
                self.samples_written = position;
                self.position.store(position, Ordering::Relaxed);
                self.output.clear();
            }
            Err(err) => {
                log::error!("failed to seek: {}", err);
            }
        }
        Ok(Act::Continue)
    }

    fn on_read(&mut self) -> Result<Act<Self>, Error> {
        if !self.samples_to_write.is_empty() {
            let writable = &self.input_packet.samples()[self.samples_to_write.clone()];
            if let Ok(written) = self.output_producer.write(writable) {
                self.samples_written += written as u64;
                self.samples_to_write.start += written;
                self.is_reading = true;
                self.this.send(FilePlaybackMsg::Read)?;
                Ok(Act::Continue)
            } else {
                // Buffer is full.  Wait a bit a try again.  We also have to indicate that the
                // read loop is not running at the moment (if we receive a `Seek` while waiting,
                // we need it to explicitly kickstart reading again).
                self.is_reading = false;
                Ok(Act::WaitOr {
                    timeout: Duration::from_millis(500),
                    timeout_msg: FilePlaybackMsg::Read,
                })
            }
        } else {
            match self.input.read_packet(&mut self.input_packet) {
                Some(_) => {
                    self.samples_to_write = 0..self.input_packet.samples().len();
                    self.is_reading = true;
                    self.this.send(FilePlaybackMsg::Read)?;
                }
                None => {
                    self.is_reading = false;
                    self.total_samples
                        .store(self.samples_written, Ordering::Relaxed);
                }
            }
            Ok(Act::Continue)
        }
    }
}
