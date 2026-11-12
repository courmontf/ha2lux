//! Lecture d'un état Home Assistant : valeur, horodatage, ancienneté.

use ha2lux_core::is_valid_lux;
use serde::Deserialize;

use crate::error::HaError;

/// Valeur d'une lecture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReadingValue {
    /// Éclairement valide (fini, 0 à 1 000 000 lx).
    Lux(f64),
    /// Réponse réussie **sans mesure** : `unavailable`, `unknown`, texte, valeur invalide.
    NoValue,
}

/// Une lecture de l'entité.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Valeur utilisable, ou absence de mesure.
    pub value: ReadingValue,
    /// État brut tel qu'affiché par Home Assistant (borné, pour le panneau d'état).
    pub raw_state: String,
    /// Horodatage de la mesure, en millisecondes Unix : `last_reported` s'il existe (mis à
    /// jour à chaque rapport, même sans changement de valeur), sinon `last_updated`.
    pub observed_ms: Option<i64>,
}

impl Reading {
    /// Ancienneté de la mesure à `now_unix_ms` (0 si l'horodatage manque : instant de
    /// réception ; jamais négative si l'horloge de Home Assistant est en avance).
    pub fn age_ms(&self, now_unix_ms: i64) -> u64 {
        self.observed_ms.map_or(0, |observed| {
            u64::try_from(now_unix_ms.saturating_sub(observed)).unwrap_or(0)
        })
    }
}

#[derive(Deserialize)]
struct StateBody {
    state: Option<serde_json::Value>,
    last_updated: Option<String>,
    last_reported: Option<String>,
}

fn parse_time(text: &Option<String>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text.as_deref()?)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Interprète le texte d'un état.
pub fn parse_state_text(state: &str) -> ReadingValue {
    match state.trim().parse::<f64>() {
        Ok(value) if is_valid_lux(value) => ReadingValue::Lux(value),
        _ => ReadingValue::NoValue,
    }
}

fn state_text(value: Option<serde_json::Value>) -> Result<String, HaError> {
    match value {
        Some(serde_json::Value::String(s)) => Ok(s.chars().take(64).collect()),
        Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
        _ => Err(HaError::Protocol(
            "champ « state » absent ou illisible".to_owned(),
        )),
    }
}

/// Interprète le corps JSON d'un `GET /api/states/<entité>` ou un `to_state` d'événement.
pub fn parse_state_body(body: &[u8]) -> Result<Reading, HaError> {
    let parsed: StateBody =
        serde_json::from_slice(body).map_err(|_| HaError::Protocol("invalid JSON".to_owned()))?;
    let raw_state = state_text(parsed.state)?;
    Ok(Reading {
        value: parse_state_text(&raw_state),
        observed_ms: parse_time(&parsed.last_reported).or_else(|| parse_time(&parsed.last_updated)),
        raw_state,
    })
}

/// Interprète un objet d'état déjà décodé (événement WebSocket).
pub fn parse_state_value(value: &serde_json::Value) -> Result<Reading, HaError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| HaError::Protocol("unreadable state".to_owned()))?;
    parse_state_body(&bytes)
}
