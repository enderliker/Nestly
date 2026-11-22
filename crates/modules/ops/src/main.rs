#![forbid(unsafe_code)]
//! Operations module: status inspection and admin-authorized restarts.
//!
//! Every handler answers the interaction, including on failure, so a user is
//! never left waiting. Destructive operations require an allowlisted user.

use std::fmt::Write as _;
use std::process::ExitCode;

use rsbot_core::{
    CommandOptionKind, CommandOptionSpec, CommandSpec, ModuleLimits, ModuleManifest,
    ProtocolVersion,
};
use rsbot_module_sdk::{CommandContext, ModuleBuilder, Response};
use serde_json::Value;

const MESSAGE_LIMIT: usize = 1_900;

/// Commands are valid Discord names (no dots).
#[must_use]
pub fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: "ops".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: ProtocolVersion::default(),
        commands: vec![
            CommandSpec::new("ops-status", "Show bot uptime and service states"),
            CommandSpec::new("ops-modules", "List supervised modules"),
            CommandSpec::new("ops-restart", "Restart a supervised module").with_options(vec![
                CommandOptionSpec::new("module", "Module id", true, CommandOptionKind::String),
            ]),
        ],
        events: vec!["interaction_create".to_owned()],
        provides: Vec::new(),
        requires: vec!["discord.rest".to_owned(), "supervisor.control".to_owned()],
        exclusive: Vec::new(),
        limits: ModuleLimits::default(),
    }
}

/// Parses `RSBOT_ADMIN_USER_IDS` (comma-separated snowflakes).
#[must_use]
pub fn admin_ids_from(raw: Option<&str>) -> Vec<u64> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| value.parse::<u64>().ok())
        .collect()
}

/// Admin commands deny everyone until an allowlist is configured.
#[must_use]
pub fn is_authorized(user_id: Option<u64>, admins: &[u64]) -> bool {
    match user_id {
        Some(user_id) => admins.contains(&user_id),
        None => false,
    }
}

/// Renders a compact status summary from the supervisor snapshot.
#[must_use]
pub fn summarize_status(status: &Value) -> String {
    let uptime = status["uptime_ms"].as_u64().unwrap_or(0);
    let edge_ready = status["edge_ready"].as_bool().unwrap_or(false);
    let services = status["services"].as_array().cloned().unwrap_or_default();
    let ready = services
        .iter()
        .filter(|service| service["state"] == "ready")
        .count();
    let mut lines = vec![format!(
        "rsbot up {}s | gateway ready: {} | {ready}/{} services ready",
        uptime / 1000,
        if edge_ready { "yes" } else { "no" },
        services.len()
    )];
    for service in &services {
        lines.push(format_service(service));
    }
    truncate(&lines.join("\n"))
}

/// Renders the module list from the supervisor snapshot.
#[must_use]
pub fn summarize_modules(status: &Value) -> String {
    let services = status["services"].as_array().cloned().unwrap_or_default();
    if services.is_empty() {
        return "No supervised services.".to_owned();
    }
    let lines: Vec<String> = services.iter().map(format_service).collect();
    truncate(&lines.join("\n"))
}

fn format_service(service: &Value) -> String {
    let id = service["id"].as_str().unwrap_or("?");
    let role = service["role"].as_str().unwrap_or("?");
    let state = service["state"].as_str().unwrap_or("?");
    let attempts = service["attempts"].as_u64().unwrap_or(0);
    let uptime = service["uptime_secs"].as_u64().unwrap_or(0);
    let mut line = format!("[{id}] {role} {state}");
    if state == "ready" {
        let _ = write!(line, " for {uptime}s");
    }
    if attempts > 0 {
        let _ = write!(line, " (attempt {attempts})");
    }
    if let Some(reason) = service["disabled_reason"].as_str() {
        let _ = write!(line, ": {reason}");
    }
    line
}

fn truncate(message: &str) -> String {
    if message.chars().count() <= MESSAGE_LIMIT {
        return message.to_owned();
    }
    message.chars().take(MESSAGE_LIMIT).collect()
}

async fn status_reply(context: &CommandContext, modules_only: bool) -> Result<Response, String> {
    let raw = context
        .supervisor_status()
        .await
        .map_err(|source| format!("cannot read supervisor status: {source}"))?;
    let status: Value =
        serde_json::from_str(&raw).map_err(|source| format!("invalid status JSON: {source}"))?;
    let message = if modules_only {
        summarize_modules(&status)
    } else {
        summarize_status(&status)
    };
    Ok(Response::ephemeral(message))
}

