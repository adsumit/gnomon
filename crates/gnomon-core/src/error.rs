//! Error type for credential discovery and the OAuth transport.
//!
//! No `Display` message in this module may contain a token or any part of an
//! `Authorization` header.

use thiserror::Error;

/// A failure while obtaining or fetching usage data.
#[derive(Debug, Error)]
pub enum SourceError {
    /// Neither the environment variable nor the credentials file yielded a token.
    #[error("no OAuth token found: set CLAUDE_CODE_OAUTH_TOKEN or run `claude` to sign in")]
    NoToken,

    /// The stored credentials carry an `expiresAt` in the past.
    #[error("OAuth token has expired — run `claude` to refresh")]
    TokenExpired,

    /// The credentials file could not be understood.
    #[error("credentials file is malformed: {0}")]
    CredentialsMalformed(String),

    /// The endpoint answered with a non-200 status.
    #[error("usage endpoint returned HTTP {0}")]
    Http(u16),

    /// A network or TLS failure.
    #[error("transport failure: {0}")]
    Transport(String),

    /// The response body was not a usage payload.
    #[error("failed to parse usage payload")]
    Parse(#[source] serde_json::Error),
}
