use rubato::{InterpolationParameters, InterpolationType, SincFixedIn, VecResampler};

use crate::{
    utils::buffer::{interleaved_to_planar, planar_to_interleaved},
    utils::resampler::{AudioResampler, ResamplingSpecs},
    Error,
};

// -------------------------------------------------------------------------------------------------

/// `AudioResampler` impl which is using a bandlimited SincFixedIn `rubato` resampler.
pub struct RubatoResampler {
    spec: ResamplingSpecs,
    resampler: SincFixedIn<f32>,
    input: Vec<Vec<f32>>,
    output: Vec<Vec<f32>>,
}

impl RubatoResampler {
    pub fn new(spec: ResamplingSpecs) -> Result<Self, Error> {
        const CHUNK_SIZE: usize = 128;
        let parameters = InterpolationParameters {
            f_cutoff: 0.95,
            interpolation: InterpolationType::Cubic,
            oversampling_factor: 128,
            sinc_len: 256,
            window: rubato::WindowFunction::BlackmanHarris2,
        };
        match SincFixedIn::new(
            spec.output_ratio(),
            1.0,
            parameters,
            CHUNK_SIZE,
            spec.channel_count,
        ) {
            Err(err) => Err(Error::ResamplingError(Box::new(err))),
            Ok(resampler) => {
                let mut input = resampler.input_buffer_allocate();
                // buffers are only allocated with the needed capacity only, not len
                for channel in input.iter_mut() {
                    channel.resize(channel.capacity(), 0.0_f32);
                }
                let output = resampler.output_buffer_allocate();
                Ok(Self {
                    resampler,
                    spec,
                    input,
                    output,
                })
            }
        }
    }
}

impl AudioResampler for RubatoResampler {
    fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<(usize, usize), Error> {
        assert_eq!(
            input.len(),
            self.input_buffer_len(),
            "invalid input buffer size"
        );
        assert_eq!(
            output.len(),
            self.output_buffer_len(),
            "invalid output buffer size"
        );
        if self.spec.input_rate == self.spec.output_rate {
            // Bypass conversion in case the sample rates are equal.
            let output = &mut output[..input.len()];
            output.copy_from_slice(input);
            return Ok((input.len(), output.len()));
        }
        interleaved_to_planar(input, &mut self.input);
        if let Err(err) = self
            .resampler
            .process_into_buffer(&self.input, &mut self.output, None)
        {
            return Err(Error::ResamplingError(Box::new(err)));
        }
        planar_to_interleaved(&self.output, output);

        Ok((input.len(), self.output.len() * self.output[0].len()))
    }

    fn input_buffer_len(&self) -> usize {
        self.input.len() * self.input[0].capacity()
    }

    fn output_buffer_len(&self) -> usize {
        self.output.len() * self.output[0].capacity()
    }
}

unsafe impl Send for RubatoResampler {}
unsafe impl Sync for RubatoResampler {}
