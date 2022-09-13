use super::AudioSource;
use crate::utils::resampler::{AudioResampler, InterpolationType, ResamplingSpecs};

// -------------------------------------------------------------------------------------------------

/// Interpolation mode of the resampler.
pub type Quality = InterpolationType;

// -------------------------------------------------------------------------------------------------

/// A source which resamples the input source, either to adjust source's sample rate to a
/// target rate or to play back a source with a different pitch.
pub struct ResampledSource {
    source: Box<dyn AudioSource>,
    output_sample_rate: u32,
    resampler: AudioResampler,
    input_buffer: ResampleBuffer,
    output_buffer: ResampleBuffer,
}

impl ResampledSource {
    /// Create a new resampled sources with the given sample rate adjustment.
    pub fn new<InputSource>(source: InputSource, output_sample_rate: u32, quality: Quality) -> Self
    where
        InputSource: AudioSource,
    {
        Self::new_with_speed(source, output_sample_rate, 1.0, quality)
    }
    /// Create a new resampled sources with the given sample rate and playback speed adjument.
    pub fn new_with_speed<InputSource>(
        source: InputSource,
        output_sample_rate: u32,
        speed: f64,
        quality: Quality,
    ) -> Self
    where
        InputSource: AudioSource,
    {
        let specs = ResamplingSpecs {
            channel_count: source.channel_count(),
            input_rate: source.sample_rate(),
            output_rate: (output_sample_rate as f64 / speed) as u32,
        };
        let resampler = AudioResampler::new(quality, specs).unwrap();
        let input_buffer = vec![0.0; resampler.input_buffer_len()];
        let output_buffer = vec![0.0; resampler.output_buffer_len()];
        Self {
            source: Box::new(source),
            resampler,
            output_sample_rate,
            input_buffer: ResampleBuffer {
                buffer: input_buffer,
                start: 0,
                end: 0,
            },
            output_buffer: ResampleBuffer {
                buffer: output_buffer,
                start: 0,
                end: 0,
            },
        }
    }
}

impl AudioSource for ResampledSource {
    fn write(&mut self, output: &mut [f32]) -> usize {
        let mut total_written = 0;
        while total_written < output.len() {
            if self.output_buffer.is_empty() {
                if self.input_buffer.is_empty() {
                    let n = self.source.write(&mut self.input_buffer.buffer);
                    self.input_buffer.buffer[n..]
                        .iter_mut()
                        .for_each(|s| *s = 0.0);
                    self.input_buffer.start = 0;
                    self.input_buffer.end = self.input_buffer.buffer.len();
                }
                let (input_consumed, output_written) = self
                    .resampler
                    .process(
                        &self.input_buffer.buffer[self.input_buffer.start..],
                        &mut self.output_buffer.buffer,
                    )
                    .unwrap();
                self.input_buffer.start += input_consumed;
                self.output_buffer.start = 0;
                self.output_buffer.end = output_written;
            }
            let source = self.output_buffer.get();
            let target = &mut output[total_written..];
            let written = self.output_buffer.len().min(target.len());
            target[..written].copy_from_slice(&source[..written]);
            total_written += written;
            self.output_buffer.start += written;
        }
        total_written
    }

    fn channel_count(&self) -> usize {
        self.source.channel_count()
    }

    fn sample_rate(&self) -> u32 {
        self.output_sample_rate
    }

    fn is_exhausted(&self) -> bool {
        self.source.is_exhausted() && self.input_buffer.is_empty()
    }
}

// -------------------------------------------------------------------------------------------------

struct ResampleBuffer {
    buffer: Vec<f32>,
    start: usize,
    end: usize,
}

impl ResampleBuffer {
    fn get(&self) -> &[f32] {
        &self.buffer[self.start..self.end]
    }

    fn len(&self) -> usize {
        self.end - self.start
    }

    fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}
