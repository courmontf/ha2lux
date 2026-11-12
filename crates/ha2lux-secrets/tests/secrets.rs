//! Token storage: fake Secret Service on a private bus, 0600 file fallback, T-SEC-03/04.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

use ha2lux_secrets::{
    ChainStore, FileSecretStore, SecretError, SecretStore, ServiceSecretStore, Stored,
};
use tempfile::TempDir;
use zbus::object_server::{ObjectServer, SignalEmitter};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const TOKEN: &str = "llat-SECRET-TOKEN-0123456789";

struct PrivateBus {
    child: Child,
    address: String,
    _dir: TempDir,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n<busconfig><type>session</type><listen>unix:dir={}</listen><auth>EXTERNAL</auth>\n<policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>\n",
                dir.path().display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        let address = address.trim().to_owned();
        (!address.is_empty()).then_some(Self {
            child,
            address,
            _dir: dir,
        })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        // The private bus is no longer needed; an error here has no consequence.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

macro_rules! bus_or_skip {
    () => {
        match PrivateBus::start() {
            Some(bus) => bus,
            None => {
                eprintln!("SKIPPED : dbus-daemon introuvable");
                return;
            }
        }
    };
}

#[derive(Clone)]
struct Item {
    id: u32,
    label: String,
    attrs: HashMap<String, String>,
    secret: Vec<u8>,
}

#[derive(Default)]
struct World {
    items: Vec<Item>,
    next: u32,
    locked: bool,
    accept_prompt: bool,
    sessions_opened: u32,
}

type Shared = Arc<Mutex<World>>;

fn lock(world: &Shared) -> std::sync::MutexGuard<'_, World> {
    world.lock().unwrap_or_else(PoisonError::into_inner)
}

fn item_path(id: u32) -> OwnedObjectPath {
    OwnedObjectPath::try_from(format!("/org/freedesktop/secrets/collection/default/{id}")).unwrap()
}

fn path(text: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(text).unwrap()
}

struct FakeService(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl FakeService {
    fn open_session(
        &self,
        algorithm: String,
        _input: OwnedValue,
    ) -> zbus::fdo::Result<(OwnedValue, OwnedObjectPath)> {
        if algorithm != "plain" {
            return Err(zbus::fdo::Error::NotSupported(
                "seul « plain » est géré".into(),
            ));
        }
        lock(&self.0).sessions_opened += 1;
        Ok((
            Value::from("").try_to_owned().unwrap(),
            path("/org/freedesktop/secrets/session/s1"),
        ))
    }

    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
        let world = lock(&self.0);
        let found: Vec<OwnedObjectPath> = world
            .items
            .iter()
            .filter(|i| attributes.iter().all(|(k, v)| i.attrs.get(k) == Some(v)))
            .map(|i| item_path(i.id))
            .collect();
        if world.locked {
            (Vec::new(), found)
        } else {
            (found, Vec::new())
        }
    }

    fn unlock(&self, objects: Vec<OwnedObjectPath>) -> (Vec<OwnedObjectPath>, OwnedObjectPath) {
        if lock(&self.0).locked {
            (Vec::new(), path("/org/freedesktop/secrets/prompt/p1"))
        } else {
            (objects, path("/"))
        }
    }

    fn read_alias(&self, name: String) -> OwnedObjectPath {
        if name == "default" {
            path("/org/freedesktop/secrets/collection/default")
        } else {
            path("/")
        }
    }
}

struct FakeCollection(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
impl FakeCollection {
    async fn create_item(
        &self,
        properties: HashMap<String, OwnedValue>,
        secret: (OwnedObjectPath, Vec<u8>, Vec<u8>, String),
        replace: bool,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let attrs: HashMap<String, String> = properties
            .get("org.freedesktop.Secret.Item.Attributes")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| HashMap::<String, String>::try_from(v).ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("attributs absents".into()))?;
        let label = properties
            .get("org.freedesktop.Secret.Item.Label")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| String::try_from(v).ok())
            .unwrap_or_default();
        let id = {
            let mut world = lock(&self.0);
            if replace {
                world.items.retain(|i| i.attrs != attrs);
            }
            world.next += 1;
            let id = world.next;
            world.items.push(Item {
                id,
                label,
                attrs,
                secret: secret.2,
            });
            id
        };
        server
            .at(
                item_path(id),
                FakeItem {
                    world: Arc::clone(&self.0),
                    id,
                },
            )
            .await?;
        Ok((item_path(id), path("/")))
    }
}

struct FakeItem {
    world: Shared,
    id: u32,
}

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl FakeItem {
    fn get_secret(
        &self,
        session: OwnedObjectPath,
    ) -> zbus::fdo::Result<(OwnedObjectPath, Vec<u8>, Vec<u8>, String)> {
        let world = lock(&self.world);
        let item = world
            .items
            .iter()
            .find(|i| i.id == self.id)
            .ok_or_else(|| zbus::fdo::Error::Failed("supprimé".into()))?;
        Ok((
            session,
            Vec::new(),
            item.secret.clone(),
            "text/plain".to_owned(),
        ))
    }

    async fn delete(&self, #[zbus(object_server)] server: &ObjectServer) -> OwnedObjectPath {
        lock(&self.world).items.retain(|i| i.id != self.id);
        // The object may have already disappeared: irrelevant.
        let _ = server.remove::<FakeItem, _>(item_path(self.id)).await;
        path("/")
    }
}

