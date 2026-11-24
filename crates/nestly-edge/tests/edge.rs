//! Edge behaviour tests that never contact Discord.
//!
//! Interaction conversion, the guild allowlist, and command registration are
//! pure functions. Fallback delivery is exercised against a loopback HTTP
//! server through `twilight-http`'s proxy support, so the real request path is
//! covered without network access.

use nestly_core::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, FallbackReason, ModuleLimits,
    ModuleManifest, ProtocolVersion,
};
use nestly_edge::{commands_from_registry, deliver_fallback, guild_allowed, interaction_payload};
use nestly_ipc::InteractionPayload;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use twilight_model::application::command::CommandType;
use twilight_model::application::interaction::application_command::CommandData;
use twilight_model::application::interaction::message_component::MessageComponentInteractionData;
use twilight_model::application::interaction::{Interaction, InteractionData, InteractionType};
use twilight_model::channel::message::component::ComponentType;
use twilight_model::id::Id;
use twilight_model::oauth::ApplicationIntegrationMap;

const TOKEN: &str = "interaction-token-0123456789";
#[allow(deprecated)]
fn interaction(kind: InteractionType, data: Option<InteractionData>) -> Interaction {
    Interaction {
        app_permissions: None,
        application_id: Id::new(111),
        attachment_size_limit: 0,
        authorizing_integration_owners: ApplicationIntegrationMap {
            guild: None,
            user: None,
        },
        channel: None,
        channel_id: Some(Id::new(222)),
        context: None,
        data,
        entitlements: Vec::new(),
        guild: None,
        guild_id: Some(Id::new(333)),
        guild_locale: None,
        id: Id::new(444),
        kind,
        locale: None,
        member: None,
        message: None,
        token: TOKEN.to_owned(),
        user: None,
    }
}

fn application_command(name: &str) -> Interaction {
    let data = CommandData {
        guild_id: Some(Id::new(333)),
        id: Id::new(555),
        kind: CommandType::ChatInput,
        name: name.to_owned(),
        options: Vec::new(),
        resolved: None,
        target_id: None,
    };
    interaction(
        InteractionType::ApplicationCommand,
        Some(InteractionData::ApplicationCommand(Box::new(data))),
    )
}

fn component(custom_id: &str) -> Interaction {
    let data = MessageComponentInteractionData {
        component_type: ComponentType::Button,
        custom_id: custom_id.to_owned(),
        resolved: None,
        values: Vec::new(),
    };
    interaction(
        InteractionType::MessageComponent,
        Some(InteractionData::MessageComponent(Box::new(data))),
    )
}

fn manifest(id: &str, commands: Vec<CommandSpec>) -> ModuleManifest {
    ModuleManifest {
        id: id.to_owned(),
        version: "0.1.0".to_owned(),
        protocol: ProtocolVersion::default(),
        commands,
        events: vec!["interaction_create".to_owned()],
        provides: Vec::new(),
        requires: vec!["discord.rest".to_owned()],
        exclusive: Vec::new(),
        limits: ModuleLimits::default(),
    }
}

#[test]
fn converts_application_command_to_payload() {
    let payload = interaction_payload(&application_command("music-play")).unwrap();
    assert_eq!(payload.id, 444);
    assert_eq!(payload.application_id, 111);
    assert_eq!(payload.kind, 2);
    assert_eq!(payload.token, TOKEN);
    assert_eq!(payload.guild_id, Some(333));
    assert_eq!(payload.channel_id, Some(222));
    assert_eq!(payload.command_name.as_deref(), Some("music-play"));
    assert!(payload.custom_id.is_none());
    let data: serde_json::Value =
        serde_json::from_str(payload.data_json.as_deref().unwrap()).unwrap();
    assert_eq!(data["name"], "music-play");
}

#[test]
fn converts_component_to_payload_with_custom_id() {
    let payload = interaction_payload(&component("mod:music:skip")).unwrap();
    assert_eq!(payload.kind, 3);
    assert_eq!(payload.custom_id.as_deref(), Some("mod:music:skip"));
    assert!(payload.command_name.is_none());
}

#[test]
fn rejects_interactions_with_an_invalid_token() {
    let mut value = application_command("music-play");
    value.token.clear();
    assert!(interaction_payload(&value).is_err());
}

#[test]
fn enforces_the_guild_allowlist() {
    assert!(guild_allowed(&[1, 2], Some(2)));
    assert!(!guild_allowed(&[1, 2], Some(3)));
    assert!(!guild_allowed(&[1, 2], None));
    assert!(!guild_allowed(&[], Some(1)));
}

