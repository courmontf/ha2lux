//! Client configuration: validated address, validated entity, **opaque** token.

use std::fmt;
use std::time::Duration;

use ha2lux_core::config::is_valid_entity_id;
use ha2lux_core::url::{UrlError, check_url};
use reqwest::Url;
use thiserror::Error;

/// Long-lived access token. Implements neither `Display` nor a revealing `Debug`: it
/// cannot end up in a log or an error message by accident.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    /// Wraps a token.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Token value, for the authorization header **only**.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The token is empty or contains only whitespace.
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// Request timeouts (8 s by default: resolution, connection, send, receive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Connection establishment.
    pub connect: Duration,
    /// Whole request.
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(8),
            request: Duration::from_secs(8),
        }
    }
}

/// Configuration error.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// Address rejected.
    #[error("invalid Home Assistant address: {0}")]
    Url(#[from] UrlError),
    /// Invalid entity identifier.
    #[error("invalid entity identifier (expected: domain.object)")]
    Entity,
    /// Token missing.
    #[error("access token missing")]
    NoToken,
}

impl ConfigError {
    /// Localizable message for a client (catalog key + arguments); see
    /// [`ha2lux_i18n::wire`]. `Display` remains the text for logs.
    pub fn to_wire(&self) -> String {
        use ha2lux_i18n::wire::encode;
        match self {
            Self::Url(error) => encode("err-ha-config-url", &[("detail", error)]),
            Self::Entity => encode("err-ha-config-entity", &[]),
            Self::NoToken => encode("err-ha-config-no-token", &[]),
        }
    }
}

/// Connection parameters for Home Assistant.
#[derive(Clone)]
pub struct HaConfig {
    /// Base address (http or https, no credentials).
    pub base_url: Url,
    /// Sensor entity (`sensor.xxx`).
    pub entity: String,
    /// Access token.
    pub token: Token,
    /// Timeouts.
    pub timeouts: Timeouts,
}

impl fmt::Debug for HaConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HaConfig")
            .field("host", &self.base_url.host_str())
            .field("entity", &self.entity)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl HaConfig {
    /// Validates and builds the configuration (same rules as the daemon configuration).
    pub fn new(url: &str, entity: &str, token: Token) -> Result<Self, ConfigError> {
        let checked = check_url(url)?;
        // `check_url` validated the form; parsing can only fail for a reason that
        // validation already covers, so we still treat it as an invalid address.
        let base_url = Url::parse(&checked.url).map_err(|_| UrlError::InvalidHost)?;
        if !is_valid_entity_id(entity) {
            return Err(ConfigError::Entity);
        }
        if token.is_blank() {
            return Err(ConfigError::NoToken);
        }
        Ok(Self {
            base_url,
            entity: entity.to_owned(),
            token,
            timeouts: Timeouts::default(),
        })
    }

    /// Address of the entity's REST endpoint.
    pub fn state_url(&self) -> Url {
        let mut url = self.base_url.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .pop_if_empty()
                .extend(["api", "states", &self.entity]);
        }
        url
    }

    /// WebSocket address (`ws://` or `wss://`, depending on the base scheme).
    pub fn websocket_url(&self) -> Url {
        let mut url = self.base_url.clone();
        // The scheme is http or https (validated): the switch cannot fail.
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        let _ = url.set_scheme(scheme);
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty().extend(["api", "websocket"]);
        }
        url
    }
}