struct FakePrompt(Shared);

#[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
impl FakePrompt {
    async fn prompt(
        &self,
        _window_id: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let accept = lock(&self.0).accept_prompt;
        if accept {
            lock(&self.0).locked = false;
        }
        FakePrompt::completed(&emitter, !accept, Value::from("").try_to_owned().unwrap()).await?;
        Ok(())
    }

    #[zbus(signal)]
    async fn completed(
        emitter: &SignalEmitter<'_>,
        dismissed: bool,
        result: OwnedValue,
    ) -> zbus::Result<()>;
}

async fn fake_secret_service(bus: &PrivateBus, world: &Shared) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at("/org/freedesktop/secrets", FakeService(Arc::clone(world)))
        .unwrap()
        .serve_at(
            "/org/freedesktop/secrets/collection/default",
            FakeCollection(Arc::clone(world)),
        )
        .unwrap()
        .serve_at(
            "/org/freedesktop/secrets/prompt/p1",
            FakePrompt(Arc::clone(world)),
        )
        .unwrap()
        .name("org.freedesktop.secrets")
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn client(bus: &PrivateBus) -> zbus::Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap()
}

const HOST: &str = "homeassistant.local:8123";

#[tokio::test]
async fn keyring_round_trip_with_the_right_attributes() {
    let bus = bus_or_skip!();
    let world: Shared = Arc::default();
    let _service = fake_secret_service(&bus, &world).await;
    let store = ServiceSecretStore::with_connection(client(&bus).await)
        .await
        .unwrap();

    assert_eq!(store.get(HOST).await.unwrap(), None);
    assert_eq!(store.set(HOST, TOKEN).await.unwrap(), Stored::Keyring);
    assert_eq!(store.get(HOST).await.unwrap().as_deref(), Some(TOKEN));

    let items = lock(&world).items.clone();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].attrs.get("application").map(String::as_str),
        Some("ha2lux")
    );
    assert_eq!(
        items[0].attrs.get("kind").map(String::as_str),
        Some("ha-token")
    );
    assert_eq!(items[0].attrs.get("host").map(String::as_str), Some(HOST));
    assert!(
        items[0].label.contains(HOST) && !items[0].label.contains(TOKEN),
        "le libellé ne contient pas le jeton : {}",
        items[0].label
    );
    assert!(lock(&world).sessions_opened >= 1);
}

#[tokio::test]
async fn t_sec_04_a_token_is_only_returned_for_the_host_it_was_stored_for() {
    let bus = bus_or_skip!();
    let world: Shared = Arc::default();
    let _service = fake_secret_service(&bus, &world).await;
    let store = ServiceSecretStore::with_connection(client(&bus).await)
        .await
        .unwrap();
    store.set(HOST, TOKEN).await.unwrap();
    for other in [
        "evil.example.com:8123",
        "homeassistant.local:8124",
        "homeassistant.local:80",
        "HOMEASSISTANT.LOCAL:8123",
    ] {
        assert_eq!(
            store.get(other).await.unwrap(),
            None,
            "le jeton de {HOST} ne doit pas être retrouvé pour {other}"
        );
    }
    assert_eq!(store.get(HOST).await.unwrap().as_deref(), Some(TOKEN));
}

#[tokio::test]
async fn setting_again_replaces_and_delete_removes() {
    let bus = bus_or_skip!();
    let world: Shared = Arc::default();
    let _service = fake_secret_service(&bus, &world).await;
    let store = ServiceSecretStore::with_connection(client(&bus).await)
        .await
        .unwrap();
    store.set(HOST, "ancien").await.unwrap();
    store.set(HOST, TOKEN).await.unwrap();
    assert_eq!(lock(&world).items.len(), 1, "remplacé, pas dupliqué");
    assert_eq!(store.get(HOST).await.unwrap().as_deref(), Some(TOKEN));
    store.set("autre.local:8123", "autre").await.unwrap();
    assert!(store.delete(HOST).await.unwrap());
    assert!(!store.delete(HOST).await.unwrap(), "déjà supprimé");
    assert_eq!(store.get(HOST).await.unwrap(), None);
    assert_eq!(
        store.get("autre.local:8123").await.unwrap().as_deref(),
        Some("autre"),
        "les autres jetons sont intacts"
    );
}

