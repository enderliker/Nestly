#![forbid(unsafe_code)]
#![doc = "Module runtime for rsbot: handshake, heartbeat, command dispatch, and Discord replies."]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rsbot_core::{FallbackReason, ModuleManifest};
use rsbot_ipc::{
    ControlAction, Envelope, IpcConnection, IpcError, IpcSink, IpcStream, client_handshake,
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{error, info, warn};
use twilight_http::Client as HttpClient;
use twilight_model::channel::message::MessageFlags;
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};
use twilight_model::id::Id;

type BoxFuture = Pin<Box<dyn Future<Output = Result<Response, ModuleError>> + Send>>;
type Handler = Arc<dyn Fn(CommandContext) -> BoxFuture + Send + Sync>;

/// Errors surfaced by the module runtime.
#[derive(Debug, Error)]
pub enum ModuleError {
    #[error("ipc error: {0}")]
    Ipc(#[from] IpcError),
    #[error("discord http error: {0}")]
    Http(#[from] twilight_http::Error),
    #[error("missing DISCORD_TOKEN in the module environment")]
    MissingToken,
    #[error("unknown command: {0}")]
    UnknownCommand(String),
    #[error("control request timed out")]
    ControlTimeout,
    #[error("control channel closed")]
    ControlClosed,
    #[error("invalid interaction: {0}")]
    InvalidInteraction(String),
}

/// A reply a command handler can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Send a message immediately.
    Message { content: String, ephemeral: bool },
    /// Acknowledge the interaction and let the handler reply later.
    Defer { ephemeral: bool },
    /// Update the original component message.
    Update { content: String },
    /// Do not respond. The supervisor will treat the interaction as timed out.
    None,
}

impl Response {
    #[must_use]
    pub fn message(content: impl Into<String>) -> Self {
        Self::Message {
            content: content.into(),
            ephemeral: false,
        }
    }

    #[must_use]
    pub fn ephemeral(content: impl Into<String>) -> Self {
        Self::Message {
            content: content.into(),
            ephemeral: true,
        }
    }

    #[must_use]
    pub fn defer_ephemeral() -> Self {
        Self::Defer { ephemeral: true }
    }

    #[must_use]
    pub fn update(content: impl Into<String>) -> Self {
        Self::Update {
            content: content.into(),
        }
    }
}

/// REST helper scoped to the module's own login.
#[derive(Clone)]
pub struct DiscordRest {
    client: Arc<HttpClient>,
}

impl DiscordRest {
    /// Build a client from the module token.
    ///
    /// # Errors
    ///
    /// Returns [`ModuleError::MissingToken`] when `DISCORD_TOKEN` is absent.
    pub fn from_env() -> Result<Self, ModuleError> {
        let token = std::env::var("DISCORD_TOKEN").map_err(|_| ModuleError::MissingToken)?;
        if token.trim().is_empty() {
            return Err(ModuleError::MissingToken);
        }
        Ok(Self {
            client: Arc::new(HttpClient::new(token)),
        })
    }

    /// Send an interaction response.
    ///
    /// # Errors
    ///
    /// Returns an HTTP error when Discord rejects the request.
    pub async fn respond(
        &self,
        interaction: &rsbot_ipc::InteractionPayload,
        response: Response,
    ) -> Result<(), ModuleError> {
        if interaction.token.is_empty() {
            return Err(ModuleError::InvalidInteraction(
                "empty interaction token".to_owned(),
            ));
        }
        let application_id = Id::new(interaction.application_id);
        let interaction_id = Id::new(interaction.id);
        let interaction_client = self.client.interaction(application_id);

        let payload = match response {
            Response::Message { content, ephemeral } => InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    content: Some(content),
                    flags: ephemeral.then_some(MessageFlags::EPHEMERAL),
                    ..InteractionResponseData::default()
                }),
            },
            Response::Defer { ephemeral } => InteractionResponse {
                kind: InteractionResponseType::DeferredChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    flags: ephemeral.then_some(MessageFlags::EPHEMERAL),
                    ..InteractionResponseData::default()
                }),
            },
            Response::Update { content } => InteractionResponse {
                kind: InteractionResponseType::UpdateMessage,
                data: Some(InteractionResponseData {
                    content: Some(content),
                    ..InteractionResponseData::default()
                }),
            },
            Response::None => return Ok(()),
        };

        interaction_client
            .create_response(interaction_id, &interaction.token, &payload)
            .await?;
        Ok(())
    }

    /// Edit the original interaction response.
    ///
    /// # Errors
    ///
    /// Returns an HTTP error when Discord rejects the request.
    pub async fn update_original(
        &self,
        interaction: &rsbot_ipc::InteractionPayload,
        content: &str,
    ) -> Result<(), ModuleError> {
        self.client
            .interaction(Id::new(interaction.application_id))
            .update_response(&interaction.token)
            .content(Some(content))
            .await?;
        Ok(())
    }
}

/// Per-command context passed to handlers.
#[derive(Clone)]
pub struct CommandContext {
    pub module_id: String,
    pub interaction: rsbot_ipc::InteractionPayload,
    pub rest: DiscordRest,
    control: ControlClient,
}

