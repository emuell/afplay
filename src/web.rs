use std::{
    io::Cursor,
    path::Path,
    sync::{Arc, Mutex},
};

use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, errors::Error as SymphoniaError,
    formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint,
};

use js_sys::{Object, Uint8Array};

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use web_sys::{AudioBufferSourceNode, AudioContext, Response};

use crate::Error;

// -------------------------------------------------------------------------------------------------

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

// -------------------------------------------------------------------------------------------------

/// A structure representing an audio player on web platform.
pub struct Player {
    context: AudioContext,
    active_sources: Arc<Mutex<Vec<AudioBufferSourceNode>>>,
}

impl Player {
    /// Create a new audio player.
    pub fn new() -> Result<Self, Error> {
        let context = AudioContext::new()
            .map_err(|_| Error::JsError("Failed to create audio context".to_string()))?;
        Ok(Self {
            context,
            active_sources: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Play audio file from a Path.
    ///
    /// Note: In WebAssembly context, this method will only work if the file
    /// is accessible via the browser's file system API. For most web applications,
    /// use `play_from_url` or `play_from_bytes` instead.
    #[allow(dead_code)]
    pub fn play<P: AsRef<Path>>(&self, _path: P) -> Result<(), Error> {
        todo!();
    }

    /// Play audio from a URL (web-specific method)
    pub async fn play_from_url(&self, url: &str) -> Result<(), Error> {
        let window = web_sys::window().ok_or(Error::JsError("Window not available".to_string()))?;

        // Fetch the audio file
        let resp_value = JsFuture::from(window.fetch_with_str(url))
            .await
            .map_err(|e| Error::JsError(format!("Failed to fetch URL: {:?}", e)))?;

        let resp: Response = resp_value
            .dyn_into()
            .map_err(|_| Error::JsError("Failed to convert response".to_string()))?;

        let array_buffer = JsFuture::from(
            resp.array_buffer()
                .map_err(|e| Error::JsError(format!("Failed to get array buffer: {:?}", e)))?,
        )
        .await
        .map_err(|e| Error::JsError(format!("Failed to get array buffer: {:?}", e)))?;

        let uint8_array = Uint8Array::new(&array_buffer);
        let bytes = uint8_array.to_vec();

        self.play_from_bytes(&bytes)
    }

    /// Play audio from bytes (useful for both web and native)
    pub fn play_from_bytes(&self, bytes: &[u8]) -> Result<(), Error> {
        // Use Symphonia to decode the audio
        let cursor = Box::new(Cursor::new(Vec::from(bytes))); // TODO
        let mss = MediaSourceStream::new(cursor, Default::default());

        // Create a hint to help the format registry guess what format reader is appropriate.
        let hint = Hint::new();

        // Use the default options when reading and decoding.
        let format_opts: FormatOptions = Default::default();
        let metadata_opts: MetadataOptions = Default::default();
        let decoder_opts: DecoderOptions = Default::default();

        // Probe the media source stream for a format.
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|_| Error::MediaFileProbeError)?;

        // Get the format reader
        let mut format = probed.format;

        // Get the default track
        let track = format
            .default_track()
            .ok_or_else(|| Error::MediaFileNotFound)?;

        // Create a decoder for the track
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &decoder_opts)
            .map_err(|e| Error::AudioDecodingError(Box::new(e)))?;

        // Get the track's sample rate and channel count
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100);
        let channels = track
            .codec_params
            .channels
            .unwrap_or(Default::default())
            .count() as u32;

        // Create a sample buffer
        let mut sample_buf = None;

        // Collect all PCM samples
        let mut all_samples = Vec::new();

        loop {
            // Get the next packet from the format reader
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(ref err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    // End of file reached
                    break;
                }
                Err(err) => {
                    return Err(Error::AudioDecodingError(Box::new(err)));
                }
            };

            // Decode the packet
            let decoded = match decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(err) => {
                    log(&format!("Decode error: {}", err));
                    continue;
                }
            };

            // Initialize the sample buffer if needed
            if sample_buf.is_none() {
                sample_buf = Some(SampleBuffer::<f32>::new(
                    decoded.capacity() as u64,
                    *decoded.spec(),
                ));
            }

            // Consume the decoder buffer
            if let Some(buf) = sample_buf.as_mut() {
                buf.copy_interleaved_ref(decoded);
                all_samples.extend_from_slice(buf.samples());
            }
        }

        // Create AudioBuffer
        let audio_buffer = self
            .context
            .create_buffer(
                channels,
                (all_samples.len() as u32) / channels,
                sample_rate as f32,
            )
            .map_err(|_| Error::JsError("Failed to create AudioBuffer".to_string()))?;

        // Fill the AudioBuffer with samples
        let channel_data =
            js_sys::Float32Array::new_with_length((all_samples.len() / channels as usize) as u32);

        for channel in 0..channels {
            for i in 0..(all_samples.len() / channels as usize) {
                channel_data.set_index(
                    i as u32,
                    all_samples[i * channels as usize + channel as usize],
                );
            }

            audio_buffer
                .copy_to_channel_with_f32_array(&channel_data, channel as i32)
                .map_err(|_| Error::JsError("Failed to copy samples to AudioBuffer".to_string()))?;
        }

        // Create and start audio buffer source
        let source = self
            .context
            .create_buffer_source()
            .map_err(|_| Error::JsError("Failed to create buffer source".to_string()))?;

        source.set_buffer(Some(&audio_buffer));
        source
            .connect_with_audio_node(&self.context.destination())
            .map_err(|_| Error::JsError("Failed to connect to audio destination".to_string()))?;

        // Add source to active sources
        if let Ok(mut sources) = self.active_sources.lock() {
            sources.push(source.clone());
        }

        // Set up onended handler to remove the source from active sources
        let active_sources = self.active_sources.clone();
        let active_source = source.clone();
        let onended_callback = Closure::wrap(Box::new(move || {
            if let Ok(mut sources) = active_sources.lock() {
                sources.retain(|s| !Object::is(s, &active_source));
            }
        }) as Box<dyn FnMut()>);

        #[allow(deprecated)]
        source.set_onended(Some(onended_callback.as_ref().unchecked_ref()));
        onended_callback.forget();

        source
            .start()
            .map_err(|_| Error::JsError("Failed to start playback".to_string()))?;

        Ok(())
    }

    /// Stop all playing audio
    pub fn stop_all(&self) -> Result<(), Error> {
        if let Ok(mut sources) = self.active_sources.lock() {
            for source in sources.iter() {
                #[allow(deprecated)]
                let _ = source.stop();
            }
            sources.clear();
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------

// Example JavaScript usage
// async function example() {
//     const player = new AFPlayer();
//     await player.play_from_url("https://example.com/audio.mp3");
//     // To stop all playing audio
//     player.stop_all();
// }

#[wasm_bindgen]
pub struct AFPlayer {
    player: Player,
}

#[wasm_bindgen]
impl AFPlayer {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<AFPlayer, JsValue> {
        let player = Player::new().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(AFPlayer { player })
    }

    #[wasm_bindgen]
    pub async fn play_from_url(&self, url: &str) -> Result<(), JsValue> {
        self.player
            .play_from_url(url)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub fn play_from_bytes(&self, bytes: &[u8]) -> Result<(), JsValue> {
        self.player
            .play_from_bytes(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub fn stop_all(&self) -> Result<(), JsValue> {
        self.player
            .stop_all()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
