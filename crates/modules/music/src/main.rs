#![forbid(unsafe_code)]
//! Music module (v1 lifecycle stub).
//!
//! Real voice playback and Lavalink are deliberately out of scope for v1. The
//! module proves the supervised lifecycle: handshake, heartbeat, command
//! dispatch, and REST replies with its own Discord client.

use std::process::ExitCode;

use rsbot_core::{CommandSpec, ModuleLimits, ModuleManifest, ProtocolVersion};
use rsbot_module_sdk::{ModuleBuilder, Response};

/// Commands are valid Discord names (no dots).
#[must_use]
pub fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: "music".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: ProtocolVersion::default(),
        commands: vec![
            CommandSpec::new("music-play", "Play a track (stub in v1)"),
            CommandSpec::new("music-skip", "Skip the current track"),
        ],
        events: vec!["interaction_create".to_owned()],
        provides: Vec::new(),
        requires: vec!["discord.rest".to_owned()],
        exclusive: vec!["voice.session".to_owned()],
        limits: ModuleLimits::default(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let result = ModuleBuilder::new(manifest())
        .command("music-play", |_context| async move {
            Ok(Response::ephemeral(
                "Music playback is not implemented yet; this build ships the lifecycle stub.",
            ))
        })
        .command("music-skip", |_context| async move {
            Ok(Response::ephemeral("Nothing is playing."))
        })
        .run()
        .await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            eprintln!("rsbot-mod-music failed: {source}");
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
        assert_eq!(names, vec!["music-play", "music-skip"]);
        assert_eq!(manifest.exclusive, vec!["voice.session"]);
        for command in &manifest.commands {
            assert_ne!(command.description, "");
        }
    }
}