#[test]
fn builds_deduplicated_commands_with_options() {
    let music = manifest(
        "music",
        vec![
            CommandSpec::new("music-play", "Play a track"),
            CommandSpec::new("music-skip", "Skip the current track"),
        ],
    );
    let ops = manifest(
        "ops",
        vec![
            CommandSpec::new("ops-restart", "Restart a module").with_options(vec![
                CommandOptionSpec::new("module", "Module id", true, CommandOptionKind::String),
                CommandOptionSpec::new(
                    "force",
                    "Skip the grace period",
                    false,
                    CommandOptionKind::Boolean,
                ),
            ]),
            // A duplicate name must never be registered twice.
            CommandSpec::new("music-play", "Duplicate should be ignored"),
        ],
    );

    let commands = commands_from_registry(&[music, ops]);
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0].name, "music-play");
    assert_eq!(commands[1].name, "music-skip");
    assert_eq!(commands[2].name, "ops-restart");
    for command in &commands {
        assert_eq!(command.kind, CommandType::ChatInput);
        assert_ne!(command.description, "");
    }
    let restart = &commands[2];
    assert_eq!(restart.options.len(), 2);
    assert_eq!(restart.options[0].name, "module");
    assert_eq!(restart.options[0].required, Some(true));
    assert_eq!(restart.options[1].name, "force");
    assert_eq!(restart.options[1].required, Some(false));
}

#[test]
fn defaults_a_missing_command_description() {
    let commands =
        commands_from_registry(&[manifest("ops", vec![CommandSpec::new("ops-status", "")])]);
    assert_eq!(commands[0].description, "ops-status command");
}

/// A loopback HTTP server that records one request and answers 204.
async fn capture_request() -> (String, oneshot::Receiver<(String, String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(end) = find_headers_end(&buffer) {
                let headers = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let body_start = end + 4;
                while buffer.len() < body_start + length {
                    let read = socket.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                }
                let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
                let mut parts = head.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                let body =
                    String::from_utf8_lossy(&buffer[body_start..body_start + length]).into_owned();
                let _ = sender.send((method, path, body));
                let _ = socket
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .await;
                return;
            }
        }
    });
    (format!("127.0.0.1:{}", address.port()), receiver)
}

fn find_headers_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn client(host: &str) -> twilight_http::Client {
    nestly_edge::install_crypto_provider();
    twilight_http::Client::builder()
        .token("test-token-0123456789".to_owned())
        .proxy(host.to_owned(), true)
        .build()
}

fn payload() -> InteractionPayload {
    interaction_payload(&application_command("music-play")).unwrap()
}

#[tokio::test]
async fn responds_ephemerally_when_the_module_never_acked() {
    let (host, captured) = capture_request().await;
    let client = client(&host);
    deliver_fallback(
        &client,
        111,
        &payload(),
        "music",
        FallbackReason::Failed,
        false,
    )
    .await
    .unwrap();

    let (method, path, body) = captured.await.unwrap();
    assert_eq!(method, "POST");
    assert!(
        path.ends_with(&format!("/interactions/444/{TOKEN}/callback")),
        "{path}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["type"], 4);
    assert_eq!(json["data"]["flags"], 64);
    let content = json["data"]["content"].as_str().unwrap();
    assert!(content.contains("temporarily unavailable"), "{content}");
    assert!(content.contains("(music)"), "{content}");
}

#[tokio::test]
async fn updates_the_original_response_when_the_module_had_acked() {
    let (host, captured) = capture_request().await;
    let client = client(&host);
    deliver_fallback(
        &client,
        111,
        &payload(),
        "music",
        FallbackReason::Crashed,
        true,
    )
    .await
    .unwrap();

    let (method, path, body) = captured.await.unwrap();
    assert_eq!(method, "PATCH");
    assert!(
        path.ends_with(&format!("/webhooks/111/{TOKEN}/messages/@original")),
        "{path}"
    );
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let content = json["content"].as_str().unwrap();
    assert!(content.contains("stopped unexpectedly"), "{content}");
    assert!(content.contains("(music)"), "{content}");
}

#[tokio::test]
async fn refuses_to_reply_for_an_invalid_payload() {
    let (host, _captured) = capture_request().await;
    let client = client(&host);
    let mut invalid = payload();
    invalid.token.clear();
    let result = deliver_fallback(
        &client,
        111,
        &invalid,
        "music",
        FallbackReason::Failed,
        false,
    )
    .await;
    assert!(result.is_err());
}

#[test]
fn option_kinds_map_to_discord_types() {
    let ops = manifest(
        "ops",
        vec![
            CommandSpec::new("ops-restart", "Restart").with_options(vec![
                CommandOptionSpec::new("module", "Module", true, CommandOptionKind::String),
                CommandOptionSpec::new("count", "Count", false, CommandOptionKind::Integer),
                CommandOptionSpec::new("force", "Force", false, CommandOptionKind::Boolean),
                CommandOptionSpec::new("user", "User", false, CommandOptionKind::User),
                CommandOptionSpec::new("channel", "Channel", false, CommandOptionKind::Channel),
            ]),
        ],
    );
    let commands = commands_from_registry(&[ops]);
    let kinds: Vec<_> = commands[0]
        .options
        .iter()
        .map(|option| option.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            twilight_model::application::command::CommandOptionType::String,
            twilight_model::application::command::CommandOptionType::Integer,
            twilight_model::application::command::CommandOptionType::Boolean,
            twilight_model::application::command::CommandOptionType::User,
            twilight_model::application::command::CommandOptionType::Channel,
        ]
    );
    assert!(commands[0].options[0].autocomplete.is_none());
    assert!(commands[0].options[0].description_localizations.is_none());
}
