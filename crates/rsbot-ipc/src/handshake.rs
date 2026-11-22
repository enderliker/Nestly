use rsbot_core::{ModuleLimits, ModuleManifest};

use crate::PROTOCOL;
use crate::codec::{IpcConnection, IpcError};
use crate::message::{Envelope, RejectCode, Role};

/// Constant-time nonce comparison.
fn nonce_matches(expected: &[u8; 32], actual: &[u8; 32]) -> bool {
    expected
        .iter()
        .zip(actual.iter())
        .fold(0_u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

async fn reject<T>(
    conn: &mut IpcConnection,
    code: RejectCode,
    reason: impl Into<String>,
) -> Result<T, IpcError> {
    let reason = reason.into();
    conn.send(&Envelope::Reject {
        code,
        reason: reason.clone(),
    })
    .await?;
    Err(IpcError::Protocol(reason))
}

/// Run the supervisor side of the handshake.
///
/// # Errors
///
/// Returns an error when the peer sends an unexpected message, the nonce or
/// protocol does not match, validation fails, or the transport fails.
pub async fn server_handshake<F>(
    conn: &mut IpcConnection,
    role: Role,
    nonce: [u8; 32],
    limits: ModuleLimits,
    validate: F,
) -> Result<ModuleManifest, IpcError>
where
    F: FnOnce(&ModuleManifest) -> Result<(), String>,
{
    conn.send(&Envelope::Challenge {
        nonce,
        protocol: PROTOCOL,
    })
    .await?;

    let hello = match conn.recv().await? {
        Some(Envelope::Hello { nonce, manifest }) => (nonce, manifest),
        Some(other) => {
            return reject(
                conn,
                RejectCode::Protocol,
                format!("expected hello, received {}", other.kind()),
            )
            .await;
        }
        None => return Err(IpcError::Closed),
    };

    let (actual_nonce, manifest) = hello;
    if !nonce_matches(&nonce, &actual_nonce) {
        return reject(conn, RejectCode::Nonce, "handshake nonce mismatch").await;
    }
    if manifest.protocol.major != PROTOCOL.major {
        return reject(
            conn,
            RejectCode::Protocol,
            format!(
                "protocol major mismatch: expected {}, got {}",
                PROTOCOL.major, manifest.protocol.major
            ),
        )
        .await;
    }
    if let Err(reason) = validate(&manifest) {
        return reject(conn, RejectCode::Manifest, reason).await;
    }

    let session_id = u64::from_le_bytes(nonce[0..8].try_into().expect("8-byte slice"));
    conn.send(&Envelope::Welcome {
        session_id,
        role,
        limits,
    })
    .await?;

    match conn.recv().await? {
        Some(Envelope::Ready { .. }) => Ok(manifest),
        Some(other) => {
            reject(
                conn,
                RejectCode::Protocol,
                format!("expected ready, received {}", other.kind()),
            )
            .await
        }
        None => Err(IpcError::Closed),
    }
}

/// Run the edge/module side of the handshake.
///
/// # Errors
///
/// Returns an error when the challenge is invalid, the supervisor rejects the
/// manifest, or the transport fails.
pub async fn client_handshake(
    conn: &mut IpcConnection,
    manifest: ModuleManifest,
) -> Result<ModuleLimits, IpcError> {
    let (nonce, protocol) = match conn.recv().await? {
        Some(Envelope::Challenge { nonce, protocol }) => (nonce, protocol),
        Some(Envelope::Reject { reason, .. }) => return Err(IpcError::Protocol(reason)),
        Some(other) => {
            return Err(IpcError::Protocol(format!(
                "expected challenge, received {}",
                other.kind()
            )));
        }
        None => return Err(IpcError::Closed),
    };

    if protocol.major != PROTOCOL.major {
        conn.send(&Envelope::Reject {
            code: RejectCode::Protocol,
            reason: format!(
                "protocol major mismatch: expected {}, got {}",
                PROTOCOL.major, protocol.major
            ),
        })
        .await?;
        return Err(IpcError::Protocol("protocol major mismatch".to_owned()));
    }

    conn.send(&Envelope::Hello { nonce, manifest }).await?;

    match conn.recv().await? {
        Some(Envelope::Welcome { limits, .. }) => {
            conn.send(&Envelope::Ready { boot_ms: 0 }).await?;
            Ok(limits)
        }
        Some(Envelope::Reject { code, reason }) => {
            Err(IpcError::Protocol(format!("rejected ({code:?}): {reason}")))
        }
        Some(other) => Err(IpcError::Protocol(format!(
            "expected welcome, received {}",
            other.kind()
        ))),
        None => Err(IpcError::Closed),
    }
}

#[cfg(test)]
mod tests {
    use rsbot_core::{CommandSpec, ModuleLimits, ModuleManifest, ProtocolVersion};

    use super::{client_handshake, server_handshake};
    use crate::codec::IpcConnection;
    use crate::message::Role;

    fn manifest() -> ModuleManifest {
        ModuleManifest {
            id: "music".to_owned(),
            version: "0.1.0".to_owned(),
            protocol: ProtocolVersion::default(),
            commands: vec![CommandSpec::new("music-play", "Play")],
            events: vec!["interaction_create".to_owned()],
            provides: Vec::new(),
            requires: vec!["discord.rest".to_owned()],
            exclusive: Vec::new(),
            limits: ModuleLimits::default(),
        }
    }

    #[tokio::test]
    async fn completes_valid_handshake() {
        let (left, right) = tokio::net::UnixStream::pair().unwrap();
        let mut server = IpcConnection::new(left);
        let mut client = IpcConnection::new(right);
        let nonce = [9_u8; 32];
        let expected = manifest();

        let server_task = tokio::spawn(async move {
            server_handshake(
                &mut server,
                Role::Module,
                nonce,
                ModuleLimits::default(),
                |live| {
                    if live.id == "music" {
                        Ok(())
                    } else {
                        Err("wrong module".to_owned())
                    }
                },
            )
            .await
        });

        let limits = client_handshake(&mut client, expected.clone())
            .await
            .unwrap();
        assert_eq!(limits, ModuleLimits::default());
        let accepted = server_task.await.unwrap().unwrap();
        assert_eq!(accepted, expected);
    }

    #[tokio::test]
    async fn rejects_bad_manifest() {
        let (left, right) = tokio::net::UnixStream::pair().unwrap();
        let mut server = IpcConnection::new(left);
        let mut client = IpcConnection::new(right);

        let server_task = tokio::spawn(async move {
            server_handshake(
                &mut server,
                Role::Module,
                [1_u8; 32],
                ModuleLimits::default(),
                |_| Err("nope".to_owned()),
            )
            .await
        });

        let result = client_handshake(&mut client, manifest()).await;
        assert!(result.is_err());
        assert!(server_task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn rejects_wrong_protocol() {
        let (left, right) = tokio::net::UnixStream::pair().unwrap();
        let mut server = IpcConnection::new(left);
        let mut client = IpcConnection::new(right);
        let mut bad = manifest();
        bad.protocol = ProtocolVersion::new(99, 0);

        let server_task = tokio::spawn(async move {
            server_handshake(
                &mut server,
                Role::Module,
                [2_u8; 32],
                ModuleLimits::default(),
                |_| Ok(()),
            )
            .await
        });

        assert!(client_handshake(&mut client, bad).await.is_err());
        assert!(server_task.await.unwrap().is_err());
    }
}
