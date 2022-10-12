use crossbeam_channel::{unbounded, Sender};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    error::Error,
    output::{AudioSink, DefaultAudioSink},
    source::{
        converted::ConvertedSource,
        file::{
            preloaded::PreloadedFileSource, streamed::StreamedFileSource, FilePlaybackMessage,
            FilePlaybackOptions, FileSource,
        },
        mixed::{MixedSource, MixedSourceMsg},
        resampled::ResamplingQuality,
        synth::SynthPlaybackMessage,
    },
    AudioSource,
};

#[cfg(any(feature = "dasp", feature = "fundsp"))]
use crate::source::synth::{SynthPlaybackOptions, SynthSource};

#[cfg(feature = "dasp")]
use crate::source::synth::dasp::DaspSynthSource;
#[cfg(feature = "dasp")]
use dasp::Signal;

#[cfg(feature = "fundsp")]
use crate::source::synth::fundsp::FunDspSynthSource;
#[cfg(feature = "fundsp")]
use fundsp::audiounit::AudioUnit64;

// -------------------------------------------------------------------------------------------------

/// A unique ID for a newly created File or Synth Sources.
pub type AudioFilePlaybackId = usize;

// -------------------------------------------------------------------------------------------------

/// Events send back from File or Synth sources via the player to the user.
pub enum AudioFilePlaybackStatusEvent {
    Position {
        /// Unique id to resolve played back sources.
        id: AudioFilePlaybackId,
        /// The file path for file based sources, else a name to somewhat identify the source.
        path: String,
        /// Source's actual playback position in wallclock-time.
        position: Duration,
    },
    Stopped {
        /// Unique id to resolve played back sources
        id: AudioFilePlaybackId,
        /// the file path for file based sources, else a name to somewhat identify the source
        path: String,
        /// true when the source finished playing (e.g. reaching EOF), false when manually stopped
        exhausted: bool,
    },
}

// -------------------------------------------------------------------------------------------------

/// Event send back from Mixer to the Player drop exhausted sources, avoiding that this happens
/// in the Mixer's real-time thread.
#[derive(Clone)]
pub struct AudioSourceDropEvent {
    #[allow(dead_code)]
    source: Arc<dyn AudioSource>,
}

impl AudioSourceDropEvent {
    pub fn new(source: Arc<dyn AudioSource>) -> Self {
        Self { source }
    }
}

// -------------------------------------------------------------------------------------------------

/// Wraps File and Synth Playback messages together into one object, allowing to easily stop them.
#[derive(Clone)]
pub enum PlaybackMessageSender {
    File(Sender<FilePlaybackMessage>),
    Synth(Sender<SynthPlaybackMessage>),
}

impl PlaybackMessageSender {
    pub fn try_send_stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            PlaybackMessageSender::File(sender) => sender.try_send(FilePlaybackMessage::Stop)?,
            PlaybackMessageSender::Synth(sender) => sender.try_send(SynthPlaybackMessage::Stop)?,
        };
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------

/// Playback controller, which drives an [`AudioSink`] and runs a [`MixedSource`] which
/// can play an unlimited number of [`FileSource`] or [`SynthSource`] at the same time.
///
/// Playback status of all sources can be tracked via an optional event channel.
/// New sources can be added any time, and can be stopped and seeked (seeking works for file
/// based sources only).
///
/// NB: For playback of [`SynthSource`]s, the `dasp-synth` feature needs to be enabled.
pub struct AudioFilePlayer {
    sink: DefaultAudioSink,
    playing_sources: Arc<Mutex<HashMap<AudioFilePlaybackId, PlaybackMessageSender>>>,
    playback_status_sender: Sender<AudioFilePlaybackStatusEvent>,
    mixer_event_sender: Sender<MixedSourceMsg>,
}

