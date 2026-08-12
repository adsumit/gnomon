//! Credential-parsing and user-agent tests.
//!
//! Nothing here touches the network or reads the real credentials file. The
//! credential document below is hand-written and its "token" is not a secret.

use chrono::Datelike;
use gnomon_core::creds::{parse_credentials_json, CredSource};
use gnomon_core::oauth::extract_cli_version;
use gnomon_core::SourceError;

/// Hand-written stand-in. `expiresAt` is 2027-01-01T00:00:00Z in milliseconds.
const VALID: &str = r#"{
  "claudeAiOauth": {
    "accessToken": "sk-ant-oat01-EXAMPLE-NOT-A-REAL-TOKEN",
    "expiresAt": 1798761600000,
    "scopes": ["user:inference"]
  }
}"#;

#[test]
fn token_debug_is_redacted() {
    let creds = parse_credentials_json(VALID).expect("valid document must parse");
    let rendered = format!("{:?}", creds.token);

    assert_eq!(rendered, "Token(<redacted>)");
    assert!(
        !rendered.contains("sk-ant-oat01"),
        "Debug must not leak the secret"
    );
    assert!(
        !rendered.contains("EXAMPLE-NOT-A-REAL-TOKEN"),
        "Debug must not leak the secret"
    );
}

#[test]
fn token_display_is_redacted() {
    let creds = parse_credentials_json(VALID).expect("valid document must parse");
    let rendered = format!("{}", creds.token);

    assert_eq!(rendered, "<redacted>");
    assert!(
        !rendered.contains("sk-ant-oat01"),
        "Display must not leak the secret"
    );
}

#[test]
fn credentials_debug_does_not_leak_token() {
    let creds = parse_credentials_json(VALID).expect("valid document must parse");
    let rendered = format!("{creds:?}");

    assert!(
        !rendered.contains("sk-ant-oat01"),
        "deriving Debug on Credentials must not expose the token"
    );
}

#[test]
fn parse_credentials_valid_document() {
    let creds = parse_credentials_json(VALID).expect("valid document must parse");

    assert_eq!(creds.token.as_str(), "sk-ant-oat01-EXAMPLE-NOT-A-REAL-TOKEN");
    assert_eq!(creds.source, CredSource::File);

    let expires_at = creds.expires_at.expect("expiresAt must be parsed");
    assert_eq!(expires_at.year(), 2027);
}

#[test]
fn parse_credentials_empty_object_is_malformed() {
    let err = parse_credentials_json("{}").expect_err("empty object must be rejected");
    assert!(
        matches!(err, SourceError::CredentialsMalformed(_)),
        "expected CredentialsMalformed, got {err:?}"
    );
}

#[test]
fn parse_credentials_invalid_json_is_malformed_not_parse() {
    let err = parse_credentials_json("not json").expect_err("invalid JSON must be rejected");
    assert!(
        matches!(err, SourceError::CredentialsMalformed(_)),
        "invalid JSON must be CredentialsMalformed, not Parse; got {err:?}"
    );
}

#[test]
fn extract_cli_version_cases() {
    assert_eq!(extract_cli_version("2.1.4\n"), Some("2.1.4".to_string()));
    assert_eq!(
        extract_cli_version("2.1.4 (Claude Code)\n"),
        Some("2.1.4".to_string())
    );
    assert_eq!(extract_cli_version("claude version 2.1.4"), None);
    assert_eq!(extract_cli_version(""), None);
    assert_eq!(extract_cli_version("\n"), None);
}
