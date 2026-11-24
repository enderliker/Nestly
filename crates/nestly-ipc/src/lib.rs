#![forbid(unsafe_code)]
#![doc = "Authenticated, length-delimited IPC for nestly supervisor, edge, and modules."]

mod codec;
mod handshake;
mod message;

pub use codec::{
    IpcConnection, IpcError, IpcSink, IpcStream, MAX_FRAME_BYTES, decode_frame, encode_frame,
};
pub use handshake::{client_handshake, server_handshake};
pub use message::{
    ControlAction, Envelope, GatewayEvent, InteractionPayload, LogLevel, RejectCode, Role,
    VoiceServerPayload, VoiceStatePayload,
};

/// Current IPC protocol version.
pub const PROTOCOL: nestly_core::ProtocolVersion = nestly_core::ProtocolVersion::new(1, 0);
