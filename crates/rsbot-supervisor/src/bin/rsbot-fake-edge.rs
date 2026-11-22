//! Deterministic fake Discord edge used by supervisor integration tests.
//!
//! It performs the real edge handshake, records module status and fallback
//! envelopes, and can inject one synthetic interaction to verify routing.

#![forbid(unsafe_code)]

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use rsbot_core::{ModuleLimits, ModuleManifest, ProtocolVersion};
use rsbot_ipc::{
    Envelope, GatewayEvent, InteractionPayload, IpcConnection, IpcSink, client_handshake,
    decode_frame, encode_frame,
};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn append_json(path: &PathBuf, value: &serde_json::Value) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    else {
        return;
    };
    let _ = serde_json::to_writer(&mut file, value);
    let _ = file.write_all(b"\n");
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mode = std::env::var("RSBOT_FAKE_EDGE_MODE").unwrap_or_else(|_| "passive".to_owned());
    let out = std::env::var("RSBOT_FAKE_EDGE_OUT").ok().map(PathBuf::from);
    let command =
        std::env::var("RSBOT_FAKE_EDGE_COMMAND").unwrap_or_else(|_| "music-play".to_owned());
    let probe_delay = Duration::from_millis(env_u64("RSBOT_FAKE_EDGE_DELAY_MS", 700));

    let stream = match rsbot_sys::take_inherited_ipc() {
        Ok(stream) => stream,
        Err(source) => {
            eprintln!("fake-edge-ipc-error: {source}");
            std::process::exit(2);
        }
    };
    let mut connection = match IpcConnection::from_std(stream) {
        Ok(connection) => connection,
        Err(source) => {
            eprintln!("fake-edge-ipc-error: {source}");
            std::process::exit(2);
        }
    };

    let manifest = ModuleManifest {
        id: "edge".to_owned(),
        version: "0.1.0".to_owned(),
        protocol: ProtocolVersion::default(),
        commands: Vec::new(),
        events: vec!["interaction_create".to_owned()],
        provides: vec!["discord.gateway".to_owned()],
        requires: vec!["discord.rest".to_owned()],
        exclusive: vec!["gateway".to_owned()],
        limits: ModuleLimits::default(),
    };

    if let Err(source) = client_handshake(&mut connection, manifest).await {
        eprintln!("fake-edge-handshake-failed: {source}");
        std::process::exit(3);
    }
    println!("fake-edge-ready");

    let (sink, mut stream) = connection.split();
    let (outbound_tx, outbound_rx) = mpsc::channel::<Envelope>(64);
    let _writer = tokio::spawn(writer_task(sink, outbound_rx));
    let _ = outbound_tx.send(Envelope::EdgeReady).await;

    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut health_seq = 0_u64;
    let mut probe_at: Option<Instant> = None;
    let mut probe_sent = false;
    let mut interaction_seq = 1_u64;

    loop {
        tokio::select! {
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(frame)) => {
                        let Ok(envelope) = decode_frame::<Envelope>(&frame) else {
                            eprintln!("fake-edge-decode-error");
                            return;
                        };
                        match envelope {
                            Envelope::Registry { modules } => {
                                if let Some(path) = &out {
                                    append_json(path, &json!({
                                        "kind": "registry",
                                        "modules": modules.iter().map(|module| module.id.clone()).collect::<Vec<_>>(),
                                    }));
                                }
                                if mode == "probe" && !probe_sent && probe_at.is_none() {
                                    probe_at = Some(Instant::now() + probe_delay);
                                }
                            }
                            Envelope::ModuleStatus { module, state, attempt } => {
                                if let Some(path) = &out {
                                    append_json(path, &json!({
                                        "kind": "status",
                                        "module": module,
                                        "state": state,
                                        "attempt": attempt,
                                    }));
                                }
                            }
                            Envelope::Fallback { module, interaction, reason, acked } => {
                                if let Some(path) = &out {
                                    append_json(path, &json!({
                                        "kind": "fallback",
                                        "module": module,
                                        "command": interaction.command_name,
                                        "reason": reason,
                                        "acked": acked,
                                    }));
                                }
                                println!("fake-edge-fallback {} {} {}", module, reason.as_str(), interaction.command_name.as_deref().unwrap_or(""));
                            }
                            Envelope::Shutdown { .. } => {
                                println!("fake-edge-shutdown");
                                return;
                            }
                            Envelope::Ping => {
                                let _ = outbound_tx.send(Envelope::Pong).await;
                            }
                            _ => {}
                        }
                    }
                    Some(Err(source)) => {
                        eprintln!("fake-edge-ipc-error: {source}");
                        return;
                    }
                    None => return,
                }
            }
            _ = tick.tick() => {
                health_seq = health_seq.wrapping_add(1);
                if outbound_tx
                    .send(Envelope::Health { seq: health_seq, rss_kb: 1024, queue_depth: 0 })
                    .await
                    .is_err()
                {
                    return;
                }
                if let Some(at) = probe_at
                    && Instant::now() >= at
                    && !probe_sent
                {
                    probe_sent = true;
                    let payload = InteractionPayload {
                        id: interaction_seq,
                        application_id: 1,
                        kind: 2,
                        token: format!("fake-interaction-token-{interaction_seq:0>20}"),
                        guild_id: None,
                        channel_id: Some(10),
                        user_id: Some(11),
                        command_name: Some(command.clone()),
                        custom_id: None,
                        data_json: Some("{\"options\":[]}".to_owned()),
                    };
                    interaction_seq = interaction_seq.wrapping_add(1);
                    let _ = outbound_tx
                        .send(Envelope::Event {
                            seq: interaction_seq,
                            event: GatewayEvent::Interaction(Box::new(payload)),
                        })
                        .await;
                    println!("fake-edge-probe {command}");
                }
            }
        }
    }
}

async fn writer_task(mut sink: IpcSink, mut rx: mpsc::Receiver<Envelope>) {
    while let Some(envelope) = rx.recv().await {
        let Ok(frame) = encode_frame(&envelope) else {
            return;
        };
        if sink.send(frame).await.is_err() {
            return;
        }
    }
}
