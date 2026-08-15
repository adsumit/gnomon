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

/// A sigset containing only SIGUSR1.
///
/// SAFETY: `sigemptyset`/`sigaddset` on a zeroed `sigset_t` is the standard
/// construction and touches nothing else.
unsafe fn sigusr1_set() -> libc::sigset_t {
    let mut set: libc::sigset_t = std::mem::zeroed();
    libc::sigemptyset(&mut set);
    libc::sigaddset(&mut set, libc::SIGUSR1);
    set
}

/// Block SIGUSR1 on this thread. MUST be the first thing `main` does.
///
/// `pthread_sigmask` sets the mask of the CALLING THREAD ONLY. New threads
/// inherit the mask of whichever thread created them, so the mask propagates
/// forwards but never backwards: a thread that already exists when this runs
/// keeps its own, unblocked mask forever.
///
/// That is not a detail. SIGUSR1's default disposition is to TERMINATE the
/// process, and the kernel delivers a process-directed signal to any one thread
/// that has it unblocked. Installing this after GTK/GLib/GDBus/GSK have started
/// their worker pools left those threads unblocked, so `--toggle-pin` killed the
/// app instead of toggling it. The only safe point is before any library has had
/// the chance to spawn anything — that is, the first statement of `main`, in
/// every mode.
pub fn block_sigusr1() {
    // SAFETY: masking a signal on the current thread; nothing here can run in a
    // signal-handler context.
    unsafe {
        let set = sigusr1_set();
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Park a thread in `sigwait` and forward each SIGUSR1 to the main thread.
///
/// `glib::unix_signal_add_local` does not exist in glib 0.22, and glib-sys does
/// not bind `g_unix_signal_add` either, so the glib integration is unavailable.
/// Rather than install a raw handler — which would run async-signal-unsafe code
/// on an arbitrary thread — SIGUSR1 is blocked everywhere and accepted
/// synchronously here. The toggle itself runs on the main thread, off the
/// channel.
///
/// [`block_sigusr1`] must already have run. This may be called late; only the
/// mask has to be early.
pub fn watch_sigusr1(tx: async_channel::Sender<()>) {
    // SAFETY: `sigwait` blocks rather than interrupting, so nothing in the
    // spawned thread runs in a signal-handler context.
    unsafe {
        let set = sigusr1_set();

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

/// Bit position of SIGUSR1 in the kernel's `SigBlk` bitmask, which is 1-based.
const SIGUSR1_BIT: u64 = 1 << (libc::SIGUSR1 as u64 - 1);

/// Verify SIGUSR1 is still blocked, and say loudly if it is not.
///
/// Two checks, because one alone would be a half-truth:
///
/// 1. `pthread_sigmask` with a null `set` reads back the mask — but only of the
///    CALLING thread. That catches the mask being installed too late relative
///    to `main`, and nothing else.
/// 2. Linux publishes every thread's blocked mask as the `SigBlk` field of
///    `/proc/self/task/<tid>/status`. Walking that directory is the only way to
///    answer the question that actually matters — *is any thread still able to
///    accept this signal* — since POSIX offers no way to read another thread's
///    mask. This is Linux-specific, which is acceptable in a Wayland-only tool.
///
/// Names each offending thread, because "some thread is unblocked" is not
/// actionable and the comm field usually identifies the library that spawned it.
pub fn verify_sigusr1_blocked(phase: &str) {
    // SAFETY: reading back the current thread's mask; `set` is null so nothing
    // is modified.
    let this_thread_blocked = unsafe {
        let mut current: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut current);
        libc::sigismember(&current, libc::SIGUSR1) == 1
    };

    if !this_thread_blocked {
        eprintln!(
            "gnomon: SIGUSR1 is NOT blocked on the main thread at {phase} — \
--toggle-pin will terminate this process instead of toggling the pin"
        );
    }

    for (tid, name) in unblocked_threads() {
        eprintln!(
            "gnomon: SIGUSR1 is NOT blocked on thread {tid} ({name}) at {phase} — \
it was spawned before the mask was installed, and --toggle-pin may kill this process"
        );
    }
}

/// Every thread whose `SigBlk` lacks SIGUSR1, as `(tid, comm)`.
///
/// An unreadable `/proc` yields an empty list rather than a false alarm: the
/// guard's job is to catch a real regression, not to invent one.
fn unblocked_threads() -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };

    let mut offenders = Vec::new();

    for entry in entries.flatten() {
        let tid = entry.file_name().to_string_lossy().into_owned();
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };

        if sigusr1_blocked_in(&status) == Some(false) {
            let name = status
                .lines()
                .find_map(|l| l.strip_prefix("Name:"))
                .map(|n| n.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());
            offenders.push((tid, name));
        }
    }

    offenders
}

