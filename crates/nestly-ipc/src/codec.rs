use std::io;

use bytes::Bytes;
use futures_util::stream::{SplitSink, SplitStream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::message::Envelope;

/// Hard cap for a single IPC frame (2 MiB).
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// IPC transport failures.
#[derive(Debug, Error)]
pub enum IpcError {
    #[error("ipc i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("ipc encode error: {0}")]
    Encode(#[from] postcard::Error),
    #[error("ipc protocol error: {0}")]
    Protocol(String),
    #[error("ipc frame too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("ipc connection closed")]
    Closed,
    #[error("ipc operation timed out")]
    Timeout,
}

pub type IpcSink = SplitSink<Framed<UnixStream, LengthDelimitedCodec>, Bytes>;
pub type IpcStream = SplitStream<Framed<UnixStream, LengthDelimitedCodec>>;

/// A framed, size-limited IPC connection.
pub struct IpcConnection {
    framed: Framed<UnixStream, LengthDelimitedCodec>,
}

impl IpcConnection {
    #[must_use]
    pub fn new(stream: UnixStream) -> Self {
        let codec = LengthDelimitedCodec::builder()
            .max_frame_length(MAX_FRAME_BYTES)
            .length_field_type::<u32>()
            .new_codec();
        Self {
            framed: Framed::new(stream, codec),
        }
    }

    /// Build from a blocking standard-library stream.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the stream cannot be put into nonblocking mode.
    pub fn from_std(stream: std::os::unix::net::UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        let stream = UnixStream::from_std(stream)?;
        Ok(Self::new(stream))
    }

    /// Send one envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if encoding fails, the frame is too large, or the
    /// socket is closed.
    pub async fn send(&mut self, envelope: &Envelope) -> Result<(), IpcError> {
        let bytes = encode_frame(envelope)?;
        futures_util::SinkExt::send(&mut self.framed, bytes).await?;
        Ok(())
    }

    /// Receive one envelope. Returns `None` on a clean EOF.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, oversized, or truncated frames.
    pub async fn recv(&mut self) -> Result<Option<Envelope>, IpcError> {
        match self.framed.next().await {
            None => Ok(None),
            Some(Err(source)) => Err(IpcError::Io(source)),
            Some(Ok(frame)) => decode_frame(&frame).map(Some),
        }
    }

    /// Split into a sink and a stream for concurrent read/write tasks.
    pub fn split(self) -> (IpcSink, IpcStream) {
        self.framed.split()
    }
}

/// Encode one envelope into a length-delimited frame payload.
///
/// # Errors
///
/// Returns [`IpcError::FrameTooLarge`] when the encoded payload exceeds the
/// protocol limit, or an encoding error.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Bytes, IpcError> {
    let bytes = postcard::to_allocvec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge(bytes.len()));
    }
    Ok(Bytes::from(bytes))
}

/// Decode one frame payload.
///
/// # Errors
///
/// Returns an error when the frame is oversized or cannot be deserialized.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, IpcError> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge(frame.len()));
    }
    postcard::from_bytes(frame).map_err(IpcError::Encode)
}

#[cfg(test)]
mod tests {
    use super::{IpcConnection, IpcError, MAX_FRAME_BYTES, decode_frame, encode_frame};
    use crate::message::Envelope;
    use nestly_core::ProtocolVersion;

    #[test]
    fn round_trips_envelope() {
        let envelope = Envelope::Challenge {
            nonce: [7; 32],
            protocol: ProtocolVersion::new(1, 0),
        };
        let bytes = encode_frame(&envelope).unwrap();
        let decoded: Envelope = decode_frame(&bytes).unwrap();
        assert_eq!(decoded, envelope);

        let voice_send = Envelope::VoiceGatewaySend {
            guild_id: 12345,
            channel_id: Some(67890),
            self_mute: false,
            self_deaf: true,
        };
        let bytes = encode_frame(&voice_send).unwrap();
        let decoded: Envelope = decode_frame(&bytes).unwrap();
        assert_eq!(decoded, voice_send);
    }

    #[test]
    fn rejects_oversized_payload() {
        let oversized = vec![0_u8; MAX_FRAME_BYTES + 1];
        let result: Result<Envelope, IpcError> = decode_frame(&oversized);
        assert!(matches!(result, Err(IpcError::FrameTooLarge(_))));
    }

    #[test]
    fn rejects_garbage() {
        let result: Result<Envelope, IpcError> = decode_frame(&[0xff, 0xff, 0xff]);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sends_and_receives_over_socketpair() {
        let (left, right) = tokio::net::UnixStream::pair().unwrap();
        let mut left = IpcConnection::new(left);
        let mut right = IpcConnection::new(right);

        left.send(&Envelope::Ping).await.unwrap();
        assert_eq!(right.recv().await.unwrap(), Some(Envelope::Ping));

        right.send(&Envelope::Pong).await.unwrap();
        assert_eq!(left.recv().await.unwrap(), Some(Envelope::Pong));
    }

    #[tokio::test]
    async fn reports_clean_eof() {
        let (left, right) = tokio::net::UnixStream::pair().unwrap();
        let mut left = IpcConnection::new(left);
        drop(right);
        assert_eq!(left.recv().await.unwrap(), None);
    }
}
