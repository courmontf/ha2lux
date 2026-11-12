//! Client minimal du Secret Service (`org.freedesktop.secrets`) : trousseau de KDE
//! (KWallet), GNOME Keyring, KeePassXC…
//!
//! Algorithme de session « plain » : le secret transite en clair **sur le bus de session**
//! (socket Unix de l'utilisateur), comme pour la plupart des clients simples. Les éléments
//! sont repérés par les attributs `application=ha2lux`, `kind=ha-token`, `host=<hôte:port>`.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::{SecretError, SecretStore, Stored};

const SERVICE: &str = "org.freedesktop.secrets";
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);
const LABEL_KEY: &str = "org.freedesktop.Secret.Item.Label";
const ATTRIBUTES_KEY: &str = "org.freedesktop.Secret.Item.Attributes";

mod proxies {
    #![allow(missing_docs)]

    use std::collections::HashMap;

    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    #[zbus::proxy(
        interface = "org.freedesktop.Secret.Service",
        default_service = "org.freedesktop.secrets",
        default_path = "/org/freedesktop/secrets"
    )]
    pub trait SecretService {
        fn open_session(
            &self,
            algorithm: &str,
            input: &Value<'_>,
        ) -> zbus::Result<(OwnedValue, OwnedObjectPath)>;
        fn search_items(
            &self,
            attributes: HashMap<&str, &str>,
        ) -> zbus::Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)>;
        fn unlock(
            &self,
            objects: Vec<OwnedObjectPath>,
        ) -> zbus::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)>;
        fn read_alias(&self, name: &str) -> zbus::Result<OwnedObjectPath>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.Secret.Collection",
        default_service = "org.freedesktop.secrets"
    )]
    pub trait SecretCollection {
        fn create_item(
            &self,
            properties: HashMap<&str, Value<'_>>,
            secret: &(OwnedObjectPath, Vec<u8>, Vec<u8>, &str),
            replace: bool,
        ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.Secret.Item",
        default_service = "org.freedesktop.secrets"
    )]
    pub trait SecretItem {
        fn get_secret(
            &self,
            session: &OwnedObjectPath,
        ) -> zbus::Result<(OwnedObjectPath, Vec<u8>, Vec<u8>, String)>;
        fn delete(&self) -> zbus::Result<OwnedObjectPath>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.Secret.Prompt",
        default_service = "org.freedesktop.secrets"
    )]
    pub trait SecretPrompt {
        fn prompt(&self, window_id: &str) -> zbus::Result<()>;
        #[zbus(signal)]
        fn completed(&self, dismissed: bool, result: OwnedValue) -> zbus::Result<()>;
    }
}

use proxies::{SecretCollectionProxy, SecretItemProxy, SecretPromptProxy, SecretServiceProxy};

/// Trousseau du bureau.
pub struct ServiceSecretStore {
    connection: zbus::Connection,
}

fn unavailable(error: &zbus::Error) -> SecretError {
    SecretError::Unavailable(error.to_string())
}

async fn timed<T>(
    future: impl std::future::Future<Output = zbus::Result<T>>,
) -> Result<T, SecretError> {
    match tokio::time::timeout(CALL_TIMEOUT, future).await {
        Ok(result) => result.map_err(|e| unavailable(&e)),
        Err(_) => Err(SecretError::Unavailable(
            "the keyring is not responding".to_owned(),
        )),
    }
}

fn is_none_path(path: &OwnedObjectPath) -> bool {
    path.as_str() == "/"
}

fn attributes(host_key: &str) -> HashMap<&str, &str> {
    HashMap::from([
        ("application", "ha2lux"),
        ("kind", "ha-token"),
        ("host", host_key),
    ])
}

impl ServiceSecretStore {
    /// Se connecte au bus de session de l'utilisateur.
    pub async fn connect_session() -> Result<Self, SecretError> {
        let connection = zbus::Connection::session()
            .await
            .map_err(|e| unavailable(&e))?;
        Self::with_connection(connection).await
    }

    /// Utilise une connexion existante ; échoue si le service n'est pas sur le bus.
    pub async fn with_connection(connection: zbus::Connection) -> Result<Self, SecretError> {
        let dbus = zbus::fdo::DBusProxy::new(&connection)
            .await
            .map_err(|e| unavailable(&e))?;
        let name = zbus::names::BusName::try_from(SERVICE)
            .map_err(|e| SecretError::Unavailable(e.to_string()))?;
        // Le service peut ne démarrer que sur demande (activation D-Bus) : on tente de l'activer.
        let present = dbus.name_has_owner(name.clone()).await.unwrap_or(false)
            || dbus
                .start_service_by_name(
                    SERVICE
                        .try_into()
                        .map_err(|e: zbus::names::Error| SecretError::Unavailable(e.to_string()))?,
                    0,
                )
                .await
                .is_ok();
        if present {
            Ok(Self { connection })
        } else {
            Err(SecretError::Unavailable(
                "no org.freedesktop.secrets service on the session bus".to_owned(),
            ))
        }
    }

