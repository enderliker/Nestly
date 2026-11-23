//! End-to-end supervisor tests using the real IPC protocol and fake children.

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

const SECRET: &str = "super-secret-token-1234567890";

fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

struct ModuleEntry {
    id: &'static str,
    script: PathBuf,
    commands: &'static [&'static str],
    exclusive: &'static [&'static str],
}

fn module_script(
    dir: &Path,
    name: &str,
    mode: &str,
    commands: &[&str],
    exclusive: &[&str],
) -> PathBuf {
    let commands = commands.join(",");
    let exclusive = exclusive.join(",");
    let body = format!(
        "export RSBOT_FAKE_MODE='{mode}'\n\
         export RSBOT_FAKE_COMMANDS='{commands}'\n\
         export RSBOT_FAKE_EVENTS='interaction_create'\n\
         export RSBOT_FAKE_REQUIRES='discord.rest'\n\
         export RSBOT_FAKE_EXCLUSIVE='{exclusive}'\n\
         export RSBOT_FAKE_RSS_MB=64\n\
         exec '{}'",
        env!("CARGO_BIN_EXE_rsbot-fake-module")
    );
    write_script(dir, name, &body)
}

fn edge_script(dir: &Path, mode: &str, out: Option<&Path>, command: &str) -> PathBuf {
    let out_line = out
        .map(|path| format!("export RSBOT_FAKE_EDGE_OUT='{}'\n", path.display()))
        .unwrap_or_default();
    let body = format!(
        "export RSBOT_FAKE_EDGE_MODE='{mode}'\n\
         export RSBOT_FAKE_EDGE_COMMAND='{command}'\n\
         export RSBOT_FAKE_EDGE_DELAY_MS=600\n\
         {out_line}\
         exec '{}'",
        env!("CARGO_BIN_EXE_rsbot-fake-edge")
    );
    write_script(dir, "fake-edge.sh", &body)
}

fn config(dir: &Path, edge: &Path, modules: &[ModuleEntry], supervisor_extra: &str) -> PathBuf {
    let state = dir.join("state");
    let alerts = dir.join("alerts.jsonl");
    let mut toml = format!(
        r#"
[discord]
application_id = "123456789012345678"
allowed_guild_ids = ["223456789012345678"]
token_env = "TEST_DISCORD_TOKEN"

[notifications]
token_env = "TEST_DISCORD_TOKEN"
cooldown_secs = 0
max_log_lines = 20

[supervisor]
state_dir = "{state}"
alert_sink = "file"
alert_file = "{alerts}"
boot_deadline_ms = 1000
handshake_timeout_ms = 500
ready_timeout_ms = 500
health_interval_ms = 300
health_miss_limit = 3
max_restart_attempts = 3
restart_backoff_ms = [50, 100, 200]
stable_reset_secs = 60
log_ring_lines = 100
max_total_rss_mb = 1024
log_level = "info"
{supervisor_extra}

[supervisor.edge]
binary = "{edge}"
enabled = true
token_env = "TEST_DISCORD_TOKEN"
"#,
        state = state.display(),
        alerts = alerts.display(),
        edge = edge.display(),
    );

    for module in modules {
        let commands = module
            .commands
            .iter()
            .map(|command| format!("\"{command}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let exclusive = module
            .exclusive
            .iter()
            .map(|capability| format!("\"{capability}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            toml,
            r#"
[[modules]]
id = "{id}"
binary = "{binary}"
token_env = "TEST_DISCORD_TOKEN"
commands = [{commands}]
events = ["interaction_create"]
requires = ["discord.rest"]
exclusive = [{exclusive}]
max_rss_mb = 64
"#,
            id = module.id,
            binary = module.script.display(),
        );
    }

    let path = dir.join("rsbot.toml");
    fs::write(&path, toml).unwrap();
    path
}

fn supervisor_command(config: &Path, extra_args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rsbot-supervisor"));
    command
        .arg("--config")
        .arg(config)
        .args(extra_args)
        .env("TEST_DISCORD_TOKEN", SECRET)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn wait_for_exit(mut child: std::process::Child, max_wait: Duration) -> Output {
    let deadline = Instant::now() + max_wait;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait_with_output().unwrap();
        }
        sleep(Duration::from_millis(20));
    }
}

fn run_for(config: &Path, duration: Duration) -> Output {
    let mut child = supervisor_command(config, &[]).spawn().unwrap();
    let pid = child.id();
    sleep(duration);
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait_with_output().unwrap();
        }
        sleep(Duration::from_millis(20));
    }
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Locates a workspace binary next to the test executable, building the module
/// crates first when a narrow `-p rsbot-supervisor` test run skipped them.
fn workspace_binary(name: &str) -> PathBuf {
    // Cargo guarantees this path points at the built fake edge, which lives in
    // the same profile directory as the other workspace binaries.
    let profile_dir = Path::new(env!("CARGO_BIN_EXE_rsbot-fake-edge"))
        .parent()
        .unwrap();
    let candidate = profile_dir.join(name);
    if candidate.exists() {
        return candidate;
    }
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "rsbot-mod-music", "-p", "rsbot-mod-ops"])
        .status()
        .expect("cargo build");
    assert!(status.success(), "cannot build the real module binaries");
    assert!(
        candidate.exists(),
        "expected {} next to the test binary",
        candidate.display()
    );
    candidate
}

