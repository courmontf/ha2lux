//! WebSocket Home Assistant contre un vrai serveur local (T-HA-14, T-HA-10 côté WebSocket).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ha2lux_ha::config::{HaConfig, Timeouts, Token};
use ha2lux_ha::error::HaError;
use ha2lux_ha::reading::ReadingValue;
use ha2lux_ha::sensors::{SensorInfo, list_illuminance_sensors, parse_sensor_lines};
use ha2lux_ha::ws::{WsClient, WsEvent};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "ws-secret-token-ABCDEF";

type Server = WebSocketStream<TcpStream>;

async fn ws_server<F, Fut>(handler: F) -> SocketAddr
where
    F: FnOnce(Server) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        handler(ws).await;
    });
    addr
}

fn config(addr: SocketAddr) -> HaConfig {
    let mut cfg =
        HaConfig::new(&format!("http://{addr}"), "sensor.lux", Token::new(TOKEN)).unwrap();
    cfg.timeouts = Timeouts {
        connect: Duration::from_secs(2),
        request: Duration::from_secs(2),
    };
    cfg
}

async fn send(ws: &mut Server, value: Value) {
    ws.send(Message::text(value.to_string())).await.unwrap();
}

async fn recv(ws: &mut Server) -> Value {
    loop {
        match ws.next().await.expect("le client a fermé").unwrap() {
            Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            Message::Close(_) => panic!("fermeture inattendue"),
            _ => {}
        }
    }
}

/// Accueil + authentification réussie ; renvoie le message d'authentification reçu.
async fn greet(ws: &mut Server) -> Value {
    send(
        ws,
        json!({"type": "auth_required", "ha_version": "2026.11.0"}),
    )
    .await;
    let auth = recv(ws).await;
    send(ws, json!({"type": "auth_ok", "ha_version": "2026.11.0"})).await;
    auth
}

#[tokio::test]
async fn handshake_subscription_events_and_heartbeat() {
    let addr = ws_server(|mut ws| async move {
        let auth = greet(&mut ws).await;
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["access_token"], TOKEN, "le jeton n'est envoyé que dans le message d'authentification");
        let subscribe = recv(&mut ws).await;
        assert_eq!(subscribe["type"], "subscribe_trigger");
        assert_eq!(subscribe["trigger"], json!({"platform": "state", "entity_id": "sensor.lux"}));
        send(&mut ws, json!({"id": subscribe["id"], "type": "result", "success": true, "result": null})).await;
        let event = |state: Value| {
            json!({"id": subscribe["id"], "type": "event", "event": {"variables": {"trigger": {"platform": "state", "entity_id": "sensor.lux", "to_state": state}}, "context": {}}})
        };
        send(&mut ws, event(json!({"state": "123.4", "last_updated": "2026-11-11T20:00:00+00:00", "last_reported": "2026-11-11T20:01:00+00:00", "attributes": {}}))).await;
        send(&mut ws, event(json!({"state": "unavailable", "attributes": {}}))).await;
        send(&mut ws, event(Value::Null)).await;
        send(&mut ws, event(json!({"state": "nan"}))).await;
        let ping = recv(&mut ws).await;
        assert_eq!(ping["type"], "ping");
        send(&mut ws, json!({"id": ping["id"], "type": "pong"})).await;
        send(&mut ws, json!({"type": "something_else"})).await;
        // Garde la connexion ouverte jusqu'à la fermeture par le client.
        while ws.next().await.is_some() {}
    })
    .await;
    let mut client = WsClient::connect(&config(addr)).await.unwrap();
    client.subscribe_entity("sensor.lux").await.unwrap();
    let WsEvent::State(first) = client.next_event().await.unwrap() else {
        panic!("état attendu")
    };
    assert_eq!(first.value, ReadingValue::Lux(123.4));
    assert!(first.observed_ms.is_some());
    let WsEvent::State(second) = client.next_event().await.unwrap() else {
        panic!()
    };
    assert_eq!(
        second.value,
        ReadingValue::NoValue,
        "unavailable : succès sans valeur"
    );
    let WsEvent::State(third) = client.next_event().await.unwrap() else {
        panic!()
    };
    assert_eq!(
        third.value,
        ReadingValue::NoValue,
        "entité supprimée (to_state nul)"
    );
    let WsEvent::State(fourth) = client.next_event().await.unwrap() else {
        panic!()
    };
    assert_eq!(fourth.value, ReadingValue::NoValue, "nan est refusé");
    client.ping().await.unwrap();
    assert_eq!(client.next_event().await.unwrap(), WsEvent::Pong);
    assert_eq!(client.next_event().await.unwrap(), WsEvent::Other);
}

