//! WebSocket Home Assistant : accueil → authentification par jeton → confirmation →
//! souscription (protocole de Home Assistant). Le jeton ne circule que dans le message
//! d'authentification ; une redirection n'est jamais suivie ; TLS est toujours validé.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
};

use crate::config::HaConfig;
use crate::error::{HaError, MAX_BODY, short};
use crate::reading::{Reading, ReadingValue, parse_state_value};

/// Événement reçu sur la souscription.
#[derive(Clone, Debug, PartialEq)]
pub enum WsEvent {
    /// Nouvel état de l'entité.
    State(Reading),
    /// Réponse au battement de cœur.
    Pong,
    /// Autre message (accusé de réception…), sans importance ici.
    Other,
}

/// Session WebSocket authentifiée.
pub struct WsClient {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    request_timeout: std::time::Duration,
}

fn map_ws_error(error: WsError) -> HaError {
    match error {
        WsError::Capacity(_) => HaError::TooLarge,
        WsError::Http(response) if response.status().is_redirection() => HaError::Redirect,
        WsError::Http(response) if matches!(response.status().as_u16(), 401 | 403) => HaError::Auth,
        WsError::Http(response) => {
            HaError::Protocol(format!("code HTTP {}", response.status().as_u16()))
        }
        WsError::Tls(e) => HaError::Tls(short(&e.to_string())),
        WsError::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => HaError::Timeout,
        WsError::Io(e) => {
            let text = e.to_string();
            if text.to_lowercase().contains("certificate") || text.to_lowercase().contains("tls") {
                HaError::Tls(short(&text))
            } else {
                HaError::Connection(short(&text))
            }
        }
        WsError::ConnectionClosed | WsError::AlreadyClosed => {
            HaError::Connection("connexion fermée".to_owned())
        }
        other => HaError::Protocol(short(&other.to_string())),
    }
}

impl WsClient {
    /// Se connecte et s'authentifie, le tout sous le délai de la requête.
    pub async fn connect(config: &HaConfig) -> Result<Self, HaError> {
        let timeout = config.timeouts.request;
        match tokio::time::timeout(timeout, Self::connect_inner(config)).await {
            Ok(result) => result,
            Err(_) => Err(HaError::Timeout),
        }
    }

    async fn connect_inner(config: &HaConfig) -> Result<Self, HaError> {
        let url = config.websocket_url();
        let request = url.as_str().into_client_request().map_err(map_ws_error)?;
        let limits = WebSocketConfig::default()
            .max_message_size(Some(MAX_BODY))
            .max_frame_size(Some(MAX_BODY));
        let connector = if url.scheme() == "wss" {
            use rustls_platform_verifier::ConfigVerifierExt;
            // Vérificateur de certificats du système : aucun contournement n'existe.
            let tls = rustls::ClientConfig::with_platform_verifier()
                .map_err(|e| HaError::Tls(short(&e.to_string())))?;
            Some(Connector::Rustls(Arc::new(tls)))
        } else {
            None
        };
        let (socket, _response) =
            connect_async_tls_with_config(request, Some(limits), true, connector)
                .await
                .map_err(map_ws_error)?;
        let mut client = Self {
            socket,
            next_id: 1,
            request_timeout: config.timeouts.request,
        };
        client.authenticate(config).await?;
        Ok(client)
    }

