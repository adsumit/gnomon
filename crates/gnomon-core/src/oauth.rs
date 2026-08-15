//! Blocking OAuth transport for the usage endpoint.
//!
//! Targets ureq 3.x. HTTP status errors are turned off at the agent level so a
//! non-200 maps to [`SourceError::Http`] rather than arriving as a transport
//! error.

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use chrono::Utc;

use crate::creds::discover_credentials;
use crate::error::SourceError;
use crate::model::UsageSnapshot;
use crate::parse::parse_snapshot;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const FALLBACK_CLI_VERSION: &str = "0.0.0";
const TIMEOUT_SECS: u64 = 10;

/// Extract a version from the raw stdout of `claude --version`.
///
/// Returns the first whitespace-separated token when it starts with an ASCII
/// digit, otherwise `None`.
pub fn extract_cli_version(raw: &str) -> Option<String> {
    // `split_whitespace` already skips leading whitespace, so an explicit
    // `trim()` here is redundant (and denied by clippy::trim_split_whitespace).
    let first = raw.split_whitespace().next()?;
    match first.chars().next() {
        Some(c) if c.is_ascii_digit() => Some(first.to_string()),
        _ => None,
    }
}

/// `claude --version`, with the signal mask reset in the child.
///
/// A blocked signal mask survives `execve`, and the GUI blocks SIGUSR1 before
/// anything else so that `--toggle-pin` cannot kill it. Without this, every
/// process gnomon spawns would inherit that block and run with SIGUSR1
/// permanently masked — a state `claude` never asked for and cannot detect.
/// Restoring the default disposition in the child is gnomon's responsibility,
/// not the child's.
fn claude_version_command() -> Command {
    let mut cmd = Command::new("claude");
    cmd.arg("--version");

    // SAFETY: `pre_exec` runs between fork and exec, where only
    // async-signal-safe calls are permitted. `sigemptyset`, `sigaddset` and
    // `pthread_sigmask` are all on the POSIX async-signal-safe list, and
    // nothing here allocates or takes a lock.
    unsafe {
        cmd.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGUSR1);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            Ok(())
        });
    }

    cmd
}

/// Build the `User-Agent`, preferring an override, then the CLI, then a constant.
fn user_agent() -> String {
    let version = std::env::var("GNOMON_CLI_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            claude_version_command()
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .as_deref()
                .and_then(extract_cli_version)
        })
        .unwrap_or_else(|| FALLBACK_CLI_VERSION.to_string());

    format!("claude-code/{version}")
}

/// Fetch the raw usage payload.
pub fn fetch_raw() -> Result<String, SourceError> {
    let creds = discover_credentials()?;

    if let Some(expires_at) = creds.expires_at {
        if expires_at <= Utc::now() {
            return Err(SourceError::TokenExpired);
        }
    }

    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        // Read the status ourselves instead of receiving it as an error.
        .http_status_as_error(false)
        .build();
    let agent: ureq::Agent = config.into();

    let mut response = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {}", creds.token.as_str()))
        .header("anthropic-beta", OAUTH_BETA)
        .header("User-Agent", &user_agent())
        .call()
        .map_err(|e| SourceError::Transport(transport_message(&e)))?;

    let status = response.status().as_u16();
    if status != 200 {
        return Err(SourceError::Http(status));
    }

    response
        .body_mut()
        .read_to_string()
        .map_err(|e| SourceError::Transport(transport_message(&e)))
}

/// Fetch and normalize a snapshot.
pub fn fetch_snapshot() -> Result<UsageSnapshot, SourceError> {
    let raw = fetch_raw()?;
    parse_snapshot(&raw).map_err(SourceError::Parse)
}

/// Describe a transport failure without echoing request headers.
///
/// ureq surfaces the target URI in some variants; the token travels only in a
/// header, which no variant renders, so the token cannot reach this string.
fn transport_message(err: &ureq::Error) -> String {
    err.to_string()
}
