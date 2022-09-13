use crossbeam_channel::{unbounded, Sender};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    converted::ConvertedSource,
    error::Error,
    file::FilePlaybackOptions,
    output::{AudioSink, DefaultAudioSink},
    resampled::Quality as ResamplingQuality,
    source::{
        file::{
            preloaded::PreloadedFileSource, streamed::StreamedFileSource, FilePlaybackMessage,
            FileSource,
        },
        mixed::{MixedSource, MixedSourceMsg},
        playback::{PlaybackId, PlaybackStatusEvent},
        synth::{SynthPlaybackMessage, SynthSource},
    },
};

#[cfg(feature = "dasp")]
use dasp::Signal;

#[cfg(feature = "dasp")]
use crate::source::synth::{dasp::DaspSynthSource, SynthPlaybackOptions};

// -------------------------------------------------------------------------------------------------

enum PlaybackMessageSender {
    File(Sender<FilePlaybackMessage>),
    Synth(Sender<SynthPlaybackMessage>),
}

pub struct AudioFilePlayer {
    sink: DefaultAudioSink,
    playing_sources: Arc<Mutex<HashMap<PlaybackId, PlaybackMessageSender>>>,
    playback_status_sender: Sender<PlaybackStatusEvent>,
    mixer_event_sender: Sender<MixedSourceMsg>,
}

/// public interface
impl AudioFilePlayer {
    const DEFAULT_STOP_FADEOUT_SECS: f32 = 0.05;
    const DEFAULT_RESAMPLING_QUALITY: ResamplingQuality = ResamplingQuality::Linear;

