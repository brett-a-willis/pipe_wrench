//! Async MLLP networking library for pipe_wrench.
//!
//! Covers: MLLP envelope framing, codecs, client sender, and server listener (NET-*).
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![warn(missing_docs)]

pub mod envelope;
pub mod error;
pub mod listener;
pub mod profile;
pub mod sender;
pub mod tls;

pub use envelope::{
    classify_malformed_bytes, format_raw_byte_preview, InboundFrame, MalformedPacketInfo,
    MllpCodec, MllpEnvelope, DEFAULT_MAX_FRAME_BYTES, MAX_RAW_PREVIEW_BYTES,
};
pub use error::NetError;
pub use hl7_core::ack;
pub use listener::{run_listener_server, ListenerCommand, ListenerEvent};
pub use profile::{
    ListenerAckSetting, ListenerProfile, ListenerTlsConfig, MaxTriesAction, NakAction, SendScope,
    SenderProfile, SenderTlsConfig,
};
pub use sender::{
    prepare_outgoing_record, resolve_send_scope, run_sender, send_single_frame_over_stream,
    NextAction, SendHistoryEntry, SenderCommand, SenderEvent, SenderState, SenderStateMachine,
    MSG_SERVER_ABRUPTLY_STOPPED, MSG_SERVER_NOT_ACKING, MSG_UNABLE_TO_CONNECT,
};
pub use tls::{build_tls_acceptor, build_tls_connector};
