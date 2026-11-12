//! Erreurs du client Home Assistant. **Aucun message ne contient le jeton** : les
//! erreurs de la bibliothèque HTTP sont nettoyées de leur URL avant d'être rendues.

use thiserror::Error;

/// Taille maximale acceptée pour une réponse (256 Kio).
pub const MAX_BODY: usize = 256 * 1024;

/// Échec d'un échange avec Home Assistant.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum HaError {
    /// Jeton refusé (401/403, `auth_invalid`). Inutile de réessayer en boucle.
    #[error("authentification refusée : jeton invalide ou révoqué")]
    Auth,
    /// L'entité n'existe pas (404).
    #[error("entité introuvable")]
    NotFound,
    /// Redirection : jamais suivie, pour que le jeton ne parte pas ailleurs.
    #[error("redirection refusée : le jeton n'est jamais renvoyé vers une autre adresse")]
    Redirect,
    /// Réponse de plus de 256 Kio.
    #[error("réponse trop volumineuse (plus de 256 Kio)")]
    TooLarge,
    /// Certificat invalide ou erreur TLS : aucun contournement n'existe.
    #[error("erreur TLS : {0}")]
    Tls(String),
    /// Délai dépassé (8 s).
    #[error("délai dépassé")]
    Timeout,
    /// Connexion impossible (résolution, refus, coupure).
    #[error("connexion impossible : {0}")]
    Connection(String),
    /// Réponse que le protocole ne permet pas d'interpréter.
    #[error("réponse inattendue : {0}")]
    Protocol(String),
}

impl HaError {
    /// Échec d'authentification : le client doit se taire longtemps (un serveur Home
    /// Assistant bannit les adresses qui échouent trop souvent).
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Auth)
    }
}

/// Classe une erreur de `reqwest` sans jamais exposer d'URL ni d'en-tête.
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

/// Tronque un message d'erreur pour le journal.
pub(crate) fn short(text: &str) -> String {
    let text = text.trim();
    let mut out: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        out.push('…');
    }
    out
}
