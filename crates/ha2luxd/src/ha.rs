//! Bridge between the daemon and Home Assistant: token (keyring) and measurement source.
//!
//! Two small traits make the daemon testable without network or keyring:
//! [`Tokens`] (token storage) and [`Sources`] (starting the measurement source).
//! The real implementations rely on `ha2lux-secrets` and `ha2lux-ha`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ha2lux_ha::config::{HaConfig, Token};
use ha2lux_ha::sensors::{SensorInfo, list_illuminance_sensors};
use ha2lux_ha::source::{HaLink, HaSource, SourceConfig, spawn, system_unix_ms};
use ha2lux_secrets::SecretStore;

/// Heap-allocated future (the traits below must remain usable as `dyn`).
pub type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Token storage, indexed by `host:port`. Errors never contain the token.
pub trait Tokens: Send + Sync {
    /// Stored token.
    fn get(&self, key: &str) -> BoxFut<Result<Option<String>, String>>;
    /// Stores the token.
    fn set(&self, key: &str, token: &str) -> BoxFut<Result<(), String>>;
    /// Deletes the token; `true` if it existed.
    fn delete(&self, key: &str) -> BoxFut<Result<bool, String>>;
}

/// Factory of the measurement source.
pub trait Sources: Send + Sync {
    /// Starts the source (the token is consumed).
    fn start(&self, url: &str, entity: &str, token: String) -> Result<HaSource, String>;
    /// Lists the illuminance sensors.
    fn sensors(&self, url: &str, token: String) -> BoxFut<Result<Vec<SensorInfo>, String>>;
}

/// What the daemon knows about Home Assistant.
#[derive(Clone)]
pub struct HaDeps {
    /// Token.
    pub tokens: Arc<dyn Tokens>,
    /// Measurement source.
    pub sources: Arc<dyn Sources>,
}

/// Adapter of a [`SecretStore`] into [`Tokens`].
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

/// Real source: REST + WebSocket.
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
