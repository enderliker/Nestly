//! Deterministic test module used by supervisor integration tests.
//!
//! It speaks the real IPC protocol but never talks to Discord. Behaviour is
//! selected with `RSBOT_FAKE_*` environment variables so tests can exercise
//! crashes, hangs, protocol mismatches, missing heartbeats, and slow commands.

#![forbid(unsafe_code)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nestly_core::{CommandSpec, ModuleLimits, ModuleManifest, ProtocolVersion};
use nestly_ipc::{
    Envelope, IpcConnection, IpcSink, IpcStream, client_handshake, decode_frame, encode_frame,
};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

fn env_list(name: &str, fallback: &str) -> Vec<String> {
    std::env::var(name)
        .or_else(|_| std::env::var(fallback))
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn env_u64(name: &str, fallback: &str, default: u64) -> u64 {
    std::env::var(name)
        .or_else(|_| std::env::var(fallback))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_str(name: &str, fallback: &str, default: &str) -> String {
    std::env::var(name)
        .or_else(|_| std::env::var(fallback))
        .unwrap_or_else(|_| default.to_owned())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mode = env_str("NESTLY_FAKE_MODE", "RSBOT_FAKE_MODE", "normal");
    let id = env_str("NESTLY_MODULE_ID", "RSBOT_MODULE_ID", "music");

    if mode == "crash-on-start" {
        eprintln!("fake-module-crash-on-start {id}");
        std::process::exit(7);
    }

    let stream = match nestly_sys::take_inherited_ipc() {
        Ok(stream) => stream,
        Err(source) => {
            eprintln!("fake-module-ipc-error {id}: {source}");
            std::process::exit(2);
        }
    };
    let mut connection = match IpcConnection::from_std(stream) {
        Ok(connection) => connection,
        Err(source) => {
            eprintln!("fake-module-ipc-error {id}: {source}");
            std::process::exit(2);
        }
    };

    if mode == "hang-handshake" {
        tokio::time::sleep(Duration::from_secs(30)).await;
        return;
    }

    let mut commands = env_list("NESTLY_FAKE_COMMANDS", "RSBOT_FAKE_COMMANDS");
    if commands.is_empty() {
        commands.push("music-play".to_owned());
    }
    let mut events = env_list("NESTLY_FAKE_EVENTS", "RSBOT_FAKE_EVENTS");
    if events.is_empty() {
        events.push("interaction_create".to_owned());
    }

    let mut manifest = ModuleManifest {
        id: id.clone(),
        version: "0.1.0".to_owned(),
        protocol: ProtocolVersion::default(),
        commands: commands
            .iter()
            .map(|name| CommandSpec::new(name.clone(), "fake command"))
            .collect(),
        events,
        provides: env_list("NESTLY_FAKE_PROVIDES", "RSBOT_FAKE_PROVIDES"),
        requires: env_list("NESTLY_FAKE_REQUIRES", "RSBOT_FAKE_REQUIRES"),
        exclusive: env_list("NESTLY_FAKE_EXCLUSIVE", "RSBOT_FAKE_EXCLUSIVE"),
        limits: ModuleLimits {
            max_rss_mb: env_u64("NESTLY_FAKE_RSS_MB", "RSBOT_FAKE_RSS_MB", 64),
            ..ModuleLimits::default()
        },
    };

    if mode == "bad-manifest" {
        manifest
            .commands
            .push(CommandSpec::new("evil-command", "not approved"));
    }

    if let Err(source) = client_handshake(&mut connection, manifest).await {
        eprintln!("fake-module-handshake-failed {id}: {source}");
        std::process::exit(3);
    }

    println!("fake-module-ready {id}");
    if let Ok(token) = std::env::var("DISCORD_TOKEN")
        && !token.is_empty()
    {
        println!("fake-token-leak {token}");
    }

    if mode == "crash-after-ready" {
        tokio::time::sleep(Duration::from_millis(50)).await;
        eprintln!("fake-module-crash-after-ready {id}");
        std::process::exit(9);
    }

    if mode == "flood" {
        let count = std::env::var("NESTLY_FAKE_FLOOD_LINES")
            .or_else(|_| std::env::var("RSBOT_FAKE_FLOOD_LINES"))
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(10_000);
        let flood_id = id.clone();
        tokio::spawn(async move {
            for line in 0..count {
                println!("flood-{flood_id}-{line}");
                if line % 100 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        });
    }

    let (sink, stream) = connection.split();
    let (outbound_tx, outbound_rx) = mpsc::channel::<Envelope>(64);
    let writer = tokio::spawn(writer_task(sink, outbound_rx));

    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut health_seq = 0_u64;
    let mut tick_logs = 0_u64;

    read_loop(
        stream,
        outbound_tx,
        &mode,
        &id,
        &mut tick,
        &mut health_seq,
        &mut tick_logs,
    )
    .await;

    let _ = writer.await;
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

async fn read_loop(
    mut stream: IpcStream,
    outbound: mpsc::Sender<Envelope>,
    mode: &str,
    id: &str,
    tick: &mut tokio::time::Interval,
    health_seq: &mut u64,
    tick_logs: &mut u64,
) {
    loop {
        tokio::select! {
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(frame)) => {
                        let Ok(envelope) = decode_frame::<Envelope>(&frame) else {
                            eprintln!("fake-module-decode-error {id}");
                            return;
                        };
                        match envelope {
                            Envelope::CommandInvoke { correlation, interaction } => {
                                let command = interaction.command_name.clone().unwrap_or_default();
                                println!("fake-command {id} {command}");
                                match mode {
                                    "ignore-command" => {}
                                    "slow-command" => {
                                        tokio::time::sleep(Duration::from_secs(10)).await;
                                    }
                                    _ => {
                                        let _ = outbound
                                            .send(Envelope::CommandAck { correlation, acked: true })
                                            .await;
                                        let _ = outbound
                                            .send(Envelope::CommandResult {
                                                correlation,
                                                ok: true,
                                                message: None,
                                            })
                                            .await;
                                    }
                                }
                            }
                            Envelope::Shutdown { .. } => {
                                println!("fake-module-shutdown {id}");
                                return;
                            }
                            Envelope::Ping => {
                                let _ = outbound.send(Envelope::Pong).await;
                            }
                            _ => {}
                        }
                    }
                    Some(Err(source)) => {
                        eprintln!("fake-module-ipc-error {id}: {source}");
                        return;
                    }
                    None => return,
                }
            }
            _ = tick.tick() => {
                if mode != "no-heartbeat" {
                    *health_seq = health_seq.wrapping_add(1);
                    if outbound
                        .send(Envelope::Health {
                            seq: *health_seq,
                            rss_kb: 1024,
                            queue_depth: 0,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                if mode == "normal" && *tick_logs < 2 {
                    *tick_logs += 1;
                    println!("fake-module-tick {id} {tick_logs}");
                }
            }
        }
    }
}
