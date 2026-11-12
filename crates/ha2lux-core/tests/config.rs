//! T-CFG (partie pure) : lecture tolérante et sérialisation (spec §10).
#![allow(clippy::unwrap_used, clippy::expect_used)]
use ha2lux_core::config::{Config, ConfigError, Severity, is_valid_entity_id};

fn load(text: &str) -> (Config, Vec<(String, Severity)>) {
    let loaded = Config::parse(text).unwrap();
    let issues = loaded
        .issues
        .iter()
        .map(|i| (i.path.clone(), i.severity))
        .collect();
    (loaded.config, issues)
}

#[test]
fn t_cfg_01_empty_file_gives_defaults() {
    let (c, issues) = load("");
    assert!(issues.is_empty());
    assert_eq!(c, Config::default());
    let presets: Vec<_> = c
        .presets
        .iter()
        .map(|p| (p.name.as_str(), p.level))
        .collect();
    assert_eq!(presets, [("Night", 30), ("Day", 80), ("Presentation", 100)]);
    assert_eq!(c.general.step, 5);
    assert_eq!(
        (c.idle.level, c.idle.minutes, c.idle.enabled),
        (5, 5, false)
    );
}

#[test]
fn default_round_trips_without_issue() {
    let text = Config::default().to_toml().unwrap();
    let loaded = Config::parse(&text).unwrap();
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.issues.is_empty(), "{:?}\n{text}", loaded.issues);
}

#[test]
fn full_round_trip() {
    let src = r#"
version = 1
[general]
step = 10
[presets]
Zen = 15
Bright = 95
[displays."DEL-A0B1-1234"]
range = [40, 100]
floor = 3
[displays."XYZ"]
range = [0, 60]
[schedule]
enabled = true
points = [{ time = "07:00", level = 60 }, { time = "23:00", level = 25 }]
[idle]
enabled = true
level = 10
minutes = 7
level_is_absolute = true
[auto]
enabled = true
url = "http://homeassistant.local:8123"
entity = "sensor.salon_illuminance"
label = "Salon"
poll_seconds = 20
[auto.tuning]
alpha = 0.1
ramp_step_ms = 200
"#;
    let first = Config::parse(src).unwrap();
    assert!(first.issues.is_empty(), "{:?}", first.issues);
    let text = first.config.to_toml().unwrap();
    let second = Config::parse(&text).unwrap();
    assert_eq!(second.config, first.config);
    assert!(second.issues.is_empty());
    assert_eq!(
        first.config.presets[0].name, "Zen",
        "ordre du fichier conservé"
    );
    assert_eq!(first.config.displays[0].floor, 3);
    assert_eq!(first.config.schedule.points.len(), 2);
    assert!(first.config.build_schedule().is_ok());
}

#[test]
fn t_cfg_02_step_clamped() {
    for (raw, want) in [(0, 1), (75, 50), (-3, 1), (5, 5)] {
        let (c, _) = load(&format!("[general]\nstep = {raw}"));
        assert_eq!(c.general.step, want);
    }
    let (c, issues) = load("[general]\nstep = \"x\"");
    assert_eq!(c.general.step, 5);
    assert_eq!(issues[0].0, "general.step");
}

#[test]
fn t_cfg_03_idle_minutes() {
    assert_eq!(load("[idle]\nminutes = 0").0.idle.minutes, 1);
    assert_eq!(load("[idle]\nminutes = 5000").0.idle.minutes, 1440);
}

#[test]
fn t_cfg_04_idle_level() {
    assert_eq!(load("[idle]\nlevel = -4").0.idle.level, 0);
    assert_eq!(load("[idle]\nlevel = 140").0.idle.level, 100);
}

#[test]
fn t_cfg_05_ranges() {
    let (c, _) = load(
        "[displays.a]\nrange=[70,80]\n[displays.b]\nrange=[0,5]\n[displays.c]\nrange=[-10,140]\n[displays.d]\nrange=\"x\"\n[displays.e]\nrange=[1,\"y\"]\n[displays.f]\nrange=[1,2,3]\n[displays.g]\n",
    );
    let r = |i: usize| (c.displays[i].range.low(), c.displays[i].range.high());
    assert_eq!(r(0), (60, 80));
    assert_eq!(r(1), (0, 20));
    assert_eq!(r(2), (0, 100));
    assert_eq!(
        (r(3), r(4), r(5), r(6)),
        ((0, 100), (0, 100), (0, 100), (0, 100))
    );
    assert_eq!(c.displays[6].floor, 1, "plancher par défaut");
}