impl AudioFilePlayer {
    /// Create a new AudioFilePlayer for the given DefaultAudioSink.
    /// Param `playback_status_sender` is an optional channel which can be used to receive
    /// playback status events for the currently playing sources.
    pub fn new(
        sink: DefaultAudioSink,
        playback_status_sender: Option<Sender<AudioFilePlaybackStatusEvent>>,
    ) -> Self {
        // Create a proxy for the playback status channel, so we can trap stop messages
        let playing_sources = Arc::new(Mutex::new(HashMap::new()));
        let (playback_status_sender_proxy, drain_send) =
            Self::handle_events(playback_status_sender, Arc::clone(&playing_sources));
        // Create a mixer source, add it to the audio sink and start running
        let mixer_source = MixedSource::new(sink.channel_count(), sink.sample_rate(), drain_send);
        let mixer_event_sender = mixer_source.event_sender();
        let mut sink = sink;
        sink.play(mixer_source);
        sink.resume();
        Self {
            sink,
            playing_sources,
            playback_status_sender: playback_status_sender_proxy,
            mixer_event_sender,
        }
    }

    /// Our audio device's actual sample rate.
    pub fn output_sample_rate(&self) -> u32 {
        self.sink.sample_rate()
    }
    /// Our audio device's actual sample channel count.
    pub fn output_channel_count(&self) -> usize {
        self.sink.channel_count()
    }
    /// Our actual playhead pos in samples (NOT sample frames)
    pub fn output_sample_position(&self) -> u64 {
        self.sink.sample_position()
    }
    /// Our actual playhead pos in sample frames
    pub fn output_sample_frame_position(&self) -> u64 {
        self.output_sample_position() / self.output_channel_count() as u64
    }

    /// Start audio playback.
    pub fn start(&mut self) {
        self.sink.resume();
    }

    /// Stop audio playback. This will only pause and thus not drop any playing sources. Use the
    /// `start` function to start it again. Use function `stop_all_sources` to drop all sources.
    pub fn stop(&mut self) {
        self.sink.pause();
    }

    /// Play a new file with the given file path and options. See [`FilePlaybackOptions`] for more info
    /// on which options can be applied.
    ///
    /// Newly played sources are always added to the final mix and won't stop other playing sources.
    pub fn play_file(
        &mut self,
        file_path: &str,
        options: FilePlaybackOptions,
    ) -> Result<AudioFilePlaybackId, Error> {
        // validate options
        if let Err(err) = options.validate() {
            return Err(err);
        }
        // create a stremed or preloaded source, depending on the options and play it
        if options.stream {
            let streamed_source = StreamedFileSource::new(
                file_path,
                Some(self.playback_status_sender.clone()),
                options,
            )?;
            self.play_file_source(
                streamed_source,
                options.speed,
                options.start_time,
                options.resampling_quality,
            )
        } else {
            let preloaded_source = PreloadedFileSource::new(
                file_path,
                Some(self.playback_status_sender.clone()),
                options,
            )?;
            self.play_file_source(
                preloaded_source,
                options.speed,
                options.start_time,
                options.resampling_quality,
            )
        }
    }

    /// Play a self created or cloned file source.
    pub fn play_file_source<Source: FileSource>(
        &mut self,
        file_source: Source,
        speed: f64,
        start_time: Option<u64>,
        resampling_quality: ResamplingQuality,
    ) -> Result<AudioFilePlaybackId, Error> {
        // memorize source in playing sources map
        let playback_id = file_source.playback_id();
        let playback_message_sender =
            PlaybackMessageSender::File(file_source.playback_message_sender());
        let mut playing_sources = self.playing_sources.lock().unwrap();
        playing_sources.insert(playback_id, playback_message_sender.clone());
        // convert file to mixer's rate and channel layout and apply optional pitch
        let converted_source = ConvertedSource::new_with_speed(
            file_source,
            self.sink.channel_count(),
            self.sink.sample_rate(),
            speed,
            resampling_quality,
        );
        // play the source by adding it to the mixer
        if let Err(err) = self.mixer_event_sender.send(MixedSourceMsg::AddSource {
            playback_id,
            playback_message_sender,
            source: Arc::new(converted_source),
            sample_time: start_time.unwrap_or(0),
        }) {
            log::error!("failed to send mixer event: {}", err);
            return Err(Error::SendError);
        }
        // return new file's id on success
        Ok(playback_id)
    }

