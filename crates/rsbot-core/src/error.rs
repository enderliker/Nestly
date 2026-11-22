use std::path::PathBuf;

use thiserror::Error;

/// Configuration loading and validation failures.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("duplicate module id: {0}")]
    DuplicateModule(String),
    #[error("duplicate command across modules: {0}")]
    DuplicateCommand(String),
    #[error("exclusive capability claimed by multiple modules: {0}")]
    DuplicateExclusive(String),
    #[error("module {module} requires missing capability: {capability}")]
    MissingCapability { module: String, capability: String },
    #[error("module {module} uses unsupported protocol major {major}")]
    ProtocolMismatch { module: String, major: u16 },
    #[error("module {0} has an invalid command name: {1}")]
    InvalidCommand(String, String),
    #[error("total declared module memory {declared_mb} MB exceeds budget {budget_mb} MB")]
    MemoryBudget { declared_mb: u64, budget_mb: u64 },
    #[error("environment variable {0} is not set")]
    MissingEnv(String),
}

/// Manifest validation failures.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid module id: {0}")]
    InvalidId(String),
    #[error("invalid module version: {0}")]
    InvalidVersion(String),
    #[error("protocol major mismatch: expected {expected}, got {got}")]
    ProtocolMismatch { expected: u16, got: u16 },
    #[error("module declares too many commands: {0}")]
    TooManyCommands(usize),
    #[error("invalid command name: {0}")]
    InvalidCommandName(String),
    #[error("duplicate command: {0}")]
    DuplicateCommand(String),
    #[error("command description too long: {0}")]
    CommandDescriptionTooLong(String),
    #[error("command {command} declares too many options: {count}")]
    TooManyOptions { command: String, count: usize },
    #[error("command {command} has an invalid option name: {option}")]
    InvalidOptionName { command: String, option: String },
    #[error("command {command} has an option description that is too long: {option}")]
    OptionDescriptionTooLong { command: String, option: String },
    #[error("command {command} has a duplicate option: {option}")]
    DuplicateOption { command: String, option: String },
    #[error("command {command} has a required option after an optional one: {option}")]
    RequiredOptionAfterOptional { command: String, option: String },
    #[error("unknown event subscription: {0}")]
    UnknownEvent(String),
    #[error("duplicate event subscription: {0}")]
    DuplicateEvent(String),
    #[error("invalid limit: {0}")]
    InvalidLimit(String),
}
