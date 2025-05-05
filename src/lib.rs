//! **afplay** is an *audio playback library for Rust*, based on the
//! [psst-core](https://github.com/jpochyla/psst/tree/master/psst-core) audio playback
//! implementation.
//!
//! It aims to be a suitable audio player backend for game engines, but can also serve
//! as a general-purpose low-latency music and sound playback engine for desktop music apps.
//!
//! It was originally developed and is used in the [AFEC-Explorer](https://github.com/emuell/AFEC-Explorer)
//! app and related projects which are using the [Tauri](https://tauri.app) app framework.
//!
//! ## Features
//!
//! - Play, seek, stop, mix and monitor playback of **preloaded** (buffered) or **streamed**
//!   (on-the-fly decoded) **audio files**.
//! - Play, stop, mix and monitor playback of **custom synth tones** thanks to
//!   [dasp](https://github.com/RustAudio/dasp) (optional feature: disabled by default).
//! - Play audio on Windows, macOS and Linux via [cpal](https://github.com/RustAudio/cpal) or
//!   [cubeb](https://github.com/mozilla/cubeb) (cpal is enabled by default).
//! - Decodes and thus plays back most **common audio file formats**, thanks to
//!   [Symphonia](https://github.com/pdeljanov/Symphonia).
//! - Files are automatically **resampled and channel mapped** to the audio output's signal specs,
//!   thanks to [libsamplerate](https://github.com/Prior99/libsamplerate-sys).
//! - Click free playback: when stopping sounds, a very short volume fade-out is applied to
//!   **avoid clicks**.
//! - Sample precise playback scheduling, e.g. to play samples in a **sequencer**.
//!
//! ### See Also
//!
//! - [afwaveplot](https://github.com/emuell/afwaveplot):
//!   to generate **waveform plots** from audio file paths or raw sample buffers.
//! - [afseq](https://github.com/emuell/afseq):
//!   experimental **algorithmic composing** framework, which uses afplay as sample player.
//!
//! ## Examples
//!
//! See [/examples](https://github.com/emuell/afplay/tree/master/examples) directory for more examples.
//!
//! ### Simple Audio Playback
//!
//! Play and stop audio files on the system's default audio output device.
//!
//! ```rust
//! use afplay::{
//!     AudioFilePlayer, AudioOutput, AudioSink, DefaultAudioOutput,
//!     FilePlaybackOptions, Error
//! };
//!
//! # fn main() { || -> Result<(), Error> { // only check if it compiles
//! #
//! // Open the default audio device (cpal or cubeb, depending on the enabled output feature)
//! let audio_output = DefaultAudioOutput::open()?;
//! // Create a player and transfer ownership of the audio output to the player.
//! let mut player = AudioFilePlayer::new(audio_output.sink(), None);
//!
//! // Play back a file with the default playback options.
//! player.play_file(
//!     "PATH_TO/some_file.wav",
//!     FilePlaybackOptions::default())?;
//! // Play back another file on top with custom playback options.
//! player.play_file(
//!     "PATH_TO/some_long_file.mp3",
//!     FilePlaybackOptions::default()
//!         .streamed() // decodes the file on-the-fly
//!         .volume_db(-6.0) // lower the volume a bit
//!         .speed(0.5) // play file at half the speed
//!         .repeat(2), // repeat, loop it 2 times
//! )?;
//!
//! // Stop all playing files: this will quickly fade-out all playing files to avoid clicks.
//! player.stop_all_sources()?;
//!
//! # Ok(()) }; }
//! ```
//!
//! ### Advanced Audio Playback
//!
//! Play, seek and stop audio files and synth sounds on the default audio output device.
//! Monitor playback status of playing files and synth tones.
//!
//! ```rust
//! use afplay::{
//!     AudioFilePlayer, AudioOutput, AudioSink, DefaultAudioOutput,
//!     AudioFilePlaybackStatusEvent, FilePlaybackOptions, SynthPlaybackOptions,
//!     Error
//! };
//!
//! #[cfg(feature = "dasp")]
//! use dasp::Signal;
//!
//! # fn main() { || -> Result<(), Error> { // only check if it compiles
//! #
//! // Open the default audio device (cpal or cubeb, depending on the enabled output feature)
//! let audio_output = DefaultAudioOutput::open()?;
//!
//! // Create an optional channel to receive playback status events ("Position", "Stopped" events)
//! // Prefer using a bounded channel here to avoid memory allocations in the audio thread.
//! let (playback_status_sender, playback_status_receiver) = crossbeam_channel::bounded(32);
//! // Create a player and transfer ownership of the audio output to the player. The player will
//! // play, mix down and manage all files and synth sources for us from here.
//! let mut player = AudioFilePlayer::new(audio_output.sink(), Some(playback_status_sender));
//!
//! // We'll start playing a file now: The file below is going to be "preloaded" because it uses
//! // the default playback options. Preloaded means it's entirely decoded first, then played back
//! // from a buffer.
//! // Files played through the player are automatically resampled and channel-mapped to match the
//! // audio output's signal specs, so there's nothing more to do to get it played:
//! let small_file_id = player.play_file(
//!     "PATH_TO/some_small_file.wav",
//!     FilePlaybackOptions::default())?;
//! // The next file is going to be decoded and streamed on the fly, which is especially handy for
//! // long files such as music, as it can start playing right away and won't need to allocate
//! // memory for the entire file.
//! // We're also repeating the file playback 2 times, lowering the volume and are pitching it
//! // down. As the player mixes down everything, we'll hear both files at the same time now:
//! let long_file_id = player.play_file(
//!     "PATH_TO/some_long_file.mp3",
//!     FilePlaybackOptions::default()
//!         .streamed()
//!         .volume_db(-6.0)
//!         .speed(0.5)
//!         .repeat(2),
//! )?;
//!
//! // !! NB: optional `dasp-synth` feature needs to be enabled for the following to work !!
//! // Let's play a simple synth tone as well. You can play any dasp::Signal here. The passed
//! // signal will be wrapped in a dasp::signal::UntilExhausted, so this can be used to create
//! // one-shots. The example below plays a sine wave for two secs at 440hz.
//! #[cfg(feature = "dasp")]
//! let sample_rate = player.output_sample_rate();
//! #[cfg(feature = "dasp")]
//! let dasp_signal = dasp::signal::from_iter(
//!     dasp::signal::rate(sample_rate as f64)
//!         .const_hz(440.0)
//!         .sine()
//!         .take(sample_rate as usize * 2),
//! );
//! #[cfg(feature = "dasp")]
//! let synth_id = player.play_dasp_synth(
//!     dasp_signal,
//!     "my_synth_sound",
//!     SynthPlaybackOptions::default())?;
//!
//! // You can optionally track playback status events from the player:
//! std::thread::spawn(move || {
//!     while let Ok(event) = playback_status_receiver.recv() {
//!         match event {
//!             AudioFilePlaybackStatusEvent::Position {
//!                 id,
//!                 path,
//!                 context: _,
//!                 position
//!             } => {
//!                 println!(
//!                     "Playback pos of source #{} '{}': {}",
//!                     id,
//!                     path,
//!                     position.as_secs_f32()
//!                 );
//!             }
//!             AudioFilePlaybackStatusEvent::Stopped {
//!                 id,
//!                 path,
//!                 context: _,
//!                 exhausted,
//!             } => {
//!                 if exhausted {
//!                     println!("Playback of #{} '{}' finished", id, path);
//!                 } else {
//!                     println!("Playback of #{} '{}' was stopped", id, path);
//!                 }
//!             }
//!         }
//!     }
//! });
//!
//! // Playing files can be seeked or stopped:
//! player.seek_source(long_file_id, std::time::Duration::from_secs(5))?;
//! player.stop_source(small_file_id)?;
//!
//! // Synths can not be seeked, but they can be stopped.
//! #[cfg(feature = "dasp")]
//! player.stop_source(synth_id)?;
//!
//! // If you only want one file to play at the same time, simply stop all playing
//! // sounds before starting a new one:
//! player.stop_all_sources()?;
//! player.play_file("PATH_TO/boom.wav", FilePlaybackOptions::default())?;
//!
//! # Ok(()) }; }
//!```
//!
//! ### Playback Sequencing
//!
//! Play a sample file sequence in time with e.g. musical beats.
//!
//! ```rust
//! use afplay::{
//!    source::file::preloaded::PreloadedFileSource, utils::speed_from_note, AudioFilePlayer,
//!    AudioOutput, DefaultAudioOutput, Error, FilePlaybackOptions,
//! };
//!
//! # fn main() { || -> Result<(), Error> { // only check if it compiles
//! #
//! // create a player
//! let mut player = AudioFilePlayer::new(DefaultAudioOutput::open()?.sink(), None);
//!
//! // calculate at which rate the sample file should be emitted
//! let beats_per_min = 120.0;
//! let samples_per_sec = player.output_sample_rate();
//! let samples_per_beat = samples_per_sec as f64 * 60.0 / beats_per_min as f64;
//!
//! // preload a sample file
//! let preloaded_sample_source = PreloadedFileSource::new(
//!     "path/to_some_file.wav",
//!     None, // we don't need a channel for playback events
//!     FilePlaybackOptions::default(),
//!     samples_per_sec,
//! )?;
//!
//! // schedule playback of the sample file every beat for 8 beats
//! let playback_start = player.output_sample_frame_position();
//! for beat_counter in 0..8 {
//!     // when is the next beat playback due?
//!     let next_beats_sample_time =
//!         (playback_start as f64 + beat_counter as f64 * samples_per_beat) as u64;
//!     // play a clone of the preloaded sample at the next beat's sample time.
//!     // cloning is very cheap as the sample buffer is shared...
//!     player.play_file_source(
//!         preloaded_sample_source.clone(
//!             FilePlaybackOptions::default()
//!               .speed(speed_from_note(60)), // middle-c
//!             samples_per_sec)?,
//!         Some(next_beats_sample_time),
//!     )?;
//! }
//! #
//! # Ok(()) }; }
//! ```
//!
//! ## Overview
//!
//! ### AudioOutput
//!
//! Audio devices are controlled via the [`AudioOutput`] and [`AudioSink`] traits.
//! The trailts are implemented either with a `cpal` or `cubeb` with feature `cpal-output`
//! or `cubeb-output`.
//!
//! The currently enabled implementation can be refered to via the
//! [`DefaultAudioOutput`] and [`DefaultAudioSink`] types.  
//!
//! ### AudioSource
//!
//! The audio output device uses the [`AudioSource`] trait to fetch data for the output. An
//! `AudioSource` is something that writes interleaved sample buffers and defines the buffers
//! audio channel layout & sample rate.
//!
//! #### Sources which produce new signals:
//! - [FileSource](`source::file::FileSource`)
//!   - [PreloadedFileSource](`source::file::preloaded::PreloadedFileSource`) decodes entire audio file, then plays it from a buffer.
//!   - [SteamedFileSource](`source::file::streamed::StreamedFileSource`) streams and decodes audio files on the fly.
//! - [SynthSource](`source::synth::SynthSource`)
//!   - [DaspSynthSource](`source::synth::dasp::DaspSynthSource`) plays a custom
//!     [dasp](https://github.com/RustAudio/dasp) signal as AudioSource.
//!   - [FunDspSynthSource](`source::synth::fundsp::FunDspSynthSource`) plays a custom
//!     [funDSP](https://github.com/SamiPerttu/fundsp/) unit as AudioSource.
//! - [EmptySource](`source::empty::EmptySource`) produces an empty buffer.
//!
//! #### Sources which modify signals of other sources:
//! - [MixedSource](`source::mixed::MixedSource`) mixes multiple other sources together.
//! - [ResampledSource](`source::resampled::ResampledSource`) resamples or pitches other sources.
//! - [ChannelMappedSource](`source::mapped::ChannelMappedSource`) changes channel layout of other sources.
//! - [ConvertedSource](`source::converted::ConvertedSource`) resamples and changes channel lyouts of other sources.
//!
//! ### AudioPlayer
//!
//! [FileSource](`source::file::FileSource`)s and [SynthSource](`source::synth::SynthSource`)s can be played back
//! and monitored via an [`AudioFilePlayer`].
//!
//! The player internally uses a `MixedSource` to mix down everything that you throw at it and uses
//! [ConvertedSource](`source::converted::ConvertedSource`) to automatically convert all played
//! sources to the audio output's current target signal specs.
//!
//! ### AudioPlaybackOptions
//!     
//! [FilePlaybackOptions](`source::file::FilePlaybackOptions`) and
//! [SynthPlaybackoptions](`source::synth::SynthPlaybackOptions`)
//! alter playback of sources, e.g. to change the volume or playback speed. Options can be
//! spefified when starting playback.

// private (partly re-exported)
mod error;
mod output;
mod player;

// public
pub mod source;
pub mod utils;

// private (public interface exported as js)
#[cfg(target_arch = "wasm32")]
mod web;

// re-exports
pub use error::Error;
pub use output::{AudioHostId, AudioOutput, AudioSink, DefaultAudioOutput, DefaultAudioSink};
pub use player::{
    AudioFilePlaybackId, AudioFilePlaybackStatusContext, AudioFilePlaybackStatusEvent,
    AudioFilePlayer,
};
pub use source::file::FilePlaybackOptions;
pub use source::synth::SynthPlaybackOptions;
pub use source::{AudioSource, AudioSourceTime};
