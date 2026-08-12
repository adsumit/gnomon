//! Native messaging frame encoding tests. Pure — no I/O, no Chrome.

use gnomon_core::nm::{decode_length, encode_frame, MAX_MESSAGE_BYTES};
use gnomon_core::SourceError;

#[test]
fn prefix_is_payload_byte_length() {
    let payload = r#"{"limits":[]}"#;
    let frame = encode_frame(payload);

    assert_eq!(frame.len(), 4 + payload.len());

    let prefix: [u8; 4] = frame[..4].try_into().expect("prefix is 4 bytes");
    assert_eq!(u32::from_le_bytes(prefix) as usize, payload.len());
    assert_eq!(&frame[4..], payload.as_bytes());
}

#[test]
fn encode_then_decode_round_trips() {
    let payload = r#"{"limits":[{"kind":"session"}]}"#;
    let frame = encode_frame(payload);

    let prefix: [u8; 4] = frame[..4].try_into().expect("prefix is 4 bytes");
    let len = decode_length(prefix).expect("length must decode");

    assert_eq!(len, payload.len());
    assert_eq!(
        std::str::from_utf8(&frame[4..4 + len]).expect("payload is UTF-8"),
        payload
    );
}

#[test]
fn decode_length_rejects_zero() {
    let err = decode_length(0u32.to_ne_bytes()).expect_err("zero must be rejected");
    assert!(
        matches!(err, SourceError::Transport(_)),
        "expected Transport, got {err:?}"
    );
}

#[test]
fn decode_length_rejects_oversized() {
    let too_big = (MAX_MESSAGE_BYTES + 1) as u32;
    let err = decode_length(too_big.to_ne_bytes()).expect_err("oversized must be rejected");

    match err {
        SourceError::Transport(msg) => assert_eq!(msg, "message too large"),
        other => panic!("expected Transport, got {other:?}"),
    }
}

#[test]
fn decode_length_accepts_exact_maximum() {
    let at_limit = MAX_MESSAGE_BYTES as u32;
    assert_eq!(
        decode_length(at_limit.to_ne_bytes()).expect("the cap itself is allowed"),
        MAX_MESSAGE_BYTES
    );
}

#[test]
fn multibyte_payload_frames_by_byte_length() {
    // 5 chars, 10 bytes: "é" is 2 bytes, "日" and "本" are 3 each, "a"/"b" are 1.
    let payload = "é日本ab";
    assert_eq!(payload.chars().count(), 5);
    assert_eq!(payload.len(), 10);

    let frame = encode_frame(payload);
    let prefix: [u8; 4] = frame[..4].try_into().expect("prefix is 4 bytes");

    assert_eq!(
        decode_length(prefix).expect("length must decode"),
        10,
        "framing must count bytes, not chars"
    );
    assert_eq!(frame.len(), 4 + 10);
}
