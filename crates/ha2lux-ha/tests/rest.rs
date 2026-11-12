//! Lecture REST contre un faux serveur local : T-HA-01..10 et garanties sur le jeton.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::time::Duration;

use ha2lux_ha::config::{ConfigError, HaConfig, Timeouts, Token};
use ha2lux_ha::error::HaError;
use ha2lux_ha::reading::{ReadingValue, parse_state_body};
use ha2lux_ha::rest::RestClient;
use support::{Reply, Server, http_server};

const TOKEN: &str = "eyJ0b2tlbiI6InNlY3JldC12YWx1ZSJ9.SECRET";

fn config(url: &str) -> HaConfig {
    let mut cfg = HaConfig::new(url, "sensor.lux", Token::new(TOKEN)).unwrap();
    cfg.timeouts = Timeouts {
        connect: Duration::from_secs(2),
        request: Duration::from_secs(2),
    };
    cfg
}

async fn read(server: &Server) -> Result<ha2lux_ha::reading::Reading, HaError> {
    RestClient::new(&config(&server.url()))
        .unwrap()
        .read()
        .await
}

#[tokio::test]
async fn t_ha_01_a_numeric_state_is_a_measurement_and_the_request_is_well_formed() {
    let server = http_server(|_, _| Reply::state("12.5")).await;
    let reading = read(&server).await.unwrap();
    assert_eq!(reading.value, ReadingValue::Lux(12.5));
    assert_eq!(reading.raw_state, "12.5");
    let request = server.last();
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("GET", "/api/states/sensor.lux")
    );
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert!(
        !request.path.contains(TOKEN),
        "le jeton n'est jamais dans l'URL"
    );
}

#[tokio::test]
async fn t_ha_02_unavailable_unknown_and_text_are_a_success_without_value() {
    for state in ["unavailable", "unknown", "abc", "", "N/A"] {
        let server = http_server(move |_, _| Reply::state(state)).await;
        let reading = read(&server)
            .await
            .unwrap_or_else(|e| panic!("{state:?} : {e}"));
        assert_eq!(reading.value, ReadingValue::NoValue, "{state:?}");
    }
}

#[tokio::test]
async fn t_ha_03_invalid_numbers_are_without_value() {
    for state in [
        "inf", "-inf", "infinity", "1e999", "nan", "NaN", "-3", "1000001", "-0.5", "1e7",
    ] {
        let server = http_server(move |_, _| Reply::state(state)).await;
        assert_eq!(
            read(&server).await.unwrap().value,
            ReadingValue::NoValue,
            "{state}"
        );
    }
}

#[tokio::test]
async fn t_ha_04_the_upper_bound_is_accepted() {
    for (state, expected) in [
        ("1000000", 1_000_000.0),
        ("0", 0.0),
        ("0.0", 0.0),
        (" 7.25 ", 7.25),
        ("1e2", 100.0),
    ] {
        let server = http_server(move |_, _| Reply::state(state)).await;
        assert_eq!(
            read(&server).await.unwrap().value,
            ReadingValue::Lux(expected),
            "{state}"
        );
    }
}

