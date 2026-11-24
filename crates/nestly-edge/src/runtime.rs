//! Long-running edge runtime: IPC handshake, gateway loop, and REST replies.

use std::collections::HashSet;
use std::sync::Arc;

use futures_util::StreamExt;
use nestly_core::{ModuleLimits, ModuleManifest, ProtocolVersion};
use nestly_ipc::{Envelope, GatewayEvent, IpcConnection, IpcSink, client_handshake, encode_frame};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use twilight_gateway::{EventTypeFlags, Intents, Shard, ShardId, StreamExt as GatewayStreamExt};
use twilight_http::Client;
use twilight_model::gateway::event::Event;

use crate::{
    EdgeError, commands_from_registry, deliver_fallback, deliver_notice, guild_allowed,
    interaction_payload, register_commands,
};

const OUTBOUND_QUEUE: usize = 256;
const COMMAND_REGISTRY_LIMIT: usize = 100;

/// The manifest presented to the supervisor. It must match
/// `Config::edge_manifest` exactly.
#[must_use]
pub fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: "edge".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: ProtocolVersion::default(),
        commands: Vec::new(),
        events: vec!["interaction_create".to_owned()],
        provides: vec!["discord.gateway".to_owned()],
        requires: vec!["discord.rest".to_owned()],
        exclusive: vec!["gateway".to_owned()],
        limits: ModuleLimits::default(),
    }
}

