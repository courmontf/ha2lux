//! Fichier d'état (`state.toml`) : ce que ha2lux mémorise lui-même, hors
//! configuration éditée à la main (spec §10.1). Partie pure, sans E/S.
//!
//! Contenu : maître courant, suspension du planning, courbe apprise, dernier
//! écran vu. **Jamais** persisté : atténuation d'inactivité, rampes, santé de
//! Home Assistant, valeurs lues (spec §10.3).

use serde::Serialize;
use toml::{Table, Value};

use crate::config::{ConfigError, ConfigIssue, Severity};
use crate::curve::Curve;
use crate::schedule::{MinuteOfDay, Suspension};

/// Version courante du schéma d'état.
pub const STATE_VERSION: u32 = 1;

/// État persisté.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    /// Dernier maître décidé.
    pub master: Option<u8>,
    /// Suspension du planning en cours.
    pub suspension: Option<Suspension>,
    /// Courbe apprise (vide = courbe par défaut).
    pub curve: Curve,
    /// Dernier écran vu (identité).
    pub last_display: Option<String>,
}

#[derive(Serialize)]
struct SuspensionDto {
    start: String,
    resume: String,
}

#[derive(Serialize)]
struct StateDto {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    master: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suspension: Option<SuspensionDto>,
    curve: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_display: Option<String>,
}

fn issue(path: &str, message: &str) -> ConfigIssue {
    ConfigIssue {
        severity: Severity::Warning,
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn read_suspension(value: &Value, issues: &mut Vec<ConfigIssue>) -> Option<Suspension> {
    let Value::Table(table) = value else {
        issues.push(issue(
            "suspension",
            "table {start, resume} expected, ignored",
        ));
        return None;
    };
    let time = |key: &str| match table.get(key) {
        Some(Value::String(text)) => MinuteOfDay::parse(text).ok(),
        _ => None,
    };
    match (time("start"), time("resume")) {
        (Some(start), Some(resume)) => Some(Suspension::from_parts(start, resume)),
        _ => {
            issues.push(issue("suspension", "invalid HH:MM times, pause ignored"));
            None
        }
    }
}

impl State {
    /// Lit un état. Les champs fautifs sont ignorés avec une remarque ; une
    /// erreur de syntaxe ou une version plus récente sont fatales (l'appelant
    /// conserve alors le fichier tel quel).
    pub fn parse(text: &str) -> Result<(Self, Vec<ConfigIssue>), ConfigError> {
        let root: Table = text.parse().map_err(|e: toml::de::Error| {
            let line = e.span().map_or(1, |s| {
                text.get(..s.start)
                    .map_or(1, |p| p.matches('\n').count() + 1)
            });
            ConfigError::Syntax {
                line,
                message: e.message().to_owned(),
            }
        })?;
        let mut issues = Vec::new();
        match root.get("version") {
            None => {}
            Some(Value::Integer(v)) if *v > i64::from(STATE_VERSION) => {
                return Err(ConfigError::UnsupportedVersion {
                    found: *v,
                    supported: STATE_VERSION,
                });
            }
            Some(Value::Integer(_)) => {}
            Some(_) => issues.push(issue("version", "entier attendu")),
        }
        let master = match root.get("master") {
            None => None,
            Some(Value::Integer(m)) if (0..=100).contains(m) => u8::try_from(*m).ok(),
            Some(_) => {
                issues.push(issue("master", "integer 0-100 expected, ignored"));
                None
            }
        };
        let suspension = root
            .get("suspension")
            .and_then(|v| read_suspension(v, &mut issues));
        let curve = match root.get("curve") {
            None => Curve::new(),
            Some(Value::String(text)) => Curve::parse(text),
            Some(_) => {
                issues.push(issue("curve", "text lux:level,… expected, ignored"));
                Curve::new()
            }
        };
        let last_display = match root.get("last_display") {
            Some(Value::String(id)) if !id.is_empty() && id.chars().count() <= 255 => {
                Some(id.clone())
            }
            None => None,
            Some(_) => {
                issues.push(issue("last_display", "invalid identifier, ignored"));
                None
            }
        };
        Ok((
            Self {
                master,
                suspension,
                curve,
                last_display,
            },
            issues,
        ))
    }

    /// Sérialise en TOML.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        let dto = StateDto {
            version: STATE_VERSION,
            master: self.master,
            suspension: self.suspension.map(|s| SuspensionDto {
                start: s.start().to_string(),
                resume: s.resume().to_string(),
            }),
            curve: self.curve.to_string(),
            last_display: self.last_display.clone(),
        };
        toml::to_string_pretty(&dto).map_err(|e| ConfigError::Serialize(e.to_string()))
    }
}
