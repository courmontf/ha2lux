//! List of illuminance sensors (`illuminance` class), with their area, to help with
//! configuration. Goes through a template rendered server-side over the WebSocket: a
//! **non-administrator** account is enough.

use ha2lux_core::config::is_valid_entity_id;

use crate::config::HaConfig;
use crate::error::HaError;
use crate::ws::WsClient;

/// Template: one `entity|name|area` line per illuminance sensor.
pub const SENSOR_TEMPLATE: &str = "{% for s in states.sensor | selectattr('attributes.device_class', 'defined') | selectattr('attributes.device_class', 'eq', 'illuminance') %}{{ s.entity_id }}|{{ s.name | replace('|', '/') | replace('\\n', ' ') }}|{{ (area_name(s.entity_id) or '') | replace('|', '/') }}\n{% endfor %}";

/// Maximum number of sensors kept.
pub const MAX_SENSORS: usize = 200;

/// An illuminance sensor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SensorInfo {
    /// Entity identifier (validated).
    pub entity: String,
    /// Display name (no control characters, bounded).
    pub name: String,
    /// Area (room), empty if unknown.
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

/// Parses the template output: invalid lines, dubious identifiers (`/`, `..`, `?`…)
/// and duplicates are ignored; result sorted and bounded.
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

/// Lists the instance's illuminance sensors (dedicated connection, closed afterwards).
pub async fn list_illuminance_sensors(config: &HaConfig) -> Result<Vec<SensorInfo>, HaError> {
    let mut ws = WsClient::connect(config).await?;
    let text = ws.render_template(SENSOR_TEMPLATE).await?;
    Ok(parse_sensor_lines(&text))
}