#[test]
fn t_cfg_06_presets() {
    let many: String = (0..12).map(|i| format!("p{i} = {i}\n")).collect();
    let (c, issues) = load(&format!("[presets]\n{many}"));
    assert_eq!(c.presets.len(), 10);
    assert_eq!(c.presets[0].name, "p0");
    assert_eq!(c.presets[9].name, "p9");
    assert!(!issues.is_empty());

    let (c, _) = load("[presets]\nBig = 130\n\"\" = 5\nNeg = -4\nstr = \"a\"");
    let got: Vec<_> = c
        .presets
        .iter()
        .map(|p| (p.name.as_str(), p.level))
        .collect();
    assert_eq!(got, [("Big", 100), ("Neg", 0)]);

    let long = "n".repeat(64);
    let (c, _) = load(&format!("[presets]\n{long} = 5\n{} = 6", "n".repeat(63)));
    assert_eq!(c.presets.len(), 1);
    assert_eq!(c.presets[0].level, 6);

    let (c, _) = load("[presets]\nNight = 1\nnight = 2");
    assert_eq!(c.presets.len(), 1, "doublon insensible à la casse écarté");
    assert_eq!(c.preset("NIGHT").unwrap().level, 1);
}

#[test]
fn t_cfg_08_newer_version_refused() {
    assert_eq!(
        Config::parse("version = 99\n[general]\nstep = 7"),
        Err(ConfigError::UnsupportedVersion {
            found: 99,
            supported: 1
        })
    );
}

#[test]
fn t_cfg_10_token_in_file_ignored_and_not_echoed() {
    let secret = "eyJ-SECRET-TOKEN-123";
    let src = format!(
        "[auto]\ntoken = \"{secret}\"\nurl = \"http://ha.local:8123\"\nentity = \"sensor.lux\""
    );
    let loaded = Config::parse(&src).unwrap();
    assert!(loaded.issues.iter().any(|i| i.path == "auto.token"));
    let dump = format!("{:?}{}", loaded, loaded.config.to_toml().unwrap());
    assert!(!dump.contains(secret), "le jeton ne doit jamais ressortir");
}

#[test]
fn syntax_error_is_fatal_and_leaks_no_content() {
    let err = Config::parse("[auto]\ntoken = \"SECRET-VALUE\nentity = 1").unwrap_err();
    assert!(matches!(err, ConfigError::Syntax { .. }));
    assert!(!err.to_string().contains("SECRET-VALUE"));
}

#[test]
fn t_cfg_12_unplugged_display_kept_up_to_32() {
    let many: String = (0..40)
        .map(|i| format!("[displays.d{i}]\nrange=[0,100]\n"))
        .collect();
    let (c, issues) = load(&many);
    assert_eq!(c.displays.len(), 32);
    assert_eq!(c.displays[31].id, "d31");
    assert!(!issues.is_empty());
}

#[test]
fn schedule_points_sanitized() {
    let (c, issues) = load(
        r#"[schedule]
enabled = true
points = [
  { time = "08:00", level = 50 },
  { time = "08:00", level = 60 },
  { time = "24:00", level = 10 },
  { time = "8:00", level = 10 },
  { time = "09:00", level = 150 },
  { time = "07:00" },
  "oops",
]"#,
    );
    let pts: Vec<_> = c
        .schedule
        .points
        .iter()
        .map(|p| (p.time.to_string(), p.level))
        .collect();
    assert_eq!(pts, [("08:00".to_owned(), 50), ("09:00".to_owned(), 100)]);
    assert!(issues.len() >= 5);
    assert!(c.build_schedule().is_ok());

    let many: String = (0..30)
        .map(|i| format!("{{ time = \"{:02}:{:02}\", level = 1 }},", i % 24, i))
        .collect();
    let (c, _) = load(&format!("[schedule]\npoints = [{many}]"));
    assert!(c.schedule.points.len() <= 24);
}

