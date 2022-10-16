use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crossbeam_channel::{unbounded, Receiver, Sender};
use symphonia::core::audio::SampleBuffer;

use super::{FilePlaybackMessage, FilePlaybackOptions, FileSource};
use crate::{
    error::Error,
    source::{
        file::{AudioFilePlaybackId, AudioFilePlaybackStatusEvent},
        resampled::ResamplingQuality,
        AudioSource, AudioSourceTime,
    },
    utils::{
        decoder::AudioDecoder,
        fader::{FaderState, VolumeFader},
        resampler::{
            cubic::CubicResampler, rubato::RubatoResampler, AudioResampler, ResamplingSpecs,
        },
        unique_usize_id,
    },
};

// -------------------------------------------------------------------------------------------------

/// A buffered, clonable file source, which decodes the entire file into a buffer before its
/// played back.
///
/// Buffers of preloaded file sources are shared (wrapped in an Arc), so cloning a source is
/// very cheap as this only copies a buffer reference and not the buffer itself. This way a file
/// can be pre-loaded once and can then be cloned and reused as often as necessary.
pub struct PreloadedFileSource {
    file_id: AudioFilePlaybackId,
    file_path: String,
    volume: f32,
    volume_fader: VolumeFader,
    fade_out_duration: Option<Duration>,
    repeat: usize,
    playback_message_send: Sender<FilePlaybackMessage>,
    playback_message_receive: Receiver<FilePlaybackMessage>,
    playback_status_send: Option<Sender<AudioFilePlaybackStatusEvent>>,
    buffer: Arc<Vec<f32>>,
    buffer_sample_rate: u32,
    buffer_channel_count: usize,
    buffer_pos: usize,
    resampler: Box<dyn AudioResampler>,
    output_sample_rate: u32,
    playback_pos_report_instant: Instant,
    playback_pos_emit_rate: Option<Duration>,
    playback_finished: bool,
}

impl PreloadedFileSource {
    pub fn new(
        file_path: &str,
        playback_status_send: Option<Sender<AudioFilePlaybackStatusEvent>>,
        options: FilePlaybackOptions,
        output_sample_rate: u32,
    ) -> Result<Self, Error> {
        // create decoder and get buffe rsignal specs
        let mut audio_decoder = AudioDecoder::new(file_path.to_string())?;
        let buffer_sample_rate = audio_decoder.signal_spec().rate;
        let buffer_channel_count = audio_decoder.signal_spec().channels.count();

        // prealloc entire buffer, when the decoder gives us a frame hint
        let buffer_capacity =
            audio_decoder.codec_params().n_frames.unwrap_or(0) as usize * buffer_channel_count;
        let mut buffer = Arc::new(Vec::with_capacity(buffer_capacity));

        // decode the entire file into our buffer in chunks of max_frames_per_packet sizes
        let decode_buffer_capacity = audio_decoder
            .codec_params()
            .max_frames_per_packet
            .unwrap_or(16 * 1024 * buffer_channel_count as u64);
        let mut decode_buffer =
            SampleBuffer::<f32>::new(decode_buffer_capacity, audio_decoder.signal_spec());

        let mut_buffer = Arc::get_mut(&mut buffer).unwrap();
        while audio_decoder.read_packet(&mut decode_buffer).is_some() {
            mut_buffer.append(&mut decode_buffer.samples().to_vec());
        }
        if buffer.is_empty() {
            // TODO: should pass a proper error here
            return Err(Error::AudioDecodingError(Box::new(
                symphonia::core::errors::Error::DecodeError("failed to decode file"),
            )));
        }

        Self::with_buffer(
            buffer,
            buffer_sample_rate,
            buffer_channel_count,
            file_path,
            playback_status_send,
            options,
            output_sample_rate,
        )
    }

