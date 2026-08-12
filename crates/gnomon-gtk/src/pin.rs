//! Pin state plumbing: the pid file, and the click-through input region.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use gtk::gdk;
use gtk::prelude::*;

/// Where the running GUI advertises its pid for `--toggle-pin`.
pub fn pid_path() -> Option<PathBuf> {
    gnomon_core::ipc::socket_path()
        .ok()
        .and_then(|s| s.parent().map(|d| d.join("gnomon.pid")))
}

/// Is this pid a live process? `kill(pid, 0)` tests existence without signalling.
pub fn is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 performs error checking only and never delivers.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Claim the pid file for this process, replacing a stale one.
pub fn write_pid_file() {
    let Some(path) = pid_path() else {
        return;
    };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // An existing file from a dead process is simply overwritten.
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(pid) = text.trim().parse::<i32>() {
            if is_alive(pid) && pid != std::process::id() as i32 {
                eprintln!("gnomon: replacing pid file owned by live process {pid}");
            }
        }
    }

    if let Ok(mut file) = std::fs::File::create(&path) {
        let _ = writeln!(file, "{}", std::process::id());
        let _ = file
            .metadata()
            .map(|m| m.permissions())
            .map(|mut p| {
                p.set_mode(0o600);
                p
            })
            .and_then(|p| std::fs::set_permissions(&path, p));
    }
}

/// Drop the pid file on a clean exit.
pub fn remove_pid_file() {
    if let Some(path) = pid_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Accept SIGUSR1 synchronously and forward it to the main thread.
///
/// `glib::unix_signal_add_local` does not exist in glib 0.22, and glib-sys does
/// not bind `g_unix_signal_add` either, so the glib integration is unavailable.
/// Rather than install a raw handler — which would run async-signal-unsafe code
/// on an arbitrary thread — this blocks SIGUSR1 process-wide and parks a thread
/// in `sigwait`, which accepts the signal synchronously. The toggle itself is
/// performed by the caller on the main thread via the returned channel.
///
/// Must be called before any other thread is spawned, so they all inherit the
/// blocked mask; otherwise a stray thread could accept the signal instead.
pub fn watch_sigusr1(tx: async_channel::Sender<()>) {
    // SAFETY: standard sigset construction; sigwait blocks rather than
    // interrupting, so nothing here runs in a signal-handler context.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGUSR1);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());

        std::thread::spawn(move || {
            let mut received: libc::c_int = 0;
            loop {
                if libc::sigwait(&set, &mut received) == 0 && tx.send_blocking(()).is_err() {
                    return;
                }
            }
        });
    }
}

/// Apply the current pin state to the window's input region.
///
/// An empty region means the compositor routes every click to whatever is
/// underneath. This must be re-applied whenever the surface is reconfigured,
/// because a new surface layout resets it.
pub fn apply_input_region(win: &adw::ApplicationWindow, pinned: bool) {
    let Some(surface) = win.surface() else {
        return;
    };

    if pinned {
        // Empty region: nothing is clickable.
        let region = gdk::cairo::Region::create();
        surface.set_input_region(Some(&region));
    } else {
        let rect = gdk::cairo::RectangleInt::new(0, 0, win.width().max(1), win.height().max(1));
        let region = gdk::cairo::Region::create_rectangle(&rect);
        surface.set_input_region(Some(&region));
    }
}