async fn restart_reply(context: &CommandContext) -> Result<Response, String> {
    let admins = admin_ids_from(std::env::var("RSBOT_ADMIN_USER_IDS").ok().as_deref());
    if !is_authorized(context.interaction.user_id, &admins) {
        return Ok(Response::ephemeral(
            "You are not authorized to restart modules.",
        ));
    }
    let Some(module) = context.option_string("module") else {
        return Ok(Response::ephemeral("Usage: /ops-restart module:<id>"));
    };
    match context.restart_module(&module).await {
        Ok(reply) if reply.ok => Ok(Response::ephemeral(format!("Restarting `{module}`."))),
        Ok(reply) => Ok(Response::ephemeral(format!(
            "Cannot restart `{module}`: {}",
            truncate(&reply.message)
        ))),
        Err(source) => Ok(Response::ephemeral(format!(
            "Cannot restart `{module}`: {source}"
        ))),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let result = ModuleBuilder::new(manifest())
        .command("ops-status", |context| async move {
            Ok(status_reply(&context, false)
                .await
                .unwrap_or_else(Response::ephemeral))
        })
        .command("ops-modules", |context| async move {
            Ok(status_reply(&context, true)
                .await
                .unwrap_or_else(Response::ephemeral))
        })
        .command("ops-restart", |context| async move {
            Ok(restart_reply(&context)
                .await
                .unwrap_or_else(Response::ephemeral))
        })
        .run()
        .await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            eprintln!("rsbot-mod-ops failed: {source}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{admin_ids_from, is_authorized, manifest, summarize_modules, summarize_status};
    use serde_json::json;

    fn status() -> serde_json::Value {
        json!({
            "uptime_ms": 12_345,
            "edge_ready": true,
            "services": [
                {"id": "edge", "role": "edge", "state": "ready", "attempts": 0, "pid": 10, "uptime_secs": 12, "disabled_reason": null},
                {"id": "music", "role": "module", "state": "restarting", "attempts": 2, "pid": null, "uptime_secs": 0, "disabled_reason": null},
                {"id": "ops", "role": "module", "state": "disabled", "attempts": 0, "pid": null, "uptime_secs": 0, "disabled_reason": "protocol mismatch"}
            ]
        })
    }

    #[test]
    fn manifest_is_valid_and_declares_expected_commands() {
        let manifest = manifest();
        manifest.validate().unwrap();
        assert_eq!(manifest.id, "ops");
        let names: Vec<&str> = manifest
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(names, vec!["ops-status", "ops-modules", "ops-restart"]);
        let restart = &manifest.commands[2];
        assert_eq!(restart.options.len(), 1);
        assert!(restart.options[0].required);
    }

    #[test]
    fn authorizes_only_allowlisted_users() {
        assert!(is_authorized(Some(7), &[7, 9]));
        assert!(!is_authorized(Some(8), &[7, 9]));
        assert!(!is_authorized(None, &[7, 9]));
        assert!(!is_authorized(Some(7), &[]));
        assert_eq!(admin_ids_from(Some("1, 2,bad,")), vec![1, 2]);
        assert_eq!(admin_ids_from(None).len(), 0);
    }

    #[test]
    fn summarizes_status_and_modules() {
        let status = status();
        let summary = summarize_status(&status);
        assert!(summary.contains("rsbot up 12s"));
        assert!(summary.contains("gateway ready: yes"));
        assert!(summary.contains("1/3 services ready"));
        assert!(summary.contains("[music] module restarting (attempt 2)"));
        assert!(summary.contains("[ops] module disabled: protocol mismatch"));

        let modules = summarize_modules(&status);
        assert_eq!(modules.lines().count(), 3);
        assert!(modules.starts_with("[edge] edge ready for 12s"));
    }

    #[test]
    fn truncates_long_summaries() {
        let services: Vec<serde_json::Value> = (0..200)
            .map(|index| json!({"id": format!("module-{index}"), "role": "module", "state": "ready", "attempts": 0, "uptime_secs": 1}))
            .collect();
        let summary = summarize_status(&json!({"services": services}));
        assert!(summary.chars().count() <= 1_900);
    }

    #[test]
    fn handles_an_empty_service_list() {
        assert_eq!(
            summarize_modules(&json!({"services": []})),
            "No supervised services."
        );
    }
}