#[tokio::test]
async fn t_ha_14_a_wrong_token_is_an_authentication_error_and_nothing_follows() {
    let addr = ws_server(|mut ws| async move {
        send(&mut ws, json!({"type": "auth_required"})).await;
        let _auth = recv(&mut ws).await;
        send(
            &mut ws,
            json!({"type": "auth_invalid", "message": "Invalid access token or password"}),
        )
        .await;
        // Le client ne doit plus rien envoyer.
        let extra = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
        assert!(
            !matches!(extra, Ok(Some(Ok(Message::Text(_))))),
            "aucune commande après un refus"
        );
    })
    .await;
    let error = WsClient::connect(&config(addr))
        .await
        .err()
        .expect("authentification refusée");
    assert_eq!(error, HaError::Auth);
    assert!(error.is_auth());
    assert!(!format!("{error}").contains(TOKEN) && !format!("{error:?}").contains(TOKEN));
}

#[tokio::test]
async fn an_unexpected_greeting_is_a_protocol_error() {
    let addr = ws_server(|mut ws| async move {
        send(&mut ws, json!({"type": "hello"})).await;
        let _ = ws.next().await;
    })
    .await;
    assert!(matches!(
        WsClient::connect(&config(addr)).await.err(),
        Some(HaError::Protocol(_))
    ));
}

