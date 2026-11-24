use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::ids::Snowflake;
use crate::manifest::{
    CommandSpec, ModuleLimits, ModuleManifest, PROTOCOL_MAJOR, ProtocolVersion,
    is_valid_command_name,
};

/// Capabilities the supervisor always provides.
pub const BUILTIN_CAPABILITIES: &[&str] = &[
    "discord.rest",
    "discord.gateway",
    "supervisor.control",
    "supervisor.status",
];

fn default_true() -> bool {
    true
}

fn default_token_env() -> String {
    "DISCORD_TOKEN".to_owned()
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("state")
}

fn default_edge_binary() -> PathBuf {
    PathBuf::from("nestly-edge")
}

fn default_protocol_major() -> u16 {
    PROTOCOL_MAJOR
}

fn default_max_total_rss_mb() -> u64 {
    4096
}

fn default_log_level() -> String {
    "info".to_owned()
}

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    pub discord: DiscordConfig,
    #[serde(default)]
    pub notifications: NotificationConfig,
    #[serde(default)]
    pub modules: Vec<ModuleConfig>,
}

/// Discord application and guild settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordConfig {
    pub application_id: Snowflake,
    #[serde(default)]
    pub guild_id: Option<Snowflake>,
    #[serde(default)]
    pub allowed_guild_ids: Vec<Snowflake>,
    /// Users allowed to run admin-only module commands. Empty denies everyone.
    #[serde(default)]
    pub admin_user_ids: Vec<Snowflake>,
    #[serde(default = "default_token_env")]
    pub token_env: String,
}

/// Supervisor policy and paths.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorConfig {
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default)]
    pub edge: EdgeConfig,
    #[serde(default = "default_boot_deadline_ms")]
    pub boot_deadline_ms: u64,
    #[serde(default = "default_handshake_timeout_ms")]
    pub handshake_timeout_ms: u64,
    #[serde(default = "default_ready_timeout_ms")]
    pub ready_timeout_ms: u64,
    #[serde(default = "default_health_interval_ms")]
    pub health_interval_ms: u64,
    #[serde(default = "default_health_miss_limit")]
    pub health_miss_limit: u32,
    #[serde(default = "default_max_restart_attempts")]
    pub max_restart_attempts: u32,
    #[serde(default = "default_restart_backoff_ms")]
    pub restart_backoff_ms: Vec<u64>,
    #[serde(default = "default_stable_reset_secs")]
    pub stable_reset_secs: u64,
    #[serde(default = "default_log_ring_lines")]
    pub log_ring_lines: usize,
    #[serde(default = "default_max_total_rss_mb")]
    pub max_total_rss_mb: u64,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub alert_sink: AlertSinkKind,
    #[serde(default)]
    pub alert_file: Option<PathBuf>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            state_dir: default_state_dir(),
            edge: EdgeConfig::default(),
            boot_deadline_ms: default_boot_deadline_ms(),
            handshake_timeout_ms: default_handshake_timeout_ms(),
            ready_timeout_ms: default_ready_timeout_ms(),
            health_interval_ms: default_health_interval_ms(),
            health_miss_limit: default_health_miss_limit(),
            max_restart_attempts: default_max_restart_attempts(),
            restart_backoff_ms: default_restart_backoff_ms(),
            stable_reset_secs: default_stable_reset_secs(),
            log_ring_lines: default_log_ring_lines(),
            max_total_rss_mb: default_max_total_rss_mb(),
            log_level: default_log_level(),
            alert_sink: AlertSinkKind::Discord,
            alert_file: None,
        }
    }
}

fn default_boot_deadline_ms() -> u64 {
    1000
}

fn default_handshake_timeout_ms() -> u64 {
    500
}

fn default_ready_timeout_ms() -> u64 {
    750
}

fn default_health_interval_ms() -> u64 {
    1000
}

fn default_health_miss_limit() -> u32 {
    3
}

fn default_max_restart_attempts() -> u32 {
    3
}

fn default_restart_backoff_ms() -> Vec<u64> {
    vec![100, 250, 500]
}

fn default_stable_reset_secs() -> u64 {
    60
}

fn default_log_ring_lines() -> usize {
    500
}

/// Where alerts are delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSinkKind {
    #[default]
    Discord,
    File,
    Null,
}

