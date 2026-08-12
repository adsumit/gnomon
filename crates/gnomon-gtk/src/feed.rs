//! Both transports merged onto one channel.
//!
//! Two plain `std::thread`s, no async runtime. Each owns a blocking sender and
//! neither is allowed to panic or to exit without saying why.

use std::time::Duration;

use gnomon_core::{ipc, oauth, UsageSnapshot};

/// Which transport produced a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Oauth,
    Bridge,
}

/// One message from the feed to the UI.
#[derive(Debug)]
pub enum Update {
    Snapshot(UsageSnapshot, Origin),
    Error(String),
}

/// SPEC: the OAuth transport must never poll faster than this.
const OAUTH_INTERVAL: Duration = Duration::from_secs(180);
/// Backoff before re-attempting to own the bridge socket.
const BRIDGE_RETRY: Duration = Duration::from_secs(5);

/// Start both transports. Returns immediately.
pub fn spawn(tx: async_channel::Sender<Update>) {
    spawn_oauth(tx.clone());
    spawn_bridge(tx);
}

/// Poll the usage endpoint: once immediately, then every 180s forever.
fn spawn_oauth(tx: async_channel::Sender<Update>) {
    std::thread::spawn(move || loop {
        let update = match oauth::fetch_snapshot() {
            Ok(snapshot) => Update::Snapshot(snapshot, Origin::Oauth),
            Err(e) => Update::Error(e.to_string()),
        };

        // A closed channel means the UI is gone; stop cleanly.
        if tx.send_blocking(update).is_err() {
            return;
        }

        std::thread::sleep(OAUTH_INTERVAL);
    });
}

/// Own the bridge socket and forward whatever the extension delivers.
fn spawn_bridge(tx: async_channel::Sender<Update>) {
    std::thread::spawn(move || loop {
        let path = match ipc::socket_path() {
            Ok(path) => path,
            Err(e) => {
                if tx.send_blocking(Update::Error(e.to_string())).is_err() {
                    return;
                }
                std::thread::sleep(BRIDGE_RETRY);
                continue;
            }
        };

        match ipc::listen(&path) {
            Ok(listener) => {
                let sender = tx.clone();
                ipc::serve(listener, move |snapshot| {
                    let _ = sender.send_blocking(Update::Snapshot(snapshot, Origin::Bridge));
                });
                // serve() only returns if the listener died. Rebind after a pause.
            }
            Err(e) => {
                let message = e.to_string();

                // A live socket means a second gnomon owns it. Retrying would
                // spin forever, so report once and stop this thread.
                if message.contains("already in use") {
                    let _ = tx.send_blocking(Update::Error(
                        "another gnomon is already running".to_string(),
                    ));
                    return;
                }

                if tx.send_blocking(Update::Error(message)).is_err() {
                    return;
                }
            }
        }

        std::thread::sleep(BRIDGE_RETRY);
    });
}
