//! The README's example configuration must be accepted as is, with no reservations.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ha2lux_core::config::Config;

/// Checks a README's example configuration (both languages).
fn check(file: &str, entity: &str) {
    let path = format!("{}/../../{file}", env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(path).unwrap();
    let block = readme
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("bloc toml dans le README");
    let loaded = Config::parse(block).unwrap();
    assert!(loaded.issues.is_empty(), "{file} : {:?}", loaded.issues);
    assert_eq!(loaded.config.presets.len(), 2, "{file}");
    assert_eq!(loaded.config.schedule.points.len(), 2, "{file}");
    assert_eq!(loaded.config.auto.entity, entity, "{file}");
}

#[test]
fn readme_example_config_parses_without_issues() {
    check("README.md", "sensor.living_room_illuminance");
    check("README.fr.md", "sensor.salon_illuminance");
}
