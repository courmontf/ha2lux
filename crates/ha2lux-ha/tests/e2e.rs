//! Le vrai `HaLink` (REST + WebSocket) piloté par le superviseur, face à un faux Home
//! Assistant qui parle HTTP et WebSocket sur le même port.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ha2lux_core::HaHealth;
use ha2lux_ha::config::{HaConfig, Timeouts, Token};
use ha2lux_ha::source::{HaLink, Output, SourceConfig, spawn, system_unix_ms};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "e2e-secret-token";

struct FakeHa {
    addr: SocketAddr,
    state: Arc<std::sync::Mutex<String>>,
    pushes: broadcast::Sender<String>,
    kick: broadcast::Sender<()>,
    rest_reads: Arc<AtomicU32>,
    ws_sessions: Arc<AtomicU32>,
}

async fn handle_http(
    mut stream: TcpStream,
    state: Arc<std::sync::Mutex<String>>,
    reads: Arc<AtomicU32>,
) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
    reads.fetch_add(1, Ordering::SeqCst);
    let authorized =
        text.contains(&format!("Bearer {TOKEN}")) || text.contains(&format!("bearer {TOKEN}"));
    let (status, body) = if !authorized {
        (401, String::new())
    } else {
        let now = chrono::Utc::now().to_rfc3339();
        (200, json!({"state": state.lock().unwrap().clone(), "last_updated": now, "last_reported": now}).to_string())
    };
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // Le client peut avoir déjà fermé : sans importance.
    let _ = stream.write_all(reply.as_bytes()).await;
}

async fn handle_ws(
    stream: TcpStream,
    mut pushes: broadcast::Receiver<String>,
    mut kick: broadcast::Receiver<()>,
    sessions: Arc<AtomicU32>,
) {
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
    sessions.fetch_add(1, Ordering::SeqCst);
    ws.send(Message::text(json!({"type": "auth_required"}).to_string()))
        .await
        .unwrap();
    let auth: Value =
        serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    if auth["access_token"] != TOKEN {
        ws.send(Message::text(json!({"type": "auth_invalid"}).to_string()))
            .await
            .unwrap();
        return;
    }
    ws.send(Message::text(json!({"type": "auth_ok"}).to_string()))
        .await
        .unwrap();
    let mut subscription = Value::Null;
    loop {
        tokio::select! {
            incoming = ws.next() => {
                let Some(Ok(message)) = incoming else { return };
                let Ok(text) = message.to_text() else { continue };
                let command: Value = serde_json::from_str(text).unwrap_or(Value::Null);
                match command["type"].as_str() {
                    Some("subscribe_trigger") => {
                        subscription = command["id"].clone();
                        ws.send(Message::text(json!({"id": command["id"], "type": "result", "success": true}).to_string())).await.unwrap();
                    }
                    Some("ping") => ws.send(Message::text(json!({"id": command["id"], "type": "pong"}).to_string())).await.unwrap(),
                    _ => {}
                }
            }
            Ok(value) = pushes.recv() => {
                let now = chrono::Utc::now().to_rfc3339();
                let event = json!({"id": subscription, "type": "event", "event": {"variables": {"trigger": {"to_state": {"state": value, "last_updated": now, "last_reported": now}}}}});
                ws.send(Message::text(event.to_string())).await.unwrap();
            }
            Ok(()) = kick.recv() => return, // coupe la connexion
        }
    }
}

async fn fake_ha() -> FakeHa {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(std::sync::Mutex::new("10".to_owned()));
    let (pushes, _) = broadcast::channel(16);
    let (kick, _) = broadcast::channel(4);
    let (rest_reads, ws_sessions) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let (s, p, k, r, w) = (
        Arc::clone(&state),
        pushes.clone(),
        kick.clone(),
        Arc::clone(&rest_reads),
        Arc::clone(&ws_sessions),
    );
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let mut peek = [0u8; 256];
            let n = stream.peek(&mut peek).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&peek[..n]).into_owned();
            if head.contains("/api/websocket") {
                tokio::spawn(handle_ws(
                    stream,
                    p.subscribe(),
                    k.subscribe(),
                    Arc::clone(&w),
                ));
            } else {
                tokio::spawn(handle_http(stream, Arc::clone(&s), Arc::clone(&r)));
            }
        }
    });
    FakeHa {
        addr,
        state,
        pushes,
        kick,
        rest_reads,
        ws_sessions,
    }
}

