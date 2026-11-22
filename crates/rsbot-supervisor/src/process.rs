use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rsbot_core::{Config, ModuleManifest, ProtocolVersion};
use rsbot_ipc::{
    Envelope, IpcConnection, IpcError, IpcSink, IpcStream, Role, decode_frame, encode_frame,
    server_handshake,
};
use rsbot_sys::{ChildHardening, configure_child_command};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec};

const MAX_LOG_LINE: usize = 64 * 1024;

/// A service the supervisor can spawn.
#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub id: String,
    pub role: Role,
    pub binary: PathBuf,
    pub token_env: String,
    pub manifest: ModuleManifest,
}

/// Which child stream a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

impl LogStream {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Events emitted by a supervised child.
#[derive(Debug)]
pub enum ManagerEvent {
    Connected {
        id: String,
        generation: u64,
        role: Role,
        manifest: ModuleManifest,
        tx: mpsc::Sender<Envelope>,
    },
    HandshakeFailed {
        id: String,
        generation: u64,
        reason: String,
        /// A fatal failure means the peer is incompatible and must stay
        /// disabled. Transport failures and timeouts are retried.
        fatal: bool,
    },
    Disconnected {
        id: String,
        generation: u64,
        reason: String,
    },
    Inbound {
        id: String,
        generation: u64,
        envelope: Envelope,
    },
    LogLine {
        id: String,
        generation: u64,
        stream: LogStream,
        line: String,
    },
}

/// A spawned child plus the IPC connection task.
pub struct SpawnedProcess {
    pub child: Child,
    pub pid: u32,
}

/// Spawn one child process and its IPC/log tasks.
///
/// # Errors
///
/// Returns a human-readable error when the binary is invalid, the child cannot
/// be spawned, or the hardening hook cannot be installed.
pub fn spawn_service(
    spec: &ServiceSpec,
    config: &Arc<Config>,
    generation: u64,
    events: mpsc::Sender<ManagerEvent>,
) -> Result<SpawnedProcess, String> {
    validate_binary(&spec.binary)?;

    let (supervisor_socket, child_socket) =
        UnixStream::pair().map_err(|source| format!("cannot create socketpair: {source}"))?;
    let child_fd = std::os::fd::AsRawFd::as_raw_fd(&child_socket);

    let module_state = config.supervisor.state_dir.join("modules").join(&spec.id);
    std::fs::create_dir_all(&module_state)
        .map_err(|source| format!("cannot create module state directory: {source}"))?;

    let mut std_command = std::process::Command::new(&spec.binary);
    std_command.env_clear();
    std_command.env("PATH", "/usr/local/bin:/usr/bin:/bin");
    std_command.env(
        "RUST_LOG",
        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()),
    );
    if let Ok(timezone) = std::env::var("TZ") {
        std_command.env("TZ", timezone);
    }
    std_command.env(
        "DISCORD_APPLICATION_ID",
        config.discord.application_id.to_string(),
    );
    if let Some(guild_id) = config.discord.guild_id {
        std_command.env("DISCORD_GUILD_ID", guild_id.to_string());
    }
    let allowed_guilds = config
        .discord
        .allowed_guild_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    std_command.env("DISCORD_ALLOWED_GUILD_IDS", allowed_guilds);
    let admin_users = config
        .discord
        .admin_user_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    std_command.env("RSBOT_ADMIN_USER_IDS", admin_users);
    std_command.env("RSBOT_MODULE_ID", &spec.id);
    std_command.env("RSBOT_STATE_DIR", &module_state);
    std_command.env("HOME", &module_state);
    if let Ok(token) = std::env::var(&spec.token_env) {
        std_command.env("DISCORD_TOKEN", token);
    }
    std_command.stdin(Stdio::null());
    std_command.stdout(Stdio::piped());
    std_command.stderr(Stdio::piped());
    if let Ok(current_dir) = std::env::current_dir() {
        std_command.current_dir(current_dir);
    }

    configure_child_command(&mut std_command, child_fd, ChildHardening::default())
        .map_err(|source| format!("cannot configure child hardening: {source}"))?;

    let mut command = Command::from(std_command);
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|source| format!("cannot spawn {}: {source}", spec.binary.display()))?;
    drop(child_socket);

    let pid = child.id().unwrap_or(0);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let nonce: [u8; 32] = rand::random();
    let validation = validation_for(spec, config.clone());
    let connection_events = events.clone();
    let connection_id = spec.id.clone();
    let role = spec.role;
    let limits = spec.manifest.limits;
    let timeout_ms = config
        .supervisor
        .handshake_timeout_ms
        .saturating_add(config.supervisor.ready_timeout_ms)
        .max(100);

    tokio::spawn(async move {
        connection_task(
            supervisor_socket,
            ConnectionContext {
                id: connection_id,
                generation,
                role,
                nonce,
                limits,
                timeout_ms,
                validation,
            },
            connection_events,
        )
        .await;
    });

    if let Some(stdout) = stdout {
        spawn_log_task(
            spec.id.clone(),
            generation,
            LogStream::Stdout,
            stdout,
            events.clone(),
        );
    }
    if let Some(stderr) = stderr {
        spawn_log_task(
            spec.id.clone(),
            generation,
            LogStream::Stderr,
            stderr,
            events,
        );
    }

    Ok(SpawnedProcess { child, pid })
}

type ManifestValidation = Arc<dyn Fn(&ModuleManifest) -> Result<(), String> + Send + Sync>;

