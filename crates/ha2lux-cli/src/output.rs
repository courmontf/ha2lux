//! Rendu des réponses du démon, en texte lisible ou en JSON (fonctions pures).

use ha2lux_i18n::t;
use ha2lux_ipc::DisplayDto;
use serde::Serialize;

use crate::client::StatusInfo;

fn kind_label(kind: &str) -> String {
    match kind {
        "internal" => t!("cli-kind-internal"),
        "external" => t!("cli-kind-external"),
        other => other.to_owned(),
    }
}

fn state_label(state: &str) -> String {
    match state {
        "controllable" => t!("cli-state-controllable"),
        "unavailable" => t!("cli-state-unavailable"),
        "not-controllable" => t!("cli-state-not-controllable"),
        "absent" => t!("cli-state-absent"),
        other => other.to_owned(),
    }
}

fn percent(value: impl std::fmt::Display) -> String {
    t!("cli-percent", value = value.to_string())
}

fn level_label(level: i32) -> String {
    if level < 0 {
        "?".to_owned()
    } else {
        percent(level)
    }
}

/// Une ligne d'écran, numérotée à partir de 1.
pub fn display_line(number: usize, d: &DisplayDto) -> String {
    t!(
        "cli-display-line",
        number = format!("{number:>2}"),
        name = d.name.as_str(),
        id = d.id.as_str(),
        kind = kind_label(&d.kind),
        state = state_label(&d.state),
        level = level_label(d.level),
        low = d.low.to_string(),
        high = d.high.to_string()
    )
}

/// `ha2lux list`.
pub fn render_list(list: &[DisplayDto]) -> String {
    if list.is_empty() {
        return t!("cli-list-empty") + "\n";
    }
    list.iter()
        .enumerate()
        .map(|(i, d)| display_line(i + 1, d) + "\n")
        .collect()
}

/// `ha2lux get` (sans `--display`).
pub fn render_get(status: &StatusInfo, list: &[DisplayDto]) -> String {
    let master = status.master.map_or_else(|| t!("cli-unknown"), percent);
    let mut out = t!(
        "cli-get-master",
        level = master,
        mode = status.mode.as_str()
    ) + "\n";
    if let Some(resume) = &status.schedule_resume {
        out.push_str(&t!("cli-get-schedule-resume", resume = resume.as_str()));
        out.push('\n');
    }
    if status.dimmed {
        out.push_str(&t!("cli-get-dimmed"));
        out.push('\n');
    }
    out.push_str(&render_list(list));
    out
}

/// `ha2lux get --display …`.
pub fn render_display_level(number: usize, d: &DisplayDto) -> String {
    t!(
        "cli-display-level",
        name = d.name.as_str(),
        number = number.to_string(),
        level = level_label(d.level)
    ) + "\n"
}

