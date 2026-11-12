//! T-CLI-01..09: argument parsing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ha2lux_cli::cli::{
    AutoCommand, Cli, Command, DisplayOpt, GetArgs, Parsed, SetArgs, StepArgs, Toggle, UsageError,
    parse,
};
use proptest::prelude::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn run(list: &[&str]) -> Cli {
    match parse(args(list)) {
        Ok(Parsed::Run(cli)) => cli,
        other => panic!("{list:?} → {other:?}"),
    }
}

fn usage(list: &[&str]) -> UsageError {
    parse(args(list)).expect_err(&format!("{list:?} aurait dû être refusé"))
}

fn no_display() -> DisplayOpt {
    DisplayOpt { display: None }
}

#[test]
fn t_cli_01_no_argument_prints_help() {
    let Ok(Parsed::Output(text)) = parse(Vec::new()) else {
        panic!("aide attendue")
    };
    for command in [
        "set", "up", "down", "get", "list", "preset", "schedule", "idle-dim", "auto", "rescan",
        "reload", "doctor",
    ] {
        assert!(
            text.contains(command),
            "{command} absent de l'aide :\n{text}"
        );
    }
    assert!(text.contains("--json"));
}

#[test]
fn help_and_version_are_successes() {
    assert!(matches!(parse(args(&["--help"])), Ok(Parsed::Output(_))));
    assert!(
        matches!(parse(args(&["set", "--help"])), Ok(Parsed::Output(t)) if t.contains("--display"))
    );
    let Ok(Parsed::Output(version)) = parse(args(&["--version"])) else {
        panic!()
    };
    assert!(version.contains("ha2lux"));
}

#[test]
fn t_cli_02_set() {
    for (input, level) in [
        (&["set", "40"][..], 40),
        (&["set", "0"], 0),
        (&["SET", "5"], 5),
        (&["Set", "100"], 100),
    ] {
        assert_eq!(
            run(input).command,
            Command::Set(SetArgs {
                level,
                target: no_display()
            }),
            "{input:?}"
        );
    }
}

#[test]
fn t_cli_03_set_usage_errors() {
    for bad in [
        &["set", "101"][..],
        &["set", "4x"],
        &["set"],
        &["set", "-1"],
        &["set", "1.5"],
        &["set", ""],
        &["set", "40", "50"],
    ] {
        usage(bad);
    }
}

#[test]
fn t_cli_04_up_down() {
    assert_eq!(
        run(&["up"]).command,
        Command::Up(StepArgs {
            step: None,
            target: no_display()
        })
    );
    assert_eq!(
        run(&["down", "10"]).command,
        Command::Down(StepArgs {
            step: Some(10),
            target: no_display()
        })
    );
    assert_eq!(
        run(&["up", "100"]).command,
        Command::Up(StepArgs {
            step: Some(100),
            target: no_display()
        })
    );
    for bad in [
        &["down", "0"][..],
        &["up", "101"],
        &["up", "-5"],
        &["down", "x"],
    ] {
        usage(bad);
    }
}

#[test]
fn t_cli_05_two_commands_or_unknown_options_are_usage_errors() {
    for bad in [
        &["get", "--list"][..],
        &["get", "list"],
        &["list", "get"],
        &["--bogus"],
        &["bogus"],
        &["set", "5", "up"],
        &["get", "-z"],
    ] {
        usage(bad);
    }
}

#[test]
fn t_cli_06_display_option_validation() {
    for bad in [
        &["set", "5", "--display"][..],
        &["set", "5", "--display", ""],
        &["set", "5", "--display", "  "],
        &["get", "--display", "A", "--display", "B"],
        &["up", "--display"],
    ] {
        usage(bad);
    }
    let cli = run(&["set", "5", "--display", "Dell U27"]);
    assert_eq!(
        cli.command,
        Command::Set(SetArgs {
            level: 5,
            target: DisplayOpt {
                display: Some("Dell U27".into())
            }
        })
    );
    // The option may precede the value.
    assert_eq!(
        run(&["set", "--display", "2", "5"]).command,
        run(&["set", "5", "--display", "2"]).command
    );
    assert_eq!(
        run(&["get", "--display", "1"]).command,
        Command::Get(GetArgs {
            target: DisplayOpt {
                display: Some("1".into())
            }
        })
    );
}

#[test]
fn t_cli_07_display_is_only_valid_with_set_up_down_get() {
    for command in [
        &["preset", "Night"][..],
        &["schedule", "on"],
        &["idle-dim", "on"],
        &["auto", "on"],
        &["auto", "status"],
        &["rescan"],
        &["reload"],
        &["list"],
        &["doctor"],
    ] {
        let mut full: Vec<&str> = command.to_vec();
        full.extend(["--display", "1"]);
        usage(&full);
    }
}

