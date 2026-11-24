use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::ManifestError;

/// Major protocol version. A mismatch is always fatal.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Minor protocol version. A newer minor is tolerated.
pub const PROTOCOL_MINOR: u16 = 0;

/// Events a module may subscribe to.
pub const ALLOWED_EVENTS: &[&str] = &[
    "interaction_create",
    "voice_state_update",
    "guild_member_add",
    "guild_member_remove",
];

/// IPC protocol version negotiated during the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
        }
    }
}

impl ProtocolVersion {
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Whether this version can speak to `other`.
    ///
    /// Major versions must match. A peer may be newer in the minor version.
    #[must_use]
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}

/// Discord option types a command may declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOptionKind {
    #[default]
    String,
    Integer,
    Boolean,
    User,
    Channel,
}

impl CommandOptionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::User => "user",
            Self::Channel => "channel",
        }
    }
}

/// One validated option of a slash command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOptionSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub kind: CommandOptionKind,
}

impl CommandOptionSpec {
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        required: bool,
        kind: CommandOptionKind,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required,
            kind,
        }
    }
}

/// A slash command owned by a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub options: Vec<CommandOptionSpec>,
}

impl CommandSpec {
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            options: Vec::new(),
        }
    }

    /// Attach options to this command.
    #[must_use]
    pub fn with_options(mut self, options: Vec<CommandOptionSpec>) -> Self {
        self.options = options;
        self
    }
}

/// Resource limits declared by a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleLimits {
    pub max_rss_mb: u64,
    pub max_ipc_msgs_per_sec: u32,
    pub max_log_lines_per_sec: u32,
}

impl Default for ModuleLimits {
    fn default() -> Self {
        Self {
            max_rss_mb: 192,
            max_ipc_msgs_per_sec: 200,
            max_log_lines_per_sec: 100,
        }
    }
}

/// Everything a module declares about itself during the IPC handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub id: String,
    pub version: String,
    pub protocol: ProtocolVersion,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub exclusive: Vec<String>,
    #[serde(default)]
    pub limits: ModuleLimits,
}

impl ModuleManifest {
    /// Validate the manifest without consulting the rest of the registry.
    ///
    /// # Errors
    ///
    /// Returns a [`ManifestError`] for malformed identifiers, commands,
    /// versions, event subscriptions, or limits.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !module_id_regex().is_match(&self.id) {
            return Err(ManifestError::InvalidId(self.id.clone()));
        }
        if semver::Version::parse(&self.version).is_err() {
            return Err(ManifestError::InvalidVersion(self.version.clone()));
        }
        if self.protocol.major != PROTOCOL_MAJOR {
            return Err(ManifestError::ProtocolMismatch {
                expected: PROTOCOL_MAJOR,
                got: self.protocol.major,
            });
        }
        if self.commands.len() > 100 {
            return Err(ManifestError::TooManyCommands(self.commands.len()));
        }
        let mut commands = HashSet::new();
        for command in &self.commands {
            if !command_name_regex().is_match(&command.name) {
                return Err(ManifestError::InvalidCommandName(command.name.clone()));
            }
            if !commands.insert(command.name.as_str()) {
                return Err(ManifestError::DuplicateCommand(command.name.clone()));
            }
            if command.description.len() > 100 {
                return Err(ManifestError::CommandDescriptionTooLong(
                    command.name.clone(),
                ));
            }
            if command.options.len() > 25 {
                return Err(ManifestError::TooManyOptions {
                    command: command.name.clone(),
                    count: command.options.len(),
                });
            }
            let mut options = HashSet::new();
            let mut saw_optional = false;
            for option in &command.options {
                if !option_name_regex().is_match(&option.name) {
                    return Err(ManifestError::InvalidOptionName {
                        command: command.name.clone(),
                        option: option.name.clone(),
                    });
                }
                if !options.insert(option.name.as_str()) {
                    return Err(ManifestError::DuplicateOption {
                        command: command.name.clone(),
                        option: option.name.clone(),
                    });
                }
                if option.description.len() > 100 {
                    return Err(ManifestError::OptionDescriptionTooLong {
                        command: command.name.clone(),
                        option: option.name.clone(),
                    });
                }
                if !option.required {
                    saw_optional = true;
                } else if saw_optional {
                    return Err(ManifestError::RequiredOptionAfterOptional {
                        command: command.name.clone(),
                        option: option.name.clone(),
                    });
                }
            }
        }
        let mut events = HashSet::new();
        for event in &self.events {
            if !ALLOWED_EVENTS.contains(&event.as_str()) {
                return Err(ManifestError::UnknownEvent(event.clone()));
            }
            if !events.insert(event.as_str()) {
                return Err(ManifestError::DuplicateEvent(event.clone()));
            }
        }
        if self.limits.max_rss_mb == 0 || self.limits.max_rss_mb > 4096 {
            return Err(ManifestError::InvalidLimit("max_rss_mb".to_owned()));
        }
        if self.limits.max_ipc_msgs_per_sec == 0 || self.limits.max_ipc_msgs_per_sec > 100_000 {
            return Err(ManifestError::InvalidLimit(
                "max_ipc_msgs_per_sec".to_owned(),
            ));
        }
        if self.limits.max_log_lines_per_sec == 0 || self.limits.max_log_lines_per_sec > 10_000 {
            return Err(ManifestError::InvalidLimit(
                "max_log_lines_per_sec".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Discord command names: 1-32 lowercase characters, digits, `-`, `_`.
#[must_use]
pub fn is_valid_command_name(name: &str) -> bool {
    command_name_regex().is_match(name)
}

fn module_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9-]{1,31}$").expect("static module id regex"))
}

fn command_name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{0,31}$").expect("static command regex"))
}

