//! Chrome native messaging frame encoding. Pure — performs no I/O.
//!
//! Chrome prefixes each message with a 4-byte unsigned length in the *platform's
//! native byte order*. gnomon targets x86_64 Linux, which is little-endian, so
//! the native encoding used here is little-endian in practice. `to_ne_bytes` /
//! `from_ne_bytes` are used deliberately rather than the `_le_` variants so the
//! framing stays correct if this is ever built for a big-endian target, where
//! Chrome would likewise use big-endian.

use crate::error::SourceError;

/// Largest message gnomon will accept in either direction.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Frame a payload: native-endian `u32` byte length, then the UTF-8 bytes.
pub fn encode_frame(payload: &str) -> Vec<u8> {
    let bytes = payload.as_bytes();
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_ne_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Decode a 4-byte length prefix.
///
/// Zero-length and oversized messages are both rejected, since neither can be a
/// usage payload and both indicate a desynchronized stream.
pub fn decode_length(prefix: [u8; 4]) -> Result<usize, SourceError> {
    let len = u32::from_ne_bytes(prefix) as usize;

    if len == 0 {
        return Err(SourceError::Transport("zero-length message".to_string()));
    }
    if len > MAX_MESSAGE_BYTES {
        return Err(SourceError::Transport("message too large".to_string()));
    }

    Ok(len)
}
