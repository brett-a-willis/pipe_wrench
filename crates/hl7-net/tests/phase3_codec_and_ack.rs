//! Phase 3 acceptance and codec tests for `hl7-net` and `hl7-core::ack`.
//!
//! Covers: NET-1..NET-4, NET-15, NET-16, NET-26, NFR-3..NFR-5, D-15..D-17.

use bytes::BytesMut;
use hl7_core::ack::{build_hl7_ack, evaluate_ack, AckCode, AckMode, AckResult};
use hl7_net::envelope::{MllpCodec, MllpEnvelope};
use hl7_net::error::NetError;
use tokio_util::codec::{Decoder, Encoder};

// NET-1: Configurable envelope markers & defaults reset
#[test]
fn envelope_markers_and_defaults_reset() {
    let mut env = MllpEnvelope {
        start_block: 0x01,
        end_data: 0x02,
        end_block: 0x03,
    };
    assert_eq!(env.start_block, 0x01);
    env.reset_defaults();
    assert_eq!(env, MllpEnvelope::DEFAULT);
}

// NET-2: Outbound framing strips internal markers, ensures trailing CR, and wraps in SB..ED+EB
#[test]
fn outbound_framing_strips_markers_and_ensures_cr() {
    let mut codec = MllpCodec::default();
    let mut buf = BytesMut::new();

    // Payload already ends with CR and has embedded SB/ED/EB bytes
    let payload_with_markers = b"MSH|^~\\&\x0B|DATA\x1C|MORE\x0D";
    codec
        .encode(payload_with_markers.as_slice(), &mut buf)
        .unwrap();

    let expected = b"\x0BMSH|^~\\&|DATA|MORE\r\x1C\x0D";
    assert_eq!(&buf[..], expected);

    // Payload WITHOUT trailing CR gets CR appended
    let mut buf2 = BytesMut::new();
    let payload_no_cr = b"MSH|^~\\&|NO_CR";
    codec.encode(payload_no_cr.as_slice(), &mut buf2).unwrap();
    let expected2 = b"\x0BMSH|^~\\&|NO_CR\r\x1C\x0D";
    assert_eq!(&buf2[..], expected2);
}

// NET-3: One frame split across N reads
#[test]
fn inbound_frame_split_across_multiple_reads() {
    let mut codec = MllpCodec::default();
    let full_bytes = b"\x0BMSH|^~\\&|TEST\r\x1C\x0D";

    let mut buf = BytesMut::new();
    // Feed 3 bytes at a time
    for chunk in full_bytes.chunks(3) {
        buf.extend_from_slice(chunk);
        if buf.len() < full_bytes.len() {
            let res = codec.decode(&mut buf).unwrap();
            assert!(res.is_none(), "partial read should not yield a frame");
        }
    }

    let frame = codec
        .decode(&mut buf)
        .unwrap()
        .expect("frame should decode");
    assert_eq!(frame.payload, b"MSH|^~\\&|TEST\r");
    assert!(!frame.missing_start_block);
    assert!(buf.is_empty());
}

// NET-3: N frames in 1 read
#[test]
fn inbound_n_frames_in_single_read() {
    let mut codec = MllpCodec::default();
    let mut buf = BytesMut::new();

    buf.extend_from_slice(b"\x0BMSG1\r\x1C\x0D\x0BMSG2\r\x1C\x0D\x0BMSG3\r\x1C\x0D");

    let f1 = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(f1.payload, b"MSG1\r");
    assert!(!f1.missing_start_block);

    let f2 = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(f2.payload, b"MSG2\r");
    assert!(!f2.missing_start_block);

    let f3 = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(f3.payload, b"MSG3\r");
    assert!(!f3.missing_start_block);

    assert!(codec.decode(&mut buf).unwrap().is_none());
}

// NET-4: Missing Start Block accepted with warning flag & garbage before SB ignored
#[test]
fn inbound_missing_start_block_and_leading_garbage() {
    let mut codec = MllpCodec::default();

    // 1. Missing SB
    let mut buf1 = BytesMut::from(&b"MSG_NO_SB\r\x1C\x0D"[..]);
    let f1 = codec.decode(&mut buf1).unwrap().unwrap();
    assert_eq!(f1.payload, b"MSG_NO_SB\r");
    assert!(f1.missing_start_block);

    // 2. Junk before SB
    let mut buf2 = BytesMut::from(&b"RANDOM_TCP_JUNK\x0BMSG_VALID\r\x1C\x0D"[..]);
    let f2 = codec.decode(&mut buf2).unwrap().unwrap();
    assert_eq!(f2.payload, b"MSG_VALID\r");
    assert!(!f2.missing_start_block);
}

// NET-4: Oversized frame returns NetError::FrameTooLarge without panicking
#[test]
fn inbound_oversized_frame_aborts_with_error() {
    let mut codec = MllpCodec::new(MllpEnvelope::DEFAULT, 64);
    let mut buf = BytesMut::new();

    // 100 bytes of data without terminator
    buf.extend_from_slice(&[b'A'; 100]);
    let err = codec.decode(&mut buf).unwrap_err();
    assert!(matches!(err, NetError::FrameTooLarge { max_bytes: 64, .. }));
}

// NET-15, NET-16, NET-26: Cross-crate integration verification
#[test]
fn round_trip_ack_generation_and_evaluation() {
    let inbound = b"MSH|^~\\&|CLIENT_APP|CLIENT_FAC|SERVER_APP|SERVER_FAC|20260101120000||ADT^A01|REQ_12345|P|2.5\rPID|1||P01\r";

    // Listener generates ACK (NET-16)
    let ack_payload = build_hl7_ack(inbound, AckCode::AA, None);

    // Sender receives and evaluates ACK (NET-26)
    let res = evaluate_ack(&ack_payload, "REQ_12345", &AckMode::Hl7);
    assert_eq!(res, AckResult::Ack);

    // Evaluation fails when matching against wrong control ID
    let res_wrong = evaluate_ack(&ack_payload, "DIFFERENT_ID", &AckMode::Hl7);
    assert_eq!(res_wrong, AckResult::Nak);

    // Evaluation fails for NAK code
    let nak_payload = build_hl7_ack(inbound, AckCode::AR, None);
    let res_nak = evaluate_ack(&nak_payload, "REQ_12345", &AckMode::Hl7);
    assert_eq!(res_nak, AckResult::Nak);
}
