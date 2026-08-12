//! Chrome native messaging host.
//!
//! Reads length-prefixed frames from stdin, validates each as a usage payload,
//! and forwards the original text to the GUI over the Unix domain socket.
//!
//! stdout is the protocol channel and carries nothing but acknowledgement
//! frames. Diagnostics go to stderr, which Chrome discards — set
//! `GNOMON_BRIDGE_LOG` to a path to get a durable record instead.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::process::ExitCode;

use gnomon_core::chrono::Utc;
use gnomon_core::{ipc, nm, parse_snapshot};

const HOST_NAME: &str = "com.gnomon.bridge";
const HOST_DESCRIPTION: &str = "gnomon usage bridge";
/// Placeholder — the real id is only known once the extension is packed.
const ALLOWED_ORIGIN: &str = "chrome-extension://REPLACE_WITH_EXTENSION_ID/";
/// Chrome identifies the calling extension with an argument of this form.
const CHROME_EXTENSION_PREFIX: &str = "chrome-extension://";
/// Env var naming a file to append host events to. Unset means no file logging.
const LOG_ENV: &str = "GNOMON_BRIDGE_LOG";

const USAGE: &str = "\
gnomon-bridge — Chrome native messaging host for the gnomon usage meter.

USAGE:
    gnomon-bridge                            read native messaging frames on stdin
    gnomon-bridge chrome-extension://<ID>/   same; Chrome passes the caller's origin
    gnomon-bridge --print-manifest           print the host manifest
    gnomon-bridge --install-manifest <ID>    write the host manifest for an extension
    gnomon-bridge --help                     show this message

ENVIRONMENT:
    GNOMON_BRIDGE_LOG   append host events to this file (Chrome discards stderr)";

/// How this invocation was asked to behave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Stdio { origin: Option<String> },
    PrintManifest,
    InstallManifest(String),
    Help,
}

/// Classify the command line. Pure: no I/O, no exits.
///
/// `args` excludes argv[0].
pub fn parse_args(args: &[String]) -> Result<Mode, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    match args.as_slice() {
        [] => Ok(Mode::Stdio { origin: None }),

        ["--help"] | ["-h"] => Ok(Mode::Help),

        ["--print-manifest"] => Ok(Mode::PrintManifest),

        ["--install-manifest"] => {
            Err("--install-manifest requires an extension ID".to_string())
        }
        ["--install-manifest", id] => {
            validate_extension_id(id)?;
            Ok(Mode::InstallManifest((*id).to_string()))
        }

        // Chrome launches the host with the caller's origin as the only
        // argument on Linux and macOS; on Windows it appends a second argument
        // (the parent window handle). Trailing arguments are ignored rather
        // than rejected: exiting on an unrecognised extra argument is exactly
        // the failure this arm exists to prevent, and it is invisible because
        // Chrome discards stderr.
        [origin, ..] if origin.starts_with(CHROME_EXTENSION_PREFIX) => Ok(Mode::Stdio {
            origin: Some((*origin).to_string()),
        }),

        _ => Err(format!("unrecognised arguments: {}", args.join(" "))),
    }
}

/// Append-only event log, enabled by `GNOMON_BRIDGE_LOG`.
///
/// Every operation is best-effort: a log that cannot be opened or written must
/// never take the host down.
struct Log {
    file: Option<std::fs::File>,
}

impl Log {
    fn open() -> Self {
        let file = std::env::var(LOG_ENV)
            .ok()
            .filter(|path| !path.is_empty())
            .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok());

        Log { file }
    }

    /// Write one timestamped line. Payload contents are never passed here.
    fn line(&mut self, message: &str) {
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{} {}", Utc::now().to_rfc3339(), message);
            let _ = file.flush();
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log = Log::open();

    match parse_args(&args) {
        Ok(Mode::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Mode::PrintManifest) => {
            log.line("startup mode=PrintManifest");
            finish(print_manifest(), &mut log)
        }
        Ok(Mode::InstallManifest(id)) => {
            log.line(&format!("startup mode=InstallManifest id={id}"));
            finish(install_manifest(&id), &mut log)
        }
        Ok(Mode::Stdio { origin }) => {
            let shown = origin.as_deref().unwrap_or("<none>");
            eprintln!("gnomon-bridge: stdio mode, origin {shown}");
            log.line(&format!("startup mode=Stdio origin={shown}"));
            run(&mut log)
        }
        Err(reason) => {
            eprintln!("gnomon-bridge: {reason}");
            log.line(&format!("exit code=1 reason={reason}"));
            ExitCode::from(1)
        }
    }
}