/// The edge process settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeConfig {
    #[serde(default = "default_edge_binary")]
    pub binary: PathBuf,
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            binary: default_edge_binary(),
            token_env: default_token_env(),
            enabled: true,
        }
    }
}

/// Alert delivery settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationConfig {
    #[serde(default)]
    pub channel_id: Option<Snowflake>,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default = "default_alert_log_lines")]
    pub max_log_lines: usize,
    #[serde(default = "default_alert_cooldown_secs")]
    pub cooldown_secs: u64,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            channel_id: None,
            token_env: None,
            max_log_lines: default_alert_log_lines(),
            cooldown_secs: default_alert_cooldown_secs(),
        }
    }
}

fn default_alert_log_lines() -> usize {
    40
}

fn default_alert_cooldown_secs() -> u64 {
    300
}

/// A supervised module.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleConfig {
    pub id: String,
    pub binary: PathBuf,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default = "default_protocol_major")]
    pub protocol: u16,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub exclusive: Vec<String>,
    #[serde(default)]
    pub max_rss_mb: u64,
    #[serde(default)]
    pub max_restarts: Option<u32>,
}

impl ModuleConfig {
    #[must_use]
    pub fn manifest(&self) -> ModuleManifest {
        ModuleManifest {
            id: self.id.clone(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: ProtocolVersion::new(self.protocol, 0),
            commands: self
                .commands
                .iter()
                .map(|name| CommandSpec::new(name, format!("{} command", self.id)))
                .collect(),
            events: self.events.clone(),
            provides: self.provides.clone(),
            requires: self.requires.clone(),
            exclusive: self.exclusive.clone(),
            limits: ModuleLimits {
                max_rss_mb: if self.max_rss_mb == 0 {
                    ModuleLimits::default().max_rss_mb
                } else {
                    self.max_rss_mb
                },
                ..ModuleLimits::default()
            },
        }
    }
}

impl Config {
    /// Load and validate a configuration file.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the file cannot be read, parsed, or
    /// validated.
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self = toml::from_str(&raw)?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let bin_dir = std::env::var_os("NESTLY_BIN_DIR")
            .or_else(|| std::env::var_os("RSBOT_BIN_DIR"))
            .map(PathBuf::from);
        config.resolve_paths_in(base, bin_dir.as_deref());
        config.apply_env_overrides();
        config.validate()?;
        Ok(config)
    }

    /// Resolve relative paths against the configuration directory.
    pub fn resolve_paths(&mut self, base: &Path) {
        self.resolve_paths_in(base, None);
    }

    /// Resolve relative paths.
    ///
    /// State, logs, and alerts resolve against the configuration directory.
    /// Relative service binaries resolve against `bin_dir` when it is set
    /// (the launcher points it at the installed release) and against the
    /// configuration directory otherwise.
    pub fn resolve_paths_in(&mut self, base: &Path, bin_dir: Option<&Path>) {
        let bin_base = bin_dir.unwrap_or(base);
        if self.supervisor.state_dir.is_relative() {
            self.supervisor.state_dir = base.join(&self.supervisor.state_dir);
        }
        if self.supervisor.edge.binary.is_relative() {
            self.supervisor.edge.binary = bin_base.join(&self.supervisor.edge.binary);
        }
        if let Some(alert_file) = &self.supervisor.alert_file
            && alert_file.is_relative()
        {
            self.supervisor.alert_file = Some(base.join(alert_file));
        }
        for module in &mut self.modules {
            if module.binary.is_relative() {
                module.binary = bin_base.join(&module.binary);
            }
        }
    }

    /// Apply alert overrides from the environment.
    ///
    /// `NESTLY_ALERT_CHANNEL` (or `RSBOT_ALERT_CHANNEL`) supplies the notification
    /// channel when the file does not, and `NESTLY_OPS_TOKEN` (or `RSBOT_OPS_TOKEN`)
    /// selects the dedicated alert token.
    pub fn apply_env_overrides(&mut self) {
        self.apply_env_overrides_from(|name| std::env::var(name).ok());
    }