fn link_config(addr: SocketAddr, token: &str) -> HaConfig {
    let mut cfg =
        HaConfig::new(&format!("http://{addr}"), "sensor.lux", Token::new(token)).unwrap();
    cfg.timeouts = Timeouts {
        connect: Duration::from_secs(2),
        request: Duration::from_secs(2),
    };
    cfg
}

fn fast() -> SourceConfig {
    SourceConfig {
        poll_period: Duration::from_millis(150),
        refresh_period: Duration::from_millis(400),
        heartbeat: Duration::from_millis(200),
        pong_timeout: Duration::from_millis(400),
        confirm_delay: Duration::from_millis(100),
        auth_pause: Duration::from_millis(800),
        backoff: ha2lux_ha::backoff::Backoff::new(
            Duration::from_millis(50),
            Duration::from_millis(400),
        ),
        ..SourceConfig::default()
    }
}

async fn expect(
    rx: &mut mpsc::UnboundedReceiver<Output>,
    what: &str,
    mut wanted: impl FnMut(&Output) -> bool,
) -> Output {
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let output = rx.recv().await.expect("source arrêtée");
            if wanted(&output) {
                return output;
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| panic!("sortie non reçue : {what}"))
}

#[tokio::test]
async fn real_link_rest_first_then_websocket_events_then_fallback_and_back() {
    let ha = fake_ha().await;
    let mut source = spawn(
        HaLink::new(link_config(ha.addr, TOKEN)).unwrap(),
        fast(),
        Arc::new(system_unix_ms),
    );

    // Le WebSocket s'ouvre en même temps que la première lecture REST : l'ordre n'est pas garanti.
    let (mut first_measurement, mut ws_up) = (None, false);
    while first_measurement.is_none() || !ws_up {
        match expect(&mut source.outputs, "démarrage", |o| {
            matches!(o, Output::Measurement { .. } | Output::Websocket(true))
        })
        .await
        {
            Output::Measurement { lux } => first_measurement = Some(lux),
            _ => ws_up = true,
        }
    }
    assert_eq!(first_measurement, Some(10.0));

    // Événement poussé par le WebSocket.
    ha.pushes.send("250".to_owned()).unwrap();
    assert_eq!(
        expect(&mut source.outputs, "mesure poussée", |o| *o
            == Output::Measurement { lux: 250.0 })
        .await,
        Output::Measurement { lux: 250.0 }
    );

    // Coupure du WebSocket : repli REST, puis reconnexion.
    *ha.state.lock().unwrap() = "75".to_owned();
    ha.kick.send(()).unwrap();
    expect(&mut source.outputs, "WebSocket coupé", |o| {
        *o == Output::Websocket(false)
    })
    .await;
    expect(&mut source.outputs, "repli REST", |o| {
        *o == Output::Measurement { lux: 75.0 }
    })
    .await;
    expect(&mut source.outputs, "reconnexion", |o| {
        *o == Output::Websocket(true)
    })
    .await;
    assert!(ha.ws_sessions.load(Ordering::SeqCst) >= 2);
    assert!(ha.rest_reads.load(Ordering::SeqCst) >= 2);

    // Confirmation rapprochée.
    let before = ha.rest_reads.load(Ordering::SeqCst);
    source.poll_soon();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        ha.rest_reads.load(Ordering::SeqCst) > before,
        "lecture de confirmation"
    );
}

#[tokio::test]
async fn real_link_with_a_wrong_token_reports_the_rejection_and_stays_quiet() {
    let ha = fake_ha().await;
    let mut source = spawn(
        HaLink::new(link_config(ha.addr, "mauvais-jeton")).unwrap(),
        fast(),
        Arc::new(system_unix_ms),
    );
    expect(&mut source.outputs, "jeton refusé", |o| {
        *o == Output::AuthRejected(true)
    })
    .await;
    expect(&mut source.outputs, "hors ligne", |o| {
        *o == Output::Health(HaHealth::Offline)
    })
    .await;
    let before = ha.rest_reads.load(Ordering::SeqCst) + ha.ws_sessions.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await; // pause d'authentification de 800 ms
    let during = ha.rest_reads.load(Ordering::SeqCst) + ha.ws_sessions.load(Ordering::SeqCst);
    assert!(
        during <= before + 1,
        "aucun harcèlement du serveur : {before} → {during}"
    );
}
