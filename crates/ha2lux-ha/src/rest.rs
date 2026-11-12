//! Lecture REST : `GET /api/states/<entité>` avec `Authorization: Bearer <jeton>`.
//!
//! Garanties : **aucune redirection suivie** (le jeton ne part jamais ailleurs), TLS
//! toujours validé (magasin de certificats du système, aucun contournement), délais de
//! 8 s, réponse bornée à 256 Kio, proxy du système respecté.

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};

use crate::config::HaConfig;
use crate::error::{HaError, MAX_BODY, from_reqwest};
use crate::reading::{Reading, parse_state_body};

/// Client REST d'une entité.
pub struct RestClient {
    client: Client,
    config: HaConfig,
}

impl RestClient {
    /// Crée le client.
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

    /// Lit l'état courant de l'entité.
    pub async fn read(&self) -> Result<Reading, HaError> {
        let mut bearer =
            HeaderValue::from_str(&format!("Bearer {}", self.config.token.expose()))
                .map_err(|_| HaError::Protocol("jeton inutilisable dans un en-tête".to_owned()))?;
        bearer.set_sensitive(true); // jamais affiché, même par les journaux de la bibliothèque
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
