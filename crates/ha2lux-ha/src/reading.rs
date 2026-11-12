//! Reading of a Home Assistant state: value, timestamp, age.

use ha2lux_core::is_valid_lux;
use serde::Deserialize;

use crate::error::HaError;

/// Value of a reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReadingValue {
    /// Valid illuminance (finite, 0 to 1,000,000 lx).
    Lux(f64),
    /// Successful response **with no measurement**: `unavailable`, `unknown`, text, invalid value.
    NoValue,
}

/// A reading of the entity.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Usable value, or no measurement.
    pub value: ReadingValue,
    /// Raw state as displayed by Home Assistant (bounded, for the status panel).
    pub raw_state: String,
    /// Timestamp of the measurement, in Unix milliseconds: `last_reported` if present (updated
    /// on every report, even without a value change), otherwise `last_updated`.
    pub observed_ms: Option<i64>,
}

impl Reading {
    /// Age of the measurement at `now_unix_ms` (0 if the timestamp is missing: time of
    /// receipt; never negative if Home Assistant's clock is ahead).
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

/// Interprets the text of a state.
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

/// Interprets the JSON body of a `GET /api/states/<entity>` or an event's `to_state`.
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

/// Interprets an already decoded state object (WebSocket event).
pub fn parse_state_value(value: &serde_json::Value) -> Result<Reading, HaError> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| HaError::Protocol("unreadable state".to_owned()))?;
    parse_state_body(&bytes)
}