/// `ha2lux auto status`.
pub fn render_auto(status: &StatusInfo) -> String {
    let mut text = String::new();
    let enabled = if status.auto_enabled {
        t!("cli-auto-enabled")
    } else {
        t!("cli-auto-disabled")
    };
    let configured = if status.auto_configured {
        t!("cli-auto-configured")
    } else if status.ha_token_present {
        t!("cli-auto-needs-url-entity")
    } else {
        t!("cli-auto-needs-url-entity-token")
    };
    text.push_str(&t!(
        "cli-auto-summary",
        enabled = enabled,
        configured = configured
    ));
    text.push('\n');
    text.push_str(&t!("cli-auto-state-line", state = auto_state(status)));
    text.push('\n');
    if status.ha_auth_rejected {
        text.push_str(&t!("cli-auto-token-rejected"));
        text.push('\n');
    }
    if let Some(raw) = &status.ha_raw {
        text.push_str(&t!("cli-auto-raw", raw = raw.as_str()));
        text.push('\n');
    }
    if let Some(lux) = status.lux_smoothed {
        let lux = format!("{lux:.0}");
        match status.lux_latest {
            Some(latest) => text.push_str(&t!(
                "cli-auto-lux-with-latest",
                lux = lux,
                latest = format!("{latest:.0}")
            )),
            None => text.push_str(&t!("cli-auto-lux", lux = lux)),
        }
        text.push('\n');
    }
    if let Some(target) = status.auto_target {
        text.push_str(&t!("cli-auto-target", level = percent(target)));
        text.push('\n');
    }
    if let Some(level) = status.master {
        text.push_str(&t!("cli-auto-applied", level = percent(level)));
        text.push('\n');
    }
    let kind = if status.curve_default {
        t!("cli-auto-curve-default")
    } else {
        t!("cli-auto-curve-learned")
    };
    let points = status
        .curve
        .iter()
        .map(|(lux, level)| {
            t!(
                "cli-auto-curve-point",
                lux = format!("{lux:.0}"),
                level = percent(level)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    text.push_str(&t!("cli-auto-curve", kind = kind, points = points));
    text.push('\n');
    text.push_str(&t!("cli-auto-source", mode = status.mode.as_str()));
    text.push('\n');
    text
}

/// Résumé de l'état du lien en une expression : inactif, connexion, actif, etc.
fn auto_state(status: &StatusInfo) -> String {
    if !status.auto_enabled {
        return t!("cli-auto-state-inactive");
    }
    if !status.auto_configured {
        return t!("cli-auto-state-not-configured");
    }
    if status.ha_auth_rejected {
        return t!("cli-auto-state-token-rejected");
    }
    match status.ha_health.as_str() {
        "offline" => t!("cli-auto-state-offline"),
        "degraded" => t!("cli-auto-state-degraded"),
        _ if status.lux_smoothed.is_none() => t!("cli-auto-state-waiting"),
        _ if status.auto_adjusting => t!("cli-auto-state-adjusting"),
        "stale" => t!("cli-auto-state-stale"),
        _ => t!("cli-auto-state-active"),
    }
}

/// Liste des capteurs d'éclairement.
pub fn render_sensors(list: &[(String, String, String)]) -> String {
    if list.is_empty() {
        return t!("cli-sensors-none") + "\n";
    }
    let mut text = String::new();
    for (entity, name, area) in list {
        if area.is_empty() {
            text.push_str(&format!("{entity}  {name}\n"));
        } else {
            text.push_str(&format!("{entity}  {name}  ({area})\n"));
        }
    }
    text
}

/// Phrase de confirmation d'un interrupteur.
pub fn toggle_message(feature: &str, on: bool, status: &StatusInfo) -> String {
    let text = match feature {
        "schedule" if on => t!("cli-toggle-schedule-on"),
        "schedule" => t!("cli-toggle-schedule-off"),
        "idle-dim" if on => t!(
            "cli-toggle-idle-on",
            level = status.idle_level.to_string(),
            minutes = status.idle_minutes.to_string()
        ),
        "idle-dim" => t!("cli-toggle-idle-off"),
        _ if on => t!("cli-toggle-auto-on"),
        _ => t!("cli-toggle-auto-off"),
    };
    text + "\n"
}

/// Sortie JSON de `get`.
#[derive(Serialize)]
pub struct GetJson<'a> {
    /// Niveau maître.
    pub master: Option<u8>,
    /// Source de base.
    pub mode: &'a str,
    /// Écrans.
    pub displays: &'a [DisplayDto],
}

/// Sortie JSON de `auto status`.
#[derive(Serialize)]
pub struct AutoJson<'a> {
    /// Activée.
    pub enabled: bool,
    /// Configurée.
    pub configured: bool,
    /// Santé Home Assistant.
    pub health: &'a str,
    /// Source de base.
    pub mode: &'a str,
    /// Un jeton est enregistré.
    pub token_present: bool,
    /// Jeton refusé.
    pub auth_rejected: bool,
    /// Flux WebSocket actif.
    pub websocket: bool,
    /// Valeur brute du capteur.
    pub raw: Option<&'a str>,
    /// Lux lissé.
    pub lux_smoothed: Option<f64>,
    /// Dernier lux.
    pub lux_latest: Option<f64>,
    /// Ajustement en cours.
    pub adjusting: bool,
    /// Niveau visé.
    pub target: Option<u8>,
    /// Niveau appliqué.
    pub level: Option<u8>,
    /// Courbe (lux, niveau).
    pub curve: &'a [(f64, u8)],
    /// Courbe par défaut.
    pub curve_default: bool,
}
