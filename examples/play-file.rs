use afplay::{
    file::FilePlaybackStatusMsg,
    AudioFilePlayer, {AudioOutput, DefaultAudioOutput},
};

fn main() -> Result<(), String> {
    // Open default device
    let audio_output = DefaultAudioOutput::open().map_err(|err| err.to_string())?;
    let audio_sink = audio_output.sink();

    // create channel for playback status events
    let (event_sx, event_rx) = crossbeam_channel::unbounded();
    let mut player = AudioFilePlayer::new(audio_sink, Some(event_sx), None);

    // pause playing until we've added all sources
    player.stop();

    // create sound sources and memorize file ids for the playback status
    let mut playing_file_ids = vec![
        player
            // this file is going to be entirely decoded first, then played back
            .play_preloaded_file("assets/altijd synth bit.wav".to_string())
            .map_err(|err| err.to_string())?,
        player
            // this file is going to be streamed on the fly
            .play_streamed_file("assets/BSQ_M14.wav".to_string())
            .map_err(|err| err.to_string())?,
    ];

    // start playing
    player.start();

    // handle events from the file sources
    let event_thread = std::thread::spawn(move || loop {
        match event_rx.recv() {
            Ok(event) => match event {
                FilePlaybackStatusMsg::Position {
                    file_id,
                    file_path: path,
                    position,
                } => {
                    println!(
                        "Playback pos of file #{} '{}': {}",
                        file_id,
                        path,
                        position.as_secs_f32()
                    );
                }
                FilePlaybackStatusMsg::Stopped {
                    file_id,
                    file_path,
                    end_of_file,
                } => {
                    if end_of_file {
                        println!("Playback of #{} '{}' finished playback", file_id, file_path);
                    } else {
                        println!("Playback of #{} '{}' was stopped", file_id, file_path);
                    }
                    playing_file_ids.retain(|v| *v != file_id);
                    if playing_file_ids.is_empty() {
                        // stop thread when all files finished
                        break;
                    }
                }
            },
            Err(err) => {
                log::info!("Playback event channel closed: '{err}'");
                break;
            }
        }
    });

    // wait until playback of all files finished
    event_thread.join().unwrap();

    Ok(())
}