/// Runs the edge until the supervisor shuts it down or the gateway closes.
///
/// # Errors
///
/// Returns an error when the environment is incomplete, the IPC handshake
/// fails, or the gateway stream ends.
pub async fn run() -> Result<(), EdgeError> {
    install_crypto_provider();
    init_logging();

    let token =
        std::env::var("DISCORD_TOKEN").map_err(|_| EdgeError::MissingEnv("DISCORD_TOKEN"))?;
    if token.trim().is_empty() {
        return Err(EdgeError::MissingEnv("DISCORD_TOKEN"));
    }
    let application_id = parse_u64("DISCORD_APPLICATION_ID", None)?;
    let guild_id = parse_u64("DISCORD_GUILD_ID", Some(0))?;
    let guild_id = (guild_id != 0).then_some(guild_id);
    let allowed_guilds = parse_list("DISCORD_ALLOWED_GUILD_IDS")?;
    let allowed_guilds = allowed_guilds
        .iter()
        .filter_map(|value| value.parse::<u64>().ok())
        .collect::<Vec<_>>();

    let stream = nestly_sys::take_inherited_ipc()?;
    let mut connection = IpcConnection::from_std(stream)?;
    let _limits = client_handshake(&mut connection, manifest()).await?;
    info!(application_id, "edge IPC ready");

    let client = Arc::new(Client::new(token.clone()));
    let auth_client = client.clone();
    tokio::spawn(async move {
        match auth_client.current_user().await {
            Ok(_) => info!("edge authenticated with the Discord REST API"),
            Err(source) => warn!(error = %source, "Discord REST authentication failed"),
        }
    });

    let (sink, mut ipc) = connection.split();
    let (outbound_tx, outbound_rx) = mpsc::channel::<Envelope>(OUTBOUND_QUEUE);
    tokio::spawn(writer_task(sink, outbound_rx));
    let _ = outbound_tx.send(Envelope::EdgeReady).await;

    let mut shard = Shard::new(
        ShardId::ONE,
        token,
        Intents::GUILDS | Intents::GUILD_VOICE_STATES,
    );
    let sender = shard.sender();
    let wanted = EventTypeFlags::INTERACTION_CREATE
        | EventTypeFlags::READY
        | EventTypeFlags::VOICE_STATE_UPDATE
        | EventTypeFlags::VOICE_SERVER_UPDATE;
    // Read the gateway in its own task. `select!` may then cancel only the
    // receiver, never an in-flight WebSocket read.
    let (gateway_tx, mut gateway_rx) = mpsc::channel::<Event>(128);
    let gateway_task = tokio::spawn(async move {
        while let Some(item) = shard.next_event(wanted).await {
            match item {
                Ok(event) => {
                    if gateway_tx.send(event).await.is_err() {
                        return;
                    }
                }
                Err(source) => warn!(error = %source, "gateway receive error"),
            }
        }
        warn!("gateway stream ended");
    });
    let mut seq = 0_u64;
    let mut known_commands: Option<HashSet<String>> = None;
    let mut last_registered: Option<Vec<twilight_model::application::command::Command>> = None;

    loop {
        tokio::select! {
            incoming = ipc.next() => {
                let Some(frame) = incoming else {
                    info!("supervisor closed the IPC channel");
                    return Ok(());
                };
                let envelope: Envelope = nestly_ipc::decode_frame(&frame?)?;
                match envelope {
                    Envelope::Registry { modules } => {
                        let mut commands = commands_from_registry(&modules);
                        if commands.len() > COMMAND_REGISTRY_LIMIT {
                            warn!(
                                declared = commands.len(),
                                limit = COMMAND_REGISTRY_LIMIT,
                                "discarding commands above the Discord limit"
                            );
                            commands.truncate(COMMAND_REGISTRY_LIMIT);
                        }
                        known_commands = Some(
                            commands
                                .iter()
                                .map(|command| command.name.clone())
                                .collect(),
                        );
                        if last_registered.as_ref() == Some(&commands) {
                            debug!("command registry unchanged");
                            continue;
                        }
                        last_registered = Some(commands.clone());
                        let client = client.clone();
                        tokio::spawn(async move {
                            match register_commands(&client, application_id, guild_id, &commands).await {
                                Ok(()) => info!(commands = commands.len(), "registered Discord commands"),
                                Err(source) => error!(error = %source, "cannot register Discord commands"),
                            }
                        });
                    }
                    Envelope::Fallback { module, interaction, reason, acked } => {
                        let client = client.clone();
                        tokio::spawn(async move {
                            if let Err(source) = deliver_fallback(
                                &client,
                                application_id,
                                &interaction,
                                &module,
                                reason,
                                acked,
                            ).await {
                                warn!(module = %module, reason = reason.as_str(), error = %source, "cannot deliver fallback");
                            }
                        });
                    }
                    Envelope::VoiceGatewaySend {
                        guild_id,
                        channel_id,
                        self_mute,
                        self_deaf,
                    } => {
                        use twilight_model::gateway::payload::outgoing::UpdateVoiceState;
                        use twilight_model::id::Id;
                        let command = UpdateVoiceState::new(
                            Id::new(guild_id),
                            channel_id.map(Id::new),
                            self_deaf,
                            self_mute,
                        );
                        if let Err(source) = sender.command(&command) {
                            warn!(error = %source, "failed to send UpdateVoiceState to gateway");
                        }
                    }
                    Envelope::Shutdown { .. } => {
                        info!("shutdown requested by the supervisor");
                        gateway_task.abort();
                        return Ok(());
                    }
                    Envelope::Ping => {
                        let _ = outbound_tx.send(Envelope::Pong).await;
                    }
                    other => debug!(kind = other.kind(), "ignoring IPC envelope"),
                }
            }
            event = gateway_rx.recv() => {
                let Some(event) = event else {
                    return Err(EdgeError::InvalidInteraction(
                        "gateway stream ended".to_owned(),
                    ));
                };
                match event {
                    Event::Ready(_) => info!("gateway is ready"),
                    Event::VoiceStateUpdate(state) => {
                        use nestly_ipc::VoiceStatePayload;
                        let payload = VoiceStatePayload {
                            guild_id: state.0.guild_id.map(twilight_model::id::Id::get),
                            channel_id: state.0.channel_id.map(twilight_model::id::Id::get),
                            user_id: state.0.user_id.get(),
                            session_id: state.0.session_id.clone(),
                        };
                        seq = seq.wrapping_add(1);
                        if outbound_tx
                            .send(Envelope::Event {
                                seq,
                                event: GatewayEvent::VoiceState(Box::new(payload)),
                            })
                            .await
                            .is_err()
                        {
                            return Err(EdgeError::InvalidInteraction(
                                "supervisor IPC closed".to_owned(),
                            ));
                        }
                    }
                    Event::VoiceServerUpdate(server) => {
                        use nestly_ipc::VoiceServerPayload;
                        let payload = VoiceServerPayload {
                            guild_id: server.guild_id.get(),
                            endpoint: server.endpoint.clone(),
                            token: server.token.clone(),
                        };
                        seq = seq.wrapping_add(1);
                        if outbound_tx
                            .send(Envelope::Event {
                                seq,
                                event: GatewayEvent::VoiceServer(Box::new(payload)),
                            })
                            .await
                            .is_err()
                        {
                            return Err(EdgeError::InvalidInteraction(
                                "supervisor IPC closed".to_owned(),
                            ));
                        }
                    }
                    Event::InteractionCreate(interaction) => {
                        let interaction = interaction.0;
                        let Ok(payload) = interaction_payload(&interaction) else {
                            warn!("dropping an interaction that cannot be encoded");
                            continue;
                        };
                        if !guild_allowed(&allowed_guilds, payload.guild_id) {
                            warn!(
                                guild_id = ?payload.guild_id,
                                "ignoring an interaction outside the guild allowlist"
                            );
                            continue;
                        }
                        if let (Some(known), Some(command)) = (&known_commands, &payload.command_name)
                            && !known.contains(command)
                        {
                            let client = client.clone();
                            let payload = payload.clone();
                            tokio::spawn(async move {
                                let _ = deliver_notice(
                                    &client,
                                    application_id,
                                    &payload,
                                    "Unknown command.",
                                    true,
                                ).await;
                            });
                            continue;
                        }
                        seq = seq.wrapping_add(1);
                        if outbound_tx
                            .send(Envelope::Event {
                                seq,
                                event: GatewayEvent::Interaction(Box::new(payload)),
                            })
                            .await
                            .is_err()
                        {
                            return Err(EdgeError::InvalidInteraction(
                                "supervisor IPC closed".to_owned(),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn writer_task(mut sink: IpcSink, mut rx: mpsc::Receiver<Envelope>) {
    use futures_util::SinkExt;
    while let Some(envelope) = rx.recv().await {
        let Ok(frame) = encode_frame(&envelope) else {
            return;
        };
        if sink.send(frame).await.is_err() {
            return;
        }
    }
}

fn parse_u64(name: &'static str, default: Option<u64>) -> Result<u64, EdgeError> {
    match std::env::var(name) {
        Ok(raw) => raw
            .trim()
            .parse()
            .map_err(|_| EdgeError::InvalidEnv { name, value: raw }),
        Err(_) => default.ok_or(EdgeError::MissingEnv(name)),
    }
}

fn parse_list(name: &'static str) -> Result<Vec<String>, EdgeError> {
    let raw = std::env::var(name).map_err(|_| EdgeError::MissingEnv(name))?;
    Ok(raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// Selects the ring provider explicitly.
///
/// `rustls` reaches this build through several dependencies, and feature
/// unification can enable both `ring` and `aws-lc-rs`; auto-detection then
/// aborts. Installing the provider once makes the choice deterministic.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stdout)
        .try_init();
}
