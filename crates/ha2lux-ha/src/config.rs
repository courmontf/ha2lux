//! Configuration du client : adresse validée, entité validée, jeton **opaque**.

use std::fmt;
use std::time::Duration;

use ha2lux_core::config::is_valid_entity_id;
use ha2lux_core::url::{UrlError, check_url};
use reqwest::Url;
use thiserror::Error;

/// Jeton d'accès longue durée. N'implémente ni `Display` ni un `Debug` révélateur : il
/// ne peut pas finir dans un journal ou un message d'erreur par accident.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    /// Enveloppe un jeton.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Valeur du jeton, pour l'en-tête d'autorisation **uniquement**.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Le jeton est vide ou ne contient que des espaces.
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// Délais d'une requête (8 s par défaut : résolution, connexion, envoi, réception).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Établissement de la connexion.
    pub connect: Duration,
    /// Requête complète.
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

/// Erreur de configuration.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// Adresse refusée.
    #[error("adresse de Home Assistant invalide : {0}")]
    Url(#[from] UrlError),
    /// Identifiant d'entité invalide.
    #[error("identifiant d'entité invalide (attendu : domaine.objet)")]
    Entity,
    /// Jeton absent.
    #[error("jeton d'accès absent")]
    NoToken,
}

impl ConfigError {
    /// Message localisable pour un client (clé de catalogue + arguments) ; voir
    /// [`ha2lux_i18n::wire`]. Le `Display` reste le texte pour les journaux.
    pub fn to_wire(&self) -> String {
        use ha2lux_i18n::wire::encode;
        match self {
            Self::Url(error) => encode("err-ha-config-url", &[("detail", error)]),
            Self::Entity => encode("err-ha-config-entity", &[]),
            Self::NoToken => encode("err-ha-config-no-token", &[]),
        }
    }
}

/// Paramètres de connexion à Home Assistant.
#[derive(Clone)]
pub struct HaConfig {
    /// Adresse de base (http ou https, sans identifiants).
    pub base_url: Url,
    /// Entité du capteur (`sensor.xxx`).
    pub entity: String,
    /// Jeton d'accès.
    pub token: Token,
    /// Délais.
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
    /// Valide et construit la configuration (même règles que la configuration du démon).
    pub fn new(url: &str, entity: &str, token: Token) -> Result<Self, ConfigError> {
        let checked = check_url(url)?;
        // `check_url` a validé la forme ; l'analyse ne peut échouer que pour une raison que
        // la validation couvre déjà, on la traite tout de même comme une adresse invalide.
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

    /// Adresse du point d'accès REST de l'entité.
    pub fn state_url(&self) -> Url {
        let mut url = self.base_url.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .pop_if_empty()
                .extend(["api", "states", &self.entity]);
        }
        url
    }

    /// Adresse du WebSocket (`ws://` ou `wss://`, selon le schéma de base).
    pub fn websocket_url(&self) -> Url {
        let mut url = self.base_url.clone();
        // Le schéma est http ou https (validé) : le changement ne peut pas échouer.
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        let _ = url.set_scheme(scheme);
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty().extend(["api", "websocket"]);
        }
        url
    }
}
