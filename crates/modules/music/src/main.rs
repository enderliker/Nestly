#![forbid(unsafe_code)]
//! Music module: voice playback for `YouTube` and `SoundCloud` via `Songbird`.

use std::process::ExitCode;
use std::sync::Arc;

use nestly_core::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ModuleLimits, ModuleManifest,
    ProtocolVersion,
};
use nestly_mod_music::player::{MusicManager, QueuedTrack};
use nestly_mod_music::source::{self, AudioProvider};
use nestly_module_sdk::{CommandContext, ModuleBuilder, Response};
use tokio::sync::Mutex;

/// Commands are valid Discord names (no dots).
#[must_use]
pub fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: "music".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: ProtocolVersion::default(),
        commands: vec![
            CommandSpec::new(
                "music-play",
                "Play a track or search from YouTube or SoundCloud",
            )
            .with_options(vec![
                CommandOptionSpec::new(
                    "query",
                    "Track title, search query, or YouTube/SoundCloud URL",
                    true,
                    CommandOptionKind::String,
                ),
                CommandOptionSpec::new(
                    "provider",
                    "Search provider: youtube or soundcloud (default: youtube)",
                    false,
                    CommandOptionKind::String,
                ),
                CommandOptionSpec::new(
                    "channel",
                    "Voice channel to join",
                    false,
                    CommandOptionKind::Channel,
                ),
            ]),
            CommandSpec::new("music-skip", "Skip the currently playing track"),
            CommandSpec::new("music-pause", "Pause or resume playback"),
            CommandSpec::new("music-stop", "Stop playback, clear queue, and leave voice"),
            CommandSpec::new(
                "music-nowplaying",
                "Show details of the currently playing track",
            ),
            CommandSpec::new("music-queue", "List tracks waiting in the queue"),
        ],
        events: vec![
            "interaction_create".to_owned(),
            "voice_state_update".to_owned(),
        ],
        provides: Vec::new(),
        requires: vec!["discord.rest".to_owned()],
        exclusive: vec!["voice.session".to_owned()],
        limits: ModuleLimits::default(),
    }
}

fn format_duration(seconds: Option<u64>) -> String {
    match seconds {
        Some(secs) => {
            let minutes = secs / 60;
            let rem_secs = secs % 60;
            format!("{minutes}:{rem_secs:02}")
        }
        None => "Live/Unknown".to_owned(),
    }
}

async fn handle_play(
    context: &CommandContext,
    manager: &Arc<Mutex<MusicManager>>,
) -> Result<Response, String> {
    let Some(guild_id) = context.interaction.guild_id else {
        return Ok(Response::ephemeral(
            "The `/music-play` command can only be used in a server.",
        ));
    };

    let query = context
        .option_string("query")
        .unwrap_or_default()
        .trim()
        .to_owned();
    if query.is_empty() {
        return Ok(Response::ephemeral("Please provide a track title or URL."));
    }

    let provider = context
        .option_string("provider")
        .map(|p| AudioProvider::from_str(&p))
        .unwrap_or_default();

    // Check user's voice channel
    let user_id = context.interaction.user_id.unwrap_or(0);
    let explicit_channel = context.option_channel_id("channel");
    let voice_channel_id = {
        let mgr = manager.lock().await;
        explicit_channel.or_else(|| mgr.user_voice_channel(guild_id, user_id))
    };

    let Some(channel_id) = voice_channel_id else {
        return Ok(Response::ephemeral(
            "Please join a voice channel first, or select a channel using the `channel` option.",
        ));
    };

    // Request the edge gateway to connect to the target voice channel
    if let Err(source) = context
        .voice_gateway_send(guild_id, Some(channel_id), false, false)
        .await
    {
        tracing::error!(error = %source, "cannot send voice join command");
        return Ok(Response::ephemeral(
            "⚠️ Something went wrong while connecting to voice. Please try again.",
        ));
    }

    let deferred = context
        .rest
        .respond(&context.interaction, Response::Defer { ephemeral: false })
        .await
        .is_ok();

    let target = source::resolve_query(&query, provider);
    let http_client = {
        let mgr = manager.lock().await;
        mgr.client()
    };

    let ytdlp_program = match source::resolve_ytdlp_program(&http_client).await {
        Ok(program) => program,
        Err(err) => {
            tracing::error!(error = %err, "yt-dlp unavailable");
            let msg = "⚠️ Something went wrong while preparing playback. Please try again.";
            if deferred {
                let _ = context
                    .rest
                    .update_original(&context.interaction, msg)
                    .await;
                return Ok(Response::None);
            }
            return Ok(Response::ephemeral(msg));
        }
    };

    let mut ytdl = source::create_source(ytdlp_program, http_client.clone(), target.clone());
    let metadata = match source::fetch_metadata(&mut ytdl).await {
        Ok(meta) => meta,
        Err(err) => {
            tracing::warn!(target = %target, error = %err, "failed to load track metadata");
            let msg = "⚠️ Something went wrong while loading that track. Please try again.";
            if deferred {
                let _ = context
                    .rest
                    .update_original(&context.interaction, msg)
                    .await;
                return Ok(Response::None);
            }
            return Ok(Response::ephemeral(msg));
        }
    };

    let title = metadata.title;
    let artist = metadata.artist;
    let duration_secs = metadata.duration_secs;

    let track = QueuedTrack {
        target,
        title: title.clone(),
        artist: artist.clone(),
        duration_secs,
        requester_id: Some(user_id),
    };

    let (was_empty, queue_len) = {
        let mut mgr = manager.lock().await;
        let player = mgr.player_mut(guild_id);
        player.ytdlp_program = ytdlp_program;
        let empty = player.current.is_none();
        player.enqueue(track, http_client);
        let len = player.queue.len();
        (empty, len)
    };

    let duration_display = format_duration(duration_secs);
    let response_text = if was_empty {
        format!("🎶 **Now Playing:** {title} by {artist} `[{duration_display}]`")
    } else {
        format!(
            "📋 **Added to Queue (Position #{queue_len}):** {title} by {artist} `[{duration_display}]`"
        )
    };

    if deferred {
        let _ = context
            .rest
            .update_original(&context.interaction, &response_text)
            .await;
        Ok(Response::None)
    } else {
        Ok(Response::message(response_text))
    }
}