fn option_name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z0-9_-]{1,32}$").expect("static option regex"))
}

#[cfg(test)]
mod tests {
    use super::{
        CommandOptionKind, CommandOptionSpec, CommandSpec, ModuleLimits, ModuleManifest,
        PROTOCOL_MAJOR, ProtocolVersion,
    };

    fn manifest() -> ModuleManifest {
        ModuleManifest {
            id: "music".to_owned(),
            version: "0.1.0".to_owned(),
            protocol: ProtocolVersion::default(),
            commands: vec![CommandSpec::new("music-play", "Play a track")],
            events: vec!["interaction_create".to_owned()],
            provides: Vec::new(),
            requires: vec!["discord.rest".to_owned()],
            exclusive: vec!["voice.session".to_owned()],
            limits: ModuleLimits::default(),
        }
    }

    #[test]
    fn accepts_valid_manifest() {
        manifest().validate().unwrap();
    }

    #[test]
    fn rejects_bad_ids_and_commands() {
        let mut bad = manifest();
        bad.id = "Music!".to_owned();
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.commands[0].name = "music.play".to_owned();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn rejects_protocol_mismatch() {
        let mut bad = manifest();
        bad.protocol = ProtocolVersion::new(PROTOCOL_MAJOR + 1, 0);
        assert!(bad.validate().is_err());
    }

    #[test]
    fn rejects_unknown_event() {
        let mut bad = manifest();
        bad.events.push("message_create".to_owned());
        assert!(bad.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_commands() {
        let mut bad = manifest();
        bad.commands.push(bad.commands[0].clone());
        assert!(bad.validate().is_err());
    }

    #[test]
    fn accepts_valid_command_options() {
        let mut with_options = manifest();
        with_options.commands[0].options = vec![
            CommandOptionSpec::new(
                "module",
                "Module to restart",
                true,
                CommandOptionKind::String,
            ),
            CommandOptionSpec::new(
                "force",
                "Skip the grace period",
                false,
                CommandOptionKind::Boolean,
            ),
        ];
        with_options.validate().unwrap();
    }

    #[test]
    fn rejects_invalid_command_options() {
        let mut bad = manifest();
        bad.commands[0].options = vec![CommandOptionSpec::new(
            "Module!",
            "Bad name",
            true,
            CommandOptionKind::String,
        )];
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.commands[0].options = vec![
            CommandOptionSpec::new("module", "Module", true, CommandOptionKind::String),
            CommandOptionSpec::new("module", "Duplicate", false, CommandOptionKind::String),
        ];
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.commands[0].options = vec![
            CommandOptionSpec::new("optional", "First", false, CommandOptionKind::String),
            CommandOptionSpec::new("required", "Second", true, CommandOptionKind::String),
        ];
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.commands[0].options = vec![CommandOptionSpec::new(
            "module",
            "x".repeat(101),
            true,
            CommandOptionKind::String,
        )];
        assert!(bad.validate().is_err());

        let mut bad = manifest();
        bad.commands[0].options = (0..26)
            .map(|index| {
                CommandOptionSpec::new(
                    format!("option-{index}"),
                    "Option",
                    false,
                    CommandOptionKind::String,
                )
            })
            .collect();
        assert!(bad.validate().is_err());
    }
}
