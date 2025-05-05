use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crossbeam_channel::Sender;
use crossbeam_queue::ArrayQueue;
use symphonia::core::audio::SampleBuffer;

use super::{FilePlaybackMessage, FilePlaybackOptions, FileSource};
use crate::{
    error::Error,
    source::{
        file::{AudioFilePlaybackId, AudioFilePlaybackStatusContext, AudioFilePlaybackStatusEvent},
        resampled::ResamplingQuality,
        AudioSource, AudioSourceTime,
    },
    utils::{
        buffer::TempBuffer,
        decoder::AudioDecoder,
        fader::{FaderState, VolumeFader},
        panning_factors,
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
    file_path: Arc<String>,
    volume: f32,
    volume_fader: VolumeFader,
    panning: f32,
    fade_out_duration: Option<Duration>,
    repeat: usize,
    buffer: Arc<Vec<f32>>,
    buffer_sample_rate: u32,
    buffer_channel_count: usize,
    buffer_pos: usize,
    resampler: Box<dyn AudioResampler>,
    resampler_input_buffer: TempBuffer,
    output_sample_rate: u32,
    playback_message_queue: Arc<ArrayQueue<FilePlaybackMessage>>,
    playback_status_send: Option<Sender<AudioFilePlaybackStatusEvent>>,
    playback_status_context: Option<AudioFilePlaybackStatusContext>,
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
        // validate options
        options.validate()?;
        // create decoder and get buffe rsignal specs
        let mut audio_decoder = AudioDecoder::new(file_path.to_string())?;
        let buffer_sample_rate = audio_decoder.signal_spec().rate;
        let buffer_channel_count = audio_decoder.signal_spec().channels.count();

        // prealloc entire buffer, when the decoder gives us a frame hint
        let buffer_capacity =
            audio_decoder.codec_params().n_frames.unwrap_or(0) as usize * buffer_channel_count + 1;
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
        } else {
            // add one extra empty sample at the end for the cubic resamplers
            let mut_buffer = Arc::get_mut(&mut buffer).unwrap();
            for _ in 0..buffer_channel_count {
                mut_buffer.push(0.0);
            }
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
        options.validate()?;
        // create a queue for playback messages
        let playback_message_queue = Arc::new(ArrayQueue::new(128));

        // create new volume fader
        let mut volume_fader = VolumeFader::new(buffer_channel_count, buffer_sample_rate);
        if let Some(duration) = options.fade_in_duration {
            if !duration.is_zero() {
                volume_fader.start_fade_in(duration);
            }
        }

        // reset context
        let playback_status_context = None;

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
        let resample_input_buffer_size = resampler.max_input_buffer_size().unwrap_or(0);
        let resampler_input_buffer = TempBuffer::new(resample_input_buffer_size);

        // create new unique file id
        let file_id = unique_usize_id();

        // copy remaining options which are applied while playback
        let volume = options.volume;
        let panning = options.panning;
        let fade_out_duration = options.fade_out_duration;
        let playback_pos_emit_rate = options.playback_pos_emit_rate;

        Ok(Self {
            file_id,
            file_path: Arc::new(file_path.into()),
            volume,
            volume_fader,
            panning,
            fade_out_duration,
            repeat: options.repeat,
            buffer,
            buffer_sample_rate,
            buffer_channel_count,
            buffer_pos: 0,
            resampler,
            resampler_input_buffer,
            output_sample_rate,
            playback_message_queue,
            playback_status_send,
            playback_status_context,
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

    /// Access to the playback panning option
    pub fn panning(&self) -> f32 {
        self.panning
    }
    /// Set new playback panning option
    pub fn set_panning(&mut self, panning: f32) {
        self.panning = panning
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
        let frames = samples / self.buffer_channel_count;
        let seconds = frames as f64 / self.output_sample_rate as f64;
        Duration::from_secs_f64(seconds)
    }
}

impl FileSource for PreloadedFileSource {
    fn playback_id(&self) -> AudioFilePlaybackId {
        self.file_id
    }

    fn playback_message_queue(&self) -> Arc<ArrayQueue<FilePlaybackMessage>> {
        self.playback_message_queue.clone()
    }

    fn playback_status_sender(&self) -> Option<Sender<AudioFilePlaybackStatusEvent>> {
        self.playback_status_send.clone()
    }
    fn set_playback_status_sender(&mut self, sender: Option<Sender<AudioFilePlaybackStatusEvent>>) {
        self.playback_status_send = sender;
    }

    fn playback_status_context(&self) -> Option<AudioFilePlaybackStatusContext> {
        self.playback_status_context.clone()
    }
    fn set_playback_status_context(&mut self, context: Option<AudioFilePlaybackStatusContext>) {
        self.playback_status_context = context;
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
        while let Some(msg) = self.playback_message_queue.pop() {
            match msg {
                FilePlaybackMessage::Seek(position) => {
                    let buffer_pos = position.as_secs_f64()
                        * self.buffer_sample_rate as f64
                        * self.buffer_channel_count as f64;
                    self.buffer_pos = (buffer_pos as usize).clamp(0, self.buffer.len());
                    self.resampler.reset();
                }
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

        // write from buffer at current position and apply volume, panning, fadeout and repeats
        let mut total_written = 0_usize;
        while total_written < output.len() {
            // write from resampled buffer into output and apply volume
            let remaining_input_len = self.buffer.len() - self.buffer_pos;
            let remaining_input_buffer =
                &self.buffer[self.buffer_pos..self.buffer_pos + remaining_input_len];
            let remaining_target = &mut output[total_written..];
            // pad input with zeros if resampler has input size constrains (should only happen in the last process calls)
            let required_input_len = self.resampler.required_input_buffer_size().unwrap_or(0);
            let (input_consumed, output_written) =
                if remaining_input_buffer.len() < required_input_len {
                    self.resampler_input_buffer.reset_range();
                    self.resampler_input_buffer
                        .copy_from(remaining_input_buffer);
                    for o in &mut self.resampler_input_buffer.get_mut()[remaining_input_len..] {
                        *o = 0.0;
                    }
                    let (_, output_written) = self
                        .resampler
                        .process(self.resampler_input_buffer.get(), remaining_target)
                        .expect("PreloadedFile resampling failed");
                    (remaining_input_len, output_written)
                } else {
                    self.resampler
                        .process(remaining_input_buffer, remaining_target)
                        .expect("PreloadedFile resampling failed")
                };

            // apply volume
            if (self.volume - 1.0).abs() > 0.0001 {
                for o in remaining_target.iter_mut() {
                    *o *= self.volume;
                }
            }

            // apply panning (for stereo samples)
            if self.panning.abs() > 0.0001 && self.buffer_channel_count >= 2 {
                let (pan_l, pan_r) = panning_factors(self.panning);
                for o in remaining_target.chunks_exact_mut(self.buffer_channel_count) {
                    // SAFETY: 0,1 is inbounds because we are splitting exact and checking for
                    // buffer_channel_count >= 2 above
                    unsafe {
                        *o.get_unchecked_mut(0) *= pan_l; // left
                        *o.get_unchecked_mut(1) *= pan_r; // right
                    }
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
                    context: self.playback_status_context.clone(),
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
                    context: self.playback_status_context.clone(),
                    path: self.file_path.clone(),
                    exhausted: self.buffer_pos >= self.buffer.len(),
                }) {
                    log::warn!("Failed to send playback event: {}", err)
                }
            }
            // mark playback as finished
            self.playback_finished = true;
        }

        total_written
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

// -------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling() {
        // add one extra zero sample for cubic resampling
        let buffer = Arc::new(vec![0.2, 1.0, 0.5, 0.0]);

        // Default
        let preloaded = PreloadedFileSource::with_buffer(
            buffer.clone(),
            44100,
            1,
            "temp_file",
            None,
            FilePlaybackOptions::default().resampling_quality(ResamplingQuality::Default),
            48000,
        );
        assert!(preloaded.is_ok());
        let mut preloaded = preloaded.unwrap();
        let mut output = vec![0.0; 1024];
        let written = preloaded.write(&mut output, &AudioSourceTime::default());

        assert_eq!(written, buffer.len() - 1);
        assert!((output.iter().sum::<f32>() - buffer.iter().sum::<f32>()).abs() < 0.1);

        // Rubato
        let preloaded = PreloadedFileSource::with_buffer(
            buffer.clone(),
            44100,
            1,
            "temp_file",
            None,
            FilePlaybackOptions::default().resampling_quality(ResamplingQuality::HighQuality),
            48000,
        );
        assert!(preloaded.is_ok());
        let mut preloaded = preloaded.unwrap();
        let mut output = vec![0.0; 1024];
        let written = preloaded.write(&mut output, &AudioSourceTime::default());

        assert!(written > buffer.len());
        assert!((output.iter().sum::<f32>() - buffer.iter().sum::<f32>()).abs() < 0.2);
        assert!(output[3..].iter().sum::<f32>() < 0.1);
    }
}
