//! Pont entre le démon et Home Assistant : jeton (trousseau) et source de mesures.
//!
//! Deux petits traits rendent le démon testable sans réseau ni trousseau :
//! [`Tokens`] (stockage du jeton) et [`Sources`] (démarrage de la source de mesures).
//! Les implémentations réelles s'appuient sur `ha2lux-secrets` et `ha2lux-ha`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ha2lux_ha::config::{HaConfig, Token};
use ha2lux_ha::sensors::{SensorInfo, list_illuminance_sensors};
use ha2lux_ha::source::{HaLink, HaSource, SourceConfig, spawn, system_unix_ms};
use ha2lux_secrets::SecretStore;

/// Futur sur le tas (les traits ci-dessous doivent rester utilisables en `dyn`).
pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Stockage du jeton, indexé par `hôte:port`. Les erreurs ne contiennent jamais le jeton.
pub trait Tokens: Send + Sync {
    /// Jeton enregistré.
    fn get(&self, key: &str) -> BoxFut<Result<Option<String>, String>>;
    /// Enregistre le jeton.
    fn set(&self, key: &str, token: &str) -> BoxFut<Result<(), String>>;
    /// Supprime le jeton ; `true` s'il existait.
    fn delete(&self, key: &str) -> BoxFut<Result<bool, String>>;
}

/// Fabrique de la source de mesures.
pub trait Sources: Send + Sync {
    /// Démarre la source (le jeton est consommé).
    fn start(&self, url: &str, entity: &str, token: String) -> Result<HaSource, String>;
    /// Liste les capteurs d'éclairement.
    fn sensors(&self, url: &str, token: String) -> BoxFut<Result<Vec<SensorInfo>, String>>;
}

/// Ce que le démon sait de Home Assistant.
#[derive(Clone)]
pub struct HaDeps {
    /// Jeton.
    pub tokens: Arc<dyn Tokens>,
    /// Source de mesures.
    pub sources: Arc<dyn Sources>,
}

/// Adaptateur d'un [`SecretStore`] en [`Tokens`].
pub struct SecretTokens<S>(pub Arc<S>);

impl<S: SecretStore + 'static> Tokens for SecretTokens<S> {
    fn get(&self, key: &str) -> BoxFut<Result<Option<String>, String>> {
        let (store, key) = (Arc::clone(&self.0), key.to_owned());
        Box::pin(async move { store.get(&key).await.map_err(|e| e.to_wire()) })
    }
    fn set(&self, key: &str, token: &str) -> BoxFut<Result<(), String>> {
        let (store, key, token) = (Arc::clone(&self.0), key.to_owned(), token.to_owned());
        Box::pin(async move {
            store
                .set(&key, &token)
                .await
                .map(|_| ())
                .map_err(|e| e.to_wire())
        })
    }
    fn delete(&self, key: &str) -> BoxFut<Result<bool, String>> {
        let (store, key) = (Arc::clone(&self.0), key.to_owned());
        Box::pin(async move { store.delete(&key).await.map_err(|e| e.to_wire()) })
    }
}

/// Source réelle : REST + WebSocket.
pub struct RealSources;

impl Sources for RealSources {
    fn start(&self, url: &str, entity: &str, token: String) -> Result<HaSource, String> {
        let config = HaConfig::new(url, entity, Token::new(token)).map_err(|e| e.to_string())?;
        let link = HaLink::new(config).map_err(|e| e.to_string())?;
        Ok(spawn(
            link,
            SourceConfig::default(),
            Arc::new(system_unix_ms),
        ))
    }

    fn sensors(&self, url: &str, token: String) -> BoxFut<Result<Vec<SensorInfo>, String>> {
        let url = url.to_owned();
        Box::pin(async move {
            let config = HaConfig::new(&url, "sensor.placeholder", Token::new(token))
                .map_err(|e| e.to_wire())?;
            list_illuminance_sensors(&config)
                .await
                .map_err(|e| e.to_wire())
        })
    }
}
