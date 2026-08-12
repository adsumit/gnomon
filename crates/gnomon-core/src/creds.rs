//! OAuth credential discovery.
//!
//! [`Token`] deliberately has no derived `Debug` — both `Debug` and `Display`
//! are implemented by hand to redact the secret, so a token cannot reach a log
//! line or a crash dump through ordinary formatting.

use std::fmt;

use chrono::{DateTime, Utc};

use crate::error::SourceError;

/// An OAuth access token. Never rendered in full by any formatting impl.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    /// The secret, for use in an `Authorization` header.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Where a credential was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredSource {
    Env,
    File,
}

/// A discovered credential.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub token: Token,
    pub expires_at: Option<DateTime<Utc>>,
    pub source: CredSource,
}

/// Parse the contents of `.credentials.json`.
///
/// Pure — performs no I/O. Invalid JSON and a missing or non-string
/// `accessToken` both yield [`SourceError::CredentialsMalformed`], never
/// [`SourceError::Parse`], which is reserved for usage payloads.
pub fn parse_credentials_json(s: &str) -> Result<Credentials, SourceError> {
    let root: serde_json::Value = serde_json::from_str(s)
        .map_err(|e| SourceError::CredentialsMalformed(format!("invalid JSON: {e}")))?;

    let oauth = root.get("claudeAiOauth").ok_or_else(|| {
        SourceError::CredentialsMalformed("missing `claudeAiOauth` object".to_string())
    })?;

    let token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            SourceError::CredentialsMalformed(
                "missing or non-string `claudeAiOauth.accessToken`".to_string(),
            )
        })?;

    // Optional. Absent or unusable values simply mean "no known expiry".
    let expires_at = oauth
        .get("expiresAt")
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .and_then(DateTime::from_timestamp_millis);

    Ok(Credentials {
        token: Token(token.to_string()),
        expires_at,
        source: CredSource::File,
    })
}

/// Locate a credential: environment first, then the credentials file.
///
/// Expiry is not checked here — that is the caller's decision.
pub fn discover_credentials() -> Result<Credentials, SourceError> {
    if let Ok(raw) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !raw.is_empty() {
            return Ok(Credentials {
                token: Token(raw),
                expires_at: None,
                source: CredSource::Env,
            });
        }
    }

    let home = std::env::var("HOME").map_err(|_| SourceError::NoToken)?;
    let path = std::path::Path::new(&home)
        .join(".claude")
        .join(".credentials.json");

    let contents = std::fs::read_to_string(&path).map_err(|_| SourceError::NoToken)?;
    parse_credentials_json(&contents)
}
