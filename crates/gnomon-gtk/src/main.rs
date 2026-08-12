//! gnomon CLI. No GTK yet — `--once` performs a single fetch and prints.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use gnomon_core::{oauth, LimitWindow, SourceError, UsageSnapshot};

/// Width the kind label is padded to in human-readable output.
const LABEL_WIDTH: usize = 10;

const USAGE: &str = "\
gnomon — a Wayland-native usage meter for Claude subscription limits.

USAGE:
    gnomon                 no UI yet
    gnomon --once          fetch once and print each limit window
    gnomon --once --json   fetch once and print the snapshot as JSON
    gnomon --help          show this message

EXIT CODES:
    0  success
    1  fetch or parse failure
    2  auth expired or invalid";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags: Vec<&str> = args.iter().map(String::as_str).collect();

    if flags.iter().any(|f| *f == "--help" || *f == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    if flags.is_empty() {
        println!("gnomon: no UI yet, try --once");
        return ExitCode::SUCCESS;
    }

    let once = flags.contains(&"--once");
    let json = flags.contains(&"--json");

    if let Some(unknown) = flags.iter().find(|f| !matches!(**f, "--once" | "--json")) {
        eprintln!("gnomon: unrecognised argument `{unknown}`");
        eprintln!("{USAGE}");
        return ExitCode::from(1);
    }

    if !once {
        eprintln!("gnomon: --json requires --once");
        return ExitCode::from(1);
    }

    match oauth::fetch_snapshot() {
        Ok(snapshot) => {
            if json {
                match serde_json::to_string_pretty(&snapshot) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("gnomon: could not serialize snapshot: {e}");
                        return ExitCode::from(1);
                    }
                }
            } else {
                print_human(&snapshot);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report(&e),
    }
}

/// Print the error and choose an exit code. Never prints a token.
fn report(err: &SourceError) -> ExitCode {
    match err {
        SourceError::TokenExpired | SourceError::Http(401) => {
            eprintln!("gnomon: auth expired or invalid — run `claude` to refresh");
            ExitCode::from(2)
        }
        other => {
            eprintln!("gnomon: {other}");
            ExitCode::from(1)
        }
    }
}

fn print_human(snapshot: &UsageSnapshot) {
    if snapshot.windows.is_empty() {
        println!("no limit windows reported");
        return;
    }

    for window in &snapshot.windows {
        println!(
            "{:<width$} {:>5.1}%  {:<8} {}",
            window.label(),
            window.percent,
            window.severity_class(),
            countdown(window),
            width = LABEL_WIDTH,
        );
    }
}

/// "resets in 3h 32m", or "—" when the window carries no reset time.
///
/// Uses `timestamp()` and `SystemTime` rather than `chrono::Utc::now()` so this
/// crate needs no direct chrono dependency.
fn countdown(window: &LimitWindow) -> String {
    let Some(resets_at) = window.resets_at else {
        return "—".to_string();
    };

    let now_secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => return "—".to_string(),
    };

    let minutes = (resets_at.timestamp() - now_secs) / 60;
    if minutes <= 0 {
        return "resets now".to_string();
    }

    let days = minutes / 1440;
    let hours = (minutes % 1440) / 60;
    let mins = minutes % 60;

    if days > 0 {
        format!("resets in {days}d {hours}h")
    } else if hours > 0 {
        format!("resets in {hours}h {mins}m")
    } else {
        format!("resets in {mins}m")
    }
}
