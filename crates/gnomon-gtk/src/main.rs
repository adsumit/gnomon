//! gnomon CLI and GUI entry point.

use std::process::ExitCode;

use gnomon_core::{oauth, SourceError, UsageSnapshot};
use gtk::glib;

mod app;
mod feed;
mod window;

/// Width the kind label is padded to in human-readable output.
const LABEL_WIDTH: usize = 10;

const USAGE: &str = "\
gnomon — a Wayland-native usage meter for Claude subscription limits.

USAGE:
    gnomon                 launch the meter as a layer surface
    gnomon --toplevel      launch as an ordinary window instead
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

    let once = flags.contains(&"--once");
    let json = flags.contains(&"--json");
    let toplevel = flags.contains(&"--toplevel");

    if let Some(unknown) = flags
        .iter()
        .find(|f| !matches!(**f, "--once" | "--json" | "--toplevel"))
    {
        eprintln!("gnomon: unrecognised argument `{unknown}`");
        eprintln!("{USAGE}");
        return ExitCode::from(1);
    }

    if !once {
        if json {
            eprintln!("gnomon: --json requires --once");
            return ExitCode::from(1);
        }
        return match app::run(toplevel) {
            code if code == glib::ExitCode::SUCCESS => ExitCode::SUCCESS,
            _ => ExitCode::from(1),
        };
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

    for w in &snapshot.windows {
        println!(
            "{:<width$} {:>5.1}%  {:<8} {}",
            w.label(),
            w.percent,
            w.severity_class(),
            window::countdown(w.resets_at),
            width = LABEL_WIDTH,
        );
    }
}