    /// Play a mono [dasp](https://github.com/RustAudio/dasp) signal with the given options.
    /// See [`SynthPlaybackOptions`] for more info about available options.
    ///
    /// The signal will be wrapped into a dasp::signal::UntilExhausted so it can be used to play
    /// create one-shots.
    ///
    /// Example one-shot signal:
    /// `dasp::signal::from_iter(
    ///     dasp::signal::rate(sample_rate as f64)
    ///         .const_hz(440.0)
    ///         .sine()
    ///         .take(sample_rate as usize * 2),
    /// )`
    /// which plays a sine wave at 440 hz for 2 seconds.
    #[cfg(feature = "dasp")]
    pub fn play_dasp_synth<SignalType>(
        &mut self,
        signal: SignalType,
        signal_name: &str,
        options: SynthPlaybackOptions,
    ) -> Result<AudioFilePlaybackId, Error>
    where
        SignalType: Signal<Frame = f64> + Send + Sync + 'static,
    {
        // validate options
        if let Err(err) = options.validate() {
            return Err(err);
        }
        // create Dasp source and play it
        let source = DaspSynthSource::new(
            signal,
            signal_name,
            options,
            self.sink.sample_rate(),
            Some(self.playback_status_sender.clone()),
        );
        self.play_synth(source, options.start_time)
    }

    /// Play a mono [funDSP](https://github.com/SamiPerttu/fundsp/) generator with the given options.
    /// See [`SynthPlaybackOptions`] for more info about available options.
    ///
    /// Example generator:
    /// `oversample(sine_hz(110.0) * 110.0 * 5.0 + 110.0 >> sine())`
    /// which plays an oversampled FM sine tone at 110 hz.
    #[cfg(feature = "dasp")]
    pub fn play_fundsp_synth(
        &mut self,
        unit: impl AudioUnit64 + 'static,
        unit_name: &str,
        options: SynthPlaybackOptions,
    ) -> Result<AudioFilePlaybackId, Error> {
        // validate options
        if let Err(err) = options.validate() {
            return Err(err);
        }
        // create Dasp source and play it
        let source = FunDspSynthSource::new(
            unit,
            unit_name,
            options,
            self.sink.sample_rate(),
            Some(self.playback_status_sender.clone()),
        );
        self.play_synth(source, options.start_time)
    }

    #[cfg(any(feature = "dasp", feature = "fundsp"))]
    fn play_synth<S: SynthSource>(
        &mut self,
        source: S,
        start_time: Option<u64>,
    ) -> Result<AudioFilePlaybackId, Error> {
        // memorize source in playing sources map
        let playback_id = source.playback_id();
        let playback_message_sender =
            PlaybackMessageSender::Synth(source.playback_message_sender());
        let mut playing_sources = self.playing_sources.lock().unwrap();
        playing_sources.insert(playback_id, playback_message_sender.clone());
        // convert file to mixer's rate and channel layout
        let converted = ConvertedSource::new(
            source,
            self.sink.channel_count(),
            self.sink.sample_rate(),
            ResamplingQuality::Default, // usually unused
        );
        // play the source
        if let Err(err) = self.mixer_event_sender.send(MixedSourceMsg::AddSource {
            playback_id,
            playback_message_sender,
            source: Arc::new(converted),
            sample_time: start_time.unwrap_or(0),
        }) {
            log::error!("failed to send mixer event: {}", err);
            return Err(Error::SendError);
        }
        // return new synth's id
        Ok(playback_id)
    }

    /// Change playback position of the given played back source. This is only supported for files and thus
    /// won't do anyththing for synths.
    pub fn seek_source(
        &mut self,
        playback_id: AudioFilePlaybackId,
        position: Duration,
    ) -> Result<(), Error> {
        let playing_sources = self.playing_sources.lock().unwrap();
        if let Some(msg_sender) = playing_sources.get(&playback_id) {
            if let PlaybackMessageSender::File(sender) = msg_sender {
                if let Err(err) = sender.send(FilePlaybackMessage::Seek(position)) {
                    log::warn!("failed to send seek command to file: {}", err.to_string());
                }
            } else {
                log::warn!("trying to seek a synth source, which is not supported");
            }
            return Ok(());
        } else {
            log::warn!("trying to seek source #{playback_id} which is not or no longer playing");
        }
        Err(Error::MediaFileNotFound)
    }