    pub fn new(
        sink: DefaultAudioSink,
        playback_status_sender_arg: Option<Sender<PlaybackStatusEvent>>,
    ) -> Self {
        // Create a proxy for the playback status channel, so we can trap stop messages
        let playing_sources = Arc::new(Mutex::new(HashMap::new()));
        let playback_status_sender = Self::handle_playback_status_messages(
            playback_status_sender_arg,
            Arc::clone(&playing_sources),
        );
        // Create a mixer source, add it to the audio sink and start running
        let mixer_source = MixedSource::new(sink.channel_count(), sink.sample_rate());
        let mixer_event_sender = mixer_source.event_sender();
        sink.play(mixer_source);
        sink.resume();
        Self {
            sink,
            playing_sources,
            playback_status_sender,
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

    /// Start audio playback.
    pub fn start(&self) {
        self.sink.resume()
    }

    /// Stop audio playback. This will only pause and thus not drop any playing sources. Use the
    /// [`start`] function to start it again. Use function [`stop_all_sources`] to drop all sources.
    pub fn stop(&self) {
        self.sink.pause()
    }

    /// Play a new file with default playback options. See [`play_file_with_options`] for more info
    /// on which options can be applied.
    pub fn play_file(&mut self, file_path: &str) -> Result<PlaybackId, Error> {
        self.play_file_with_options(file_path, FilePlaybackOptions::default())
    }
    /// Play a new file with the given file path and options. See [`FilePlaybackOptions`] for more info
    /// on which options can be applied.
    ///
    /// Newly played sources are always added to the final mix and won't stop other playing sources.
    pub fn play_file_with_options(
        &mut self,
        file_path: &str,
        options: FilePlaybackOptions,
    ) -> Result<PlaybackId, Error> {
        if options.stream {
            let streamed_source = StreamedFileSource::new(
                file_path,
                Some(self.playback_status_sender.clone()),
                options,
            )?;
            self.play_file_source(streamed_source, Some(options.speed))
        } else {
            let preloaded_source = PreloadedFileSource::new(
                file_path,
                Some(self.playback_status_sender.clone()),
                options,
            )?;
            self.play_file_source(preloaded_source, Some(options.speed))
        }
    }

    /// Play a self created or cloned file source.
    pub fn play_file_source<Source: FileSource>(
        &mut self,
        file_source: Source,
        playback_speed: Option<f64>,
    ) -> Result<PlaybackId, Error> {
        // memorize source in playing sources map
        let playback_id = file_source.playback_id();
        let playback_message_sender: Sender<FilePlaybackMessage> =
            file_source.playback_message_sender();
        let mut playing_sources = self.playing_sources.lock().unwrap();
        playing_sources.insert(
            playback_id,
            PlaybackMessageSender::File(playback_message_sender),
        );
        // convert file to mixer's rate and channel layout and apply optional pitch
        let converted_source = ConvertedSource::new_with_speed(
            file_source,
            self.sink.channel_count(),
            self.sink.sample_rate(),
            playback_speed.unwrap_or(1.0),
            Self::DEFAULT_RESAMPLING_QUALITY,
        );
        // play the source by adding it to the mixer
        if let Err(err) = self.mixer_event_sender.send(MixedSourceMsg::AddSource {
            source: Box::new(converted_source),
        }) {
            log::error!("failed to send mixer event: {}", err);
            return Err(Error::SendError);
        }
        // return new file's id on success
        Ok(playback_id)
    }

    /// Play a mono f64 dasp signal with default playback options. See [`play_dasp_synth_with_options`]
    /// for more info.
    #[cfg(feature = "dasp")]
    pub fn play_dasp_synth<SignalType>(
        &mut self,
        signal: SignalType,
        signal_name: &str,
    ) -> Result<PlaybackId, Error>
    where
        SignalType: Signal<Frame = f64> + Send + 'static,
    {
        self.play_dasp_synth_with_options(signal, signal_name, SynthPlaybackOptions::default())
    }
    /// Play a mono dasp signal with the given options. See [`SynthPlaybackOptions`] for more info
    /// about available options.
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
    pub fn play_dasp_synth_with_options<SignalType>(
        &mut self,
        signal: SignalType,
        signal_name: &str,
        options: SynthPlaybackOptions,
    ) -> Result<PlaybackId, Error>
    where
        SignalType: Signal<Frame = f64> + Send + 'static,
    {
        let source = DaspSynthSource::new(
            signal,
            signal_name,
            options,
            self.sink.sample_rate(),
            Some(self.playback_status_sender.clone()),
        );
        self.play_synth(source)
    }

    #[allow(dead_code)]
    fn play_synth<S: SynthSource>(&mut self, source: S) -> Result<PlaybackId, Error> {
        // memorize source in playing sources map
        let playback_id = source.playback_id();
        let mut playing_sources = self.playing_sources.lock().unwrap();
        playing_sources.insert(
            playback_id,
            PlaybackMessageSender::Synth(source.playback_message_sender()),
        );
        // convert file to mixer's rate and channel layout
        let converted = ConvertedSource::new(
            source,
            self.sink.channel_count(),
            self.sink.sample_rate(),
            Self::DEFAULT_RESAMPLING_QUALITY,
        );
        // play the source
        if let Err(err) = self.mixer_event_sender.send(MixedSourceMsg::AddSource {
            source: Box::new(converted),
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
        playback_id: PlaybackId,
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

    /// Stop a playing file or synth source with default fade-out duration.
    pub fn stop_source(&mut self, playback_id: PlaybackId) -> Result<(), Error> {
        self.stop_source_with_fadeout(
            playback_id,
            Duration::from_secs_f32(Self::DEFAULT_STOP_FADEOUT_SECS),
        )
    }
    /// Stop a playing file or synth source with the given fade-out duration.
    pub fn stop_source_with_fadeout(
        &mut self,
        playback_id: PlaybackId,
        fadeout: Duration,
    ) -> Result<(), Error> {
        let mut playing_sources = self.playing_sources.lock().unwrap();
        if let Some(msg_sender) = playing_sources.get(&playback_id) {
            match msg_sender {
                PlaybackMessageSender::File(file_sender) => {
                    if let Err(err) = file_sender.send(FilePlaybackMessage::Stop(fadeout)) {
                        log::warn!(
                            "failed to send stop command to file source: {}",
                            err.to_string()
                        );
                    }
                }
                PlaybackMessageSender::Synth(synth_sender) => {
                    if let Err(err) = synth_sender.send(SynthPlaybackMessage::Stop(fadeout)) {
                        log::warn!(
                            "failed to send stop command to synth source: {}",
                            err.to_string()
                        );
                    }
                }
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

    /// Stop all playing sources.
    pub fn stop_all_sources(&mut self) -> Result<(), Error> {
        let playing_source_ids: Vec<PlaybackId>;
        {
            let playing_sources = self.playing_sources.lock().unwrap();
            playing_source_ids = playing_sources.keys().copied().collect();
        }
        for source_id in playing_source_ids {
            self.stop_source(source_id)?;
        }
        Ok(())
    }
}

/// details
impl AudioFilePlayer {
    fn handle_playback_status_messages(
        playback_sender_arg: Option<Sender<PlaybackStatusEvent>>,
        playing_sources: Arc<Mutex<HashMap<PlaybackId, PlaybackMessageSender>>>,
    ) -> Sender<PlaybackStatusEvent> {
        let (send_proxy, recv_proxy) = unbounded::<PlaybackStatusEvent>();

        std::thread::Builder::new()
            .name("audio_player_messages".to_string())
            .spawn(move || {
                while let Ok(msg) = recv_proxy.recv() {
                    if let PlaybackStatusEvent::Stopped {
                        id,
                        path: _,
                        exhausted: _,
                    } = msg
                    {
                        playing_sources.lock().unwrap().remove(&id);
                    }
                    if let Some(sender) = &playback_sender_arg {
                        if let Err(err) = sender.send(msg) {
                            log::warn!("failed to send file status message: {}", err);
                        }
                    }
                }
            })
            .expect("failed to spawn audio message thread");

        send_proxy
    }
}
