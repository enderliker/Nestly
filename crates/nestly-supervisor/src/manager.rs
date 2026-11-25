use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nestly_core::{Config, FallbackReason, ModuleState, Redactor};
use nestly_ipc::{ControlAction, Envelope, GatewayEvent, InteractionPayload, LogLevel, Role};
use serde_json::{Value, json};
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::alert::{Alert, AlertSink};
use crate::process::{ManagerEvent, ServiceSpec, spawn_service};

const TICK_MS: u64 = 50;
const STATUS_WRITE_MS: u64 = 1_000;
const ACK_GRACE_SECS: u64 = 900;

/// How the supervisor should run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Normal,
    BootProbe,
}

struct ManagedService {
    spec: ServiceSpec,
    /// The last manifest accepted during a handshake. The registry sent to the
    /// edge uses this instead of the configuration manifest so command options
    /// declared by the module reach Discord.
    live_manifest: Option<nestly_core::ModuleManifest>,
    state: ModuleState,
    attempts: u32,
    generation: u64,
    child: Option<Child>,
    tx: Option<mpsc::Sender<Envelope>>,
    pid: Option<u32>,
    ready_at: Option<Instant>,
    last_health: Instant,
    restart_at: Option<Instant>,
    ring: VecDeque<String>,
    episode: u64,
    last_alert: Option<Instant>,
    disabled_reason: Option<String>,
    started_at: Instant,
    max_restarts: u32,
}

struct PendingInteraction {
    module: String,
    interaction: InteractionPayload,
    acked: bool,
    deadline: Instant,
}

/// The supervisor kernel.
///
/// The bool fields are independent lifecycle flags, not a state enum: boot
/// logging, edge readiness, and the two shutdown paths can overlap.
#[allow(clippy::struct_excessive_bools)]
pub struct Supervisor {
    config: Arc<Config>,
    redactor: Arc<Redactor>,
    alert: AlertSink,
    specs: BTreeMap<String, ServiceSpec>,
    services: BTreeMap<String, ManagedService>,
    command_owners: HashMap<String, String>,
    pending: HashMap<u64, PendingInteraction>,
    events_tx: mpsc::Sender<ManagerEvent>,
    events_rx: mpsc::Receiver<ManagerEvent>,
    started: Instant,
    boot_deadline: Instant,
    boot_logged: bool,
    edge_ready: bool,
    next_correlation: u64,
    shutdown: bool,
    stopped: bool,
    status_path: PathBuf,
    last_status_write: Instant,
    host: String,
}