    /// Create a new preloaded file source with the given decoded and possibly shared file buffer.
    pub fn with_buffer(
        buffer: Arc<Vec<f32>>,
        buffer_sample_rate: u32,
        buffer_channel_count: usize,
        file_path: &str,
        playback_status_send: Option<Sender<AudioFilePlaybackStatusEvent>>,
        options: FilePlaybackOptions,
        output_sample_rate: u32,
    ) -> Result<Self, Error> {
        // validate options
        if let Err(err) = options.validate() {
            return Err(err);
        }
        // create a channel for playback messages
        let (playback_message_send, playback_message_receive) = unbounded::<FilePlaybackMessage>();

        // create new volume fader
        let mut volume_fader = VolumeFader::new(buffer_channel_count, buffer_sample_rate);
        if let Some(duration) = options.fade_in_duration {
            if !duration.is_zero() {
                volume_fader.start_fade_in(duration);
            }
        }

        // create resampler
        let resampler_specs = ResamplingSpecs::new(
            buffer_sample_rate,
            (output_sample_rate as f64 / options.speed) as u32,
            buffer_channel_count,
        );
        let resampler: Box<dyn AudioResampler> = match options.resampling_quality {
            ResamplingQuality::HighQuality => Box::new(RubatoResampler::new(resampler_specs)?),
            ResamplingQuality::Default => Box::new(CubicResampler::new(resampler_specs)?),
        };

        // create new unique file id
        let file_id = unique_usize_id();

        // copy remaining options which are applied while playback
        let volume = options.volume;
        let fade_out_duration = options.fade_out_duration;
        let playback_pos_emit_rate = options.playback_pos_emit_rate;

        Ok(Self {
            file_id,
            file_path: file_path.into(),
            volume,
            volume_fader,
            fade_out_duration,
            repeat: options.repeat,
            playback_message_receive,
            playback_message_send,
            playback_status_send,
            buffer,
            buffer_sample_rate,
            buffer_channel_count,
            buffer_pos: 0,
            resampler,
            output_sample_rate,
            playback_pos_report_instant: Instant::now(),
            playback_pos_emit_rate,
            playback_finished: false,
        })
    }

    /// Create a copy of this preloaded source with the given playback options.
    pub fn clone(
        &self,
        options: FilePlaybackOptions,
        output_sample_rate: u32,
    ) -> Result<Self, Error> {
        Self::with_buffer(
            self.buffer(),
            self.buffer_sample_rate(),
            self.buffer_channel_count(),
            &self.file_path,
            self.playback_status_send.clone(),
            options,
            output_sample_rate,
        )
    }

    /// Access to the playback volume option
    pub fn volume(&self) -> f32 {
        self.volume
    }
    /// Set a new  playback volume option
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume
    }

    /// Get sample rate of our raw preloaded file's buffer
    pub fn buffer_sample_rate(&self) -> u32 {
        self.buffer_sample_rate
    }
    /// Get number of channels in our raw preloaded file's buffer
    pub fn buffer_channel_count(&self) -> usize {
        self.buffer_channel_count
    }
    /// Shared read-only access to the raw preloaded file's buffer
    pub fn buffer(&self) -> Arc<Vec<f32>> {
        self.buffer.clone()
    }

    fn should_report_pos(&self) -> bool {
        if let Some(report_duration) = self.playback_pos_emit_rate {
            self.playback_pos_report_instant.elapsed() >= report_duration
        } else {
            false
        }
    }

    fn samples_to_duration(&self, samples: usize) -> Duration {
        let frames = samples / self.buffer_channel_count as usize;
        let seconds = frames as f64 / self.output_sample_rate as f64;
        Duration::from_millis((seconds * 1000.0) as u64)
    }
}

impl FileSource for PreloadedFileSource {
    fn playback_message_sender(&self) -> Sender<FilePlaybackMessage> {
        self.playback_message_send.clone()
    }

    fn playback_id(&self) -> AudioFilePlaybackId {
        self.file_id
    }

    fn total_frames(&self) -> Option<u64> {
        Some(self.buffer.len() as u64 / self.channel_count() as u64)
    }

    fn current_frame_position(&self) -> u64 {
        self.buffer_pos as u64 / self.channel_count() as u64
    }

