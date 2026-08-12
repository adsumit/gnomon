//! Unix domain socket IPC between `gnomon-bridge` and the GUI.
//!
//! There is no listening network socket anywhere in gnomon — not on loopback,
//! not elsewhere. Access control is filesystem permissions: a 0600 socket inside
//! a 0700 directory under the user's runtime dir.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::error::SourceError;
use crate::model::UsageSnapshot;
use crate::nm::MAX_MESSAGE_BYTES;
use crate::parse::parse_snapshot;

/// Directory mode: owner-only traversal.
const DIR_MODE: u32 = 0o700;
/// Socket mode: owner-only read/write.
const SOCK_MODE: u32 = 0o600;

/// Path of the bridge socket: `$XDG_RUNTIME_DIR/gnomon/bridge.sock`.
///
/// When `XDG_RUNTIME_DIR` is unset, falls back to `/run/user/<uid>`, taking the
/// uid from the owner of `/proc/self`.
pub fn socket_path() -> Result<PathBuf, SourceError> {
    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(format!("/run/user/{}", current_uid()?)),
    };

    Ok(runtime_dir.join("gnomon").join("bridge.sock"))
}

/// The current process's uid, read from the owner of `/proc/self`.
///
/// `/proc/self` is owned by the process's real uid, so this is exact and needs
/// no environment. `$HOME` would only be a proxy (it can be unset, or owned by
/// another user), and `/proc/self/loginuid` is an audit field that is commonly
/// unset (`4294967295`) under systemd services and in containers.
fn current_uid() -> Result<u32, SourceError> {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .map_err(|e| SourceError::Transport(format!("cannot determine uid: {e}")))
}

/// Bind the bridge socket, creating its parent directory if needed.
///
/// A leftover socket file is removed only when nothing is listening on it — a
/// live socket is never unlinked out from under a running GUI.
pub fn listen(path: &Path) -> Result<UnixListener, SourceError> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(DIR_MODE)
                .create(parent)
                .map_err(|e| {
                    SourceError::Transport(format!(
                        "cannot create {}: {}",
                        parent.display(),
                        e
                    ))
                })?;
        }
    }

    if path.exists() {
        // If something answers, the socket is live — refuse rather than unlink.
        if UnixStream::connect(path).is_ok() {
            return Err(SourceError::Transport(format!(
                "{} is already in use by a running gnomon",
                path.display()
            )));
        }
        std::fs::remove_file(path).map_err(|e| {
            SourceError::Transport(format!("cannot remove stale {}: {}", path.display(), e))
        })?;
    }

    let listener = UnixListener::bind(path)
        .map_err(|e| SourceError::Transport(format!("cannot bind {}: {}", path.display(), e)))?;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCK_MODE)).map_err(|e| {
        SourceError::Transport(format!("cannot chmod {}: {}", path.display(), e))
    })?;

    Ok(listener)
}

/// Accept connections forever, invoking `on_snapshot` for each valid payload.
///
/// Each connection carries newline-delimited JSON, one raw usage payload per
/// line. A line that fails to parse is skipped; it is never fatal and never
/// panics. Lines longer than [`MAX_MESSAGE_BYTES`] are discarded unread.
pub fn serve(listener: UnixListener, mut on_snapshot: impl FnMut(UsageSnapshot)) {
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            // A failed accept must not take the server down.
            Err(_) => continue,
        };

        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();

        loop {
            match read_bounded_line(&mut reader, &mut buf) {
                Ok(Line::Eof) | Err(_) => break,
                Ok(Line::Oversized) => continue,
                Ok(Line::Ready) => {
                    let Ok(text) = std::str::from_utf8(&buf) else {
                        continue;
                    };
                    if text.trim().is_empty() {
                        continue;
                    }
                    if let Ok(snapshot) = parse_snapshot(text) {
                        on_snapshot(snapshot);
                    }
                }
            }
        }
    }
}

/// Outcome of one bounded line read.
enum Line {
    Ready,
    Oversized,
    Eof,
}

/// Read one newline-terminated line, capped at [`MAX_MESSAGE_BYTES`].
///
/// An oversized line is drained to its terminator so the stream stays aligned.
fn read_bounded_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    buf.clear();

    // Scoped so the limited reborrow ends before `fill_buf` below.
    let read = {
        let mut limited = std::io::Read::take(&mut *reader, MAX_MESSAGE_BYTES as u64 + 1);
        limited.read_until(b'\n', buf)?
    };

    if read == 0 {
        return Ok(Line::Eof);
    }

    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(Line::Ready);
    }

    // No terminator. Either the peer stopped mid-line at EOF (still usable), or
    // the line exceeded the cap (discard the remainder).
    if buf.len() <= MAX_MESSAGE_BYTES {
        return Ok(Line::Ready);
    }

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        match available.iter().position(|b| *b == b'\n') {
            Some(pos) => {
                reader.consume(pos + 1);
                break;
            }
            None => {
                let len = available.len();
                reader.consume(len);
            }
        }
    }

    Ok(Line::Oversized)
}

/// Deliver one raw usage payload to the GUI.
///
/// A refused connection means the GUI is not running. That is an ordinary
/// condition, not a fault worth logging loudly.
///
/// The payload is written on a single line. Raw newlines are replaced with
/// spaces first: in valid JSON a literal newline can only be insignificant
/// whitespace between tokens (inside a string it must appear escaped), so this
/// preserves the document exactly while satisfying the line framing. The text is
/// not parsed and re-serialized — key order, spacing, and number formatting all
/// survive untouched.
pub fn send(path: &Path, raw_json: &str) -> Result<(), SourceError> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| SourceError::Transport(format!("cannot connect to {}: {}", path.display(), e)))?;

    let single_line: String = raw_json
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();

    stream
        .write_all(single_line.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
        .and_then(|()| stream.flush())
        .map_err(|e| SourceError::Transport(format!("cannot write to {}: {}", path.display(), e)))
}
