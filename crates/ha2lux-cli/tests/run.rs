//! Exécution des commandes contre un faux démon : sorties, JSON, codes de sortie.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::time::Duration;

use ha2lux_cli::cli::{Parsed, parse};
use ha2lux_cli::client::{Answer, Call, CallError, Daemon, StatusInfo};
use ha2lux_cli::run::{Outcome, execute, execute_with_input};
use ha2lux_ipc::DisplayDto;

struct Fake {
    displays: Vec<DisplayDto>,
    status: StatusInfo,
    fail: Option<CallError>,
    calls: RefCell<Vec<Call>>,
    silent: bool,
}

impl Fake {
    fn new() -> Self {
        Self {
            displays: vec![
                screen("A", "DELL U2720Q", "controllable", 40),
                screen("B", "ASUS VG248", "controllable", 70),
            ],
            status: StatusInfo {
                master: Some(55),
                mode: "static".into(),
                step: 5,
                idle_level: 5,
                idle_minutes: 5,
                ..StatusInfo::default()
            },
            fail: None,
            calls: RefCell::new(Vec::new()),
            silent: false,
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    fn writes(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| !matches!(c, Call::List | Call::Status | Call::Sensors))
            .collect()
    }
}

fn screen(id: &str, name: &str, state: &str, level: i32) -> DisplayDto {
    DisplayDto {
        id: id.into(),
        name: name.into(),
        kind: "external".into(),
        state: state.into(),
        level,
        low: 0,
        high: 100,
        max_raw: 100,
    }
}

impl Daemon for Fake {
    async fn call(&self, call: Call) -> Result<Answer, CallError> {
        self.calls.borrow_mut().push(call.clone());
        if self.silent {
            std::future::pending::<()>().await;
        }
        match call {
            Call::List => Ok(Answer::List(self.displays.clone())),
            Call::Status => Ok(Answer::Status(Box::new(self.status.clone()))),
            Call::Sensors => Ok(Answer::Sensors(vec![(
                "sensor.salon_lux".into(),
                "Salon".into(),
                "Pièce".into(),
            )])),
            _ => self.fail.clone().map_or(Ok(Answer::Done), Err),
        }
    }
}

async fn run(daemon: &Fake, input: &[&str]) -> Outcome {
    let Ok(Parsed::Run(cli)) = parse(input.iter().map(|s| (*s).to_owned())) else {
        panic!("{input:?} : analyse")
    };
    execute(&cli, daemon, Duration::from_secs(5)).await
}

#[tokio::test]
async fn set_master_sends_one_call_and_prints_nothing() {
    let fake = Fake::new();
    let outcome = run(&fake, &["set", "40"]).await;
    assert_eq!(outcome, Outcome::default());
    assert_eq!(fake.calls(), [Call::SetMaster(40)]);
}

#[tokio::test]
async fn set_on_a_display_resolves_it_then_sets_its_own_level() {
    let fake = Fake::new();
    assert_eq!(
        run(&fake, &["set", "25", "--display", "asus"]).await.code,
        0
    );
    assert_eq!(
        fake.writes(),
        [Call::SetDisplay {
            id: "B".into(),
            level: 25
        }]
    );
    let fake = Fake::new();
    run(&fake, &["set", "10", "--display", "1"]).await;
    assert_eq!(
        fake.writes(),
        [Call::SetDisplay {
            id: "A".into(),
            level: 10
        }]
    );
}

#[tokio::test]
async fn t_cli_10_to_13_selection_failures_exit_3_and_show_the_list() {
    let fake = Fake::new();
    let out_of_range = run(&fake, &["set", "5", "--display", "7"]).await;
    assert_eq!(out_of_range.code, 3);
    assert!(
        out_of_range.stderr.contains("n°7 introuvable")
            && out_of_range.stderr.contains("DELL U2720Q"),
        "{}",
        out_of_range.stderr
    );
    assert!(fake.writes().is_empty());

    let mut twin = Fake::new();
    twin.displays = vec![
        screen("A", "Twin", "controllable", 1),
        screen("B", "Twin", "controllable", 2),
    ];
    let ambiguous = run(&twin, &["get", "--display", "twin"]).await;
    assert_eq!(ambiguous.code, 3);
    assert!(
        ambiguous.stderr.contains("n°1, n°2"),
        "{}",
        ambiguous.stderr
    );

    let none = run(&fake, &["get", "--display", "zzz"]).await;
    assert_eq!(none.code, 3);
    assert!(none.stderr.contains("aucun écran ne correspond"));
}

#[tokio::test]
async fn t_cli_14_unavailable_target_fails_for_set_but_get_works() {
    let mut fake = Fake::new();
    fake.displays[1] = screen("B", "ASUS VG248", "unavailable", -1);
    let set = run(&fake, &["set", "5", "--display", "2"]).await;
    assert_eq!(set.code, 3);
    assert!(set.stderr.contains("écran indisponible"), "{}", set.stderr);
    assert!(
        fake.writes().is_empty(),
        "aucune écriture vers un écran indisponible"
    );
    let get = run(&fake, &["get", "--display", "2"]).await;
    assert_eq!(get.code, 0);
    assert!(
        get.stdout.contains("ASUS VG248") && get.stdout.contains('?'),
        "{}",
        get.stdout
    );
    assert_eq!(run(&fake, &["up", "--display", "2"]).await.code, 3);
}

#[tokio::test]
async fn t_cli_16_set_without_controllable_display_fails() {
    let mut fake = Fake::new();
    fake.fail = Some(CallError::Failed("aucun écran contrôlable".into()));
    let outcome = run(&fake, &["set", "40"]).await;
    assert_eq!(outcome.code, 3);
    assert!(outcome.stderr.contains("aucun écran contrôlable"));
}

#[tokio::test]
async fn t_cli_17_unknown_preset_shows_the_choices() {
    let mut fake = Fake::new();
    fake.fail = Some(CallError::Failed(
        "preset inconnu : Zzz ; disponibles : Night=30, Day=80, Presentation=100".into(),
    ));
    let outcome = run(&fake, &["preset", "Zzz"]).await;
    assert_eq!(outcome.code, 3);
    assert!(
        outcome
            .stderr
            .contains("Night=30, Day=80, Presentation=100"),
        "{}",
        outcome.stderr
    );
    let ok = Fake::new();
    assert_eq!(run(&ok, &["preset", "Night"]).await.code, 0);
    assert_eq!(ok.calls(), [Call::ApplyPreset("Night".into())]);
}

#[tokio::test]
async fn t_cli_18_idle_dim_on_recalls_level_and_delay() {
    let mut fake = Fake::new();
    fake.status.idle_level = 10;
    fake.status.idle_minutes = 7;
    let outcome = run(&fake, &["idle-dim", "on"]).await;
    assert_eq!(outcome.code, 0);
    assert!(
        outcome.stdout.contains("niveau 10 %") && outcome.stdout.contains("7 min"),
        "{}",
        outcome.stdout
    );
    assert_eq!(
        fake.writes(),
        [Call::SetMode {
            feature: "idle-dim",
            on: true
        }]
    );
    assert!(
        run(&fake, &["idle-dim", "OFF"])
            .await
            .stdout
            .contains("désactivée")
    );
}

#[tokio::test]
async fn t_cli_19_schedule_on_without_points_warns_but_succeeds() {
    let fake = Fake::new();
    let outcome = run(&fake, &["schedule", "on"]).await;
    assert_eq!(outcome.code, 0);
    assert!(outcome.stdout.contains("planning activé"));
    assert!(outcome.stderr.contains("aucun point"), "{}", outcome.stderr);
    let mut with_points = Fake::new();
    with_points.status.schedule_points = 4;
    assert!(
        run(&with_points, &["schedule", "on"])
            .await
            .stderr
            .is_empty()
    );
    assert!(
        run(&fake, &["schedule", "off"]).await.stderr.is_empty(),
        "pas d'avertissement à la désactivation"
    );
}

#[tokio::test]
async fn up_and_down_use_the_daemon_default_step() {
    let mut fake = Fake::new();
    fake.status.step = 7;
    run(&fake, &["up"]).await;
    run(&fake, &["down"]).await;
    run(&fake, &["up", "20"]).await;
    run(&fake, &["down", "100"]).await;
    assert_eq!(
        fake.writes(),
        [
            Call::StepMaster(7),
            Call::StepMaster(-7),
            Call::StepMaster(20),
            Call::StepMaster(-100)
        ]
    );
}

#[tokio::test]
async fn up_down_on_one_display_changes_only_that_screens_level_and_clamps() {
    let fake = Fake::new();
    run(&fake, &["up", "10", "--display", "A"]).await;
    run(&fake, &["down", "50", "--display", "1"]).await;
    run(&fake, &["up", "50", "--display", "ASUS"]).await;
    assert_eq!(
        fake.writes(),
        [
            Call::SetDisplay {
                id: "A".into(),
                level: 50
            },
            Call::SetDisplay {
                id: "A".into(),
                level: 0
            },
            Call::SetDisplay {
                id: "B".into(),
                level: 100
            },
        ]
    );
}

#[tokio::test]
async fn get_and_list_text_output() {
    let fake = Fake::new();
    let list = run(&fake, &["list"]).await.stdout;
    assert!(
        list.contains(" 1  DELL U2720Q  [A]  externe  contrôlable  niveau 40 %  plage 0-100"),
        "{list}"
    );
    assert!(list.contains(" 2  ASUS VG248"));
    let get = run(&fake, &["get"]).await.stdout;
    assert!(get.starts_with("maître : 55 %  (source : static)"), "{get}");
    assert!(
        run(&fake, &["get", "--display", "ASUS"])
            .await
            .stdout
            .contains("ASUS VG248 (n°2) : 70 %")
    );
    let mut empty = Fake::new();
    empty.displays.clear();
    assert!(run(&empty, &["list"]).await.stdout.contains("aucun écran"));
}

#[tokio::test]
async fn get_shows_suspension_and_dimming() {
    let mut fake = Fake::new();
    fake.status.schedule_resume = Some("20:00".into());
    fake.status.dimmed = true;
    let get = run(&fake, &["get"]).await.stdout;
    assert!(
        get.contains("planning suspendu jusqu'à 20:00") && get.contains("atténuation"),
        "{get}"
    );
}

#[tokio::test]
async fn json_outputs_are_valid_and_complete() {
    let fake = Fake::new();
    let list: serde_json::Value =
        serde_json::from_str(&run(&fake, &["--json", "list"]).await.stdout).unwrap();
    assert_eq!(list[0]["id"], "A");
    assert_eq!(list[1]["level"], 70);
    let get: serde_json::Value =
        serde_json::from_str(&run(&fake, &["get", "--json"]).await.stdout).unwrap();
    assert_eq!(
        (get["master"].clone(), get["mode"].clone()),
        (55.into(), "static".into())
    );
    assert_eq!(get["displays"].as_array().unwrap().len(), 2);
    let one: serde_json::Value = serde_json::from_str(
        &run(&fake, &["get", "--display", "2", "--json"])
            .await
            .stdout,
    )
    .unwrap();
    assert_eq!(one["name"], "ASUS VG248");
    let done: serde_json::Value =
        serde_json::from_str(&run(&fake, &["set", "10", "--json"]).await.stdout).unwrap();
    assert_eq!(done["ok"], true);
    let auto: serde_json::Value =
        serde_json::from_str(&run(&fake, &["auto", "status", "--json"]).await.stdout).unwrap();
    assert_eq!(
        (auto["enabled"].clone(), auto["configured"].clone()),
        (false.into(), false.into())
    );
}

#[tokio::test]
async fn json_errors_go_to_stdout_with_the_same_exit_code() {
    let mut fake = Fake::new();
    fake.fail = Some(CallError::Failed("écran inconnu : x".into()));
    let outcome = run(&fake, &["--json", "set", "10"]).await;
    assert_eq!(outcome.code, 3);
    assert!(outcome.stderr.is_empty());
    let body: serde_json::Value = serde_json::from_str(&outcome.stdout).unwrap();
    assert_eq!(
        (body["ok"].clone(), body["code"].clone()),
        (false.into(), 3.into())
    );
    assert!(body["error"].as_str().unwrap().contains("écran inconnu"));
    let selection = run(&Fake::new(), &["--json", "set", "10", "--display", "9"]).await;
    assert_eq!(selection.code, 3);
    assert!(
        serde_json::from_str::<serde_json::Value>(&selection.stdout).is_ok(),
        "{}",
        selection.stdout
    );
}

#[tokio::test]
async fn exit_codes_follow_the_error_class() {
    for (error, code) in [
        (CallError::Usage("hors bornes".into()), 1),
        (CallError::Absent("pas de bus".into()), 2),
        (CallError::Failed("refusé".into()), 3),
        (CallError::NoReply("délai".into()), 4),
        (CallError::Other("anomalie".into()), 4),
    ] {
        let mut fake = Fake::new();
        fake.fail = Some(error.clone());
        assert_eq!(run(&fake, &["rescan"]).await.code, code, "{error:?}");
    }
    let mut absent = Fake::new();
    absent.fail = Some(CallError::Absent("x".into()));
    let outcome = run(&absent, &["reload"]).await;
    assert!(
        outcome.stderr.contains("systemctl --user start ha2luxd"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test(start_paused = true)]
async fn t_cli_15_a_daemon_that_does_not_answer_gives_exit_4() {
    let mut fake = Fake::new();
    fake.silent = true;
    let Ok(Parsed::Run(cli)) = parse(["set".to_owned(), "40".to_owned()]) else {
        panic!()
    };
    let outcome = execute(&cli, &fake, Duration::from_secs(3)).await;
    assert_eq!(outcome.code, 4);
    assert!(
        outcome.stderr.contains("ne répond pas"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn auto_commands() {
    let fake = Fake::new();
    assert!(
        run(&fake, &["auto", "status"])
            .await
            .stdout
            .contains("désactivée")
    );
    assert_eq!(
        run(&fake, &["auto", "reset-curve"]).await.stdout,
        "courbe apprise oubliée\n"
    );
    run(&fake, &["auto", "on"]).await;
    run(&fake, &["auto", "off"]).await;
    run(&fake, &["rescan"]).await;
    run(&fake, &["reload"]).await;
    assert_eq!(
        fake.writes(),
        [
            Call::ResetCurve,
            Call::SetMode {
                feature: "auto",
                on: true
            },
            Call::SetMode {
                feature: "auto",
                on: false
            },
            Call::Rescan,
            Call::Reload,
        ]
    );
}

#[tokio::test]
async fn one_command_per_call_means_no_extra_daemon_traffic() {
    let fake = Fake::new();
    run(&fake, &["set", "40"]).await;
    assert_eq!(fake.calls().len(), 1, "set sans --display : un seul appel");
}

async fn run_with_token(daemon: &Fake, input: &[&str], token: &'static str) -> Outcome {
    let Ok(Parsed::Run(cli)) = parse(input.iter().map(|s| (*s).to_owned())) else {
        panic!("{input:?} : analyse")
    };
    execute_with_input(&cli, daemon, Duration::from_secs(5), &|| {
        Ok(token.to_owned())
    })
    .await
}

#[tokio::test]
async fn auto_token_reads_stdin_and_never_prints_it() {
    let fake = Fake::new();
    let outcome = run_with_token(&fake, &["auto", "token"], "  abc.def \n").await;
    assert_eq!(outcome.code, 0);
    assert!(!outcome.stdout.contains("abc") && !outcome.stderr.contains("abc"));
    assert_eq!(
        fake.calls(),
        vec![Call::SetToken(ha2lux_cli::client::Secret("abc.def".into()))]
    );
    assert!(!format!("{:?}", fake.calls()).contains("abc"));
}

#[tokio::test]
async fn auto_token_refuses_empty_input() {
    let fake = Fake::new();
    let outcome = run_with_token(&fake, &["auto", "token"], "\n").await;
    assert_eq!(outcome.code, 1);
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn auto_token_clear_and_sensors() {
    let fake = Fake::new();
    assert_eq!(run(&fake, &["auto", "token", "--clear"]).await.code, 0);
    assert_eq!(fake.calls(), vec![Call::ClearToken]);
    let outcome = run(&fake, &["auto", "sensors"]).await;
    assert_eq!(outcome.code, 0);
    assert!(outcome.stdout.contains("sensor.salon_lux") && outcome.stdout.contains("Pièce"));
    let json = run(&fake, &["--json", "auto", "sensors"]).await;
    assert!(json.stdout.contains("\"entity\": \"sensor.salon_lux\""));
}

#[tokio::test]
async fn auto_status_shows_the_live_state() {
    let mut fake = Fake::new();
    fake.status.auto_enabled = true;
    fake.status.auto_configured = true;
    fake.status.ha_token_present = true;
    fake.status.ha_health = "ok".into();
    fake.status.lux_smoothed = Some(320.0);
    fake.status.auto_target = Some(42);
    fake.status.curve = vec![(0.0, 10), (1000.0, 90)];
    fake.status.curve_default = true;
    let text = run(&fake, &["auto", "status"]).await.stdout;
    for needle in [
        "actif",
        "320 lx",
        "42 %",
        "55 %",
        "par défaut",
        "1000 lx → 90 %",
    ] {
        assert!(text.contains(needle), "{needle} absent de {text}");
    }
    fake.status.ha_auth_rejected = true;
    assert!(
        run(&fake, &["auto", "status"])
            .await
            .stdout
            .contains("jeton refusé")
    );
}
