use rsbot_core::{FallbackReason, ModuleLimits, ModuleManifest, ModuleState, ProtocolVersion};
use serde::{Deserialize, Serialize};

/// Who is on the other end of an IPC connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Edge,
    Module,
}

/// Reason a handshake was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    Protocol,
    Nonce,
    Manifest,
    Unauthorized,
    Busy,
    Internal,
}

/// A compact Discord interaction forwarded to a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionPayload {
    pub id: u64,
    pub application_id: u64,
    pub kind: u8,
    pub token: String,
    pub guild_id: Option<u64>,
    pub channel_id: Option<u64>,
    pub user_id: Option<u64>,
    pub command_name: Option<String>,
    pub custom_id: Option<String>,
    pub data_json: Option<String>,
}

impl InteractionPayload {
    /// Validate size and shape before routing an untrusted payload.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error when a field exceeds its limit.
    pub fn validate(&self) -> Result<(), String> {
        if self.token.is_empty() || self.token.len() > 512 {
            return Err("interaction token length is invalid".to_owned());
        }
        if let Some(name) = &self.command_name
            && (name.is_empty() || name.len() > 64)
        {
            return Err("command name length is invalid".to_owned());
        }
        if let Some(custom_id) = &self.custom_id
            && (custom_id.is_empty() || custom_id.len() > 256)
        {
            return Err("custom id length is invalid".to_owned());
        }
        if let Some(data) = &self.data_json
            && data.len() > 512 * 1024
        {
            return Err("interaction data exceeds 512 KiB".to_owned());
        }
        Ok(())
    }
}

/// A Discord voice state update forwarded from the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceStatePayload {
    pub guild_id: Option<u64>,
    pub channel_id: Option<u64>,
    pub user_id: u64,
    pub session_id: String,
}

/// A Discord voice server update forwarded from the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceServerPayload {
    pub guild_id: u64,
    pub endpoint: Option<String>,
    pub token: String,
}

/// Events the edge forwards to the supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayEvent {
    Interaction(Box<InteractionPayload>),
    VoiceState(Box<VoiceStatePayload>),
    VoiceServer(Box<VoiceServerPayload>),
}

/// A supervisor control request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Restart { module: String },
    Status,
    Shutdown,
}

/// Log levels carried over IPC. Modules normally log to stdout instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// The complete supervisor <-> edge/module protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Envelope {
    Challenge {
        nonce: [u8; 32],
        protocol: ProtocolVersion,
    },
    Hello {
        nonce: [u8; 32],
        manifest: ModuleManifest,
    },
    Welcome {
        session_id: u64,
        role: Role,
        limits: ModuleLimits,
    },
    Reject {
        code: RejectCode,
        reason: String,
    },
    Ready {
        boot_ms: u64,
    },
    Health {
        seq: u64,
        rss_kb: u64,
        queue_depth: u32,
    },
    Event {
        seq: u64,
        event: GatewayEvent,
    },
    CommandInvoke {
        correlation: u64,
        interaction: Box<InteractionPayload>,
    },
    CommandAck {
        correlation: u64,
        acked: bool,
    },
    CommandResult {
        correlation: u64,
        ok: bool,
        message: Option<String>,
    },
    Fallback {
        module: String,
        interaction: Box<InteractionPayload>,
        reason: FallbackReason,
        acked: bool,
    },
    Registry {
        modules: Vec<ModuleManifest>,
    },
    ModuleStatus {
        module: String,
        state: ModuleState,
        attempt: u32,
    },
    Control {
        request_id: u64,
        action: ControlAction,
    },
    ControlResult {
        request_id: u64,
        ok: bool,
        message: String,
    },
    StatusRequest {
        request_id: u64,
    },
    StatusResult {
        request_id: u64,
        json: String,
    },
    Log {
        level: LogLevel,
        target: String,
        message: String,
    },
    Shutdown {
        deadline_ms: u64,
    },
    VoiceGatewaySend {
        guild_id: u64,
        channel_id: Option<u64>,
        self_mute: bool,
        self_deaf: bool,
    },
    EdgeReady,
    Ping,
    Pong,
}

impl Envelope {
    /// Human-readable name for logs.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Challenge { .. } => "challenge",
            Self::Hello { .. } => "hello",
            Self::Welcome { .. } => "welcome",
            Self::Reject { .. } => "reject",
            Self::Ready { .. } => "ready",
            Self::Health { .. } => "health",
            Self::Event { .. } => "event",
            Self::CommandInvoke { .. } => "command_invoke",
            Self::CommandAck { .. } => "command_ack",
            Self::CommandResult { .. } => "command_result",
            Self::Fallback { .. } => "fallback",
            Self::Registry { .. } => "registry",
            Self::ModuleStatus { .. } => "module_status",
            Self::Control { .. } => "control",
            Self::ControlResult { .. } => "control_result",
            Self::StatusRequest { .. } => "status_request",
            Self::StatusResult { .. } => "status_result",
            Self::Log { .. } => "log",
            Self::Shutdown { .. } => "shutdown",
            Self::VoiceGatewaySend { .. } => "voice_gateway_send",
            Self::EdgeReady => "edge_ready",
            Self::Ping => "ping",
            Self::Pong => "pong",
        }
    }
}
