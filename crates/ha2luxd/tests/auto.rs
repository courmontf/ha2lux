//! Auto-brightness in the daemon: token, measurement source, published state (fake Home Assistant).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ha2lux_core::DisplayKind;
use ha2lux_ha::error::HaError;
use ha2lux_ha::reading::{Reading, ReadingValue};
use ha2lux_ha::sensors::SensorInfo;
use ha2lux_ha::source::{Feed, FeedEvent, HaSource, Link, SourceConfig, spawn};
use ha2luxd::api::{ApiError, Reply, Request, Secret, StatusSnapshot};
use ha2luxd::ha::{BoxFut, HaDeps, Sources, Tokens};
use ha2luxd::hardware::MockHardware;
use support::Rig;

const CONFIG: &str =
    "[auto]\nenabled = true\nurl = \"http://ha.local:8123\"\nentity = \"sensor.salon_lux\"\n";

#[derive(Default)]
struct MemTokens(Mutex<HashMap<String, String>>);

impl Tokens for MemTokens {
    fn get(&self, key: &str) -> BoxFut<Result<Option<String>, String>> {
        let value = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned();
        Box::pin(async move { Ok(value) })
    }
    fn set(&self, key: &str, token: &str) -> BoxFut<Result<(), String>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), token.to_owned());
        Box::pin(async { Ok(()) })
    }
    fn delete(&self, key: &str) -> BoxFut<Result<bool, String>> {
        let existed = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key)
            .is_some();
        Box::pin(async move { Ok(existed) })
    }
}

struct NoFeed;
impl Feed for NoFeed {
    async fn next(&mut self) -> Result<FeedEvent, HaError> {
        std::future::pending().await
    }
    async fn ping(&mut self) -> Result<(), HaError> {
        Ok(())
    }
}

struct FakeLink(Arc<Mutex<f64>>);
impl Link for FakeLink {
    type Feed = NoFeed;
    async fn read(&self) -> Result<Reading, HaError> {
        let lux = *self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(Reading {
            value: ReadingValue::Lux(lux),
            raw_state: format!("{lux}"),
            observed_ms: None,
        })
    }
    async fn connect(&self) -> Result<NoFeed, HaError> {
        Err(HaError::Connection("pas de WebSocket".to_owned()))
    }
}

struct FakeSources {
    lux: Arc<Mutex<f64>>,
    started: Mutex<Vec<(String, String, String)>>,
}

impl Sources for FakeSources {
    fn start(&self, url: &str, entity: &str, token: String) -> Result<HaSource, String> {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((url.to_owned(), entity.to_owned(), token));
        let config = SourceConfig {
            websocket: false,
            ..SourceConfig::default()
        };
        Ok(spawn(
            FakeLink(Arc::clone(&self.lux)),
            config,
            Arc::new(|| 0),
        ))
    }
    fn sensors(&self, _url: &str, _token: String) -> BoxFut<Result<Vec<SensorInfo>, String>> {
        Box::pin(async {
            Ok(vec![SensorInfo {
                entity: "sensor.salon_lux".to_owned(),
                name: "Salon".to_owned(),
                area: "Salon".to_owned(),
            }])
        })
    }
}

struct Fixture {
    rig: Rig,
    tokens: Arc<MemTokens>,
    sources: Arc<FakeSources>,
}

async fn fixture(config: &str, token: Option<(&str, &str)>) -> Fixture {
    let tokens = Arc::new(MemTokens::default());
    if let Some((key, value)) = token {
        tokens.set(key, value).await.unwrap();
    }
    let sources = Arc::new(FakeSources {
        lux: Arc::new(Mutex::new(300.0)),
        started: Mutex::new(Vec::new()),
    });
    let ha = HaDeps {
        tokens: Arc::clone(&tokens) as _,
        sources: Arc::clone(&sources) as _,
    };
    let hardware = MockHardware::new();
    hardware.add("A", DisplayKind::External, 100, 50);
    let rig = Rig::start_with_ha(hardware, Some(config), None, Some(ha))
        .await
        .unwrap();
    Fixture {
        rig,
        tokens,
        sources,
    }
}