async fn handle_skip(
    context: &CommandContext,
    manager: &Arc<Mutex<MusicManager>>,
) -> Result<Response, String> {
    let Some(guild_id) = context.interaction.guild_id else {
        return Ok(Response::ephemeral("Can only be used in a server."));
    };

    let http_client = {
        let mgr = manager.lock().await;
        mgr.client()
    };

    let next_track = {
        let mut mgr = manager.lock().await;
        let player = mgr.player_mut(guild_id);
        player.skip(http_client)
    };

    match next_track {
        Some(next) => Ok(Response::message(format!(
            "⏭️ Skipped! Now playing: **{}** by {}",
            next.title, next.artist
        ))),
        None => Ok(Response::message(
            "⏭️ Skipped current track. Queue is now empty.",
        )),
    }
}

async fn handle_pause(
    context: &CommandContext,
    manager: &Arc<Mutex<MusicManager>>,
) -> Result<Response, String> {
    let Some(guild_id) = context.interaction.guild_id else {
        return Ok(Response::ephemeral("Can only be used in a server."));
    };

    let result = {
        let mut mgr = manager.lock().await;
        let player = mgr.player_mut(guild_id);
        player.pause_or_resume()
    };

    match result {
        Ok(true) => Ok(Response::message("⏸️ Paused playback.")),
        Ok(false) => Ok(Response::message("▶️ Resumed playback.")),
        Err(msg) => Ok(Response::ephemeral(msg)),
    }
}

async fn handle_stop(
    context: &CommandContext,
    manager: &Arc<Mutex<MusicManager>>,
) -> Result<Response, String> {
    let Some(guild_id) = context.interaction.guild_id else {
        return Ok(Response::ephemeral("Can only be used in a server."));
    };

    {
        let mut mgr = manager.lock().await;
        let player = mgr.player_mut(guild_id);
        player.stop();
    }

    // Leave the voice channel
    let _ = context
        .voice_gateway_send(guild_id, None, false, false)
        .await;

    Ok(Response::message(
        "⏹️ Stopped playback, cleared the queue, and disconnected.",
    ))
}

async fn handle_nowplaying(
    context: &CommandContext,
    manager: &Arc<Mutex<MusicManager>>,
) -> Result<Response, String> {
    let Some(guild_id) = context.interaction.guild_id else {
        return Ok(Response::ephemeral("Can only be used in a server."));
    };

    let mgr = manager.lock().await;
    let Some(player) = mgr.player(guild_id) else {
        return Ok(Response::ephemeral("Nothing is currently playing."));
    };

    match player.now_playing() {
        Some(track) => {
            let duration = format_duration(track.duration_secs);
            Ok(Response::message(format!(
                "🎵 **Now Playing:** {} by {}\n⏱️ **Duration:** `{}`",
                track.title, track.artist, duration
            )))
        }
        None => Ok(Response::ephemeral("Nothing is currently playing.")),
    }
}