impl Supervisor {
    /// Build a supervisor from validated configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when state directories cannot be created.
    pub fn new(config: Config, redactor: Arc<Redactor>) -> Result<Self, String> {
        let config = Arc::new(config);
        let state_dir = &config.supervisor.state_dir;
        for directory in [
            state_dir.clone(),
            state_dir.join("modules"),
            state_dir.join("alerts"),
            state_dir.join("logs"),
        ] {
            std::fs::create_dir_all(&directory)
                .map_err(|source| format!("cannot create {}: {source}", directory.display()))?;
        }

        let mut specs = BTreeMap::new();
        if config.supervisor.edge.enabled {
            specs.insert(
                "edge".to_owned(),
                ServiceSpec {
                    id: "edge".to_owned(),
                    role: Role::Edge,
                    binary: config.supervisor.edge.binary.clone(),
                    token_env: config.supervisor.edge.token_env.clone(),
                    manifest: config.edge_manifest(),
                },
            );
        }
        for module in &config.modules {
            if module.enabled {
                specs.insert(
                    module.id.clone(),
                    ServiceSpec {
                        id: module.id.clone(),
                        role: Role::Module,
                        binary: module.binary.clone(),
                        token_env: module.token_env.clone(),
                        manifest: module.manifest(),
                    },
                );
            }
        }

        let mut command_owners = HashMap::new();
        for module in &config.modules {
            for command in &module.commands {
                command_owners.insert(command.clone(), module.id.clone());
            }
        }

        let mut services = BTreeMap::new();
        let now = Instant::now();
        for (id, spec) in &specs {
            let max_restarts = config
                .module(id)
                .and_then(|module| module.max_restarts)
                .unwrap_or(config.supervisor.max_restart_attempts);
            services.insert(
                id.clone(),
                ManagedService {
                    spec: spec.clone(),
                    live_manifest: None,
                    state: ModuleState::Stopped,
                    attempts: 0,
                    generation: 0,
                    child: None,
                    tx: None,
                    pid: None,
                    ready_at: None,
                    last_health: now,
                    restart_at: None,
                    ring: VecDeque::with_capacity(config.supervisor.log_ring_lines),
                    episode: 0,
                    last_alert: None,
                    disabled_reason: None,
                    started_at: now,
                    max_restarts,
                },
            );
        }

        let (events_tx, events_rx) = mpsc::channel(2_048);
        let status_path = state_dir.join("status.json");
        let boot_deadline =
            now + Duration::from_millis(config.supervisor.boot_deadline_ms.max(100));

        Ok(Self {
            alert: AlertSink::from_config(&config, redactor.clone()),
            config,
            redactor,
            specs,
            services,
            command_owners,
            pending: HashMap::new(),
            events_tx,
            events_rx,
            started: now,
            boot_deadline,
            boot_logged: false,
            edge_ready: false,
            next_correlation: 1,
            shutdown: false,
            stopped: false,
            status_path,
            last_status_write: now,
            host: std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_owned()),
        })
    }

    /// Run until shutdown (or until boot completion in boot-probe mode).
    ///
    /// # Errors
    ///
    /// Returns an error when the supervisor cannot start its services or a boot
    /// probe deadline is missed.
    pub async fn run(mut self, mode: RunMode) -> Result<(), String> {
        self.spawn_all();
        let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|source| format!("cannot install SIGTERM handler: {source}"))?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|source| format!("cannot install SIGINT handler: {source}"))?;

        loop {
            tokio::select! {
                Some(event) = self.events_rx.recv() => {
                    self.handle_event(event).await;
                }
                _ = tick.tick() => {
                    self.tick().await;
                }
                _ = terminate.recv() => {
                    info!("SIGTERM received; shutting down");
                    self.shutdown().await;
                    return Ok(());
                }
                _ = interrupt.recv() => {
                    info!("SIGINT received; shutting down");
                    self.shutdown().await;
                    return Ok(());
                }
            }

            if self.shutdown {
                self.shutdown().await;
                return Ok(());
            }

            if mode == RunMode::BootProbe {
                if self.boot_failed() {
                    self.finish_boot_probe(false).await;
                    return Err("boot probe failed: a service entered a failed state".to_owned());
                }
                if self.all_ready() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    self.finish_boot_probe(true).await;
                    return Ok(());
                }
                if Instant::now() >= self.boot_deadline {
                    self.finish_boot_probe(false).await;
                    return Err("boot probe deadline exceeded".to_owned());
                }
            }
        }
    }

    fn spawn_all(&mut self) {
        let ids: Vec<String> = self.specs.keys().cloned().collect();
        for id in ids {
            self.spawn_service_by_id(&id, false);
        }
    }

    fn spawn_service_by_id(&mut self, id: &str, is_restart: bool) {
        let Some(spec) = self.specs.get(id).cloned() else {
            return;
        };
        let Some(service) = self.services.get_mut(id) else {
            return;
        };
        service.generation = service.generation.wrapping_add(1);
        let generation = service.generation;
        service.state = ModuleState::Starting;
        service.started_at = Instant::now();
        service.last_health = Instant::now();
        service.restart_at = None;
        if !is_restart {
            service.attempts = 0;
        }

        match spawn_service(&spec, &self.config, generation, self.events_tx.clone()) {
            Ok(spawned) => {
                info!(
                    module = %id,
                    pid = spawned.pid,
                    generation,
                    "spawned service"
                );
                service.child = Some(spawned.child);
                service.pid = Some(spawned.pid);
                service.tx = None;
            }
            Err(reason) => {
                error!(module = %id, error = %reason, "failed to spawn service");
                self.handle_spawn_failure(id, &reason);
            }
        }
    }

    async fn handle_event(&mut self, event: ManagerEvent) {
        match event {
            ManagerEvent::Connected {
                id,
                generation,
                role,
                manifest,
                tx,
            } => {
                let Some(service) = self.services.get_mut(&id) else {
                    return;
                };
                if service.generation != generation {
                    return;
                }
                service.tx = Some(tx);
                if role == Role::Module {
                    service.live_manifest = Some(manifest.clone());
                }
                service.state = ModuleState::Ready;
                service.ready_at = Some(Instant::now());
                service.last_health = Instant::now();
                service.disabled_reason = None;
                info!(
                    module = %id,
                    ?role,
                    version = %manifest.version,
                    "service ready"
                );
                if role == Role::Edge {
                    self.edge_ready = false;
                }
                self.send_status_to_edge(&id);
                self.send_registry_to_edge();
                if !self.boot_logged && self.all_ready() {
                    self.boot_logged = true;
                    let boot_ms = self.started.elapsed().as_millis();
                    info!(boot_ms, "all services ready");
                    if boot_ms > u128::from(self.config.supervisor.boot_deadline_ms) {
                        warn!(
                            boot_ms,
                            budget_ms = self.config.supervisor.boot_deadline_ms,
                            "boot budget exceeded"
                        );
                    }
                }
            }
            ManagerEvent::HandshakeFailed {
                id,
                generation,
                reason,
                fatal,
            } => {
                {
                    let Some(service) = self.services.get(&id) else {
                        return;
                    };
                    if service.generation != generation {
                        return;
                    }
                }
                if fatal {
                    if let Some(service) = self.services.get_mut(&id) {
                        service.state = ModuleState::Disabled;
                        service.disabled_reason = Some(reason.clone());
                    }
                    self.kill_child(&id);
                    error!(
                        module = %id,
                        reason = %reason,
                        "service disabled after incompatible handshake"
                    );
                    self.send_status_to_edge(&id);
                    self.raise_alert(&id, reason, None);
                } else {
                    error!(module = %id, reason = %reason, "service failed during handshake");
                    self.handle_failure(&id, reason).await;
                }
            }
            ManagerEvent::Disconnected {
                id,
                generation,
                reason,
            } => {
                let Some(service) = self.services.get(&id) else {
                    return;
                };
                if service.generation != generation
                    || matches!(service.state, ModuleState::Disabled | ModuleState::Stopped)
                {
                    return;
                }
                self.handle_failure(&id, reason).await;
            }
            ManagerEvent::Inbound {
                id,
                generation,
                envelope,
            } => {
                let Some(service) = self.services.get(&id) else {
                    return;
                };
                if service.generation != generation {
                    return;
                }
                self.handle_inbound(&id, envelope).await;
            }
            ManagerEvent::LogLine {
                id, stream, line, ..
            } => {
                self.handle_log_line(&id, stream.as_str(), &line);
            }
        }
    }

    async fn handle_inbound(&mut self, id: &str, envelope: Envelope) {
        match envelope {
            Envelope::Health { .. } => {
                let recovered = {
                    let Some(service) = self.services.get_mut(id) else {
                        return;
                    };
                    service.last_health = Instant::now();
                    if service.state == ModuleState::Degraded {
                        service.state = ModuleState::Ready;
                        true
                    } else {
                        false
                    }
                };
                if recovered {
                    self.send_status_to_edge(id);
                }
            }
            Envelope::Event { seq, event } => match event {
                GatewayEvent::Interaction(interaction) => {
                    self.route_interaction(*interaction).await;
                }
                voice @ (GatewayEvent::VoiceState(_) | GatewayEvent::VoiceServer(_)) => {
                    self.route_voice_event(seq, voice).await;
                }
            },
            Envelope::VoiceGatewaySend { .. } => {
                self.send_to("edge", envelope).await;
            }
            Envelope::CommandAck { correlation, acked } => {
                if let Some(pending) = self.pending.get_mut(&correlation)
                    && acked
                {
                    pending.acked = true;
                    pending.deadline = Instant::now() + Duration::from_secs(ACK_GRACE_SECS);
                }
            }
            Envelope::CommandResult { correlation, .. } => {
                self.pending.remove(&correlation);
            }
            Envelope::EdgeReady => {
                self.edge_ready = true;
                info!("edge gateway is ready");
            }
            Envelope::Control { request_id, action } => {
                self.handle_control(id, request_id, action).await;
            }
            Envelope::StatusRequest { request_id } => {
                let status = self.status_json().to_string();
                self.send_to(
                    id,
                    Envelope::StatusResult {
                        request_id,
                        json: status,
                    },
                )
                .await;
            }
            Envelope::Ping => {
                self.send_to(id, Envelope::Pong).await;
            }
            Envelope::Log {
                level,
                target,
                message,
            } => {
                let line = format!("{target}: {message}");
                let level = match level {
                    LogLevel::Error => "ERROR",
                    LogLevel::Warn => "WARN",
                    LogLevel::Info => "INFO",
                    LogLevel::Debug => "DEBUG",
                    LogLevel::Trace => "TRACE",
                };
                self.handle_log_line(id, level, &line);
            }
            other => {
                debug!(module = %id, kind = other.kind(), "ignoring envelope");
            }
        }
    }

    async fn handle_control(&mut self, id: &str, request_id: u64, action: ControlAction) {
        match action {
            ControlAction::Restart { module } => {
                if self.services.contains_key(&module) {
                    self.restart_now(&module);
                    self.send_to(
                        id,
                        Envelope::ControlResult {
                            request_id,
                            ok: true,
                            message: format!("restart requested for {module}"),
                        },
                    )
                    .await;
                } else {
                    self.send_to(
                        id,
                        Envelope::ControlResult {
                            request_id,
                            ok: false,
                            message: format!("unknown module: {module}"),
                        },
                    )
                    .await;
                }
            }
            ControlAction::Status => {
                self.send_to(
                    id,
                    Envelope::StatusResult {
                        request_id,
                        json: self.status_json().to_string(),
                    },
                )
                .await;
            }
            ControlAction::Shutdown => {
                self.shutdown = true;
                self.send_to(
                    id,
                    Envelope::ControlResult {
                        request_id,
                        ok: true,
                        message: "shutdown requested".to_owned(),
                    },
                )
                .await;
            }
        }
    }

    fn restart_now(&mut self, id: &str) {
        let Some(service) = self.services.get_mut(id) else {
            return;
        };
        if matches!(service.state, ModuleState::Disabled) {
            warn!(module = %id, "refusing to restart a disabled service");
            return;
        }
        service.attempts = 0;
        service.restart_at = Some(Instant::now());
        service.state = ModuleState::Restarting;
        self.kill_child(id);
        info!(module = %id, "manual restart requested");
    }

    async fn route_interaction(&mut self, interaction: InteractionPayload) {
        if let Err(reason) = interaction.validate() {
            warn!(reason = %reason, "invalid interaction payload");
            return;
        }
        if let Some(guild_id) = interaction.guild_id
            && !self
                .config
                .discord
                .allowed_guild_ids
                .iter()
                .any(|allowed| allowed.get() == guild_id)
        {
            warn!(guild_id, "interaction from a guild that is not allowlisted");
            return;
        }
        let owner = self.owner_for(&interaction);
        let Some(owner) = owner else {
            warn!(
                command = ?interaction.command_name,
                custom_id = ?interaction.custom_id,
                "interaction has no owning module"
            );
            return;
        };
        let Some(service) = self.services.get(&owner) else {
            self.send_fallback(&owner, &interaction, FallbackReason::Failed, false)
                .await;
            return;
        };
        if !service.state.is_routable() {
            let reason = fallback_for_state(service.state);
            self.send_fallback(&owner, &interaction, reason, false)
                .await;
            return;
        }
        let Some(tx) = service.tx.clone() else {
            self.send_fallback(&owner, &interaction, FallbackReason::Restarting, false)
                .await;
            return;
        };

        let correlation = self.next_correlation;
        self.next_correlation = self.next_correlation.wrapping_add(1);
        self.pending.insert(
            correlation,
            PendingInteraction {
                module: owner.clone(),
                interaction: interaction.clone(),
                acked: false,
                deadline: Instant::now() + Duration::from_secs(3),
            },
        );
        if tx
            .send(Envelope::CommandInvoke {
                correlation,
                interaction: Box::new(interaction.clone()),
            })
            .await
            .is_err()
        {
            self.pending.remove(&correlation);
            self.send_fallback(&owner, &interaction, FallbackReason::Crashed, false)
                .await;
        }
    }

    async fn route_voice_event(&mut self, seq: u64, event: GatewayEvent) {
        let voice_module = self.specs.values().find_map(|spec| {
            if spec.manifest.exclusive.iter().any(|c| c == "voice.session") {
                Some(spec.id.clone())
            } else {
                None
            }
        });
        if let Some(target) = voice_module {
            self.send_to(&target, Envelope::Event { seq, event }).await;
        }
    }

    fn owner_for(&self, interaction: &InteractionPayload) -> Option<String> {
        if let Some(command) = &interaction.command_name
            && let Some(owner) = self.command_owners.get(command)
        {
            return Some(owner.clone());
        }
        if let Some(custom_id) = &interaction.custom_id
            && let Some(rest) = custom_id.strip_prefix("mod:")
            && let Some((owner, _)) = rest.split_once(':')
            && self.services.contains_key(owner)
        {
            return Some(owner.to_owned());
        }
        None
    }

    async fn send_fallback(
        &self,
        module: &str,
        interaction: &InteractionPayload,
        reason: FallbackReason,
        acked: bool,
    ) {
        let envelope = Envelope::Fallback {
            module: module.to_owned(),
            interaction: Box::new(interaction.clone()),
            reason,
            acked,
        };
        if let Some(edge) = self.services.get("edge") {
            if let Some(tx) = &edge.tx {
                if tx.send(envelope).await.is_err() {
                    error!("cannot deliver fallback to edge");
                }
            } else {
                warn!(
                    command = ?interaction.command_name,
                    reason = reason.as_str(),
                    "edge is not connected; cannot deliver fallback"
                );
            }
        } else {
            warn!(
                command = ?interaction.command_name,
                reason = reason.as_str(),
                "edge is disabled; cannot deliver fallback"
            );
        }
    }

    fn handle_log_line(&mut self, id: &str, stream: &str, line: &str) {
        let redacted = self.redactor.redact(line);
        if stream == "stdout" {
            info!(module = %id, "{}", redacted);
        } else {
            info!(module = %id, stream = %stream, "{}", redacted);
        }
        if let Some(service) = self.services.get_mut(id) {
            service.ring.push_back(redacted);
            while service.ring.len() > self.config.supervisor.log_ring_lines {
                service.ring.pop_front();
            }
        }
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        self.reap_children().await;
        self.check_health(now).await;
        self.check_restarts(now);
        self.check_pending(now).await;
        self.check_stable(now);
        self.write_status(now);
    }

    async fn reap_children(&mut self) {
        let ids: Vec<String> = self.services.keys().cloned().collect();
        for id in ids {
            let status = {
                let Some(service) = self.services.get_mut(&id) else {
                    continue;
                };
                let Some(child) = service.child.as_mut() else {
                    continue;
                };
                match child.try_wait() {
                    Ok(Some(status)) => Some(status),
                    Ok(None) => None,
                    Err(source) => {
                        error!(module = %id, error = %source, "cannot poll child");
                        None
                    }
                }
            };
            if let Some(status) = status {
                self.handle_failure(&id, format!("process exited: {status}"))
                    .await;
            }
        }
    }

    async fn check_health(&mut self, now: Instant) {
        let interval = Duration::from_millis(self.config.supervisor.health_interval_ms);
        let limit = self.config.supervisor.health_miss_limit;
        let ids: Vec<String> = self.services.keys().cloned().collect();
        for id in ids {
            let action = {
                let Some(service) = self.services.get_mut(&id) else {
                    continue;
                };
                if !matches!(service.state, ModuleState::Ready | ModuleState::Degraded) {
                    continue;
                }
                let elapsed = now.duration_since(service.last_health);
                if elapsed > interval * limit {
                    Some(true)
                } else if elapsed > interval && service.state == ModuleState::Ready {
                    service.state = ModuleState::Degraded;
                    Some(false)
                } else {
                    None
                }
            };
            match action {
                Some(true) => {
                    self.handle_failure(&id, "health check timed out".to_owned())
                        .await;
                }
                Some(false) => {
                    warn!(module = %id, "service degraded: missed heartbeat");
                    self.send_status_to_edge(&id);
                }
                None => {}
            }
        }
    }

    fn check_restarts(&mut self, now: Instant) {
        let ids: Vec<String> = self.services.keys().cloned().collect();
        for id in ids {
            let due = {
                let Some(service) = self.services.get(&id) else {
                    continue;
                };
                service.state == ModuleState::Restarting
                    && service.restart_at.is_some_and(|at| at <= now)
            };
            if due {
                self.spawn_service_by_id(&id, true);
            }
        }
    }

    async fn check_pending(&mut self, now: Instant) {
        let expired: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| !pending.acked && pending.deadline <= now)
            .map(|(correlation, _)| *correlation)
            .collect();
        for correlation in expired {
            if let Some(pending) = self.pending.remove(&correlation) {
                warn!(
                    module = %pending.module,
                    "interaction acknowledgement timed out"
                );
                self.send_fallback(
                    &pending.module,
                    &pending.interaction,
                    FallbackReason::Timeout,
                    false,
                )
                .await;
            }
        }
    }

    fn check_stable(&mut self, now: Instant) {
        let stable = Duration::from_secs(self.config.supervisor.stable_reset_secs);
        for service in self.services.values_mut() {
            if service.state == ModuleState::Ready
                && service.attempts > 0
                && service
                    .ready_at
                    .is_some_and(|ready| now.duration_since(ready) >= stable)
            {
                service.attempts = 0;
            }
        }
    }

    async fn handle_failure(&mut self, id: &str, reason: String) {
        let (attempts, should_alert) = {
            let Some(service) = self.services.get_mut(id) else {
                return;
            };
            if matches!(
                service.state,
                ModuleState::Disabled | ModuleState::Stopped | ModuleState::Failed
            ) {
                return;
            }
            if service.state == ModuleState::Restarting && service.restart_at.is_some() {
                return;
            }
            service.state = ModuleState::Restarting;
            service.attempts = service.attempts.saturating_add(1);
            service.tx = None;
            service.ready_at = None;
            service.restart_at = None;
            (service.attempts, service.attempts > service.max_restarts)
        };
        self.kill_child(id);
        self.fail_pending_for(id, FallbackReason::Crashed).await;
        error!(module = %id, attempts, reason = %reason, "service failed");
        self.send_status_to_edge(id);
        if should_alert {
            if let Some(service) = self.services.get_mut(id) {
                service.state = ModuleState::Failed;
            }
            self.raise_alert(id, reason, None);
            self.send_status_to_edge(id);
        } else {
            self.schedule_restart(id);
        }
    }

    fn handle_spawn_failure(&mut self, id: &str, reason: &str) {
        let (attempts, should_alert) = {
            let Some(service) = self.services.get_mut(id) else {
                return;
            };
            service.attempts = service.attempts.saturating_add(1);
            service.state = ModuleState::Restarting;
            (service.attempts, service.attempts > service.max_restarts)
        };
        self.send_status_to_edge(id);
        if should_alert {
            if let Some(service) = self.services.get_mut(id) {
                service.state = ModuleState::Failed;
            }
            error!(module = %id, attempts, reason = %reason, "service failed to spawn");
            self.raise_alert(id, reason.to_owned(), None);
            self.send_status_to_edge(id);
        } else {
            self.schedule_restart(id);
        }
    }

    fn schedule_restart(&mut self, id: &str) {
        let backoff = &self.config.supervisor.restart_backoff_ms;
        let Some(service) = self.services.get_mut(id) else {
            return;
        };
        let index = usize::try_from(service.attempts.saturating_sub(1))
            .unwrap_or(0)
            .min(backoff.len().saturating_sub(1));
        let delay = backoff.get(index).copied().unwrap_or(500);
        service.restart_at = Some(Instant::now() + Duration::from_millis(delay));
        info!(module = %id, delay_ms = delay, "restart scheduled");
    }

    fn kill_child(&mut self, id: &str) {
        let Some(service) = self.services.get_mut(id) else {
            return;
        };
        service.tx = None;
        if let Some(mut child) = service.child.take() {
            let _ = child.start_kill();
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
        service.pid = None;
    }

    async fn fail_pending_for(&mut self, id: &str, reason: FallbackReason) {
        let correlations: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.module == id)
            .map(|(correlation, _)| *correlation)
            .collect();
        for correlation in correlations {
            if let Some(pending) = self.pending.remove(&correlation) {
                self.send_fallback(&pending.module, &pending.interaction, reason, pending.acked)
                    .await;
            }
        }
    }

    fn raise_alert(&mut self, id: &str, reason: String, exit_status: Option<String>) {
        let sink = self.alert.clone();
        let host = self.host.clone();
        let max_logs = self.config.notifications.max_log_lines;
        let cooldown = Duration::from_secs(self.config.notifications.cooldown_secs);
        let alert = {
            let Some(service) = self.services.get_mut(id) else {
                return;
            };
            if service
                .last_alert
                .is_some_and(|last| last.elapsed() < cooldown)
            {
                return;
            }
            service.last_alert = Some(Instant::now());
            service.episode = service.episode.saturating_add(1);
            let logs: Vec<String> = service
                .ring
                .iter()
                .rev()
                .take(max_logs)
                .rev()
                .cloned()
                .collect();
            Alert {
                module: id.to_owned(),
                version: service.spec.manifest.version.clone(),
                episode: service.episode,
                attempts: service.attempts,
                exit_status,
                uptime_secs: service
                    .ready_at
                    .map_or(0, |ready| ready.elapsed().as_secs()),
                host,
                reason,
                logs,
            }
        };
        tokio::spawn(async move {
            sink.send(&alert).await;
        });
    }

    fn send_status_to_edge(&self, id: &str) {
        let Some(service) = self.services.get(id) else {
            return;
        };
        let envelope = Envelope::ModuleStatus {
            module: id.to_owned(),
            state: service.state,
            attempt: service.attempts,
        };
        if let Some(edge) = self.services.get("edge")
            && let Some(tx) = &edge.tx
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(envelope).await;
            });
        }
    }

    fn send_registry_to_edge(&self) {
        let modules = self
            .services
            .values()
            .filter(|service| service.spec.role == Role::Module)
            .map(|service| {
                service
                    .live_manifest
                    .clone()
                    .unwrap_or_else(|| service.spec.manifest.clone())
            })
            .collect::<Vec<_>>();
        if let Some(edge) = self.services.get("edge")
            && let Some(tx) = &edge.tx
        {
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(Envelope::Registry { modules }).await;
            });
        }
    }

    async fn send_to(&self, id: &str, envelope: Envelope) {
        let Some(service) = self.services.get(id) else {
            return;
        };
        if let Some(tx) = &service.tx
            && tx.send(envelope).await.is_err()
        {
            warn!(module = %id, "cannot send envelope to service");
        }
    }

    fn all_ready(&self) -> bool {
        !self.services.is_empty()
            && self
                .services
                .values()
                .all(|service| service.state == ModuleState::Ready)
    }

    fn boot_failed(&self) -> bool {
        self.services
            .values()
            .any(|service| matches!(service.state, ModuleState::Failed | ModuleState::Disabled))
    }

    async fn finish_boot_probe(&mut self, ok: bool) {
        let boot_ms = self.started.elapsed().as_millis();
        let report = json!({
            "ok": ok,
            "boot_ms": boot_ms,
            "budget_ms": self.config.supervisor.boot_deadline_ms,
            "services": self.service_statuses(),
        });
        println!("BOOT_PROBE {report}");
        self.shutdown().await;
    }

    fn status_json(&self) -> Value {
        json!({
            "uptime_ms": self.started.elapsed().as_millis(),
            "boot_logged": self.boot_logged,
            "edge_ready": self.edge_ready,
            "services": self.service_statuses(),
        })
    }

    fn service_statuses(&self) -> Vec<Value> {
        self.services
            .values()
            .map(|service| {
                json!({
                    "id": service.spec.id.clone(),
                    "role": service.spec.role,
                    "state": service.state,
                    "attempts": service.attempts,
                    "pid": service.pid,
                    "uptime_secs": service.ready_at.map_or(0, |ready| ready.elapsed().as_secs()),
                    "disabled_reason": service.disabled_reason.clone(),
                })
            })
            .collect()
    }

    fn write_status(&mut self, now: Instant) {
        if now.duration_since(self.last_status_write) < Duration::from_millis(STATUS_WRITE_MS) {
            return;
        }
        self.last_status_write = now;
        let path = self.status_path.clone();
        let status = self.status_json();
        let temporary = path.with_extension("json.tmp");
        if let Err(source) = std::fs::write(&temporary, status.to_string()) {
            debug!(error = %source, "cannot write status file");
            return;
        }
        if let Err(source) = std::fs::rename(&temporary, &path) {
            debug!(error = %source, "cannot replace status file");
        }
    }

    async fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.shutdown = true;
        info!("stopping all services");
        for service in self.services.values() {
            if let Some(tx) = &service.tx {
                let _ = tokio::time::timeout(
                    Duration::from_millis(50),
                    tx.send(Envelope::Shutdown { deadline_ms: 1_000 }),
                )
                .await;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        for id in self.services.keys().cloned().collect::<Vec<_>>() {
            self.kill_child(&id);
        }
        for service in self.services.values_mut() {
            service.state = ModuleState::Stopped;
        }
        self.write_status(Instant::now() + Duration::from_secs(1));
    }
}

fn fallback_for_state(state: ModuleState) -> FallbackReason {
    match state {
        ModuleState::Starting => FallbackReason::Starting,
        ModuleState::Restarting => FallbackReason::Restarting,
        ModuleState::Failed => FallbackReason::Failed,
        ModuleState::Disabled => FallbackReason::Disabled,
        ModuleState::Stopped => FallbackReason::Stopped,
        ModuleState::Ready | ModuleState::Degraded => FallbackReason::Crashed,
    }
}

#[cfg(test)]
mod tests {
    use super::fallback_for_state;
    use nestly_core::{FallbackReason, ModuleState};

    #[test]
    fn maps_states_to_fallback_reasons() {
        assert_eq!(
            fallback_for_state(ModuleState::Restarting),
            FallbackReason::Restarting
        );
        assert_eq!(
            fallback_for_state(ModuleState::Failed),
            FallbackReason::Failed
        );
        assert_eq!(
            fallback_for_state(ModuleState::Disabled),
            FallbackReason::Disabled
        );
        assert_eq!(
            fallback_for_state(ModuleState::Ready),
            FallbackReason::Crashed
        );
    }
}