#[test]
fn boots_all_services_under_one_second() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "normal",
        &["music-play", "music-skip"],
        &["voice.session"],
    );
    let ops = module_script(dir.path(), "ops.sh", "normal", &["ops-status"], &[]);
    let config = config(
        dir.path(),
        &edge,
        &[
            ModuleEntry {
                id: "music",
                script: music,
                commands: &["music-play", "music-skip"],
                exclusive: &["voice.session"],
            },
            ModuleEntry {
                id: "ops",
                script: ops,
                commands: &["ops-status"],
                exclusive: &[],
            },
        ],
        "",
    );

    let output = wait_for_exit(
        supervisor_command(&config, &["--boot-probe"])
            .spawn()
            .unwrap(),
        Duration::from_secs(10),
    );
    assert!(
        output.status.success(),
        "boot probe failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );

    let line = stdout(&output)
        .lines()
        .find(|line| line.starts_with("BOOT_PROBE "))
        .expect("boot probe report")
        .to_owned();
    let report: Value = serde_json::from_str(&line["BOOT_PROBE ".len()..]).unwrap();
    assert_eq!(report["ok"], Value::Bool(true));
    let boot_ms = report["boot_ms"].as_u64().unwrap();
    assert!(
        boot_ms < 1000,
        "boot took {boot_ms} ms, expected less than 1000 ms"
    );

    let services = report["services"].as_array().unwrap();
    assert_eq!(services.len(), 3);
    for service in services {
        assert_eq!(service["state"], "ready", "{service}");
    }
}

#[test]
fn both_modules_log_simultaneously() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "normal",
        &["music-play", "music-skip"],
        &["voice.session"],
    );
    let ops = module_script(dir.path(), "ops.sh", "normal", &["ops-status"], &[]);
    let config = config(
        dir.path(),
        &edge,
        &[
            ModuleEntry {
                id: "music",
                script: music,
                commands: &["music-play", "music-skip"],
                exclusive: &["voice.session"],
            },
            ModuleEntry {
                id: "ops",
                script: ops,
                commands: &["ops-status"],
                exclusive: &[],
            },
        ],
        "",
    );

    let output = run_for(&config, Duration::from_millis(1_500));
    assert!(output.status.success(), "{}", stderr(&output));
    let logs = stdout(&output);
    assert!(logs.contains("fake-module-ready music"), "{logs}");
    assert!(logs.contains("fake-module-ready ops"), "{logs}");
    assert!(logs.contains("fake-module-tick music"), "{logs}");
    assert!(logs.contains("fake-module-tick ops"), "{logs}");
}