    async fn service(&self) -> Result<SecretServiceProxy<'_>, SecretError> {
        SecretServiceProxy::new(&self.connection)
            .await
            .map_err(|e| unavailable(&e))
    }

    async fn open_session(
        &self,
        service: &SecretServiceProxy<'_>,
    ) -> Result<OwnedObjectPath, SecretError> {
        let (_output, session) = timed(service.open_session("plain", &Value::from(""))).await?;
        Ok(session)
    }

    /// Attend la fin d'une invite (déverrouillage). `Ok(true)` si acceptée.
    async fn run_prompt(&self, path: &OwnedObjectPath) -> Result<bool, SecretError> {
        if is_none_path(path) {
            return Ok(true);
        }
        let prompt = SecretPromptProxy::builder(&self.connection)
            .path(path.clone())
            .map_err(|e| unavailable(&e))?
            .build()
            .await
            .map_err(|e| unavailable(&e))?;
        let mut completed = prompt
            .receive_completed()
            .await
            .map_err(|e| unavailable(&e))?;
        timed(prompt.prompt("")).await?;
        match tokio::time::timeout(PROMPT_TIMEOUT, completed.next()).await {
            Ok(Some(signal)) => Ok(!signal.args().map_err(|e| unavailable(&e))?.dismissed),
            _ => Ok(false),
        }
    }

    async fn find(
        &self,
        service: &SecretServiceProxy<'_>,
        host_key: &str,
    ) -> Result<Vec<OwnedObjectPath>, SecretError> {
        let (mut unlocked, locked) = timed(service.search_items(attributes(host_key))).await?;
        if unlocked.is_empty() && !locked.is_empty() {
            let (now_unlocked, prompt) = timed(service.unlock(locked)).await?;
            unlocked = now_unlocked;
            if unlocked.is_empty() {
                if !self.run_prompt(&prompt).await? {
                    return Err(SecretError::Locked);
                }
                unlocked = timed(service.search_items(attributes(host_key))).await?.0;
            }
        }
        Ok(unlocked)
    }

    async fn item(&self, path: OwnedObjectPath) -> Result<SecretItemProxy<'_>, SecretError> {
        SecretItemProxy::builder(&self.connection)
            .path(path)
            .map_err(|e| unavailable(&e))?
            .build()
            .await
            .map_err(|e| unavailable(&e))
    }
}

impl SecretStore for ServiceSecretStore {
    async fn get(&self, host_key: &str) -> Result<Option<String>, SecretError> {
        let service = self.service().await?;
        let found = self.find(&service, host_key).await?;
        let Some(path) = found.into_iter().next() else {
            return Ok(None);
        };
        let session = self.open_session(&service).await?;
        let item = self.item(path).await?;
        let (_, _parameters, value, _content_type) = timed(item.get_secret(&session)).await?;
        String::from_utf8(value)
            .map(Some)
            .map_err(|_| SecretError::Other("secret illisible (UTF-8 attendu)".to_owned()))
    }

    async fn set(&self, host_key: &str, token: &str) -> Result<Stored, SecretError> {
        let service = self.service().await?;
        let session = self.open_session(&service).await?;
        let collection_path = timed(service.read_alias("default")).await?;
        if is_none_path(&collection_path) {
            return Err(SecretError::Unavailable(
                "no default collection in the keyring".to_owned(),
            ));
        }
        let collection = SecretCollectionProxy::builder(&self.connection)
            .path(collection_path)
            .map_err(|e| unavailable(&e))?
            .build()
            .await
            .map_err(|e| unavailable(&e))?;
        let attrs: HashMap<String, String> = attributes(host_key)
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let properties = HashMap::from([
            (
                LABEL_KEY,
                Value::from(format!("ha2lux — Home Assistant ({host_key})")),
            ),
            (ATTRIBUTES_KEY, Value::from(attrs)),
        ]);
        let secret = (session, Vec::new(), token.as_bytes().to_vec(), "text/plain");
        let (_item, prompt) = timed(collection.create_item(properties, &secret, true)).await?;
        if !self.run_prompt(&prompt).await? {
            return Err(SecretError::Locked);
        }
        Ok(Stored::Keyring)
    }

    async fn delete(&self, host_key: &str) -> Result<bool, SecretError> {
        let service = self.service().await?;
        let found = self.find(&service, host_key).await?;
        let existed = !found.is_empty();
        for path in found {
            let prompt = timed(self.item(path).await?.delete()).await?;
            self.run_prompt(&prompt).await?;
        }
        Ok(existed)
    }
}