#[test]
fn t_cli_08_preset() {
    assert_eq!(
        run(&["preset", "Night"]).command,
        Command::Preset {
            name: "Night".into()
        }
    );
    usage(&["preset"]);
    usage(&["preset", "-x"]);
    // The name is passed as is: case is only compared by the daemon.
    assert_eq!(
        run(&["preset", "NiGhT"]).command,
        Command::Preset {
            name: "NiGhT".into()
        }
    );
    assert_eq!(
        run(&["PRESET", "Day"]).command,
        Command::Preset { name: "Day".into() }
    );
}

#[test]
fn t_cli_09_toggles() {
    assert_eq!(
        run(&["schedule", "on"]).command,
        Command::Schedule { state: Toggle::On }
    );
    assert_eq!(
        run(&["idle-dim", "OFF"]).command,
        Command::IdleDim { state: Toggle::Off }
    );
    assert_eq!(
        run(&["IDLE-DIM", "On"]).command,
        Command::IdleDim { state: Toggle::On }
    );
    usage(&["schedule", "maybe"]);
    usage(&["schedule"]);
    usage(&["idle-dim", "1"]);
}

#[test]
fn auto_subcommands() {
    assert_eq!(run(&["auto", "on"]).command, Command::Auto(AutoCommand::On));
    assert_eq!(
        run(&["AUTO", "OFF"]).command,
        Command::Auto(AutoCommand::Off)
    );
    assert_eq!(
        run(&["auto", "status"]).command,
        Command::Auto(AutoCommand::Status)
    );
    assert_eq!(
        run(&["auto", "reset-curve"]).command,
        Command::Auto(AutoCommand::ResetCurve)
    );
    usage(&["auto"]);
    usage(&["auto", "bogus"]);
}

#[test]
fn json_is_global() {
    assert!(run(&["--json", "list"]).json);
    assert!(run(&["list", "--json"]).json);
    assert!(run(&["get", "--display", "1", "--json"]).json);
    assert!(!run(&["list"]).json);
}

#[test]
fn doctor_options() {
    assert_eq!(run(&["doctor"]).command, Command::Doctor { probe: false });
    assert_eq!(
        run(&["doctor", "--probe"]).command,
        Command::Doctor { probe: true }
    );
    usage(&["list", "--probe"]);
}

#[test]
fn option_values_are_never_lowercased() {
    let cli = run(&["SET", "5", "--display", "DELL U2720Q"]);
    assert_eq!(
        cli.command,
        Command::Set(SetArgs {
            level: 5,
            target: DisplayOpt {
                display: Some("DELL U2720Q".into())
            }
        })
    );
}

proptest! {
    #[test]
    fn parser_never_panics(input in proptest::collection::vec(".{0,12}", 0..6)) {
        let _ = parse(input);
    }
}

#[test]
fn usage_errors_and_help_are_in_french() {
    let level = usage(&["set", "101"]).0;
    assert!(
        level.contains("valeur invalide")
            && level.contains("entre 0 et 100")
            && level.contains("NIVEAU"),
        "{level}"
    );
    assert!(usage(&["bogus"]).0.contains("commande inconnue : bogus"));
    assert!(usage(&["set"]).0.contains("argument requis manquant"));
    assert!(
        usage(&["get", "--list"])
            .0
            .contains("option ou argument inattendu")
    );
    assert!(usage(&["preset"]).0.contains("NAME") || usage(&["preset"]).0.contains("requis"));
    let toggle = usage(&["schedule", "maybe"]).0;
    assert!(
        toggle.contains("valeur invalide")
            && toggle.contains("valeurs possibles")
            && toggle.contains("on"),
        "{toggle}"
    );
    assert!(usage(&["down", "0"]).0.contains("entre 1 et 100"));
    assert!(
        usage(&["set", "5", "--display", ""])
            .0
            .contains("ne peut pas être vide")
    );
    for message in [usage(&["set", "101"]).0, usage(&["bogus"]).0] {
        assert!(message.contains("--help"), "{message}");
        assert!(
            !message.contains("invalid value") && !message.contains("unexpected"),
            "{message}"
        );
    }
    let Ok(Parsed::Output(help)) = parse(args(&["--help"])) else {
        panic!()
    };
    assert!(
        help.contains("Utilisation :")
            && help.contains("Commandes")
            && help.contains("Affiche l'aide"),
        "{help}"
    );
    assert!(
        !help.contains("Print help") && !help.contains("Usage:"),
        "{help}"
    );
    let Ok(Parsed::Output(set_help)) = parse(args(&["set", "-h"])) else {
        panic!()
    };
    assert!(
        set_help.contains("NIVEAU") && set_help.contains("--display"),
        "{set_help}"
    );
}