#[test]
fn redacts_secrets_from_child_logs() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "normal",
        &["music-play", "music-skip"],
        &["voice.session"],
    );
    let config = config(
        dir.path(),
        &edge,
        &[ModuleEntry {
            id: "music",
            script: music,
            commands: &["music-play", "music-skip"],
            exclusive: &["voice.session"],
        }],
        "",
    );

    let output = run_for(&config, Duration::from_millis(1_500));
    assert!(output.status.success(), "{}", stderr(&output));
    let logs = stdout(&output);
    assert!(logs.contains("fake-token-leak"), "{logs}");
    assert!(!logs.contains(SECRET), "secret leaked into supervisor logs");
    assert!(logs.contains("***"), "secret was not replaced by ***");
}

#[test]
fn crashing_module_restarts_then_raises_alert() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "crash-after-ready",
        &["music-play", "music-skip"],
        &["voice.session"],
    );
    let config = config(
        dir.path(),
        &edge,
        &[ModuleEntry {
            id: "music",
            script: music,
            commands: &["music-play", "music-skip"],
            exclusive: &["voice.session"],
        }],
        "",
    );

    let output = run_for(&config, Duration::from_millis(3_000));
    assert!(output.status.success(), "{}", stderr(&output));

    let alerts = read_jsonl(&dir.path().join("alerts.jsonl"));
    assert!(
        !alerts.is_empty(),
        "no alert written\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let alert = &alerts[0];
    assert_eq!(alert["module"], "music");
    assert!(alert["attempts"].as_u64().unwrap() >= 3);
    assert!(alert["logs"].as_array().unwrap().len() <= 20);
    let logs = stdout(&output);
    assert!(logs.contains("service failed"), "{logs}");
    assert!(logs.contains("fake-module-crash-after-ready"), "{logs}");
}

#[test]
fn unavailable_module_intercepts_command_with_fallback() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("edge-events.jsonl");
    let edge = edge_script(dir.path(), "probe", Some(&out), "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "crash-on-start",
        &["music-play", "music-skip"],
        &["voice.session"],
    );
    let config = config(
        dir.path(),
        &edge,
        &[ModuleEntry {
            id: "music",
            script: music,
            commands: &["music-play", "music-skip"],
            exclusive: &["voice.session"],
        }],
        "",
    );

    let output = run_for(&config, Duration::from_millis(2_500));
    assert!(output.status.success(), "{}", stderr(&output));

    let events = read_jsonl(&out);
    let fallback = events
        .iter()
        .find(|event| event["kind"] == "fallback")
        .unwrap_or_else(|| {
            panic!(
                "no fallback recorded\nevents: {events:?}\nstdout:\n{}\nstderr:\n{}",
                stdout(&output),
                stderr(&output)
            )
        });
    assert_eq!(fallback["command"], "music-play");
    let reason = fallback["reason"].as_str().unwrap();
    assert!(
        ["starting", "restarting", "failed"].contains(&reason),
        "unexpected fallback reason: {reason}"
    );
}

#[test]
fn incompatible_manifest_disables_module_and_fails_boot() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "bad-manifest",
        &["music-play", "music-skip"],
        &["voice.session"],
    );
    let config = config(
        dir.path(),
        &edge,
        &[ModuleEntry {
            id: "music",
            script: music,
            commands: &["music-play", "music-skip"],
            exclusive: &["voice.session"],
        }],
        "",
    );

    let output = wait_for_exit(
        supervisor_command(&config, &["--boot-probe"])
            .spawn()
            .unwrap(),
        Duration::from_secs(10),
    );
    assert!(!output.status.success());
    let logs = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        logs.contains("handshake failure") || logs.contains("service disabled"),
        "{logs}"
    );

    let alerts = read_jsonl(&dir.path().join("alerts.jsonl"));
    assert_eq!(alerts[0]["module"], "music");
    assert!(
        alerts[0]["reason"]
            .as_str()
            .unwrap()
            .contains("commands that do not match configuration")
    );
}