    /// Immediately stop a playing file or synth source. NB: This will fade-out the source when a
    /// stop_fade_out_duration option was set in the playback options it got started with.
    pub fn stop_source(&mut self, playback_id: AudioFilePlaybackId) -> Result<(), Error> {
        let mut playing_sources = self.playing_sources.lock().unwrap();
        if let Some(msg_sender) = playing_sources.get(&playback_id) {
            if let Err(err) = msg_sender.try_send_stop() {
                log::warn!(
                    "failed to send stop command to file source: {}",
                    err.to_string()
                );
            }
            // we shortly will receive an Exhaused event which removes the source, but neverthless
            // remove it now, to force all following attempts to stop this source to fail
            playing_sources.remove(&playback_id);
            return Ok(());
        } else {
            // log::warn!("trying to stop source #{playback_id} which is not or no longer playing");
        }
        Err(Error::MediaFileNotFound)
    }

    /// Stop a playing file or synth source at a given sample time in future.
    pub fn stop_source_at_sample_time(
        &mut self,
        playback_id: AudioFilePlaybackId,
        stop_time: u64,
    ) -> Result<(), Error> {
        // check if the given playback id is still know (playing)
        let playing_sources = self.playing_sources.lock().unwrap();
        if playing_sources.contains_key(&playback_id) {
            // pass stop request to mixer
            if let Err(err) = self.mixer_event_sender.send(MixedSourceMsg::StopSource {
                playback_id,
                sample_time: stop_time,
            }) {
                log::error!("failed to send mixer event: {}", err);
                return Err(Error::SendError);
            }
            // NB: do not remove from playing_sources, as the event may apply in a long time in future.
            Ok(())
        } else {
            Err(Error::MediaFileNotFound)
        }
    }

    /// Immediately stop all playing and possibly scheduled sources.
    pub fn stop_all_sources(&mut self) -> Result<(), Error> {
        // stop everything which is playing now
        let playing_source_ids: Vec<AudioFilePlaybackId>;
        {
            let playing_sources = self.playing_sources.lock().unwrap();
            playing_source_ids = playing_sources.keys().copied().collect();
        }
        for source_id in playing_source_ids {
            self.stop_source(source_id)?;
        }
        // remove all upcoming, scheduled sources in the mixer too
        if let Err(err) = self
            .mixer_event_sender
            .send(MixedSourceMsg::RemoveAllPendingSources)
        {
            log::error!("failed to send mixer event: {}", err);
            return Err(Error::SendError);
        }
        Ok(())
    }
}

/// details
impl AudioFilePlayer {
    fn handle_events(
        playback_sender: Option<Sender<AudioFilePlaybackStatusEvent>>,
        playing_sources: Arc<Mutex<HashMap<AudioFilePlaybackId, PlaybackMessageSender>>>,
    ) -> (
        Sender<AudioFilePlaybackStatusEvent>,
        Sender<AudioSourceDropEvent>,
    ) {
        let (drop_send, drop_recv) = unbounded::<AudioSourceDropEvent>();
        let (playback_send_proxy, playback_recv_proxy) =
            unbounded::<AudioFilePlaybackStatusEvent>();

        std::thread::Builder::new()
            .name("audio_player_messages".to_string())
            .spawn(move || loop {
                crossbeam_channel::select! {
                    recv(drop_recv) -> _msg => {
                        // nothing to do apart from receiving the message...
                    }
                    recv(playback_recv_proxy) -> msg => {
                        if let Ok(event) = msg {
                           if let AudioFilePlaybackStatusEvent::Stopped {
                            id,
                            path: _,
                            exhausted: _,
                            } = event {
                                playing_sources.lock().unwrap().remove(&id);
                            }
                            if let Some(sender) = &playback_sender {
                                if let Err(err) = sender.send(event) {
                                    log::warn!("failed to send file status message: {}", err);
                                }
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn audio message thread");

        (playback_send_proxy, drop_send)
    }
}