impl CommandContext {
    /// Raw interaction data as JSON.
    #[must_use]
    pub fn data(&self) -> Option<Value> {
        self.interaction
            .data_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
    }

    /// Read a top-level string option by name.
    #[must_use]
    pub fn option_string(&self, name: &str) -> Option<String> {
        self.data()?
            .get("options")?
            .as_array()?
            .iter()
            .find(|option| option.get("name").and_then(Value::as_str) == Some(name))
            .and_then(|option| option.get("value"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    /// Ask the supervisor to restart another module.
    ///
    /// # Errors
    ///
    /// Returns an error when the supervisor does not answer in time.
    pub async fn restart_module(&self, module: &str) -> Result<ControlReply, ModuleError> {
        self.control.restart(module).await
    }

    /// Fetch the supervisor status snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the supervisor does not answer in time.
    pub async fn supervisor_status(&self) -> Result<String, ModuleError> {
        self.control.status().await
    }
}

/// Result of a control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlReply {
    pub ok: bool,
    pub message: String,
}

enum Pending {
    Control(oneshot::Sender<ControlReply>),
    Status(oneshot::Sender<String>),
}

#[derive(Clone)]
struct ControlClient {
    tx: mpsc::Sender<Envelope>,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
}

impl ControlClient {
    fn new(tx: mpsc::Sender<Envelope>) -> Self {
        Self {
            tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    async fn restart(&self, module: &str) -> Result<ControlReply, ModuleError> {
        let request_id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending control map")
            .insert(request_id, Pending::Control(sender));
        self.tx
            .send(Envelope::Control {
                request_id,
                action: ControlAction::Restart {
                    module: module.to_owned(),
                },
            })
            .await
            .map_err(|_| ModuleError::ControlClosed)?;
        timeout(Duration::from_secs(2), receiver)
            .await
            .map_err(|_| ModuleError::ControlTimeout)?
            .map_err(|_| ModuleError::ControlClosed)
    }

    async fn status(&self) -> Result<String, ModuleError> {
        let request_id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending control map")
            .insert(request_id, Pending::Status(sender));
        self.tx
            .send(Envelope::StatusRequest { request_id })
            .await
            .map_err(|_| ModuleError::ControlClosed)?;
        timeout(Duration::from_secs(2), receiver)
            .await
            .map_err(|_| ModuleError::ControlTimeout)?
            .map_err(|_| ModuleError::ControlClosed)
    }

    fn resolve_control(&self, request_id: u64, reply: ControlReply) {
        if let Some(Pending::Control(sender)) = self
            .pending
            .lock()
            .expect("pending control map")
            .remove(&request_id)
        {
            let _ = sender.send(reply);
        }
    }

    fn resolve_status(&self, request_id: u64, json: String) {
        if let Some(Pending::Status(sender)) = self
            .pending
            .lock()
            .expect("pending control map")
            .remove(&request_id)
        {
            let _ = sender.send(json);
        }
    }
}

/// Builder used by module binaries.
pub struct ModuleBuilder {
    manifest: ModuleManifest,
    handlers: HashMap<String, Handler>,
}

impl ModuleBuilder {
    #[must_use]
    pub fn new(manifest: ModuleManifest) -> Self {
        Self {
            manifest,
            handlers: HashMap::new(),
        }
    }

    /// Register a command handler.
    #[must_use]
    pub fn command<F, Fut>(mut self, name: &str, handler: F) -> Self
    where
        F: Fn(CommandContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Response, ModuleError>> + Send + 'static,
    {
        let handler: Handler = Arc::new(move |context| Box::pin(handler(context)));
        self.handlers.insert(name.to_owned(), handler);
        self
    }

    /// Run the module until the supervisor shuts it down or the IPC closes.
    ///
    /// # Errors
    ///
    /// Returns an error when the handshake fails, the token is missing, or the
    /// supervisor disconnects.
    pub async fn run(self) -> Result<(), ModuleError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        init_logging(&self.manifest.id);
        let stream = rsbot_sys::take_inherited_ipc().map_err(IpcError::Io)?;
        let mut connection = IpcConnection::from_std(stream).map_err(IpcError::Io)?;
        let limits = client_handshake(&mut connection, self.manifest.clone()).await?;
        info!(
            module = %self.manifest.id,
            protocol = ?self.manifest.protocol,
            "module ready"
        );

        let (sink, stream) = connection.split();
        let (outbound_tx, outbound_rx) = mpsc::channel::<Envelope>(256);
        let writer = tokio::spawn(writer_task(sink, outbound_rx));
        let control = ControlClient::new(outbound_tx.clone());

        let heartbeat = tokio::spawn(heartbeat_task(
            outbound_tx.clone(),
            limits.max_ipc_msgs_per_sec,
        ));
        let handlers = Arc::new(self.handlers);
        let rest = DiscordRest::from_env()?;
        let module_id = self.manifest.id;

        let result = read_loop(
            stream,
            outbound_tx.clone(),
            handlers,
            rest,
            control,
            module_id,
        )
        .await;

        heartbeat.abort();
        drop(outbound_tx);
        let _ = writer.await;
        result
    }
}

async fn writer_task(mut sink: IpcSink, mut rx: mpsc::Receiver<Envelope>) -> Result<(), IpcError> {
    use futures_util::SinkExt;
    while let Some(envelope) = rx.recv().await {
        let bytes = rsbot_ipc::encode_frame(&envelope)?;
        if let Err(source) = sink.send(bytes).await {
            return Err(IpcError::Io(source));
        }
    }
    Ok(())
}

async fn heartbeat_task(tx: mpsc::Sender<Envelope>, _rate_limit: u32) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut seq = 0_u64;
    loop {
        interval.tick().await;
        seq = seq.wrapping_add(1);
        if tx
            .send(Envelope::Health {
                seq,
                rss_kb: 0,
                queue_depth: 0,
            })
            .await
            .is_err()
        {
            return;
        }
    }
}

async fn read_loop(
    mut stream: IpcStream,
    outbound: mpsc::Sender<Envelope>,
    handlers: Arc<HashMap<String, Handler>>,
    rest: DiscordRest,
    control: ControlClient,
    module_id: String,
) -> Result<(), ModuleError> {
    while let Some(frame) = stream.next().await {
        let frame = frame.map_err(IpcError::Io)?;
        let envelope: Envelope = rsbot_ipc::decode_frame(&frame)?;
        match envelope {
            Envelope::CommandInvoke {
                correlation,
                interaction,
            } => {
                let interaction = *interaction;
                if let Err(reason) = interaction.validate() {
                    warn!(%correlation, %reason, "rejecting invalid interaction");
                    let _ = outbound
                        .send(Envelope::CommandResult {
                            correlation,
                            ok: false,
                            message: Some(reason),
                        })
                        .await;
                    continue;
                }
                let command_name = interaction.command_name.clone().unwrap_or_default();
                let Some(handler) = handlers.get(&command_name).cloned() else {
                    let _ = outbound
                        .send(Envelope::CommandResult {
                            correlation,
                            ok: false,
                            message: Some(format!("unknown command: {command_name}")),
                        })
                        .await;
                    continue;
                };
                let context = CommandContext {
                    module_id: module_id.clone(),
                    interaction: interaction.clone(),
                    rest: rest.clone(),
                    control: control.clone(),
                };
                let outbound = outbound.clone();
                let rest = rest.clone();
                tokio::spawn(async move {
                    let outcome = handler(context).await;
                    match outcome {
                        Ok(Response::None) => {
                            let _ = outbound
                                .send(Envelope::CommandResult {
                                    correlation,
                                    ok: true,
                                    message: None,
                                })
                                .await;
                        }
                        Ok(response) => {
                            let ack_required = !matches!(response, Response::None);
                            match rest.respond(&interaction, response).await {
                                Ok(()) => {
                                    if ack_required {
                                        let _ = outbound
                                            .send(Envelope::CommandAck {
                                                correlation,
                                                acked: true,
                                            })
                                            .await;
                                    }
                                    let _ = outbound
                                        .send(Envelope::CommandResult {
                                            correlation,
                                            ok: true,
                                            message: None,
                                        })
                                        .await;
                                }
                                Err(source) => {
                                    error!(%correlation, error = %source, "failed to respond");
                                    let _ = outbound
                                        .send(Envelope::CommandResult {
                                            correlation,
                                            ok: false,
                                            message: Some(source.to_string()),
                                        })
                                        .await;
                                }
                            }
                        }
                        Err(source) => {
                            warn!(%correlation, error = %source, "command handler failed");
                            let _ = outbound
                                .send(Envelope::CommandResult {
                                    correlation,
                                    ok: false,
                                    message: Some(source.to_string()),
                                })
                                .await;
                        }
                    }
                });
            }
            Envelope::ControlResult {
                request_id,
                ok,
                message,
            } => control.resolve_control(request_id, ControlReply { ok, message }),
            Envelope::StatusResult { request_id, json } => {
                control.resolve_status(request_id, json);
            }
            Envelope::Ping => {
                let _ = outbound.send(Envelope::Pong).await;
            }
            Envelope::Shutdown { .. } => {
                info!(module = %module_id, "shutdown requested");
                return Ok(());
            }
            Envelope::Registry { .. }
            | Envelope::ModuleStatus { .. }
            | Envelope::Pong
            | Envelope::Health { .. }
            | Envelope::Ready { .. } => {}
            other => {
                warn!(kind = other.kind(), "unexpected envelope in module runtime");
            }
        }
    }
    Err(IpcError::Closed.into())
}

fn init_logging(module: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stdout)
        .try_init();
    info!(module = %module, "starting module runtime");
}

/// Convert a fallback reason into a user-facing message for module authors.
#[must_use]
pub fn fallback_message(reason: FallbackReason, module: &str) -> String {
    reason.user_message(module)
}

/// Re-export protocol types for modules that want structured logs.
pub use rsbot_ipc::{LogLevel as ModuleLogLevel, Role as ModuleRole};
