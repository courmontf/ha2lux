//! Liste des capteurs d'éclairement (classe `illuminance`), avec leur zone, pour l'aide
//! à la configuration. Passe par un gabarit rendu côté serveur par le WebSocket : un
//! compte **non administrateur** suffit.

use ha2lux_core::config::is_valid_entity_id;

use crate::config::HaConfig;
use crate::error::HaError;
use crate::ws::WsClient;

/// Gabarit : une ligne `entité|nom|zone` par capteur d'éclairement.
pub const SENSOR_TEMPLATE: &str = "{% for s in states.sensor | selectattr('attributes.device_class', 'defined') | selectattr('attributes.device_class', 'eq', 'illuminance') %}{{ s.entity_id }}|{{ s.name | replace('|', '/') | replace('\\n', ' ') }}|{{ (area_name(s.entity_id) or '') | replace('|', '/') }}\n{% endfor %}";

/// Nombre maximal de capteurs conservés.
pub const MAX_SENSORS: usize = 200;

/// Un capteur d'éclairement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SensorInfo {
    /// Identifiant d'entité (validé).
    pub entity: String,
    /// Nom d'affichage (sans caractère de contrôle, borné).
    pub name: String,
    /// Zone (pièce), vide si inconnue.
    pub area: String,
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(127)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Analyse la sortie du gabarit : lignes invalides, identifiants douteux (`/`, `..`, `?`…)
/// et doublons ignorés ; résultat trié et borné.
pub fn parse_sensor_lines(text: &str) -> Vec<SensorInfo> {
    let mut sensors: Vec<SensorInfo> = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(3, '|');
        let (Some(entity), name, area) = (parts.next().map(str::trim), parts.next(), parts.next())
        else {
            continue;
        };
        if !is_valid_entity_id(entity) || sensors.iter().any(|s| s.entity == entity) {
            continue;
        }
        sensors.push(SensorInfo {
            entity: entity.to_owned(),
            name: name
                .map(clean)
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| entity.to_owned()),
            area: area.map(clean).unwrap_or_default(),
        });
    }
    sensors.sort_by(|a, b| a.entity.cmp(&b.entity));
    sensors.truncate(MAX_SENSORS);
    sensors
}

/// Liste les capteurs d'éclairement de l'instance (connexion dédiée, refermée ensuite).
pub async fn list_illuminance_sensors(config: &HaConfig) -> Result<Vec<SensorInfo>, HaError> {
    let mut ws = WsClient::connect(config).await?;
    let text = ws.render_template(SENSOR_TEMPLATE).await?;
    Ok(parse_sensor_lines(&text))
}