    async fn recv_json(&mut self) -> Result<Value, HaError> {
        loop {
            match self.socket.next().await {
                None => return Err(HaError::Connection("connexion fermée".to_owned())),
                Some(Err(error)) => return Err(map_ws_error(error)),
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(text.as_str())
                        .map_err(|_| HaError::Protocol("JSON invalide".to_owned()));
                }
                Some(Ok(Message::Close(_))) => {
                    return Err(HaError::Connection("fermée par Home Assistant".to_owned()));
                }
                Some(Ok(_)) => {} // ping/pong de bas niveau, binaire : ignorés
            }
        }
    }

    async fn send_json(&mut self, value: &Value) -> Result<(), HaError> {
        self.socket
            .send(Message::text(value.to_string()))
            .await
            .map_err(map_ws_error)
    }

    async fn authenticate(&mut self, config: &HaConfig) -> Result<(), HaError> {
        let hello = self.recv_json().await?;
        if hello["type"] != "auth_required" {
            return Err(HaError::Protocol("message d'accueil inattendu".to_owned()));
        }
        // Le jeton ne quitte la machine que dans ce message, sur la connexion déjà validée.
        self.send_json(&json!({"type": "auth", "access_token": config.token.expose()}))
            .await?;
        match self.recv_json().await?["type"].as_str() {
            Some("auth_ok") => Ok(()),
            Some("auth_invalid") => Err(HaError::Auth),
            _ => Err(HaError::Protocol(
                "réponse d'authentification inattendue".to_owned(),
            )),
        }
    }

    fn next_command_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// S'abonne aux changements d'état de `entity` (`subscribe_trigger`, ouvert à tout compte).
    pub async fn subscribe_entity(&mut self, entity: &str) -> Result<(), HaError> {
        let id = self.next_command_id();
        self.send_json(&json!({"id": id, "type": "subscribe_trigger", "trigger": {"platform": "state", "entity_id": entity}}))
            .await?;
        let limit = self.request_timeout;
        let wait = async {
            loop {
                let message = self.recv_json().await?;
                if message["id"] == id && message["type"] == "result" {
                    return if message["success"] == true {
                        Ok(())
                    } else {
                        Err(HaError::Protocol(format!(
                            "souscription refusée ({})",
                            message["error"]["code"].as_str().unwrap_or("?")
                        )))
                    };
                }
            }
        };
        tokio::time::timeout(limit, wait)
            .await
            .map_err(|_| HaError::Timeout)?
    }

    /// Envoie un battement de cœur ; la réponse arrive par [`WsClient::next_event`].
    pub async fn ping(&mut self) -> Result<(), HaError> {
        let id = self.next_command_id();
        self.send_json(&json!({"id": id, "type": "ping"})).await
    }

    /// Prochain événement. **Annulable sans perte** : l'appelant peut abandonner l'attente.
    pub async fn next_event(&mut self) -> Result<WsEvent, HaError> {
        let message = self.recv_json().await?;
        Ok(match message["type"].as_str() {
            Some("pong") => WsEvent::Pong,
            Some("event") => match message["event"]["variables"]["trigger"].get("to_state") {
                Some(Value::Null) => WsEvent::State(Reading {
                    value: ReadingValue::NoValue,
                    raw_state: "unavailable".to_owned(),
                    observed_ms: None,
                }),
                Some(state) => WsEvent::State(parse_state_value(state)?),
                None => WsEvent::Other,
            },
            _ => WsEvent::Other,
        })
    }

    /// Fait rendre un gabarit par Home Assistant et renvoie le texte obtenu. Ouvert à tout
    /// compte (l'équivalent REST exige un administrateur).
    pub async fn render_template(&mut self, template: &str) -> Result<String, HaError> {
        let id = self.next_command_id();
        self.send_json(&json!({"id": id, "type": "render_template", "template": template, "timeout": 5, "report_errors": true})).await?;
        let limit = self.request_timeout;
        let wait = async {
            loop {
                let message = self.recv_json().await?;
                if message["id"] != id {
                    continue;
                }
                match message["type"].as_str() {
                    Some("result") if message["success"] != true => {
                        return Err(HaError::Protocol(format!(
                            "gabarit refusé ({})",
                            message["error"]["code"].as_str().unwrap_or("?")
                        )));
                    }
                    Some("event") => {
                        if let Some(result) = message["event"]["result"].as_str() {
                            return Ok(result.to_owned());
                        }
                        if let Some(error) = message["event"]["error"].as_str() {
                            return Err(HaError::Protocol(format!(
                                "gabarit en erreur : {}",
                                short(error)
                            )));
                        }
                    }
                    _ => {}
                }
            }
        };
        tokio::time::timeout(limit, wait)
            .await
            .map_err(|_| HaError::Timeout)?
    }
}