#[tokio::test]
async fn a_locked_keyring_is_unlocked_through_the_prompt() {
    let bus = bus_or_skip!();
    let world: Shared = Arc::default();
    let _service = fake_secret_service(&bus, &world).await;
    let store = ServiceSecretStore::with_connection(client(&bus).await)
        .await
        .unwrap();
    store.set(HOST, TOKEN).await.unwrap();
    {
        let mut w = lock(&world);
        w.locked = true;
        w.accept_prompt = true;
    }
    assert_eq!(
        store.get(HOST).await.unwrap().as_deref(),
        Some(TOKEN),
        "déverrouillé par l'invite acceptée"
    );
    {
        let mut w = lock(&world);
        w.locked = true;
        w.accept_prompt = false;
    }
    let error = store.get(HOST).await.unwrap_err();
    assert!(matches!(error, SecretError::Locked), "{error:?}");
    assert!(!format!("{error}").contains(TOKEN));
}

#[tokio::test]
async fn without_a_secret_service_the_keyring_is_unavailable() {
    let bus = bus_or_skip!();
    let error = ServiceSecretStore::with_connection(client(&bus).await)
        .await
        .err()
        .expect("aucun service");
    assert!(matches!(error, SecretError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn the_file_store_uses_0600_and_refuses_a_world_readable_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("sub/secrets.json");
    let store = FileSecretStore::new(&file);
    assert_eq!(
        store.get(HOST).await.unwrap(),
        None,
        "fichier absent : pas de jeton"
    );
    assert_eq!(store.set(HOST, TOKEN).await.unwrap(), Stored::File);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600,
        "0600 dès la création"
    );
    store.set("autre:80", "t2").await.unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(store.get(HOST).await.unwrap().as_deref(), Some(TOKEN));
    assert_eq!(store.get("absent:1").await.unwrap(), None);
    assert!(store.delete("autre:80").await.unwrap());
    assert!(!store.delete("autre:80").await.unwrap());
    // Another user could read it: flat refusal, without reading.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = store.get(HOST).await.unwrap_err();
    assert!(matches!(error, SecretError::TooOpen(_)), "{error:?}");
    assert!(!format!("{error}").contains(TOKEN));
    assert!(
        store.set(HOST, "x").await.is_err(),
        "on n'écrit pas non plus dans un fichier ouvert"
    );
}

#[tokio::test]
async fn a_corrupt_secrets_file_never_leaks_its_content() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("secrets.json");
    std::fs::write(&file, format!("{{\"{HOST}\": \"{TOKEN}\" oups")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let error = FileSecretStore::new(&file).get(HOST).await.unwrap_err();
    assert!(
        !format!("{error}").contains(TOKEN) && !format!("{error:?}").contains(TOKEN),
        "{error:?}"
    );
}

#[tokio::test]
async fn the_chain_prefers_the_keyring_and_removes_the_file_copy() {
    let bus = bus_or_skip!();
    let world: Shared = Arc::default();
    let _service = fake_secret_service(&bus, &world).await;
    let dir = TempDir::new().unwrap();
    let file = FileSecretStore::new(dir.path().join("secrets.json"));
    file.set(HOST, "copie-fichier").await.unwrap();
    let chain = ChainStore::new(
        Some(
            ServiceSecretStore::with_connection(client(&bus).await)
                .await
                .unwrap(),
        ),
        FileSecretStore::new(dir.path().join("secrets.json")),
    );
    assert_eq!(
        chain.get(HOST).await.unwrap().as_deref(),
        Some("copie-fichier"),
        "lu dans le fichier tant que le trousseau n'a rien"
    );
    assert_eq!(chain.set(HOST, TOKEN).await.unwrap(), Stored::Keyring);
    assert_eq!(chain.get(HOST).await.unwrap().as_deref(), Some(TOKEN));
    assert_eq!(
        file.get(HOST).await.unwrap(),
        None,
        "la copie du fichier a été retirée"
    );
    assert!(chain.delete(HOST).await.unwrap());
    assert_eq!(chain.get(HOST).await.unwrap(), None);
}

#[tokio::test]
async fn the_chain_falls_back_to_the_file_when_there_is_no_keyring() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("secrets.json");
    let chain = ChainStore::new(None, FileSecretStore::new(&path));
    assert_eq!(chain.set(HOST, TOKEN).await.unwrap(), Stored::File);
    assert_eq!(chain.get(HOST).await.unwrap().as_deref(), Some(TOKEN));
    assert_eq!(chain.get("autre:1").await.unwrap(), None);
    assert!(chain.delete(HOST).await.unwrap());
}

#[tokio::test]
async fn the_chain_ignores_a_broken_keyring() {
    let bus = bus_or_skip!();
    let world: Shared = Arc::default();
    let service = fake_secret_service(&bus, &world).await;
    let store = ServiceSecretStore::with_connection(client(&bus).await)
        .await
        .unwrap();
    drop(service); // the keyring disappears midway
    let dir = TempDir::new().unwrap();
    let chain = ChainStore::new(
        Some(store),
        FileSecretStore::new(dir.path().join("secrets.json")),
    );
    assert_eq!(chain.set(HOST, TOKEN).await.unwrap(), Stored::File);
    assert_eq!(chain.get(HOST).await.unwrap().as_deref(), Some(TOKEN));
}
