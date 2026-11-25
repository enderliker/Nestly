//! Live Discord voice playback integration test (ignored in CI).
//!
//! Run with:
//! `set -a && source .env && set +a && cargo test -p nestly-mod-music --test voice_playback -- --ignored --nocapture`

use std::time::Duration;

use nestly_mod_music::player::{MusicManager, QueuedTrack};
use nestly_mod_music::source::{self, AudioProvider};
use nestly_module_sdk::{VoiceServerPayload, VoiceStatePayload};
use songbird::tracks::PlayMode;
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_model::gateway::payload::outgoing::UpdateVoiceState;
use twilight_model::id::Id;

const TEST_GUILD_ID: u64 = 1_466_197_561_214_898_430;
const TEST_VOICE_CHANNEL_ID: u64 = 1_539_446_296_908_144_690;

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires live Discord token, network access, and target voice channel"]
async fn plays_audio_in_test_voice_channel() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let token = std::env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN must be set");
    let app_id: u64 = std::env::var("DISCORD_APPLICATION_ID")
        .unwrap_or_else(|_| "1481425627369439364".to_owned())
        .parse()
        .expect("invalid DISCORD_APPLICATION_ID");

    let client = reqwest::Client::new();
    let ytdlp = source::resolve_ytdlp_program(&client)
        .await
        .expect("yt-dlp should resolve or download");
    println!("Resolved yt-dlp: {ytdlp}");

    let target = source::resolve_query(
        "https://www.youtube.com/watch?v=jNQXAC9IVRw",
        AudioProvider::Youtube,
    );
    let mut ytdl = source::create_source(ytdlp, client.clone(), target.clone());
    let metadata = source::fetch_metadata(&mut ytdl)
        .await
        .expect("metadata fetch should succeed");
    println!(
        "Fetched metadata: {} by {} ({:?}s)",
        metadata.title, metadata.artist, metadata.duration_secs
    );

    let mut manager = MusicManager::new(client.clone());
    manager.bot_user_id = Some(app_id);

    let mut shard = Shard::new(
        ShardId::ONE,
        token,
        Intents::GUILDS | Intents::GUILD_VOICE_STATES,
    );
    let sender = shard.sender();
    let wanted = EventTypeFlags::READY
        | EventTypeFlags::VOICE_STATE_UPDATE
        | EventTypeFlags::VOICE_SERVER_UPDATE;

    // Wait for gateway READY, then join the target voice channel.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut joined_sent = false;
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(item)) =
            tokio::time::timeout(Duration::from_secs(2), shard.next_event(wanted)).await
        else {
            continue;
        };
        let event = item.expect("gateway event error");
        match event {
            Event::Ready(_) if !joined_sent => {
                joined_sent = true;
                sender
                    .command(&UpdateVoiceState::new(
                        Id::new(TEST_GUILD_ID),
                        Some(Id::new(TEST_VOICE_CHANNEL_ID)),
                        false,
                        false,
                    ))
                    .expect("send UpdateVoiceState");
            }
            Event::VoiceStateUpdate(state) => {
                manager.handle_voice_state(VoiceStatePayload {
                    guild_id: state.0.guild_id.map(Id::get),
                    channel_id: state.0.channel_id.map(Id::get),
                    user_id: state.0.user_id.get(),
                    session_id: state.0.session_id.clone(),
                });
            }
            Event::VoiceServerUpdate(server) => {
                manager.handle_voice_server(VoiceServerPayload {
                    guild_id: server.guild_id.get(),
                    endpoint: server.endpoint.clone(),
                    token: server.token.clone(),
                });
            }
            _ => {}
        }

        if manager
            .player(TEST_GUILD_ID)
            .is_some_and(|player| player.is_connected)
        {
            break;
        }
    }

    let player = manager.player_mut(TEST_GUILD_ID);
    assert!(player.is_connected, "player failed to connect to voice");
    player.ytdlp_program = ytdlp;
    player.enqueue(
        QueuedTrack {
            target,
            title: metadata.title,
            artist: metadata.artist,
            duration_secs: metadata.duration_secs,
            requester_id: None,
        },
        client,
    );

    let handle = player.handle.clone().expect("track handle must exist");
    tokio::time::sleep(Duration::from_secs(6)).await;

    let info = handle.get_info().await.expect("track info");
    println!(
        "Playback state after 6s: playing={:?}, position={:?}",
        info.playing, info.position
    );
    assert_eq!(info.playing, PlayMode::Play, "track should be playing");
    assert!(
        info.position >= Duration::from_secs(2),
        "expected audio position >= 2s, got {:?}",
        info.position
    );

    assert_eq!(player.pause_or_resume(), Ok(true));
    assert_eq!(player.pause_or_resume(), Ok(false));

    player.stop();
    let _ = sender.command(&UpdateVoiceState::new(
        Id::new(TEST_GUILD_ID),
        None,
        false,
        false,
    ));
}
