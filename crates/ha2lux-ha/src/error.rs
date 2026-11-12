//! Home Assistant client errors. **No message contains the token**: errors
//! from the HTTP library have their URL stripped before being returned.

use thiserror::Error;

/// Maximum accepted response size (256 KiB).
pub const MAX_BODY: usize = 256 * 1024;

/// Failure of an exchange with Home Assistant.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum HaError {
    /// Token rejected (401/403, `auth_invalid`). No point retrying in a loop.
    #[error("authentication refused: token invalid or revoked")]
    Auth,
    /// The entity does not exist (404).
    #[error("entity not found")]
    NotFound,
    /// Redirect: never followed, so the token is not sent elsewhere.
    #[error("redirect refused: the token is never sent to another address")]
    Redirect,
    /// Response larger than 256 KiB.
    #[error("response too large (over 256 KiB)")]
    TooLarge,
    /// Invalid certificate or TLS error: there is no bypass.
    #[error("TLS error: {0}")]
    Tls(String),
    /// Timeout exceeded (8 s).
    #[error("timed out")]
    Timeout,
    /// Connection impossible (resolution, refusal, drop).
    #[error("connection failed: {0}")]
    Connection(String),
    /// Response the protocol does not allow interpreting.
    #[error("unexpected response: {0}")]
    Protocol(String),
}

impl HaError {
    /// Authentication failure: the client must stay quiet for a long time (a Home
    /// Assistant server bans addresses that fail too often).
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Auth)
    }

    /// Localizable message for a client (catalog key + arguments); see
    /// [`ha2lux_i18n::wire`]. `Display` remains the text for logs.
    pub fn to_wire(&self) -> String {
        use ha2lux_i18n::wire::encode;
        match self {
            Self::Auth => encode("err-ha-auth", &[]),
            Self::NotFound => encode("err-ha-not-found", &[]),
            Self::Redirect => encode("err-ha-redirect", &[]),
            Self::TooLarge => encode("err-ha-too-large", &[]),
            Self::Tls(detail) => encode("err-ha-tls", &[("detail", detail)]),
            Self::Timeout => encode("err-ha-timeout", &[]),
            Self::Connection(detail) => encode("err-ha-connection", &[("detail", detail)]),
            Self::Protocol(detail) => encode("err-ha-protocol", &[("detail", detail)]),
        }
    }
}

/// Classifies a `reqwest` error without ever exposing a URL or header.
pub(crate) fn from_reqwest(error: reqwest::Error) -> HaError {
    let error = error.without_url();
    if error.is_timeout() {
        return HaError::Timeout;
    }
    if error.is_redirect() {
        return HaError::Redirect;
    }
    let mut chain = String::new();
    let mut source: Option<&dyn std::error::Error> = Some(&error);
    while let Some(e) = source {
        chain.push_str(&e.to_string());
        chain.push(' ');
        source = e.source();
    }
    let lower = chain.to_lowercase();
    if [
        "certificate",
        "unknownissuer",
        "invalid peer",
        "tls",
        "handshake",
        "rustls",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        return HaError::Tls(short(&chain));
    }
    if error.is_connect() || error.is_request() {
        return HaError::Connection(short(&chain));
    }
    HaError::Protocol(short(&chain))
}

/// Truncates an error message for the log.
pub(crate) fn short(text: &str) -> String {
    let text = text.trim();
    let mut out: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        out.push('…');
    }
    out
}
