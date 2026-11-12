//! Rendu des réponses du démon, en texte lisible ou en JSON (fonctions pures).

use ha2lux_ipc::DisplayDto;
use serde::Serialize;

use crate::client::StatusInfo;

fn kind_label(kind: &str) -> &str {
    match kind {
        "internal" => "interne",
        "external" => "externe",
        other => other,
    }
}

fn state_label(state: &str) -> &str {
    match state {
        "controllable" => "contrôlable",
        "unavailable" => "indisponible",
        "not-controllable" => "non contrôlable",
        "absent" => "absent",
        other => other,
    }
}

fn level_label(level: i32) -> String {
    if level < 0 {
        "?".to_owned()
    } else {
        format!("{level} %")
    }
}

/// Une ligne d'écran, numérotée à partir de 1.
pub fn display_line(number: usize, d: &DisplayDto) -> String {
    format!(
        "{number:>2}  {}  [{}]  {}  {}  niveau {}  plage {}-{}",
        d.name,
        d.id,
        kind_label(&d.kind),
        state_label(&d.state),
        level_label(d.level),
        d.low,
        d.high
    )
}

/// `ha2lux list`.
pub fn render_list(list: &[DisplayDto]) -> String {
    if list.is_empty() {
        return "aucun écran détecté\n".to_owned();
    }
    list.iter()
        .enumerate()
        .map(|(i, d)| display_line(i + 1, d) + "\n")
        .collect()
}

/// `ha2lux get` (sans `--display`).
pub fn render_get(status: &StatusInfo, list: &[DisplayDto]) -> String {
    let master = status
        .master
        .map_or("inconnu".to_owned(), |m| format!("{m} %"));
    let mut out = format!("maître : {master}  (source : {})\n", status.mode);
    if let Some(resume) = &status.schedule_resume {
        out.push_str(&format!("planning suspendu jusqu'à {resume}\n"));
    }
    if status.dimmed {
        out.push_str("atténuation d'inactivité en cours\n");
    }
    out.push_str(&render_list(list));
    out
}

/// `ha2lux get --display …`.
pub fn render_display_level(number: usize, d: &DisplayDto) -> String {
    format!("{} (n°{number}) : {}\n", d.name, level_label(d.level))
}

/// `ha2lux auto status`.
pub fn render_auto(status: &StatusInfo) -> String {
    let mut text = String::new();
    let enabled = if status.auto_enabled {
        "activée"
    } else {
        "désactivée"
    };
    let configured = if status.auto_configured {
        "configurée"
    } else if status.ha_token_present {
        "non configurée (URL et entité requises)"
    } else {
        "non configurée (URL, entité et jeton requis : ha2lux auto token)"
    };
    text.push_str(&format!("auto-luminosité : {enabled}, {configured}\n"));
    text.push_str(&format!("état : {}\n", auto_state(status)));
    if status.ha_auth_rejected {
        text.push_str(
            "jeton refusé par Home Assistant : enregistrez-en un nouveau (ha2lux auto token)\n",
        );
    }
    if let Some(raw) = &status.ha_raw {
        text.push_str(&format!("valeur brute du capteur : {raw}\n"));
    }
    if let Some(lux) = status.lux_smoothed {
        text.push_str(&format!("éclairement lissé : {lux:.0} lx"));
        if let Some(latest) = status.lux_latest {
            text.push_str(&format!(" (dernière mesure : {latest:.0} lx)"));
        }
        text.push('\n');
    }
    if let Some(target) = status.auto_target {
        text.push_str(&format!("niveau visé par la courbe : {target} %\n"));
    }
    if let Some(level) = status.master {
        text.push_str(&format!("niveau appliqué : {level} %\n"));
    }
    text.push_str(&format!(
        "courbe ({}) : {}\n",
        if status.curve_default {
            "par défaut"
        } else {
            "apprise"
        },
        status
            .curve
            .iter()
            .map(|(lux, level)| format!("{lux:.0} lx → {level} %"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    text.push_str(&format!("source de pilotage : {}\n", status.mode));
    text
}

/// Résumé de l'état du lien en une expression : inactif, connexion, actif, etc.
fn auto_state(status: &StatusInfo) -> &'static str {
    if !status.auto_enabled {
        return "inactif";
    }
    if !status.auto_configured {
        return "non configuré";
    }
    if status.ha_auth_rejected {
        return "jeton refusé";
    }
    match status.ha_health.as_str() {
        "offline" => "hors ligne",
        "degraded" => "dégradé",
        _ if status.lux_smoothed.is_none() => "en attente d'une mesure",
        _ if status.auto_adjusting => "en ajustement",
        "stale" => "pas de mesure récente",
        _ => "actif",
    }
}

/// Liste des capteurs d'éclairement.
pub fn render_sensors(list: &[(String, String, String)]) -> String {
    if list.is_empty() {
        return "aucun capteur d'éclairement (device_class illuminance) trouvé\n".to_owned();
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
    match feature {
        "schedule" => format!("planning {}\n", if on { "activé" } else { "désactivé" }),
        "idle-dim" if on => format!(
            "atténuation d'inactivité activée : niveau {} %, après {} min d'inactivité\n",
            status.idle_level, status.idle_minutes
        ),
        "idle-dim" => "atténuation d'inactivité désactivée\n".to_owned(),
        _ => format!(
            "auto-luminosité {}\n",
            if on { "activée" } else { "désactivée" }
        ),
    }
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
