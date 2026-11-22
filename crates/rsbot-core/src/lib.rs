#![forbid(unsafe_code)]
#![doc = "Core configuration, manifest, state, and redaction types for rsbot."]

pub mod config;
pub mod error;
pub mod ids;
pub mod manifest;
pub mod redact;
pub mod state;

pub use config::{
    AlertSinkKind, Config, EdgeConfig, ModuleConfig, NotificationConfig, SupervisorConfig,
};
pub use error::{ConfigError, ManifestError};
pub use ids::Snowflake;
pub use manifest::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ModuleLimits, ModuleManifest,
    PROTOCOL_MAJOR, PROTOCOL_MINOR, ProtocolVersion,
};
pub use redact::Redactor;
pub use state::{FallbackReason, ModuleState};
