//! L'exemple de configuration du README doit être accepté tel quel, sans réserve.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ha2lux_core::config::Config;

#[test]
fn readme_example_config_parses_without_issues() {
    let readme =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md")).unwrap();
    let block = readme
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("bloc toml dans le README");
    let loaded = Config::parse(block).unwrap();
    assert!(loaded.issues.is_empty(), "{:?}", loaded.issues);
    assert_eq!(loaded.config.presets.len(), 2);
    assert_eq!(loaded.config.schedule.points.len(), 2);
    assert_eq!(loaded.config.auto.entity, "sensor.salon_illuminance");
}