#[test]
fn duplicate_commands_are_rejected_by_check() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let first = module_script(
        dir.path(),
        "first.sh",
        "normal",
        &["music-play"],
        &["voice.session"],
    );
    let second = module_script(dir.path(), "second.sh", "normal", &["music-play"], &[]);
    let config = config(
        dir.path(),
        &edge,
        &[
            ModuleEntry {
                id: "music",
                script: first,
                commands: &["music-play"],
                exclusive: &["voice.session"],
            },
            ModuleEntry {
                id: "ops",
                script: second,
                commands: &["music-play"],
                exclusive: &[],
            },
        ],
        "",
    );

    let output = wait_for_exit(
        supervisor_command(&config, &["--check"]).spawn().unwrap(),
        Duration::from_secs(5),
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("duplicate command"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn timed_out_interaction_falls_back() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("edge-events.jsonl");
    let edge = edge_script(dir.path(), "probe", Some(&out), "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "ignore-command",
        &["music-play", "music-skip"],
        &["voice.session"],
    );
    let config = config(
        dir.path(),
        &edge,
        &[ModuleEntry {
            id: "music",
            script: music,
            commands: &["music-play", "music-skip"],
            exclusive: &["voice.session"],
        }],
        "",
    );

    let output = run_for(&config, Duration::from_millis(4_500));
    assert!(output.status.success(), "{}", stderr(&output));

    let events = read_jsonl(&out);
    let fallback = events
        .iter()
        .find(|event| event["kind"] == "fallback")
        .unwrap_or_else(|| panic!("no fallback recorded: {events:?}"));
    assert_eq!(fallback["reason"], "timeout");
}

#[test]
fn real_modules_boot_under_one_second() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = workspace_binary("rsbot-mod-music");
    let ops = workspace_binary("rsbot-mod-ops");
    let state = dir.path().join("state");
    let alerts = dir.path().join("alerts.jsonl");
    let toml = format!(
        r#"
[discord]
application_id = "123456789012345678"
allowed_guild_ids = ["223456789012345678"]
token_env = "TEST_DISCORD_TOKEN"

[notifications]
token_env = "TEST_DISCORD_TOKEN"
cooldown_secs = 0
max_log_lines = 20

[supervisor]
state_dir = "{state}"
alert_sink = "file"
alert_file = "{alerts}"
boot_deadline_ms = 1000
handshake_timeout_ms = 500
ready_timeout_ms = 500
health_interval_ms = 300
health_miss_limit = 3
max_restart_attempts = 3
restart_backoff_ms = [50, 100, 200]
stable_reset_secs = 60
log_ring_lines = 100
max_total_rss_mb = 1024
log_level = "info"

[supervisor.edge]
binary = "{edge}"
enabled = true
token_env = "TEST_DISCORD_TOKEN"

[[modules]]
id = "music"
binary = "{music}"
token_env = "TEST_DISCORD_TOKEN"
commands = ["music-play", "music-skip"]
events = ["interaction_create"]
requires = ["discord.rest"]
exclusive = ["voice.session"]
max_rss_mb = 192

[[modules]]
id = "ops"
binary = "{ops}"
token_env = "TEST_DISCORD_TOKEN"
commands = ["ops-status", "ops-modules", "ops-restart"]
events = ["interaction_create"]
requires = ["discord.rest", "supervisor.control"]
max_rss_mb = 192
"#,
        state = state.display(),
        alerts = alerts.display(),
        edge = edge.display(),
        music = music.display(),
        ops = ops.display(),
    );
    let config = dir.path().join("rsbot.toml");
    fs::write(&config, toml).unwrap();

    let output = wait_for_exit(
        supervisor_command(&config, &["--boot-probe"])
            .spawn()
            .unwrap(),
        Duration::from_secs(10),
    );
    assert!(
        output.status.success(),
        "boot probe failed\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let line = stdout(&output)
        .lines()
        .find(|line| line.starts_with("BOOT_PROBE "))
        .expect("boot probe report")
        .to_owned();
    let report: Value = serde_json::from_str(&line["BOOT_PROBE ".len()..]).unwrap();
    assert_eq!(report["ok"], Value::Bool(true));
    let boot_ms = report["boot_ms"].as_u64().unwrap();
    assert!(
        boot_ms < 1000,
        "real modules booted in {boot_ms} ms, expected less than 1000 ms"
    );
    let services = report["services"].as_array().unwrap();
    assert_eq!(services.len(), 3);
    for service in services {
        assert_eq!(service["state"], "ready", "{service}");
    }
}

/// Replaces a scalar key inside the generated [supervisor] section.
fn override_supervisor(config: &Path, overrides: &[(&str, &str)]) {
    let text = fs::read_to_string(config).unwrap();
    let mut lines: Vec<String> = text.lines().map(ToOwned::to_owned).collect();
    for (key, value) in overrides {
        let prefix = format!("{key} = ");
        let replacement = format!("{key} = {value}");
        let mut found = false;
        for line in &mut lines {
            if line.starts_with(&prefix) {
                *line = replacement.clone();
                found = true;
            }
        }
        assert!(
            found,
            "supervisor key {key} is not present in the generated config"
        );
    }
    fs::write(config, format!("{}\n", lines.join("\n"))).unwrap();
}

#[test]
fn missing_heartbeats_restart_then_alert() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "no-heartbeat",
        &["music-play"],
        &["voice.session"],
    );
    let config = config(
        dir.path(),
        &edge,
        &[ModuleEntry {
            id: "music",
            script: music,
            commands: &["music-play"],
            exclusive: &["voice.session"],
        }],
        "",
    );
    override_supervisor(
        &config,
        &[
            ("health_interval_ms", "150"),
            ("health_miss_limit", "2"),
            ("max_restart_attempts", "2"),
            ("restart_backoff_ms", "[50, 50, 50]"),
        ],
    );

    let output = run_for(&config, Duration::from_millis(3_000));
    assert!(output.status.success(), "{}", stderr(&output));
    let logs = stdout(&output);
    assert!(logs.contains("health check timed out"), "{logs}");
    assert!(logs.contains("service failed"), "{logs}");

    let alerts = read_jsonl(&dir.path().join("alerts.jsonl"));
    assert!(!alerts.is_empty(), "no alert for the hung module");
    assert_eq!(alerts[0]["module"], "music");
    assert!(
        alerts[0]["reason"]
            .as_str()
            .unwrap()
            .contains("health check timed out"),
        "{}",
        alerts[0]["reason"]
    );
}