async fn status(rig: &Rig) -> StatusSnapshot {
    match rig.ask(Request::Status).await.unwrap() {
        Reply::Status(status) => *status,
        other => panic!("réponse inattendue : {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn token_found_at_startup_configures_auto_and_follows_light() {
    let f = fixture(CONFIG, Some(("ha.local:8123", "secret-token"))).await;
    f.rig.advance(Duration::from_secs(5)).await;
    let s = status(&f.rig).await;
    assert!(s.auto_configured && s.ha_token_present);
    assert_eq!(s.mode, "auto-lux");
    assert!((s.lux_smoothed.unwrap() - 300.0).abs() < 1e-6);
    assert_eq!(s.ha_raw.as_deref(), Some("300"));
    assert!(!s.curve.is_empty() && s.curve_default);
    // The source received the address, entity and token from the keyring.
    let started = f.sources.started.lock().unwrap().clone();
    assert_eq!(
        started,
        vec![(
            "http://ha.local:8123".to_owned(),
            "sensor.salon_lux".to_owned(),
            "secret-token".to_owned()
        )]
    );
    // The applied level follows the curve for 300 lx.
    f.rig.advance(Duration::from_secs(60)).await;
    let s = status(&f.rig).await;
    assert_eq!(s.master, s.auto_target);
}

#[tokio::test(start_paused = true)]
async fn without_token_auto_is_refused_until_token_is_stored() {
    let f = fixture(CONFIG, None).await;
    let s = status(&f.rig).await;
    assert!(!s.auto_configured && !s.ha_token_present);
    assert!(f.sources.started.lock().unwrap().is_empty());
    let refused = f
        .rig
        .ask(Request::SetMode {
            feature: "auto".to_owned(),
            on: true,
        })
        .await;
    assert!(matches!(refused, Err(ApiError::NotConfigured(_))));

    f.rig
        .ok(Request::SetToken(Secret("  nouveau-jeton \n".to_owned())))
        .await;
    f.rig.advance(Duration::from_secs(5)).await;
    assert!(status(&f.rig).await.auto_configured);
    assert_eq!(f.sources.started.lock().unwrap()[0].2, "nouveau-jeton");
    f.rig
        .ok(Request::SetMode {
            feature: "auto".to_owned(),
            on: true,
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn clearing_the_token_stops_the_source() {
    let f = fixture(CONFIG, Some(("ha.local:8123", "t"))).await;
    f.rig.advance(Duration::from_secs(5)).await;
    assert!(status(&f.rig).await.auto_configured);
    f.rig.ok(Request::ClearToken).await;
    f.rig.advance(Duration::from_secs(5)).await;
    let s = status(&f.rig).await;
    assert!(!s.auto_configured && s.ha_raw.is_none());
}

#[tokio::test(start_paused = true)]
async fn token_is_bound_to_the_host() {
    // T-SEC-04: the first server's token is never presented to another.
    let f = fixture(CONFIG, Some(("ha.local:8123", "t"))).await;
    f.rig.advance(Duration::from_secs(5)).await;
    assert!(status(&f.rig).await.auto_configured);
    let other = CONFIG.replace("ha.local", "autre.example");
    support::write_file(&f.rig.paths.config_file, &other);
    f.rig.ok(Request::Reload).await;
    f.rig.advance(Duration::from_secs(5)).await;
    assert!(!status(&f.rig).await.auto_configured);
    assert_eq!(f.sources.started.lock().unwrap().len(), 1);
    assert!(f.tokens.get("autre.example").await.unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn sensors_are_listed_with_the_stored_token() {
    let f = fixture(CONFIG, Some(("ha.local:8123", "t"))).await;
    match f.rig.ask(Request::ListSensors).await.unwrap() {
        Reply::Sensors(list) => assert_eq!(
            list,
            vec![(
                "sensor.salon_lux".to_owned(),
                "Salon".to_owned(),
                "Salon".to_owned()
            )]
        ),
        other => panic!("réponse inattendue : {other:?}"),
    }
    let none = fixture("", None).await;
    assert!(matches!(
        none.rig.ask(Request::ListSensors).await,
        Err(ApiError::NotConfigured(_))
    ));
}

#[test]
fn token_never_appears_in_debug_output() {
    let text = format!("{:?}", Request::SetToken(Secret("super-secret".to_owned())));
    assert!(!text.contains("super-secret"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn a_big_light_change_is_confirmed_then_applied() {
    let f = fixture(CONFIG, Some(("ha.local:8123", "t"))).await;
    f.rig.advance(Duration::from_secs(60)).await;
    let before = status(&f.rig).await;
    *f.sources.lux.lock().unwrap() = 20_000.0;
    // Next measurement (REST fallback), confirmation at ~3 s, then ramp.
    f.rig.advance(Duration::from_secs(120)).await;
    let after = status(&f.rig).await;
    assert_ne!(before.auto_target, after.auto_target);
    assert_eq!(after.master, after.auto_target);
    assert!(!after.auto_adjusting);
}