    /// Testable variant of [`Config::apply_env_overrides`].
    pub fn apply_env_overrides_from<F>(&mut self, mut get: F)
    where
        F: FnMut(&str) -> Option<String>,
    {
        if self.notifications.channel_id.is_none()
            && let Some(raw) = get("NESTLY_ALERT_CHANNEL").or_else(|| get("RSBOT_ALERT_CHANNEL"))
            && let Ok(channel) = Snowflake::parse(&raw)
        {
            self.notifications.channel_id = Some(channel);
        }
        if self.notifications.token_env.is_none() {
            if get("NESTLY_OPS_TOKEN").is_some() {
                self.notifications.token_env = Some("NESTLY_OPS_TOKEN".to_owned());
            } else if get("RSBOT_OPS_TOKEN").is_some() {
                self.notifications.token_env = Some("RSBOT_OPS_TOKEN".to_owned());
            }
        }
    }

    /// Validate the configuration and cross-module compatibility.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] describing the first incompatibility.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.discord.application_id.get() == 0 {
            return Err(ConfigError::Invalid(
                "discord.application_id must be greater than zero".to_owned(),
            ));
        }
        if self.discord.token_env.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "discord.token_env must not be empty".to_owned(),
            ));
        }
        if self.discord.allowed_guild_ids.is_empty() {
            return Err(ConfigError::Invalid(
                "discord.allowed_guild_ids must contain at least one guild".to_owned(),
            ));
        }
        if let Some(guild_id) = self.discord.guild_id
            && !self.discord.allowed_guild_ids.contains(&guild_id)
        {
            return Err(ConfigError::Invalid(format!(
                "discord.guild_id {guild_id} is not present in allowed_guild_ids"
            )));
        }

        let mut ids = HashSet::new();
        let mut commands: HashMap<&str, &str> = HashMap::new();
        let mut exclusive: HashMap<&str, &str> = HashMap::new();
        let mut provided: HashSet<&str> = BUILTIN_CAPABILITIES.iter().copied().collect();
        let mut declared_mb = 0_u64;

        for module in &self.modules {
            if !ids.insert(module.id.as_str()) {
                return Err(ConfigError::DuplicateModule(module.id.clone()));
            }
            if module.protocol != PROTOCOL_MAJOR {
                return Err(ConfigError::ProtocolMismatch {
                    module: module.id.clone(),
                    major: module.protocol,
                });
            }
            for command in &module.commands {
                if !is_valid_command_name(command) {
                    return Err(ConfigError::InvalidCommand(
                        module.id.clone(),
                        command.clone(),
                    ));
                }
                if let Some(previous) = commands.insert(command, &module.id) {
                    return Err(ConfigError::DuplicateCommand(format!(
                        "{command} owned by both {previous} and {}",
                        module.id
                    )));
                }
            }
            for capability in &module.exclusive {
                if let Some(previous) = exclusive.insert(capability, &module.id) {
                    return Err(ConfigError::DuplicateExclusive(format!(
                        "{capability} claimed by both {previous} and {}",
                        module.id
                    )));
                }
            }
            for capability in &module.provides {
                provided.insert(capability);
            }
            declared_mb += if module.max_rss_mb == 0 {
                ModuleLimits::default().max_rss_mb
            } else {
                module.max_rss_mb
            };
        }

        for module in &self.modules {
            for requirement in &module.requires {
                if !provided.contains(requirement.as_str()) {
                    return Err(ConfigError::MissingCapability {
                        module: module.id.clone(),
                        capability: requirement.clone(),
                    });
                }
            }
        }

        if declared_mb > self.supervisor.max_total_rss_mb {
            return Err(ConfigError::MemoryBudget {
                declared_mb,
                budget_mb: self.supervisor.max_total_rss_mb,
            });
        }

        if self.supervisor.restart_backoff_ms.is_empty() {
            return Err(ConfigError::Invalid(
                "supervisor.restart_backoff_ms must not be empty".to_owned(),
            ));
        }
        if self.supervisor.max_restart_attempts == 0 {
            return Err(ConfigError::Invalid(
                "supervisor.max_restart_attempts must be greater than zero".to_owned(),
            ));
        }
        if self.supervisor.health_interval_ms == 0 || self.supervisor.health_miss_limit == 0 {
            return Err(ConfigError::Invalid(
                "health interval and miss limit must be greater than zero".to_owned(),
            ));
        }
        if self.notifications.max_log_lines == 0 || self.notifications.max_log_lines > 200 {
            return Err(ConfigError::Invalid(
                "notifications.max_log_lines must be between 1 and 200".to_owned(),
            ));
        }
        Ok(())
    }

    /// Validate a live module manifest against the configured allowlist.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the module is unknown, disabled, or
    /// declares commands, events, capabilities, or limits that were not
    /// approved in configuration.
    pub fn validate_live_manifest(&self, manifest: &ModuleManifest) -> Result<(), ConfigError> {
        let module = self
            .modules
            .iter()
            .find(|module| module.id == manifest.id)
            .ok_or_else(|| ConfigError::Invalid(format!("unknown module: {}", manifest.id)))?;
        if !module.enabled {
            return Err(ConfigError::Invalid(format!(
                "module {} is disabled",
                manifest.id
            )));
        }
        if manifest.protocol.major != module.protocol {
            return Err(ConfigError::ProtocolMismatch {
                module: manifest.id.clone(),
                major: manifest.protocol.major,
            });
        }
        let configured_commands: HashSet<&str> =
            module.commands.iter().map(String::as_str).collect();
        let live_commands: HashSet<&str> = manifest
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        if configured_commands != live_commands {
            return Err(ConfigError::Invalid(format!(
                "module {} declared commands that do not match configuration",
                manifest.id
            )));
        }
        let configured_events: HashSet<&str> = module.events.iter().map(String::as_str).collect();
        let live_events: HashSet<&str> = manifest.events.iter().map(String::as_str).collect();
        if configured_events != live_events {
            return Err(ConfigError::Invalid(format!(
                "module {} declared events that do not match configuration",
                manifest.id
            )));
        }
        if module.provides != manifest.provides
            || module.requires != manifest.requires
            || module.exclusive != manifest.exclusive
        {
            return Err(ConfigError::Invalid(format!(
                "module {} declared capabilities that do not match configuration",
                manifest.id
            )));
        }
        let configured_limit = if module.max_rss_mb == 0 {
            ModuleLimits::default().max_rss_mb
        } else {
            module.max_rss_mb
        };
        if manifest.limits.max_rss_mb > configured_limit {
            return Err(ConfigError::Invalid(format!(
                "module {} requested {} MB but is limited to {} MB",
                manifest.id, manifest.limits.max_rss_mb, configured_limit
            )));
        }
        Ok(())
    }

    /// Collect secret values from the environment for redaction.
    #[must_use]
    pub fn redaction_secrets(&self) -> Vec<String> {
        self.redaction_secrets_from(|name| std::env::var(name).ok())
    }

    /// Testable variant of [`Config::redaction_secrets`].
    #[must_use]
    pub fn redaction_secrets_from<F>(&self, mut get: F) -> Vec<String>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut names: Vec<&str> = Vec::new();
        names.push(self.discord.token_env.as_str());
        names.push(self.supervisor.edge.token_env.as_str());
        if let Some(token_env) = &self.notifications.token_env {
            names.push(token_env);
        }
        for module in &self.modules {
            names.push(module.token_env.as_str());
        }
        names.sort_unstable();
        names.dedup();
        names
            .into_iter()
            .filter_map(&mut get)
            .filter(|value| !value.is_empty())
            .collect()
    }

    /// The manifest the edge process must present during the handshake.
    #[must_use]
    pub fn edge_manifest(&self) -> ModuleManifest {
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

    #[must_use]
    pub fn module(&self, id: &str) -> Option<&ModuleConfig> {
        self.modules.iter().find(|module| module.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    const VALID: &str = r#"
[discord]
application_id = "123456789012345678"
guild_id = "223456789012345678"
allowed_guild_ids = ["223456789012345678"]
token_env = "TEST_DISCORD_TOKEN"

[supervisor]
state_dir = "state"
alert_sink = "null"
max_total_rss_mb = 1024

[[modules]]
id = "music"
binary = "nestly-mod-music"
token_env = "TEST_DISCORD_TOKEN"
commands = ["music-play", "music-skip"]
events = ["interaction_create"]
requires = ["discord.rest"]
exclusive = ["voice.session"]
max_rss_mb = 128

[[modules]]
id = "ops"
binary = "nestly-mod-ops"
token_env = "TEST_DISCORD_TOKEN"
commands = ["ops-status"]
events = ["interaction_create"]
"#;

    #[test]
    fn parses_valid_config() {
        let config: Config = toml::from_str(VALID).unwrap();
        config.validate().unwrap();
        assert_eq!(config.modules.len(), 2);
        assert_eq!(config.module("music").unwrap().commands.len(), 2);
    }

    #[test]
    fn rejects_duplicate_commands() {
        let mut config: Config = toml::from_str(VALID).unwrap();
        config.modules[1].commands = vec!["music-play".to_owned()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_exclusive_capability() {
        let mut config: Config = toml::from_str(VALID).unwrap();
        config.modules[1].exclusive = vec!["voice.session".to_owned()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_missing_capability() {
        let mut config: Config = toml::from_str(VALID).unwrap();
        config.modules[0].requires = vec!["voice.provider".to_owned()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_memory_budget_overrun() {
        let mut config: Config = toml::from_str(VALID).unwrap();
        config.supervisor.max_total_rss_mb = 64;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_discord_command_name() {
        let mut config: Config = toml::from_str(VALID).unwrap();
        config.modules[0].commands = vec!["music.play".to_owned()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn collects_unique_redaction_secrets() {
        let config: Config = toml::from_str(VALID).unwrap();
        let secrets = config.redaction_secrets_from(|name| {
            if name == "TEST_DISCORD_TOKEN" {
                Some("secret-value-123".to_owned())
            } else {
                None
            }
        });
        assert_eq!(secrets, vec!["secret-value-123".to_owned()]);
    }

    #[test]
    fn applies_alert_environment_overrides() {
        use crate::Snowflake;

        let mut config: Config = toml::from_str(VALID).unwrap();
        assert!(config.notifications.channel_id.is_none());
        config.apply_env_overrides_from(|name| match name {
            "NESTLY_ALERT_CHANNEL" => Some("987654321098765432".to_owned()),
            "NESTLY_OPS_TOKEN" => Some("ops-token".to_owned()),
            _ => None,
        });
        assert_eq!(
            config.notifications.channel_id,
            Some(Snowflake::new(987_654_321_098_765_432))
        );
        assert_eq!(
            config.notifications.token_env.as_deref(),
            Some("NESTLY_OPS_TOKEN")
        );

        let mut fallback: Config = toml::from_str(VALID).unwrap();
        fallback.apply_env_overrides_from(|name| match name {
            "RSBOT_ALERT_CHANNEL" => Some("987654321098765432".to_owned()),
            "RSBOT_OPS_TOKEN" => Some("ops-token".to_owned()),
            _ => None,
        });
        assert_eq!(
            fallback.notifications.channel_id,
            Some(Snowflake::new(987_654_321_098_765_432))
        );
        assert_eq!(
            fallback.notifications.token_env.as_deref(),
            Some("RSBOT_OPS_TOKEN")
        );

        let mut invalid: Config = toml::from_str(VALID).unwrap();
        invalid.apply_env_overrides_from(|name| match name {
            "NESTLY_ALERT_CHANNEL" => Some("not-a-snowflake".to_owned()),
            _ => None,
        });
        assert!(invalid.notifications.channel_id.is_none());
    }

    #[test]
    fn resolves_service_binaries_against_the_release_directory() {
        use std::path::{Path, PathBuf};

        let mut config: Config = toml::from_str(VALID).unwrap();
        config.resolve_paths_in(
            Path::new("/etc/nestly"),
            Some(Path::new("/opt/nestly/1.0.0")),
        );
        assert_eq!(
            config.supervisor.edge.binary,
            PathBuf::from("/opt/nestly/1.0.0/nestly-edge")
        );
        assert_eq!(
            config.modules[0].binary,
            PathBuf::from("/opt/nestly/1.0.0/nestly-mod-music")
        );
        assert_eq!(
            config.supervisor.state_dir,
            PathBuf::from("/etc/nestly/state")
        );
    }

    #[test]
    fn resolves_service_binaries_beside_the_configuration_by_default() {
        use std::path::{Path, PathBuf};

        let mut config: Config = toml::from_str(VALID).unwrap();
        config.resolve_paths(Path::new("/etc/nestly"));
        assert_eq!(
            config.modules[0].binary,
            PathBuf::from("/etc/nestly/nestly-mod-music")
        );
    }

    #[test]
    fn live_manifest_must_match_configuration() {
        let config: Config = toml::from_str(VALID).unwrap();
        let manifest = config.module("music").unwrap().manifest();
        config.validate_live_manifest(&manifest).unwrap();

        let mut tampered = manifest;
        tampered.commands.push(crate::CommandSpec::new(
            "music-evil",
            "Not approved by configuration",
        ));
        assert!(config.validate_live_manifest(&tampered).is_err());
    }
}