#[tokio::test]
async fn a_refused_subscription_is_reported() {
    let addr = ws_server(|mut ws| async move {
        greet(&mut ws).await;
        let subscribe = recv(&mut ws).await;
        send(&mut ws, json!({"id": subscribe["id"], "type": "result", "success": false, "error": {"code": "invalid_format", "message": "x"}})).await;
        while ws.next().await.is_some() {}
    })
    .await;
    let mut client = WsClient::connect(&config(addr)).await.unwrap();
    let error = client.subscribe_entity("sensor.lux").await.unwrap_err();
    assert!(
        matches!(&error, HaError::Protocol(m) if m.contains("invalid_format")),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_oversized_message_is_refused() {
    let addr = ws_server(|mut ws| async move {
        greet(&mut ws).await;
        let subscribe = recv(&mut ws).await;
        send(
            &mut ws,
            json!({"id": subscribe["id"], "type": "result", "success": true}),
        )
        .await;
        send(
            &mut ws,
            json!({"type": "event", "pad": "x".repeat(300 * 1024)}),
        )
        .await;
        while ws.next().await.is_some() {}
    })
    .await;
    let mut client = WsClient::connect(&config(addr)).await.unwrap();
    client.subscribe_entity("sensor.lux").await.unwrap();
    assert_eq!(client.next_event().await.unwrap_err(), HaError::TooLarge);
}

#[tokio::test]
async fn a_closed_connection_is_a_connection_error() {
    let addr = ws_server(|mut ws| async move {
        greet(&mut ws).await;
        let _ = ws.close(None).await;
    })
    .await;
    let mut client = WsClient::connect(&config(addr)).await.unwrap();
    assert!(matches!(
        client.next_event().await.unwrap_err(),
        HaError::Connection(_)
    ));
}

#[tokio::test]
async fn a_silent_server_times_out() {
    let addr = ws_server(|ws| async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(ws);
    })
    .await;
    let mut cfg = config(addr);
    cfg.timeouts = Timeouts {
        connect: Duration::from_millis(300),
        request: Duration::from_millis(300),
    };
    let started = std::time::Instant::now();
    assert_eq!(WsClient::connect(&cfg).await.err(), Some(HaError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn a_redirect_during_the_upgrade_is_never_followed_and_the_token_never_leaves() {
    let elsewhere = support::http_server(|_, _| support::Reply::status(200)).await;
    let target = format!("{}/api/websocket", elsewhere.url());
    let first = support::http_server(move |_, _| {
        let mut reply = support::Reply::status(301);
        reply.headers.push(("Location".into(), target.clone()));
        reply
    })
    .await;
    let error = WsClient::connect(&config(first.addr))
        .await
        .err()
        .expect("redirection refusée");
    assert_eq!(error, HaError::Redirect);
    assert_eq!(
        elsewhere.count(),
        0,
        "la cible de la redirection n'a rien reçu"
    );
    assert_eq!(first.count(), 1);
}

#[tokio::test]
async fn http_401_on_upgrade_is_an_authentication_error() {
    let server = support::http_server(|_, _| support::Reply::status(401)).await;
    assert_eq!(
        WsClient::connect(&config(server.addr)).await.err(),
        Some(HaError::Auth)
    );
}

#[tokio::test]
async fn wss_with_an_invalid_certificate_is_refused_without_any_bypass() {
    let server = support::https_server_with_self_signed_cert().await;
    let mut cfg = HaConfig::new(
        &format!("https://{}", server.addr),
        "sensor.lux",
        Token::new(TOKEN),
    )
    .unwrap();
    cfg.timeouts = Timeouts {
        connect: Duration::from_secs(2),
        request: Duration::from_secs(2),
    };
    let error = WsClient::connect(&cfg)
        .await
        .err()
        .expect("certificat refusé");
    assert!(matches!(error, HaError::Tls(_)), "{error:?}");
    assert_eq!(
        server.count(),
        0,
        "aucune requête HTTP (donc aucun jeton) n'est partie"
    );
}

#[tokio::test]
async fn sensors_are_listed_with_their_area_through_a_server_side_template() {
    let addr = ws_server(|mut ws| async move {
        let auth = greet(&mut ws).await;
        assert_eq!(auth["type"], "auth");
        let command = recv(&mut ws).await;
        assert_eq!(command["type"], "render_template");
        assert!(command["template"].as_str().unwrap().contains("illuminance"), "filtre sur la classe illuminance");
        send(&mut ws, json!({"id": command["id"], "type": "result", "success": true, "result": null})).await;
        let lines = "sensor.salon_lux|Lumière salon|Salon\nsensor.bureau_lux|Bureau|\nsensor.cave_lux|Cave|Sous-sol\n";
        send(&mut ws, json!({"id": command["id"], "type": "event", "event": {"result": lines, "listeners": {"all": false}}})).await;
        while ws.next().await.is_some() {}
    })
    .await;
    let sensors = list_illuminance_sensors(&config(addr)).await.unwrap();
    let names: Vec<_> = sensors
        .iter()
        .map(|s| (s.entity.as_str(), s.name.as_str(), s.area.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("sensor.bureau_lux", "Bureau", ""),
            ("sensor.cave_lux", "Cave", "Sous-sol"),
            ("sensor.salon_lux", "Lumière salon", "Salon")
        ]
    );
}

#[tokio::test]
async fn a_template_error_is_reported_clearly() {
    let addr = ws_server(|mut ws| async move {
        greet(&mut ws).await;
        let command = recv(&mut ws).await;
        send(&mut ws, json!({"id": command["id"], "type": "result", "success": false, "error": {"code": "template_error", "message": "boom"}})).await;
        while ws.next().await.is_some() {}
    })
    .await;
    let error = list_illuminance_sensors(&config(addr)).await.unwrap_err();
    assert!(
        matches!(&error, HaError::Protocol(m) if m.contains("template_error")),
        "{error:?}"
    );
}

#[test]
fn sensor_lines_are_validated_and_sanitized() {
    let text = "sensor.ok|Nom|Zone\n\
                sensor/../etc|Piège|Zone\n\
                sensor.x?y=1|Piège|\n\
                Sensor.Majuscule|Piège|\n\
                nopipe\n\
                \n\
                sensor.ok|Doublon|Autre\n\
                sensor.ctrl|Nom\u{7}avec\u{1b}[31m contrôle|Zo\nne\n\
                sensor.sansnom||Cuisine\n\
                sensor.pipes|a|b|c\n";
    let sensors = parse_sensor_lines(text);
    let find = |entity: &str| sensors.iter().find(|s| s.entity == entity).cloned();
    assert_eq!(
        find("sensor.ok"),
        Some(SensorInfo {
            entity: "sensor.ok".into(),
            name: "Nom".into(),
            area: "Zone".into()
        })
    );
    assert_eq!(
        find("sensor.sansnom").unwrap().name,
        "sensor.sansnom",
        "nom absent : l'identifiant sert de nom"
    );
    assert!(
        find("sensor/../etc").is_none()
            && find("sensor.x?y=1").is_none()
            && find("Sensor.Majuscule").is_none()
    );
    let ctrl = find("sensor.ctrl").unwrap();
    assert!(ctrl.name.chars().all(|c| !c.is_control()), "{ctrl:?}");
    assert_eq!(
        find("sensor.pipes").unwrap().area,
        "b|c",
        "au plus trois champs"
    );
    assert_eq!(
        sensors.iter().filter(|s| s.entity == "sensor.ok").count(),
        1,
        "doublon écarté"
    );
}

#[test]
fn the_sensor_list_is_bounded_and_names_are_truncated() {
    let many: String = (0..500)
        .map(|i| format!("sensor.s{i}|{}|\n", "n".repeat(500)))
        .collect();
    let sensors = parse_sensor_lines(&many);
    assert_eq!(sensors.len(), 200);
    assert!(sensors.iter().all(|s| s.name.chars().count() <= 127));
}