#[test]
fn hanging_handshake_is_retried_then_alerted() {
    let dir = TempDir::new().unwrap();
    let edge = edge_script(dir.path(), "passive", None, "music-play");
    let music = module_script(
        dir.path(),
        "music.sh",
        "hang-handshake",
        &["music-play"],
        &["voice.session"],
    );
    let config = config(
        dir.path(),
        &edge,
        &[ModuleEntry {
            id: "music",
            script: music,
            commands: &["music-play"],
            exclusive: &["voice.session"],
        }],
        "",
    );
    override_supervisor(
        &config,
        &[
            ("handshake_timeout_ms", "100"),
            ("ready_timeout_ms", "100"),
            ("max_restart_attempts", "2"),
            ("restart_backoff_ms", "[50, 50, 50]"),
        ],
    );

    let output = run_for(&config, Duration::from_millis(3_000));
    assert!(output.status.success(), "{}", stderr(&output));
    let logs = stdout(&output);
    assert!(logs.contains("handshake timed out"), "{logs}");

    let alerts = read_jsonl(&dir.path().join("alerts.jsonl"));
    assert!(!alerts.is_empty(), "no alert for the hanging handshake");
    assert_eq!(alerts[0]["module"], "music");
    assert!(
        alerts[0]["reason"]
            .as_str()
            .unwrap()
            .contains("handshake timed out"),
        "{}",
        alerts[0]["reason"]
    );
}