fn validation_for(spec: &ServiceSpec, config: Arc<Config>) -> ManifestValidation {
    let expected_id = spec.id.clone();
    let role = spec.role;
    Arc::new(move |manifest: &ModuleManifest| {
        if manifest.id != expected_id {
            return Err(format!(
                "expected module id {expected_id}, got {}",
                manifest.id
            ));
        }
        match role {
            Role::Module => config
                .validate_live_manifest(manifest)
                .map_err(|source| source.to_string()),
            Role::Edge => validate_edge_manifest(manifest),
        }
    })
}

fn validate_edge_manifest(manifest: &ModuleManifest) -> Result<(), String> {
    if manifest.protocol.major != ProtocolVersion::default().major {
        return Err("edge protocol major mismatch".to_owned());
    }
    if !manifest.commands.is_empty() {
        return Err("edge must not own commands".to_owned());
    }
    if !manifest.exclusive.iter().any(|value| value == "gateway") {
        return Err("edge must claim the gateway capability".to_owned());
    }
    Ok(())
}

struct ConnectionContext {
    id: String,
    generation: u64,
    role: Role,
    nonce: [u8; 32],
    limits: rsbot_core::ModuleLimits,
    timeout_ms: u64,
    validation: ManifestValidation,
}

async fn connection_task(
    socket: UnixStream,
    context: ConnectionContext,
    events: mpsc::Sender<ManagerEvent>,
) {
    let ConnectionContext {
        id,
        generation,
        role,
        nonce,
        limits,
        timeout_ms,
        validation,
    } = context;
    if let Ok(credentials) = rsbot_sys::peer_credentials(&socket)
        && credentials.uid != rsbot_sys::effective_uid()
    {
        let _ = events
            .send(ManagerEvent::HandshakeFailed {
                id,
                generation,
                reason: format!("peer uid {} is not the supervisor uid", credentials.uid),
                fatal: true,
            })
            .await;
        return;
    }

    let mut connection = IpcConnection::new(socket);
    let handshake = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        server_handshake(&mut connection, role, nonce, limits, |manifest| {
            validation(manifest)
        }),
    )
    .await;

    let manifest = match handshake {
        Ok(Ok(manifest)) => manifest,
        Ok(Err(source)) => {
            // A protocol rejection means the peer is incompatible (bad
            // manifest, nonce, or protocol version) and must not be retried.
            // Closed sockets and I/O failures are ordinary crashes.
            let fatal = matches!(source, IpcError::Protocol(_));
            let _ = events
                .send(ManagerEvent::HandshakeFailed {
                    id,
                    generation,
                    reason: source.to_string(),
                    fatal,
                })
                .await;
            return;
        }
        Err(_) => {
            let _ = events
                .send(ManagerEvent::HandshakeFailed {
                    id,
                    generation,
                    reason: "handshake timed out".to_owned(),
                    fatal: false,
                })
                .await;
            return;
        }
    };

    let (sink, stream) = connection.split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Envelope>(256);
    if events
        .send(ManagerEvent::Connected {
            id: id.clone(),
            generation,
            role,
            manifest,
            tx: outbound_tx,
        })
        .await
        .is_err()
    {
        return;
    }

    if let Err(reason) = relay_loop(
        id.clone(),
        generation,
        sink,
        stream,
        &mut outbound_rx,
        &events,
    )
    .await
    {
        let _ = events
            .send(ManagerEvent::Disconnected {
                id,
                generation,
                reason,
            })
            .await;
    }
}

async fn relay_loop(
    id: String,
    generation: u64,
    mut sink: IpcSink,
    mut stream: IpcStream,
    outbound_rx: &mut mpsc::Receiver<Envelope>,
    events: &mpsc::Sender<ManagerEvent>,
) -> Result<(), String> {
    use futures_util::SinkExt;

    loop {
        tokio::select! {
            outbound = outbound_rx.recv() => {
                let Some(envelope) = outbound else {
                    return Ok(());
                };
                let frame = encode_frame(&envelope).map_err(|source| source.to_string())?;
                sink.send(frame).await.map_err(|source| source.to_string())?;
            }
            frame = stream.next() => {
                let Some(frame) = frame else {
                    return Err("ipc closed".to_owned());
                };
                let frame = frame.map_err(|source| source.to_string())?;
                let envelope: Envelope = decode_frame(&frame).map_err(|source| source.to_string())?;
                if events
                    .send(ManagerEvent::Inbound {
                        id: id.clone(),
                        generation,
                        envelope,
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
        }
    }
}

fn spawn_log_task<R>(
    id: String,
    generation: u64,
    stream: LogStream,
    reader: R,
    events: mpsc::Sender<ManagerEvent>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_LOG_LINE));
        while let Some(item) = lines.next().await {
            match item {
                Ok(line) => {
                    if events
                        .send(ManagerEvent::LogLine {
                            id: id.clone(),
                            generation,
                            stream,
                            line,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(source) => {
                    let _ = events
                        .send(ManagerEvent::LogLine {
                            id: id.clone(),
                            generation,
                            stream,
                            line: format!("[log stream error: {source}]"),
                        })
                        .await;
                    return;
                }
            }
        }
    });
}

fn validate_binary(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| format!("cannot stat {}: {source}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("refusing to execute symlink {}", path.display()));
    }
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} is not executable", path.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_binary;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn rejects_missing_and_non_executable_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("module");
        assert!(validate_binary(&path).is_err());
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        assert!(validate_binary(&path).is_err());
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        validate_binary(&path).unwrap();
    }
}
