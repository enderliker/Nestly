#![forbid(unsafe_code)]
#![doc = "rsbot edge: the single Discord gateway, command registry, and fallback replier."]

mod runtime;

pub use runtime::{install_crypto_provider, run};

use std::collections::BTreeMap;

use rsbot_core::{CommandOptionKind, FallbackReason, ModuleManifest};
use rsbot_ipc::InteractionPayload;
use thiserror::Error;
use twilight_http::Client;
use twilight_model::application::command::{
    Command, CommandOption, CommandOptionType, CommandType,
};
use twilight_model::application::interaction::application_command::CommandData;
use twilight_model::application::interaction::message_component::MessageComponentInteractionData;
use twilight_model::application::interaction::modal::ModalInteractionData;
use twilight_model::application::interaction::{Interaction, InteractionData, InteractionType};
use twilight_model::channel::message::MessageFlags;
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};
use twilight_model::id::Id;

/// Edge failures.
#[derive(Debug, Error)]
pub enum EdgeError {
    #[error("ipc error: {0}")]
    Ipc(#[from] rsbot_ipc::IpcError),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("discord http error: {0}")]
    Http(#[from] twilight_http::Error),
    #[error("invalid interaction: {0}")]
    InvalidInteraction(String),
    #[error("missing environment variable: {0}")]
    MissingEnv(&'static str),
    #[error("invalid environment variable {name}: {value}")]
    InvalidEnv { name: &'static str, value: String },
    #[error("error raised by tracing subscriber: {0}")]
    Logging(String),
}

/// Converts a Discord interaction into the compact IPC payload.
///
/// The raw `data` object is serialized once so modules can read options
/// without deserializing Discord types.
///
/// # Errors
///
/// Returns [`EdgeError::InvalidInteraction`] when the payload fails the IPC
/// validation rules (for example an empty token).
pub fn interaction_payload(interaction: &Interaction) -> Result<InteractionPayload, EdgeError> {
    let (command_name, custom_id, data_json) = match &interaction.data {
        Some(InteractionData::ApplicationCommand(data)) => command_fields(data),
        Some(InteractionData::MessageComponent(data)) => {
            component_fields(data.custom_id.as_str(), data)
        }
        Some(InteractionData::ModalSubmit(data)) => modal_fields(data.custom_id.as_str(), data),
        Some(_) | None => (None, None, None),
    };

    #[allow(deprecated)]
    let channel_id = interaction
        .channel
        .as_ref()
        .map(|channel| channel.id.get())
        .or_else(|| interaction.channel_id.map(Id::get));

    let payload = InteractionPayload {
        id: interaction.id.get(),
        application_id: interaction.application_id.get(),
        kind: interaction_kind(interaction.kind),
        token: interaction.token.clone(),
        guild_id: interaction.guild_id.map(Id::get),
        channel_id,
        user_id: interaction.author_id().map(Id::get),
        command_name,
        custom_id,
        data_json,
    };
    payload.validate().map_err(EdgeError::InvalidInteraction)?;
    Ok(payload)
}

// A command data object only carries its own name; subcommands are nested in
// options, which modules receive in `data_json`.
fn command_fields(data: &CommandData) -> (Option<String>, Option<String>, Option<String>) {
    let json = serde_json::to_string(data).ok();
    (Some(data.name.clone()), None, json)
}

fn component_fields(
    custom_id: &str,
    data: &MessageComponentInteractionData,
) -> (Option<String>, Option<String>, Option<String>) {
    let json = serde_json::to_string(data).ok();
    (
        None,
        Some(custom_id.to_owned()).filter(|value| !value.is_empty()),
        json,
    )
}

fn modal_fields(
    custom_id: &str,
    data: &ModalInteractionData,
) -> (Option<String>, Option<String>, Option<String>) {
    let json = serde_json::to_string(data).ok();
    (
        None,
        Some(custom_id.to_owned()).filter(|value| !value.is_empty()),
        json,
    )
}

const fn interaction_kind(kind: InteractionType) -> u8 {
    kind as u8
}

/// Whether an interaction may be handled for its guild.
///
/// v1 is guild-only: when an allowlist is configured, direct messages and
/// guilds outside it are rejected.
#[must_use]
pub fn guild_allowed(allowed: &[u64], guild_id: Option<u64>) -> bool {
    match guild_id {
        Some(guild_id) => allowed.contains(&guild_id),
        None => false,
    }
}

/// Builds the Discord command set from the accepted module manifests.
///
/// Command names are globally unique in the supervisor; the first declaration
/// wins here as a defensive measure.
#[must_use]
pub fn commands_from_registry(modules: &[ModuleManifest]) -> Vec<Command> {
    let mut commands: BTreeMap<String, Command> = BTreeMap::new();
    for module in modules {
        for spec in &module.commands {
            let description = if spec.description.is_empty() {
                format!("{} command", spec.name)
            } else {
                spec.description.clone()
            };
            commands
                .entry(spec.name.clone())
                .or_insert_with(|| Command {
                    application_id: None,
                    contexts: None,
                    default_member_permissions: None,
                    description,
                    description_localizations: None,
                    #[allow(deprecated)]
                    dm_permission: None,
                    guild_id: None,
                    id: None,
                    integration_types: None,
                    kind: CommandType::ChatInput,
                    name: spec.name.clone(),
                    name_localizations: None,
                    nsfw: None,
                    options: spec
                        .options
                        .iter()
                        .map(|option| CommandOption {
                            autocomplete: None,
                            channel_types: None,
                            choices: None,
                            description: option.description.clone(),
                            description_localizations: None,
                            kind: option_kind(option.kind),
                            max_length: None,
                            max_value: None,
                            min_length: None,
                            min_value: None,
                            name: option.name.clone(),
                            name_localizations: None,
                            options: None,
                            file_types: None,
                            required: Some(option.required),
                        })
                        .collect(),
                    version: Id::new(1),
                });
        }
    }
    commands.into_values().collect()
}

const fn option_kind(kind: CommandOptionKind) -> CommandOptionType {
    match kind {
        CommandOptionKind::String => CommandOptionType::String,
        CommandOptionKind::Integer => CommandOptionType::Integer,
        CommandOptionKind::Boolean => CommandOptionType::Boolean,
        CommandOptionKind::User => CommandOptionType::User,
        CommandOptionKind::Channel => CommandOptionType::Channel,
    }
}

/// Sends the supervisor's fallback reply for an interaction.
///
/// When the module already acknowledged the interaction, the original response
/// is edited. Otherwise an immediate ephemeral message is created. Both paths
/// use the interaction token, never the gateway.
///
/// # Errors
///
/// Returns an error for an invalid payload or when Discord rejects the reply.
pub async fn deliver_fallback(
    client: &Client,
    application_id: u64,
    payload: &InteractionPayload,
    module: &str,
    reason: FallbackReason,
    acked: bool,
) -> Result<(), EdgeError> {
    let message = reason.user_message(module);
    deliver_message(client, application_id, payload, &message, true, acked).await
}

/// Sends a short notice for an interaction the edge handles without a module.
///
/// # Errors
///
/// Returns an error for an invalid payload or when Discord rejects the reply.
pub async fn deliver_notice(
    client: &Client,
    application_id: u64,
    payload: &InteractionPayload,
    message: &str,
    ephemeral: bool,
) -> Result<(), EdgeError> {
    deliver_message(client, application_id, payload, message, ephemeral, false).await
}

async fn deliver_message(
    client: &Client,
    application_id: u64,
    payload: &InteractionPayload,
    message: &str,
    ephemeral: bool,
    edit_original: bool,
) -> Result<(), EdgeError> {
    payload.validate().map_err(EdgeError::InvalidInteraction)?;
    let interaction = client.interaction(Id::new(application_id));
    if edit_original {
        interaction
            .update_response(&payload.token)
            .content(Some(message))
            .await?;
        return Ok(());
    }
    let response = InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            content: Some(message.to_owned()),
            flags: ephemeral.then_some(MessageFlags::EPHEMERAL),
            ..InteractionResponseData::default()
        }),
    };
    interaction
        .create_response(Id::new(payload.id), &payload.token, &response)
        .await?;
    Ok(())
}

/// Registers the command set for the configured guild, or globally when no
/// guild is pinned. Guild registration propagates instantly.
///
/// # Errors
///
/// Returns the Discord error when registration fails.
pub async fn register_commands(
    client: &Client,
    application_id: u64,
    guild_id: Option<u64>,
    commands: &[Command],
) -> Result<(), EdgeError> {
    let interaction = client.interaction(Id::new(application_id));
    if let Some(guild_id) = guild_id {
        interaction
            .set_guild_commands(Id::new(guild_id), commands)
            .await?;
    } else {
        interaction.set_global_commands(commands).await?;
    }
    Ok(())
}