#[test]
fn auto_requires_url_and_entity() {
    let (c, issues) = load("[auto]\nenabled = true");
    assert!(!c.auto.enabled);
    assert!(issues.iter().any(|(p, _)| p == "auto.enabled"));
    let (c, _) = load("[auto]\nenabled = true\nurl = \"ha.local:8123\"\nentity = \"sensor.lux\"");
    assert!(c.auto.enabled);
    assert_eq!(c.auto.url, "http://ha.local:8123", "URL normalisée");
}

#[test]
fn auto_rejects_bad_url_and_entity() {
    let (c, issues) = load("[auto]\nurl = \"http://user:pw@ha.local\"\nentity = \"sensor/../x?y\"");
    assert!(c.auto.url.is_empty() && c.auto.entity.is_empty());
    assert!(issues.iter().any(|(p, _)| p == "auto.url"));
    assert!(issues.iter().any(|(p, _)| p == "auto.entity"));
}

#[test]
fn auto_http_public_warns() {
    let loaded = Config::parse("[auto]\nurl = \"http://ha.example.com\"").unwrap();
    assert!(
        loaded
            .issues
            .iter()
            .any(|i| i.path == "auto.url" && i.severity == Severity::Warning)
    );
    assert_eq!(loaded.config.auto.url, "http://ha.example.com");
}

#[test]
fn tuning_bounds_and_conversions() {
    let (c, _) = load("[auto.tuning]\nalpha = 5\ngate_minimum = 0\nramp_step_ms = 1\nbogus = 1");
    let t = &c.auto.tuning;
    assert_eq!(t.alpha, 1.0);
    assert_eq!(t.gate_minimum, 1);
    assert_eq!(t.ramp_step_ms, 20);
    assert_eq!(t.filter_params().alpha, 1.0);
    assert_eq!(t.gate_params().minimum, 1);
    assert_eq!(
        Config::default().auto.tuning.learn_policy(),
        ha2lux_core::LearnPolicy::default()
    );
}

#[test]
fn wrong_section_type_keeps_rest() {
    let (c, issues) = load("idle = 3\n[general]\nstep = 9");
    assert_eq!(c.idle, Config::default().idle);
    assert_eq!(c.general.step, 9);
    assert!(issues.iter().any(|(p, _)| p == "idle"));
}

#[test]
fn entity_id_validation() {
    assert!(is_valid_entity_id("sensor.salon_illuminance"));
    for bad in [
        "",
        "sensor",
        "sensor.",
        ".x",
        "sensor/x.y",
        "a..b",
        "sensor.x?y",
        "Sensor.x",
        "a.b.c",
        &format!("a.{}", "b".repeat(130)),
    ] {
        assert!(!is_valid_entity_id(bad), "{bad}");
    }
}

#[test]
fn language_defaults_to_auto_and_is_normalized() {
    assert_eq!(load("").0.general.language, "auto");
    assert_eq!(
        load("[general]\nlanguage = \"fr\"").0.general.language,
        "fr"
    );
    // Casse et espaces tolérés ; `pt-br` est un code valide.
    assert_eq!(
        load("[general]\nlanguage = \" EN \"").0.general.language,
        "en"
    );
    assert_eq!(
        load("[general]\nlanguage = \"pt-BR\"").0.general.language,
        "pt-br"
    );
    let (config, issues) = load("[general]\nlanguage = \"AUTO\"");
    assert_eq!(config.general.language, "auto");
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn invalid_language_falls_back_to_auto_with_a_warning() {
    for bad in [
        "français",
        "fr fr",
        "-fr",
        "1",
        "klingon-de-la-mort",
        "fr/en",
    ] {
        let (config, issues) = load(&format!("[general]\nlanguage = \"{bad}\""));
        assert_eq!(config.general.language, "auto", "{bad}");
        assert!(
            issues
                .iter()
                .any(|(path, sev)| path == "general.language" && *sev == Severity::Warning),
            "{bad} : {issues:?}"
        );
    }
    // Mauvais type : signalé aussi.
    let (config, issues) = load("[general]\nlanguage = 3");
    assert_eq!(config.general.language, "auto");
    assert!(issues.iter().any(|(path, _)| path == "general.language"));
}

#[test]
fn language_survives_a_rewrite_of_the_file() {
    let (config, _) = load("[general]\nlanguage = \"en\"");
    let text = config.to_toml().unwrap();
    assert!(text.contains("language = \"en\""), "{text}");
    assert_eq!(load(&text).0.general.language, "en");
}