async fn handle_queue(
    context: &CommandContext,
    manager: &Arc<Mutex<MusicManager>>,
) -> Result<Response, String> {
    let Some(guild_id) = context.interaction.guild_id else {
        return Ok(Response::ephemeral("Can only be used in a server."));
    };

    let mgr = manager.lock().await;
    let Some(player) = mgr.player(guild_id) else {
        return Ok(Response::ephemeral("The queue is currently empty."));
    };

    let mut lines = Vec::new();
    if let Some(curr) = player.now_playing() {
        lines.push(format!(
            "▶️ **Now Playing:** {} by {} `[{}]`",
            curr.title,
            curr.artist,
            format_duration(curr.duration_secs)
        ));
    }

    let queue = player.queue_list();
    if queue.is_empty() {
        if lines.is_empty() {
            return Ok(Response::ephemeral("The queue is currently empty."));
        }
    } else {
        lines.push("\n**Up Next:**".to_owned());
        for (idx, track) in queue.iter().take(10).enumerate() {
            lines.push(format!(
                "{}. **{}** by {} `[{}]`",
                idx + 1,
                track.title,
                track.artist,
                format_duration(track.duration_secs)
            ));
        }
        if queue.len() > 10 {
            lines.push(format!("*...and {} more tracks*", queue.len() - 10));
        }
    }

    Ok(Response::message(lines.join("\n")))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let client = reqwest::Client::new();
    let manager = Arc::new(Mutex::new(MusicManager::new(client)));

    // Try resolving application bot user id from DISCORD_APPLICATION_ID
    if let Ok(app_id_str) = std::env::var("DISCORD_APPLICATION_ID")
        && let Ok(app_id) = app_id_str.parse::<u64>()
    {
        manager.lock().await.bot_user_id = Some(app_id);
    }

    let voice_state_mgr = manager.clone();
    let voice_server_mgr = manager.clone();

    let play_mgr = manager.clone();
    let skip_mgr = manager.clone();
    let pause_mgr = manager.clone();
    let stop_mgr = manager.clone();
    let np_mgr = manager.clone();
    let queue_mgr = manager.clone();

    let result = ModuleBuilder::new(manifest())
        .on_voice_state(move |state| {
            let mgr = voice_state_mgr.clone();
            async move {
                mgr.lock().await.handle_voice_state(state);
            }
        })
        .on_voice_server(move |server| {
            let mgr = voice_server_mgr.clone();
            async move {
                mgr.lock().await.handle_voice_server(server);
            }
        })
        .command("music-play", move |context| {
            let mgr = play_mgr.clone();
            async move {
                Ok(handle_play(&context, &mgr).await.unwrap_or_else(|err| {
                    tracing::error!(error = %err, "music-play handler failed");
                    Response::ephemeral("⚠️ Something went wrong. Please try again.")
                }))
            }
        })
        .command("music-skip", move |context| {
            let mgr = skip_mgr.clone();
            async move {
                Ok(handle_skip(&context, &mgr).await.unwrap_or_else(|err| {
                    tracing::error!(error = %err, "music-skip handler failed");
                    Response::ephemeral("⚠️ Something went wrong. Please try again.")
                }))
            }
        })
        .command("music-pause", move |context| {
            let mgr = pause_mgr.clone();
            async move {
                Ok(handle_pause(&context, &mgr).await.unwrap_or_else(|err| {
                    tracing::error!(error = %err, "music-pause handler failed");
                    Response::ephemeral("⚠️ Something went wrong. Please try again.")
                }))
            }
        })
        .command("music-stop", move |context| {
            let mgr = stop_mgr.clone();
            async move {
                Ok(handle_stop(&context, &mgr).await.unwrap_or_else(|err| {
                    tracing::error!(error = %err, "music-stop handler failed");
                    Response::ephemeral("⚠️ Something went wrong. Please try again.")
                }))
            }
        })
        .command("music-nowplaying", move |context| {
            let mgr = np_mgr.clone();
            async move {
                Ok(handle_nowplaying(&context, &mgr)
                    .await
                    .unwrap_or_else(|err| {
                        tracing::error!(error = %err, "music-nowplaying handler failed");
                        Response::ephemeral("⚠️ Something went wrong. Please try again.")
                    }))
            }
        })
        .command("music-queue", move |context| {
            let mgr = queue_mgr.clone();
            async move {
                Ok(handle_queue(&context, &mgr).await.unwrap_or_else(|err| {
                    tracing::error!(error = %err, "music-queue handler failed");
                    Response::ephemeral("⚠️ Something went wrong. Please try again.")
                }))
            }
        })
        .run()
        .await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            eprintln!("nestly-mod-music failed: {source}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::manifest;

    #[test]
    fn manifest_is_valid_and_declares_expected_commands() {
        let manifest = manifest();
        manifest.validate().unwrap();
        assert_eq!(manifest.id, "music");
        let names: Vec<&str> = manifest
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "music-play",
                "music-skip",
                "music-pause",
                "music-stop",
                "music-nowplaying",
                "music-queue"
            ]
        );
        assert_eq!(manifest.exclusive, vec!["voice.session"]);
        for command in &manifest.commands {
            assert_ne!(command.description, "");
        }
    }
}