#[tokio::test]
async fn numeric_json_state_is_tolerated() {
    let server = http_server(|_, _| Reply::json(r#"{"state":37.5}"#)).await;
    assert_eq!(read(&server).await.unwrap().value, ReadingValue::Lux(37.5));
}

#[tokio::test]
async fn t_ha_05_401_and_403_are_authentication_errors() {
    for status in [401, 403] {
        let server = http_server(move |_, _| Reply::status(status)).await;
        let error = read(&server).await.unwrap_err();
        assert_eq!(error, HaError::Auth, "{status}");
        assert!(error.is_auth());
    }
}

#[tokio::test]
async fn t_ha_06_404_is_entity_not_found() {
    let server = http_server(|_, _| Reply::status(404)).await;
    assert_eq!(read(&server).await.unwrap_err(), HaError::NotFound);
}

#[tokio::test]
async fn t_ha_07_redirects_are_never_followed_and_the_token_never_leaves() {
    let elsewhere = http_server(|_, _| Reply::state("999")).await;
    let target = format!("{}/api/states/sensor.lux", elsewhere.url());
    for status in [301, 302, 307, 308] {
        let location = target.clone();
        let server = http_server(move |_, _| {
            let mut reply = Reply::status(status);
            reply.headers.push(("Location".into(), location.clone()));
            reply
        })
        .await;
        assert_eq!(
            read(&server).await.unwrap_err(),
            HaError::Redirect,
            "{status}"
        );
        assert_eq!(server.count(), 1, "une seule requête, aucune relance");
    }
    assert_eq!(
        elsewhere.count(),
        0,
        "la cible de la redirection n'a reçu AUCUNE requête, donc jamais le jeton"
    );
}

#[tokio::test]
async fn t_ha_08_a_response_over_256_kib_is_a_protocol_failure() {
    let big = "x".repeat(257 * 1024);
    let body = format!(r#"{{"state":"1","pad":"{big}"}}"#);
    let server = http_server(move |_, _| Reply::json(&body)).await;
    assert_eq!(
        read(&server).await.unwrap_err(),
        HaError::TooLarge,
        "annoncée par Content-Length"
    );

    // Sans Content-Length : la limite s'applique en cours de lecture.
    let big = "y".repeat(300 * 1024);
    let streamed = format!(r#"{{"state":"1","pad":"{big}"}}"#);
    let server = http_server(move |_, _| Reply {
        streamed: true,
        ..Reply::json(&streamed)
    })
    .await;
    assert_eq!(
        read(&server).await.unwrap_err(),
        HaError::TooLarge,
        "flux sans longueur annoncée"
    );

    let under = format!(r#"{{"state":"5","pad":"{}"}}"#, "z".repeat(100 * 1024));
    let server = http_server(move |_, _| Reply::json(&under)).await;
    assert_eq!(
        read(&server).await.unwrap().value,
        ReadingValue::Lux(5.0),
        "100 Kio passent"
    );
}

#[tokio::test]
async fn t_ha_09_silence_beyond_the_timeout_is_a_timeout() {
    let server = http_server(|_, _| Reply::hang()).await;
    let mut cfg = config(&server.url());
    cfg.timeouts = Timeouts {
        connect: Duration::from_millis(300),
        request: Duration::from_millis(300),
    };
    let started = std::time::Instant::now();
    let error = RestClient::new(&cfg).unwrap().read().await.unwrap_err();
    assert_eq!(error, HaError::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn t_ha_10_an_invalid_certificate_is_refused_without_any_bypass() {
    let server = support::https_server_with_self_signed_cert().await;
    let url = format!("https://{}", server.addr);
    let error = RestClient::new(&config(&url))
        .unwrap()
        .read()
        .await
        .unwrap_err();
    assert!(matches!(error, HaError::Tls(_)), "{error:?}");
    assert_eq!(
        server.count(),
        0,
        "la requête (et donc le jeton) n'est jamais partie : poignée de main refusée"
    );
}

#[tokio::test]
async fn connection_refused_and_server_errors_are_classified() {
    // Un port réservé puis libéré : personne n'écoute, la connexion est refusée.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", free.local_addr().unwrap());
    drop(free);
    let error = RestClient::new(&config(&url))
        .unwrap()
        .read()
        .await
        .unwrap_err();
    assert!(matches!(error, HaError::Connection(_)), "{error:?}");
    for status in [500, 502, 503] {
        let server = http_server(move |_, _| Reply::status(status)).await;
        assert!(
            matches!(read(&server).await.unwrap_err(), HaError::Protocol(m) if m.contains(&status.to_string()))
        );
    }
    let server = http_server(|_, _| Reply::json("not json")).await;
    assert!(matches!(
        read(&server).await.unwrap_err(),
        HaError::Protocol(_)
    ));
    let server = http_server(|_, _| Reply::json(r#"{"no_state":1}"#)).await;
    assert!(matches!(
        read(&server).await.unwrap_err(),
        HaError::Protocol(_)
    ));
}

#[tokio::test]
async fn a_base_path_is_preserved_for_reverse_proxies() {
    let server = http_server(|_, _| Reply::state("3")).await;
    let cfg = config(&format!("{}/homeassistant/", server.url()));
    RestClient::new(&cfg).unwrap().read().await.unwrap();
    assert_eq!(server.last().path, "/homeassistant/api/states/sensor.lux");
}

#[tokio::test]
async fn the_token_never_appears_in_errors_debug_output_or_logs_of_a_failed_call() {
    let server = http_server(|_, _| Reply::status(401)).await;
    let cfg = config(&server.url());
    let error = RestClient::new(&cfg).unwrap().read().await.unwrap_err();
    for text in [
        format!("{error}"),
        format!("{error:?}"),
        format!("{cfg:?}"),
        format!("{:?}", cfg.token),
    ] {
        assert!(
            !text.contains(TOKEN) && !text.contains("SECRET"),
            "fuite du jeton : {text}"
        );
    }
    assert!(format!("{cfg:?}").contains("Token(***)"));
}

#[test]
fn t_sec_04_config_validation() {
    let token = || Token::new("t");
    assert!(HaConfig::new("homeassistant.local:8123", "sensor.lux", token()).is_ok());
    assert!(matches!(
        HaConfig::new("", "sensor.lux", token()),
        Err(ConfigError::Url(_))
    ));
    assert!(
        matches!(
            HaConfig::new("http://user:pw@ha.local", "sensor.lux", token()),
            Err(ConfigError::Url(_))
        ),
        "pas d'identifiants dans l'URL"
    );
    assert!(matches!(
        HaConfig::new("ftp://ha.local", "sensor.lux", token()),
        Err(ConfigError::Url(_))
    ));
    for entity in [
        "",
        "sensor",
        "sensor/../x",
        "sensor.x?y=1",
        "a.b.c",
        "Sensor.x",
        "sensor.x#f",
    ] {
        assert_eq!(
            HaConfig::new("ha.local", entity, token()).unwrap_err(),
            ConfigError::Entity,
            "{entity}"
        );
    }
    assert_eq!(
        HaConfig::new("ha.local", "sensor.lux", Token::new("   ")).unwrap_err(),
        ConfigError::NoToken
    );
}

#[test]
fn urls_are_built_for_http_and_https_and_keep_the_port() {
    let cfg = HaConfig::new(
        "https://ha.example.com:8443",
        "sensor.salon_lux",
        Token::new("t"),
    )
    .unwrap();
    assert_eq!(
        cfg.state_url().as_str(),
        "https://ha.example.com:8443/api/states/sensor.salon_lux"
    );
    assert_eq!(
        cfg.websocket_url().as_str(),
        "wss://ha.example.com:8443/api/websocket"
    );
    let cfg = HaConfig::new("192.168.1.10:8123", "sensor.x", Token::new("t")).unwrap();
    assert_eq!(
        cfg.websocket_url().as_str(),
        "ws://192.168.1.10:8123/api/websocket"
    );
}

#[test]
fn last_reported_is_preferred_over_last_updated_for_freshness() {
    let body = br#"{"state":"0","last_updated":"2026-11-11T20:00:00+00:00","last_reported":"2026-11-11T22:30:00+00:00"}"#;
    let reading = parse_state_body(body).unwrap();
    let reported = chrono::DateTime::parse_from_rfc3339("2026-11-11T22:30:00+00:00")
        .unwrap()
        .timestamp_millis();
    assert_eq!(reading.observed_ms, Some(reported));
    assert_eq!(reading.age_ms(reported + 90_000), 90_000);
    let only_updated =
        parse_state_body(br#"{"state":"0","last_updated":"2026-11-11T20:00:00+00:00"}"#).unwrap();
    assert!(only_updated.observed_ms.is_some());
    let none = parse_state_body(br#"{"state":"0"}"#).unwrap();
    assert_eq!(
        (none.observed_ms, none.age_ms(5_000_000)),
        (None, 0),
        "sans horodatage : instant de réception"
    );
    assert_eq!(
        reading.age_ms(reported - 5_000),
        0,
        "horloge de HA en avance : jamais négative"
    );
    assert_eq!(
        parse_state_body(br#"{"state":"1","last_updated":"garbage"}"#)
            .unwrap()
            .observed_ms,
        None
    );
}

mod robust {
    use ha2lux_ha::reading::{parse_state_body, parse_state_text};
    use ha2lux_ha::sensors::parse_sensor_lines;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        #[test]
        fn reading_parsers_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..300), text in "\\PC{0,60}") {
            let _ = parse_state_body(&bytes);
            let _ = parse_state_text(&text);
        }

        #[test]
        fn json_like_bodies_never_panic(state in "\\PC{0,20}", extra in "\\PC{0,20}") {
            let body = format!("{{\"state\":\"{}\",\"last_updated\":\"{}\"}}", state.replace(['"', '\\'], ""), extra.replace(['"', '\\'], ""));
            let _ = parse_state_body(body.as_bytes());
        }

        #[test]
        fn sensor_list_parser_never_panics(text in "\\PC{0,400}") {
            let _ = parse_sensor_lines(&text);
        }
    }
}
