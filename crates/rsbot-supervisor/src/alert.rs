use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rsbot_core::{AlertSinkKind, Config, Redactor};
use serde::Serialize;
use serde_json::{Value, json};
use tracing::{error, warn};

/// One failure episode delivered to operators.
#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub module: String,
    pub version: String,
    pub episode: u64,
    pub attempts: u32,
    pub exit_status: Option<String>,
    pub uptime_secs: u64,
    pub host: String,
    pub reason: String,
    pub logs: Vec<String>,
}

impl Alert {
    fn payload(&self, redactor: &Redactor) -> Value {
        let logs = self
            .logs
            .iter()
            .map(|line| redactor.redact(line))
            .collect::<Vec<_>>()
            .join("\n");
        let logs = truncate(&logs, 1_900);
        let fields = vec![
            json!({"name": "Module", "value": self.module.clone(), "inline": true}),
            json!({"name": "Version", "value": self.version.clone(), "inline": true}),
            json!({"name": "Episode", "value": self.episode.to_string(), "inline": true}),
            json!({"name": "Attempts", "value": self.attempts.to_string(), "inline": true}),
            json!({
                "name": "Exit",
                "value": self.exit_status.clone().unwrap_or_else(|| "unknown".to_owned()),
                "inline": true
            }),
            json!({"name": "Uptime", "value": format!("{}s", self.uptime_secs), "inline": true}),
            json!({"name": "Host", "value": self.host.clone(), "inline": true}),
            json!({"name": "Reason", "value": truncate(&self.reason, 1_000), "inline": false}),
        ];
        json!({
            "embeds": [{
                "title": format!("[ALERT] Module {} is not working", self.module),
                "description": format!("```\n{logs}\n```"),
                "color": 15_158_332_u32,
                "fields": fields,
            }],
            "allowed_mentions": {"parse": []},
        })
    }

    fn redacted_json(&self, redactor: &Redactor) -> Value {
        json!({
            "module": self.module.clone(),
            "version": self.version.clone(),
            "episode": self.episode,
            "attempts": self.attempts,
            "exit_status": self.exit_status.clone(),
            "uptime_secs": self.uptime_secs,
            "host": self.host.clone(),
            "reason": redactor.redact(&self.reason),
            "logs": self.logs.iter().map(|line| redactor.redact(line)).collect::<Vec<_>>(),
        })
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut output: String = value.chars().take(max_chars).collect();
    output.push_str("...");
    output
}

/// Alert delivery backend.
#[derive(Clone)]
pub enum AlertSink {
    Discord(DiscordSink),
    File(FileSink),
    Null,
}

impl AlertSink {
    /// Build the configured sink, falling back to a file when Discord is not
    /// fully configured. Never fails startup.
    #[must_use]
    pub fn from_config(config: &Config, redactor: Arc<Redactor>) -> Self {
        let fallback = FileSink::new(
            config.supervisor.alert_file.clone().unwrap_or_else(|| {
                config
                    .supervisor
                    .state_dir
                    .join("alerts")
                    .join("alerts.jsonl")
            }),
            redactor.clone(),
        );
        match config.supervisor.alert_sink {
            AlertSinkKind::Null => Self::Null,
            AlertSinkKind::File => Self::File(fallback),
            AlertSinkKind::Discord => {
                let channel_id = config.notifications.channel_id;
                let token = config
                    .notifications
                    .token_env
                    .as_deref()
                    .unwrap_or(&config.discord.token_env);
                let token = std::env::var(token).ok();
                match (channel_id, token) {
                    (Some(channel_id), Some(token)) if !token.trim().is_empty() => {
                        let client = reqwest::Client::builder()
                            .timeout(Duration::from_secs(5))
                            .build()
                            .unwrap_or_default();
                        Self::Discord(DiscordSink {
                            client,
                            token,
                            channel_id: channel_id.get(),
                            redactor,
                            fallback,
                        })
                    }
                    _ => {
                        warn!(
                            "discord alerts are not configured; writing alerts to {}",
                            fallback.path.display()
                        );
                        Self::File(fallback)
                    }
                }
            }
        }
    }

    /// Deliver an alert. Delivery failures are logged and persisted, never
    /// propagated to the supervisor loop.
    pub async fn send(&self, alert: &Alert) {
        match self {
            Self::Null => {}
            Self::File(sink) => {
                if let Err(source) = sink.write(&alert.redacted_json(&sink.redactor)) {
                    error!(error = %source, "cannot write alert file");
                }
            }
            Self::Discord(sink) => sink.send(alert).await,
        }
    }
}

/// File-based alert sink used for tests and as a fallback.
#[derive(Clone)]
pub struct FileSink {
    path: PathBuf,
    redactor: Arc<Redactor>,
}

impl FileSink {
    #[must_use]
    pub fn new(path: PathBuf, redactor: Arc<Redactor>) -> Self {
        Self { path, redactor }
    }

    fn write(&self, payload: &Value) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true).mode(0o600);
        let mut file = options.open(&self.path)?;
        serde_json::to_writer(&mut file, payload)?;
        file.write_all(b"\n")?;
        file.flush()
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Discord webhook/API alert sink.
#[derive(Clone)]
pub struct DiscordSink {
    client: reqwest::Client,
    token: String,
    channel_id: u64,
    redactor: Arc<Redactor>,
    fallback: FileSink,
}

impl DiscordSink {
    async fn send(&self, alert: &Alert) {
        let payload = alert.payload(&self.redactor);
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            self.channel_id
        );
        let mut last_error = None;
        for attempt in 0..3 {
            let response = self
                .client
                .post(&url)
                .header("Authorization", format!("Bot {}", self.token))
                .json(&payload)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => return,
                Ok(response) => {
                    last_error = Some(format!("discord returned {}", response.status()));
                }
                Err(source) => {
                    last_error = Some(source.to_string());
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
            }
        }
        error!(
            error = last_error.as_deref().unwrap_or("unknown"),
            "discord alert delivery failed; persisting locally"
        );
        if let Err(source) = self.fallback.write(&alert.redacted_json(&self.redactor)) {
            error!(error = %source, "cannot persist failed alert");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rsbot_core::Redactor;

    use super::{Alert, AlertSink, FileSink};

    fn alert() -> Alert {
        Alert {
            module: "music".to_owned(),
            version: "0.1.0".to_owned(),
            episode: 1,
            attempts: 3,
            exit_status: Some("exit status: 1".to_owned()),
            uptime_secs: 2,
            host: "test".to_owned(),
            reason: "crashed".to_owned(),
            logs: vec!["token super-secret-123".to_owned()],
        }
    }

    #[tokio::test]
    async fn file_sink_writes_redacted_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("alerts.jsonl");
        let sink = AlertSink::File(FileSink::new(
            path.clone(),
            Arc::new(Redactor::new(["super-secret-123"])),
        ));
        sink.send(&alert()).await;
        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.contains("\"module\":\"music\""));
        assert!(!contents.contains("super-secret-123"));
    }

    #[test]
    fn payload_has_no_mentions() {
        let redactor = Redactor::new(["super-secret-123"]);
        let payload = alert().payload(&redactor);
        assert_eq!(payload["allowed_mentions"]["parse"], serde_json::json!([]));
        assert!(!payload.to_string().contains("super-secret-123"));
    }

    #[test]
    fn file_sink_path_is_visible() {
        let sink = FileSink::new(
            std::path::PathBuf::from("x"),
            Arc::new(Redactor::new(Vec::<String>::new())),
        );
        assert_eq!(sink.path().to_string_lossy(), "x");
    }
}