/// Read one `/proc/<tid>/status` blob: is SIGUSR1 in its `SigBlk` mask?
///
/// `None` means the field was absent or unparseable. The caller treats that as
/// "no evidence" rather than "unblocked", so a kernel that changes the format
/// produces silence instead of a stream of false alarms.
fn sigusr1_blocked_in(status: &str) -> Option<bool> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("SigBlk:"))
        .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        .map(|mask| mask & SIGUSR1_BIT != 0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigusr1_is_bit_nine_because_sigblk_is_one_based() {
        assert_eq!(libc::SIGUSR1, 10, "Linux signal numbering");
        assert_eq!(SIGUSR1_BIT, 0x200, "signal N is bit N-1, so 10 is bit 9");
    }

    #[test]
    fn a_status_blob_with_sigusr1_blocked_reads_as_blocked() {
        let status = "Name:\tgnomon\nSigBlk:\t0000000000000200\n";
        assert_eq!(sigusr1_blocked_in(status), Some(true));
    }

    #[test]
    fn a_status_blob_without_sigusr1_reads_as_unblocked() {
        // This is the shape the killer threads had: mask set, but not SIGUSR1.
        let status = "Name:\tgmain\nSigBlk:\t0000000000004000\n";
        assert_eq!(sigusr1_blocked_in(status), Some(false));
        assert_eq!(
            sigusr1_blocked_in("Name:\tpool\nSigBlk:\t0000000000000000\n"),
            Some(false)
        );
    }

    #[test]
    fn an_unreadable_status_blob_is_no_evidence_either_way() {
        // Never `Some(false)`: a format change must not manufacture warnings.
        assert_eq!(sigusr1_blocked_in("Name:\tgnomon\n"), None);
        assert_eq!(sigusr1_blocked_in("SigBlk:\tnot-hex\n"), None);
        assert_eq!(sigusr1_blocked_in(""), None);
    }

    /// The end-to-end claim: after `block_sigusr1`, both halves of the guard
    /// agree that this thread is blocked — the portable readback, and the
    /// `/proc` path the guard uses for threads it cannot ask directly.
    #[test]
    fn blocking_is_visible_to_both_halves_of_the_guard() {
        block_sigusr1();

        // SAFETY: reading back this thread's mask; `set` is null.
        let via_pthread = unsafe {
            let mut current: libc::sigset_t = std::mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut current);
            libc::sigismember(&current, libc::SIGUSR1) == 1
        };
        assert!(via_pthread, "pthread_sigmask readback");

        // `/proc/thread-self` is the calling thread's own directory.
        let status = std::fs::read_to_string("/proc/thread-self/status")
            .expect("Linux /proc must be mounted");
        assert_eq!(sigusr1_blocked_in(&status), Some(true), "/proc SigBlk");
    }

    /// A thread created AFTER the mask inherits it. A thread created before
    /// would not — which is the entire defect, and why the mask moved to the
    /// first line of `main`.
    #[test]
    fn a_thread_spawned_after_the_mask_inherits_it() {
        block_sigusr1();

        let inherited = std::thread::spawn(|| {
            let status = std::fs::read_to_string("/proc/thread-self/status").unwrap();
            sigusr1_blocked_in(&status)
        })
        .join()
        .unwrap();

        assert_eq!(inherited, Some(true));
    }
}
