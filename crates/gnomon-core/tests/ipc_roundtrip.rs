//! Unix socket round-trip tests. No network, no Chrome.
//!
//! Each test binds its own socket under `std::env::temp_dir()` and removes it
//! afterwards. `serve` never returns, so it runs on a detached thread and
//! results come back over a channel.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use gnomon_core::ipc;

const LIVE_OAUTH: &str = include_str!("fixtures/live_oauth.json");

/// A socket path unique to this test binary, process, and call site.
fn temp_socket(tag: &str) -> PathBuf {
    let unique = format!(
        "gnomon-test-{}-{}-{:?}.sock",
        tag,
        std::process::id(),
        std::thread::current().id()
    );
    std::env::temp_dir().join(unique)
}

/// Bind, serve on a thread, and hand back snapshots' window counts.
fn spawn_server(path: &Path) -> mpsc::Receiver<usize> {
    let listener = ipc::listen(path).expect("listener must bind");
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        ipc::serve(listener, move |snapshot| {
            // A closed receiver just means the test finished.
            let _ = tx.send(snapshot.windows.len());
        });
    });

    rx
}

#[test]
fn send_delivers_a_snapshot() {
    let path = temp_socket("deliver");
    let rx = spawn_server(&path);

    ipc::send(&path, LIVE_OAUTH).expect("send must succeed");

    let windows = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a snapshot must arrive");
    assert_eq!(windows, 2, "live_oauth.json carries 2 windows");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn malformed_line_does_not_block_the_next_one() {
    let path = temp_socket("malformed");
    let rx = spawn_server(&path);

    // Both lines travel on the SAME connection.
    let mut stream = UnixStream::connect(&path).expect("connect must succeed");
    stream
        .write_all(b"{ not json at all\n")
        .expect("write must succeed");

    let single_line: String = LIVE_OAUTH.replace(['\n', '\r'], " ");
    stream
        .write_all(single_line.as_bytes())
        .expect("write must succeed");
    stream.write_all(b"\n").expect("write must succeed");
    stream.flush().expect("flush must succeed");

    let windows = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the valid line must still arrive");
    assert_eq!(windows, 2, "the malformed line must be skipped, not fatal");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn send_to_missing_socket_errors_without_panicking() {
    let path = temp_socket("absent");
    assert!(
        !path.exists(),
        "this path must not exist for the test to mean anything"
    );

    let result = ipc::send(&path, LIVE_OAUTH);
    assert!(
        result.is_err(),
        "sending with no GUI listening must be an Err, not a panic"
    );
}