    fn end_of_track(&self) -> bool {
        self.playback_finished
    }
}

impl AudioSource for PreloadedFileSource {
    fn write(&mut self, output: &mut [f32], _time: &AudioSourceTime) -> usize {
        // consume playback messages
        while let Ok(msg) = self.playback_message_receive.try_recv() {
            match msg {
                FilePlaybackMessage::Seek(position) => {
                    let buffer_pos = position.as_secs_f64()
                        * self.buffer_sample_rate as f64
                        * self.buffer_channel_count as f64;
                    self.buffer_pos = (buffer_pos as usize).clamp(0, self.buffer.len());
                    self.resampler.reset();
                }
                FilePlaybackMessage::Read => (),
                FilePlaybackMessage::Stop => {
                    if let Some(duration) = self.fade_out_duration {
                        if !duration.is_zero() {
                            self.volume_fader.start_fade_out(duration);
                        } else {
                            self.playback_finished = true;
                        }
                    } else {
                        self.playback_finished = true;
                    }
                }
            }
        }

        // quickly bail out when we've finished playing
        if self.playback_finished {
            return 0;
        }

        // write from buffer at current position and apply volume, fadeout and repeats
        let mut total_written = 0_usize;
        while total_written < output.len() {
            // write from resampled buffer into output and apply volume
            let remaining_input_len = self.buffer.len() - self.buffer_pos;
            let remaining_input_buffer =
                &self.buffer[self.buffer_pos..self.buffer_pos + remaining_input_len];
            let remaining_target = &mut output[total_written..];
            let (input_consumed, output_written) = self
                .resampler
                .process(remaining_input_buffer, remaining_target)
                .expect("PreloadedFile resampling failed");

            // apply volume
            if (self.volume - 1.0).abs() > 0.0001 {
                for o in remaining_target.iter_mut() {
                    *o *= self.volume;
                }
            }

            // apply volume fading
            let written_target = &mut output[total_written..total_written + output_written];
            self.volume_fader.process(written_target);

            // maintain buffer pos
            self.buffer_pos += input_consumed;
            total_written += output_written;

            // loop or stop when reaching end of file
            let end_of_file = self.buffer_pos >= self.buffer.len();
            if end_of_file {
                if self.repeat > 0 {
                    if self.repeat != usize::MAX {
                        self.repeat -= 1;
                    }
                    self.buffer_pos = 0;
                } else {
                    break;
                }
            }
        }

        // send Position change Event
        if let Some(event_send) = &self.playback_status_send {
            if self.should_report_pos() {
                self.playback_pos_report_instant = Instant::now();
                // NB: try_send: we want to ignore full channels on playback pos events and don't want to block
                if let Err(err) = event_send.try_send(AudioFilePlaybackStatusEvent::Position {
                    id: self.file_id,
                    path: self.file_path.clone(),
                    position: self.samples_to_duration(self.buffer_pos),
                }) {
                    log::warn!("Failed to send playback event: {}", err)
                }
            }
        }

        // check if we've finished playing and send Stopped events
        let end_of_file = self.buffer_pos >= self.buffer.len();
        let fade_out_completed = self.volume_fader.state() == FaderState::Finished
            && self.volume_fader.target_volume() == 0.0;
        if end_of_file || fade_out_completed {
            if let Some(event_send) = &self.playback_status_send {
                if let Err(err) = event_send.try_send(AudioFilePlaybackStatusEvent::Stopped {
                    id: self.file_id,
                    path: self.file_path.clone(),
                    exhausted: self.buffer_pos >= self.buffer.len(),
                }) {
                    log::warn!("Failed to send playback event: {}", err)
                }
            }
            // mark playback as finished
            self.playback_finished = true;
        }

        total_written as usize
    }

    fn channel_count(&self) -> usize {
        self.buffer_channel_count
    }

    fn sample_rate(&self) -> u32 {
        self.output_sample_rate
    }

    fn is_exhausted(&self) -> bool {
        self.playback_finished
    }
}