/// Report a one-shot subcommand's outcome.
fn finish(result: Result<(), String>, log: &mut Log) -> ExitCode {
    match result {
        Ok(()) => {
            log.line("exit code=0");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gnomon-bridge: {e}");
            log.line(&format!("exit code=1 reason={e}"));
            ExitCode::from(1)
        }
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

    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;

    let path = dir.join(format!("{HOST_NAME}.json"));
    let text = manifest_json(&format!("chrome-extension://{extension_id}/"))?;

    std::fs::write(&path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;

    println!("wrote {}", path.display());
    println!("{text}");
    Ok(())
}

/// Pump frames until Chrome closes the port.
///
/// Every well-formed frame is answered with exactly one ack before the next
/// read. No condition short of stdout itself closing ends the loop early.
fn run(log: &mut Log) -> ExitCode {
    let socket = match ipc::socket_path() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("gnomon-bridge: {e}");
            log.line(&format!("exit code=1 reason={e}"));
            return ExitCode::from(1);
        }
    };

    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();

    loop {
        match read_frame(&mut stdin) {
            // Chrome closed the port.
            Ok(None) => {
                log.line("stdin eof");
                log.line("exit code=0");
                return ExitCode::SUCCESS;
            }
            Ok(Some(payload)) => {
                log.line(&format!("frame received bytes={}", payload.len()));

                let ack = handle_payload(&payload, &socket, log);

                match write_ack(&mut stdout, &ack) {
                    Ok(()) => log.line(&format!("ack written ok={}", ack.is_ok())),
                    Err(_) => {
                        eprintln!("gnomon-bridge: stdout closed, exiting");
                        log.line("ack failed stdout closed");
                        log.line("exit code=0");
                        return ExitCode::SUCCESS;
                    }
                }
            }
            Err(reason) => {
                // Not a well-formed frame: the stream is desynchronized and
                // there is no reliable way to find the next boundary.
                eprintln!("gnomon-bridge: {reason}");
                log.line(&format!("frame error reason={reason}"));
                let _ = write_ack(&mut stdout, &Ack::err(&reason));
                log.line("exit code=1");
                return ExitCode::from(1);
            }
        }
    }
}

/// Validate a payload and forward it verbatim.
///
/// A missing GUI is reported to Chrome rather than swallowed, so the extension's
/// stats distinguish "host unreachable" from "host ran, nobody listening".
fn handle_payload(payload: &[u8], socket: &std::path::Path, log: &mut Log) -> Ack {
    let Ok(text) = std::str::from_utf8(payload) else {
        log.line("parse outcome=not-utf8");
        return Ack::err("payload is not valid UTF-8");
    };

    if parse_snapshot(text).is_err() {
        log.line("parse outcome=not-a-usage-document");
        return Ack::err("payload is not a usage document");
    }
    log.line("parse outcome=ok");

    // Forward the original text, not a re-serialization, so the GUI parses
    // exactly what Chrome delivered.
    match ipc::send(socket, text) {
        Ok(()) => {
            log.line("ipc send outcome=ok");
            Ack::ok()
        }
        Err(e) => {
            eprintln!("gnomon-bridge: not forwarded ({e})");
            log.line(&format!("ipc send outcome=failed reason={e}"));
            Ack::err("gui not running")
        }
    }
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

    fn is_ok(&self) -> bool {
        self.0 == r#"{"ok":true}"#
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
    use super::{parse_args, validate_extension_id, Mode};

    /// 32 characters, all within a-p.
    const VALID: &str = "abcdefghijklmnopabcdefghijklmnop";
    const ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop/";

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    // ---- validate_extension_id ----

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

    // ---- parse_args ----

    #[test]
    fn no_arguments_is_stdio_without_origin() {
        assert_eq!(
            parse_args(&args(&[])).expect("empty args must parse"),
            Mode::Stdio { origin: None }
        );
    }

    #[test]
    fn chrome_origin_is_stdio_with_origin() {
        assert_eq!(
            parse_args(&args(&[ORIGIN])).expect("an origin must parse"),
            Mode::Stdio {
                origin: Some(ORIGIN.to_string())
            }
        );
    }

    #[test]
    fn chrome_origin_tolerates_a_trailing_argument() {
        // Windows appends the parent window handle. Must not error.
        assert_eq!(
            parse_args(&args(&[ORIGIN, "--parent-window=12345"]))
                .expect("a trailing argument must be tolerated"),
            Mode::Stdio {
                origin: Some(ORIGIN.to_string())
            }
        );
    }

    #[test]
    fn print_manifest_flag() {
        assert_eq!(
            parse_args(&args(&["--print-manifest"])).expect("must parse"),
            Mode::PrintManifest
        );
    }

    #[test]
    fn help_flags() {
        assert_eq!(parse_args(&args(&["--help"])).expect("must parse"), Mode::Help);
        assert_eq!(parse_args(&args(&["-h"])).expect("must parse"), Mode::Help);
    }

    #[test]
    fn install_manifest_with_valid_id() {
        assert_eq!(
            parse_args(&args(&["--install-manifest", VALID])).expect("must parse"),
            Mode::InstallManifest(VALID.to_string())
        );
    }

    #[test]
    fn install_manifest_without_id_errors() {
        assert!(parse_args(&args(&["--install-manifest"])).is_err());
    }

    #[test]
    fn install_manifest_with_invalid_id_errors() {
        assert!(parse_args(&args(&["--install-manifest", "TOO-SHORT"])).is_err());
        assert!(parse_args(&args(&["--install-manifest", &VALID.to_uppercase()])).is_err());
    }

    #[test]
    fn unknown_flag_errors() {
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["https://example.com/"])).is_err());
    }
}
