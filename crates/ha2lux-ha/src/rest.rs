//! REST read: `GET /api/states/<entity>` with `Authorization: Bearer <token>`.
//!
//! Guarantees: **no redirect followed** (the token is never sent elsewhere), TLS
//! always validated (system certificate store, no bypass), timeouts of
//! 8 s, response capped at 256 KiB, system proxy honored.

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};

use crate::config::HaConfig;
use crate::error::{HaError, MAX_BODY, from_reqwest};
use crate::reading::{Reading, parse_state_body};

/// REST client for an entity.
pub struct RestClient {
    client: Client,
    config: HaConfig,
}

impl RestClient {
    /// Creates the client.
    pub fn new(config: &HaConfig) -> Result<Self, HaError> {
        let client = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(config.timeouts.connect)
            .timeout(config.timeouts.request)
            .user_agent(concat!("ha2lux/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(from_reqwest)?;
        Ok(Self {
            client,
            config: config.clone(),
        })
    }

    /// Reads the entity's current state.
    pub async fn read(&self) -> Result<Reading, HaError> {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {}", self.config.token.expose()))
            .map_err(|_| HaError::Protocol("token unusable in a header".to_owned()))?;
        bearer.set_sensitive(true); // never displayed, even by the library's logs
        let mut response = self
            .client
            .get(self.config.state_url())
            .header(AUTHORIZATION, bearer)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(from_reqwest)?;
        let status = response.status();
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(HaError::Auth),
            StatusCode::NOT_FOUND => return Err(HaError::NotFound),
            s if s.is_redirection() => return Err(HaError::Redirect),
            s if !s.is_success() => {
                return Err(HaError::Protocol(format!("code HTTP {}", s.as_u16())));
            }
            _ => {}
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BODY as u64)
        {
            return Err(HaError::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(from_reqwest)? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(HaError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        parse_state_body(&body)
    }
}
