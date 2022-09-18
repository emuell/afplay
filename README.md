# afplay

**afplay** is a cross-platform *audio playback library for Rust*, based on the
[psst-core](https://github.com/jpochyla/psst/tree/master/psst-core) audio playback
implementation.

It aims to be a suitable player for game engines, but can also be used as a
general-purpose low-latency playback engine for music applications.

It was originally developed and is used in the [AFEC-Explorer](https://github.com/emuell/AFEC-Explorer)
app and related projects which are using the [Tauri](https://tauri.app) app framework.

### Features

- Play, seek, stop, mix and monitor playback of **preloaded** (buffered) or **streamed**
  (on-the-fly decoded) **audio files**.
- Play, stop, mix and monitor playback of **custom synth tones** thanks to
  [dasp](https://github.com/RustAudio/dasp) (optional feature: disabled by default).
- Play audio on Windows, macOS and Linux via [cpal](https://github.com/RustAudio/cpal) or
  [cubeb](https://github.com/mozilla/cubeb) (cpal is enabled by default).
- Decodes and thus plays back most **common audio file formats**, thanks to
  [Symphonia](https://github.com/pdeljanov/Symphonia).
- Files are automatically **resampled and channel mapped** to the audio output's signal specs,
  thanks to [libsamplerate](https://github.com/Prior99/libsamplerate-sys).
- Click free playback: when stopping sounds, a very short volume fade-out is applied to
  **avoid clicks** (de-clicking for seeking is a TODO).
- Generate **waveform plots** from audio file paths or raw sample buffers.

### See Also

- [afwaveplot](https://github.com/emuell/afwaveplot):
  to generate **waveform plots** from audio file paths or raw sample buffers.

### Examples

See [/examples](https://github.com/emuell/afplay/tree/master/examples) directory for more examples.

#### Simple Audio Playback

Play and stop audio files on the system's default audio output device.

```rust
use afplay::{
    AudioFilePlayer, AudioOutput, AudioSink, DefaultAudioOutput,
    source::file::FilePlaybackOptions, Error
};

// Open the default audio device (cpal or cubeb, depending on the enabled output feature)
let audio_output = DefaultAudioOutput::open()?;
// Create a player and transfer ownership of the audio output to the player.
let mut player = AudioFilePlayer::new(audio_output.sink(), None);

// Play back a file with the default playback options.
player.play_file("PATH_TO/some_file.wav")?;
// Play back another file on top with custom playback options.
player.play_file_with_options(
    "PATH_TO/some_long_file.mp3",
    FilePlaybackOptions::default()
        .streamed() // decodes the file on-the-fly
        .volume_db(-6.0) // lower the volume a bit
        .speed(0.5) // play file at half the speed
        .repeat(2), // repeat, loop it 2 times
)?;

// Stop all playing files: this will quickly fade-out all playing files to avoid clicks.
player.stop_all_playing_sources()?;

```

#### Advanced Audio Playback

Play, seek and stop audio files and synth sounds on the default audio output device.
Monitor playback status of playing files and synth tones.

```rust
use afplay::{
    AudioFilePlayer, AudioOutput, AudioSink, DefaultAudioOutput,
    source::file::FilePlaybackOptions, AudioFilePlaybackStatusEvent, Error
};

use dasp::Signal;

// Open the default audio device (cpal or cubeb, depending on the enabled output feature)
let audio_output = DefaultAudioOutput::open()?;

// Create an optional channel to receive playback status events ("Position", "Stopped" events)
let (playback_status_sender, playback_status_receiver) = crossbeam_channel::unbounded();
// Create a player and transfer ownership of the audio output to the player. The player will
// play, mix down and manage all files and synth sources for us from here.
let mut player = AudioFilePlayer::new(audio_output.sink(), Some(playback_status_sender));

// We'll start playing a file now: The file below is going to be "preloaded" because it uses
// the default playback options. Preloaded means it's entirely decoded first, then played back
// from a buffer.
// Files played through the player are automatically resampled and channel-mapped to match the
// audio output's signal specs, so there's nothing more to do to get it played:
let small_file_id = player.play_file("PATH_TO/some_small_file.wav")?;
// The next file is going to be decoded and streamed on the fly, which is especially handy for
// long files such as music, as it can start playing right away and won't need to allocate
// memory for the entire file.
// We're also repeating the file playback 2 times, lowering the volume and are pitching it
// down. As the player mixes down everything, we'll hear both files at the same time now:
let long_file_id = player.play_file_with_options(
    "PATH_TO/some_long_file.mp3",
    FilePlaybackOptions::default()
        .streamed()
        .volume_db(-6.0)
        .speed(0.5)
        .repeat(2),
)?;

// !! NB: optional `dasp-synth` feature needs to be enabled for the following to work !!
// Let's play a simple synth tone as well. You can play any dasp::Signal here. The passed
// signal will be wrapped in a dasp::signal::UntilExhausted, so this can be used to create
// one-shots. The example below plays a sine wave for two secs at 440hz.
let sample_rate = player.output_sample_rate();
let dasp_signal = dasp::signal::from_iter(
    dasp::signal::rate(sample_rate as f64)
        .const_hz(440.0)
        .sine()
        .take(sample_rate as usize * 2),
);
let synth_id = player.play_dasp_synth(dasp_signal, "my_synth_sound")?;

// You can optionally track playback status events from the player:
std::thread::spawn(move || {
    while let Ok(event) = playback_status_receiver.recv() {
        match event {
            AudioFilePlaybackStatusEvent::Position { id, path, position } => {
                println!(
                    "Playback pos of source #{} '{}': {}",
                    id,
                    path,
                    position.as_secs_f32()
                );
            }
            AudioFilePlaybackStatusEvent::Stopped {
                id,
                path,
                exhausted,
            } => {
                if exhausted {
                    println!("Playback of #{} '{}' finished", id, path);
                } else {
                    println!("Playback of #{} '{}' was stopped", id, path);
                }
            }
        }
    }
});

// Playing files can be seeked or stopped:
player.seek_source(long_file_id, std::time::Duration::from_secs(5))?;
player.stop_source(small_file_id)?;
// Synths can not be seeked, but they can be stopped.
player.stop_source(synth_id)?;

// If you only want one file to play at the same time, simply stop all playing
// sounds before starting a new one:
player.stop_all_playing_sources()?;
player.play_file("PATH_TO/boom.wav")?;

```

## License

afplay is distributed under the terms of both the MIT license and the Apache License (Version 2.0).

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
