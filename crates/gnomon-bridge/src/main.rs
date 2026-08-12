//! Chrome native messaging host.
//!
//! Reads length-prefixed frames from stdin, validates each as a usage payload,
//! and forwards the original text to the GUI over the Unix domain socket.
//!
//! stdout is the protocol channel and carries nothing but acknowledgement
//! frames. Every diagnostic goes to stderr.

use std::io::{Read, Write};
use std::process::ExitCode;

use gnomon_core::{ipc, nm, parse_snapshot};

const HOST_NAME: &str = "com.gnomon.bridge";
const HOST_DESCRIPTION: &str = "gnomon usage bridge";
/// Placeholder — the real id is only known once the extension is packed.
const ALLOWED_ORIGIN: &str = "chrome-extension://REPLACE_WITH_EXTENSION_ID/";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--print-manifest") => match print_manifest() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("gnomon-bridge: {e}");
                ExitCode::from(1)
            }
        },
        Some("--install-manifest") => match args.get(1) {
            Some(id) => match install_manifest(id) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("gnomon-bridge: {e}");
                    ExitCode::from(1)
                }
            },
            None => {
                eprintln!("gnomon-bridge: --install-manifest requires an extension ID");
                ExitCode::from(1)
            }
        },
        Some(unknown) => {
            eprintln!("gnomon-bridge: unrecognised argument `{unknown}`");
            ExitCode::from(1)
        }
        None => run(),
    }
}

/// Build the host manifest for a given extension origin.
fn manifest_json(origin: &str) -> Result<String, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("cannot resolve current executable: {e}"))?;

    let manifest = serde_json::json!({
        "name": HOST_NAME,
        "description": HOST_DESCRIPTION,
        "path": exe.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [origin],
    });

    serde_json::to_string_pretty(&manifest).map_err(|e| format!("cannot serialize manifest: {e}"))
}

/// Print the Chrome native messaging host manifest. Installs nothing.
fn print_manifest() -> Result<(), String> {
    println!("{}", manifest_json(ALLOWED_ORIGIN)?);
    Ok(())
}

/// A Chrome extension ID is exactly 32 characters drawn from `a`–`p`.
fn validate_extension_id(id: &str) -> Result<(), String> {
    let count = id.chars().count();
    if count != 32 {
        return Err(format!(
            "extension ID must be exactly 32 characters, got {count}"
        ));
    }

    if let Some(bad) = id.chars().find(|c| !matches!(c, 'a'..='p')) {
        return Err(format!(
            "extension ID must use only lowercase letters a-p, found `{bad}`"
        ));
    }

    Ok(())
}

/// Write the host manifest into Chrome's NativeMessagingHosts directory.
fn install_manifest(extension_id: &str) -> Result<(), String> {
    validate_extension_id(extension_id)?;

    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let dir = std::path::Path::new(&home)
        .join(".config")
        .join("google-chrome")
        .join("NativeMessagingHosts");

    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;

    let path = dir.join(format!("{HOST_NAME}.json"));
    let text = manifest_json(&format!("chrome-extension://{extension_id}/"))?;

    std::fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;

    println!("wrote {}", path.display());
    println!("{text}");
    Ok(())
}

/// Pump frames until Chrome closes the port.
fn run() -> ExitCode {
    let socket = match ipc::socket_path() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("gnomon-bridge: {e}");
            return ExitCode::from(1);
        }
    };

    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();

    loop {
        match read_frame(&mut stdin) {
            // Chrome closed the port.
            Ok(None) => return ExitCode::SUCCESS,
            Ok(Some(payload)) => {
                let ack = handle_payload(&payload, &socket);
                if write_ack(&mut stdout, &ack).is_err() {
                    eprintln!("gnomon-bridge: stdout closed, exiting");
                    return ExitCode::SUCCESS;
                }
            }
            Err(reason) => {
                // The stream is desynchronized; acknowledge, then stop.
                eprintln!("gnomon-bridge: {reason}");
                let _ = write_ack(&mut stdout, &Ack::err(&reason));
                return ExitCode::from(1);
            }
        }
    }
}

/// Validate a payload and forward it verbatim.
fn handle_payload(payload: &[u8], socket: &std::path::Path) -> Ack {
    let Ok(text) = std::str::from_utf8(payload) else {
        return Ack::err("payload is not valid UTF-8");
    };

    if parse_snapshot(text).is_err() {
        return Ack::err("payload is not a usage document");
    }

    // Forward the original text, not a re-serialization, so the GUI parses
    // exactly what Chrome saw.
    if let Err(e) = ipc::send(socket, text) {
        // The GUI simply is not running. Not a protocol failure.
        eprintln!("gnomon-bridge: not forwarded ({e})");
    }

    Ack::ok()
}

/// An acknowledgement written back to Chrome.
///
/// Built as text rather than via `serde_json::json!` so `ok` stays the first
/// key: serde_json's default map is sorted, and preserving insertion order
/// would require pulling in the `preserve_order` feature. The reason string is
/// still escaped by serde_json, so an arbitrary message cannot break the frame.
struct Ack(String);

impl Ack {
    fn ok() -> Self {
        Ack(r#"{"ok":true}"#.to_string())
    }

    fn err(reason: &str) -> Self {
        let escaped = serde_json::to_string(reason).unwrap_or_else(|_| "\"unknown\"".to_string());
        Ack(format!(r#"{{"ok":false,"error":{escaped}}}"#))
    }
}

fn write_ack(out: &mut impl Write, ack: &Ack) -> std::io::Result<()> {
    out.write_all(&nm::encode_frame(&ack.0))?;
    out.flush()
}

/// Read one frame. `Ok(None)` is a clean EOF at a frame boundary.
fn read_frame(input: &mut impl Read) -> Result<Option<Vec<u8>>, String> {
    let mut prefix = [0u8; 4];
    let mut filled = 0;

    while filled < 4 {
        match input.read(&mut prefix[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err("truncated length prefix".to_string()),
            Ok(n) => filled += n,
            Err(e) => return Err(format!("stdin read failed: {e}")),
        }
    }

    let len = nm::decode_length(prefix).map_err(|e| e.to_string())?;

    let mut payload = vec![0u8; len];
    input
        .read_exact(&mut payload)
        .map_err(|e| format!("truncated payload: {e}"))?;

    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::validate_extension_id;

    /// 32 characters, all within a-p.
    const VALID: &str = "abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn accepts_a_valid_id() {
        assert_eq!(VALID.len(), 32);
        assert!(validate_extension_id(VALID).is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_extension_id("").is_err());
    }

    #[test]
    fn rejects_thirty_one_characters() {
        let short = &VALID[..31];
        assert_eq!(short.len(), 31);
        assert!(validate_extension_id(short).is_err());
    }

    #[test]
    fn rejects_thirty_three_characters() {
        let long = format!("{VALID}a");
        assert_eq!(long.len(), 33);
        assert!(validate_extension_id(&long).is_err());
    }

    #[test]
    fn rejects_uppercase() {
        let upper = VALID.to_uppercase();
        assert_eq!(upper.len(), 32);
        assert!(validate_extension_id(&upper).is_err());
    }

    #[test]
    fn rejects_letter_outside_a_to_p() {
        // Correct length, lowercase, but 'z' is out of range.
        let with_z = format!("z{}", &VALID[1..]);
        assert_eq!(with_z.len(), 32);
        assert!(validate_extension_id(&with_z).is_err());
    }
}
