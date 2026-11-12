//! Exécution d'une commande : appels au démon, rendu, code de sortie.
//!
//! Aucune E/S ici hormis les appels au [`Daemon`] : le résultat est un [`Outcome`]
//! (texte pour stdout et stderr, code de sortie), ce qui rend la logique testable.

use std::time::Duration;

use ha2lux_i18n::t;
use ha2lux_ipc::{DisplayDto, FEATURE_AUTO, FEATURE_IDLE_DIM, FEATURE_SCHEDULE};
use serde::Serialize;

use crate::cli::{AutoCommand, Cli, Command, DisplayOpt, StepArgs};
use crate::client::{Answer, Call, CallError, Daemon, Secret, StatusInfo};
use crate::output;
use crate::select::select;

/// Résultat d'une commande.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Texte pour la sortie standard.
    pub stdout: String,
    /// Texte pour la sortie d'erreur.
    pub stderr: String,
    /// Code de sortie : 0 succès, 1 usage, 2 démon absent, 3 échec, 4 démon sans réponse.
    pub code: u8,
}

/// Ligne d'erreur pour stderr.
fn error_line(message: &str) -> String {
    t!("cli-error", message = message) + "\n"
}

impl Outcome {
    fn out(text: impl Into<String>) -> Self {
        Self {
            stdout: text.into(),
            ..Self::default()
        }
    }

    fn ok(json: bool) -> Self {
        if json {
            Self::out("{\"ok\":true}\n")
        } else {
            Self::default()
        }
    }

    fn json<T: Serialize>(value: &T) -> Self {
        match serde_json::to_string_pretty(value) {
            Ok(text) => Self::out(text + "\n"),
            Err(error) => Self {
                stderr: error_line(&t!("cli-err-json", error = error.to_string())),
                code: 3,
                ..Self::default()
            },
        }
    }

    /// Échec avec un message ; en mode JSON, l'erreur est renvoyée en JSON sur stdout.
    fn fail(json: bool, code: u8, message: &str) -> Self {
        if json {
            let body = serde_json::json!({ "ok": false, "code": code, "error": message });
            Self {
                stdout: format!("{body}\n"),
                code,
                ..Self::default()
            }
        } else {
            Self {
                stderr: error_line(message),
                code,
                ..Self::default()
            }
        }
    }

    fn from_call(json: bool, error: &CallError) -> Self {
        Self::fail(json, error.exit_code(), &error.message())
    }
}

async fn call<D: Daemon>(daemon: &D, timeout: Duration, call: Call) -> Result<Answer, CallError> {
    match tokio::time::timeout(timeout, daemon.call(call)).await {
        Ok(result) => result,
        Err(_) => Err(CallError::NoReply(t!(
            "cli-err-no-reply",
            seconds = timeout.as_secs_f32().to_string()
        ))),
    }
}

async fn list<D: Daemon>(daemon: &D, timeout: Duration) -> Result<Vec<DisplayDto>, CallError> {
    match call(daemon, timeout, Call::List).await? {
        Answer::List(list) => Ok(list),
        _ => Err(CallError::Other(t!("cli-err-unexpected-reply"))),
    }
}

async fn status<D: Daemon>(daemon: &D, timeout: Duration) -> Result<StatusInfo, CallError> {
    match call(daemon, timeout, Call::Status).await? {
        Answer::Status(status) => Ok(*status),
        _ => Err(CallError::Other(t!("cli-err-unexpected-reply"))),
    }
}

/// Échec de sélection d'écran : message, puis la liste pour s'y retrouver.
fn selection_failure(json: bool, message: &str, list: &[DisplayDto]) -> Outcome {
    let mut outcome = Outcome::fail(json, 3, message);
    if !json {
        outcome.stderr.push_str(&t!("cli-displays-available"));
        outcome.stderr.push('\n');
        outcome.stderr.push_str(&output::render_list(list));
        outcome.stderr.push_str(&t!("cli-see-list"));
        outcome.stderr.push('\n');
    }
    outcome
}

/// Résout `--display` ; `Ok(None)` sans option.
async fn resolve<D: Daemon>(
    daemon: &D,
    timeout: Duration,
    json: bool,
    opt: &DisplayOpt,
) -> Result<Option<(usize, DisplayDto, Vec<DisplayDto>)>, Outcome> {
    let Some(spec) = &opt.display else {
        return Ok(None);
    };
    let displays = list(daemon, timeout)
        .await
        .map_err(|e| Outcome::from_call(json, &e))?;
    match select(&displays, spec) {
        Ok((number, display)) => {
            let display = display.clone();
            Ok(Some((number, display, displays)))
        }
        Err(error) => Err(selection_failure(json, &error.to_string(), &displays)),
    }
}

fn require_controllable(
    json: bool,
    display: &DisplayDto,
    all: &[DisplayDto],
) -> Result<(), Outcome> {
    if display.state == "controllable" {
        Ok(())
    } else {
        Err(selection_failure(
            json,
            &t!("cli-err-display-unavailable", name = display.name.as_str()),
            all,
        ))
    }
}

async fn step<D: Daemon>(
    daemon: &D,
    timeout: Duration,
    json: bool,
    args: &StepArgs,
    sign: i32,
) -> Outcome {
    let amount = match args.step {
        Some(step) => i64::from(step),
        None => match status(daemon, timeout).await {
            Ok(status) => i64::from(status.step),
            Err(error) => return Outcome::from_call(json, &error),
        },
    };
    let signed = i64::from(sign) * amount;
    match resolve(daemon, timeout, json, &args.target).await {
        Err(outcome) => outcome,
        Ok(None) => match call(
            daemon,
            timeout,
            Call::StepMaster(i32::try_from(signed).unwrap_or(0)),
        )
        .await
        {
            Ok(_) => Outcome::ok(json),
            Err(error) => Outcome::from_call(json, &error),
        },
        Ok(Some((_, display, all))) => {
            if let Err(outcome) = require_controllable(json, &display, &all) {
                return outcome;
            }
            if display.level < 0 {
                return Outcome::fail(
                    json,
                    3,
                    &t!("cli-err-level-unknown", name = display.name.as_str()),
                );
            }
            let level = (i64::from(display.level) + signed).clamp(0, 100);
            match call(
                daemon,
                timeout,
                Call::SetDisplay {
                    id: display.id,
                    level: u32::try_from(level).unwrap_or(0),
                },
            )
            .await
            {
                Ok(_) => Outcome::ok(json),
                Err(error) => Outcome::from_call(json, &error),
            }
        }
    }
}

async fn toggle<D: Daemon>(
    daemon: &D,
    timeout: Duration,
    json: bool,
    feature: &'static str,
    on: bool,
) -> Outcome {
    if let Err(error) = call(daemon, timeout, Call::SetMode { feature, on }).await {
        return Outcome::from_call(json, &error);
    }
    let status = match status(daemon, timeout).await {
        Ok(status) => status,
        Err(error) => return Outcome::from_call(json, &error),
    };
    if json {
        return Outcome::ok(true);
    }
    let mut outcome = Outcome::out(output::toggle_message(feature, on, &status));
    if feature == FEATURE_SCHEDULE && on && status.schedule_points == 0 {
        outcome.stderr = t!("cli-warn-schedule-empty") + "\n";
    }
    outcome
}

/// Exécute `cli` contre `daemon`, chaque appel étant borné par `timeout`.
pub async fn execute<D: Daemon>(cli: &Cli, daemon: &D, timeout: Duration) -> Outcome {
    execute_with_input(cli, daemon, timeout, &read_token_from_stdin).await
}

/// Lit le jeton sur l'entrée standard : une ligne, 4 Kio au plus.
fn read_token_from_stdin() -> std::io::Result<String> {
    use std::io::{BufRead, Read};
    let mut text = String::new();
    std::io::BufReader::new(std::io::stdin().take(4096)).read_line(&mut text)?;
    Ok(text)
}

/// Comme [`execute`], avec la source du jeton (entrée standard en production).
pub async fn execute_with_input<D: Daemon>(
    cli: &Cli,
    daemon: &D,
    timeout: Duration,
    input: &dyn Fn() -> std::io::Result<String>,
) -> Outcome {
    let json = cli.json;
    match &cli.command {
        Command::Set(args) => match resolve(daemon, timeout, json, &args.target).await {
            Err(outcome) => outcome,
            Ok(None) => match call(daemon, timeout, Call::SetMaster(args.level)).await {
                Ok(_) => Outcome::ok(json),
                Err(error) => Outcome::from_call(json, &error),
            },
            Ok(Some((_, display, all))) => {
                if let Err(outcome) = require_controllable(json, &display, &all) {
                    return outcome;
                }
                match call(
                    daemon,
                    timeout,
                    Call::SetDisplay {
                        id: display.id,
                        level: args.level,
                    },
                )
                .await
                {
                    Ok(_) => Outcome::ok(json),
                    Err(error) => Outcome::from_call(json, &error),
                }
            }
        },
        Command::Up(args) => step(daemon, timeout, json, args, 1).await,
        Command::Down(args) => step(daemon, timeout, json, args, -1).await,
        Command::Get(args) => {
            let displays = match list(daemon, timeout).await {
                Ok(displays) => displays,
                Err(error) => return Outcome::from_call(json, &error),
            };
            if let Some(spec) = &args.target.display {
                return match select(&displays, spec) {
                    Ok((number, display)) if json => {
                        let _ = number;
                        Outcome::json(display)
                    }
                    Ok((number, display)) => {
                        Outcome::out(output::render_display_level(number, display))
                    }
                    Err(error) => selection_failure(json, &error.to_string(), &displays),
                };
            }
            match status(daemon, timeout).await {
                Ok(status) if json => Outcome::json(&output::GetJson {
                    master: status.master,
                    mode: &status.mode,
                    displays: &displays,
                }),
                Ok(status) => Outcome::out(output::render_get(&status, &displays)),
                Err(error) => Outcome::from_call(json, &error),
            }
        }
        Command::List => match list(daemon, timeout).await {
            Ok(displays) if json => Outcome::json(&displays),
            Ok(displays) => Outcome::out(output::render_list(&displays)),
            Err(error) => Outcome::from_call(json, &error),
        },
        Command::Preset { name } => {
            match call(daemon, timeout, Call::ApplyPreset(name.clone())).await {
                Ok(_) => Outcome::ok(json),
                Err(error) => Outcome::from_call(json, &error),
            }
        }
        Command::Schedule { state } => {
            toggle(daemon, timeout, json, FEATURE_SCHEDULE, state.is_on()).await
        }
        Command::IdleDim { state } => {
            toggle(daemon, timeout, json, FEATURE_IDLE_DIM, state.is_on()).await
        }
        Command::Auto(AutoCommand::On) => toggle(daemon, timeout, json, FEATURE_AUTO, true).await,
        Command::Auto(AutoCommand::Off) => toggle(daemon, timeout, json, FEATURE_AUTO, false).await,
        Command::Auto(AutoCommand::Status) => match status(daemon, timeout).await {
            Ok(status) if json => Outcome::json(&output::AutoJson {
                enabled: status.auto_enabled,
                configured: status.auto_configured,
                health: &status.ha_health,
                mode: &status.mode,
                token_present: status.ha_token_present,
                auth_rejected: status.ha_auth_rejected,
                websocket: status.ha_websocket,
                raw: status.ha_raw.as_deref(),
                lux_smoothed: status.lux_smoothed,
                lux_latest: status.lux_latest,
                adjusting: status.auto_adjusting,
                target: status.auto_target,
                level: status.master,
                curve: &status.curve,
                curve_default: status.curve_default,
            }),
            Ok(status) => Outcome::out(output::render_auto(&status)),
            Err(error) => Outcome::from_call(json, &error),
        },
        Command::Auto(AutoCommand::Token { clear: true }) => {
            match call(daemon, timeout, Call::ClearToken).await {
                Ok(_) if json => Outcome::ok(true),
                Ok(_) => Outcome::out(t!("cli-token-cleared") + "\n"),
                Err(error) => Outcome::from_call(json, &error),
            }
        }
        Command::Auto(AutoCommand::Token { clear: false }) => {
            let token = match input() {
                Ok(text) => text.trim().to_owned(),
                Err(error) => {
                    return Outcome::fail(
                        json,
                        1,
                        &t!("cli-err-token-read", error = error.to_string()),
                    );
                }
            };
            if token.is_empty() {
                return Outcome::fail(
                    json,
                    1,
                    &t!("cli-err-token-empty"),
                );
            }
            match call(daemon, timeout, Call::SetToken(Secret(token))).await {
                Ok(_) if json => Outcome::ok(true),
                Ok(_) => Outcome::out(t!("cli-token-saved") + "\n"),
                Err(error) => Outcome::from_call(json, &error),
            }
        }
        Command::Auto(AutoCommand::Sensors) => match call(daemon, timeout, Call::Sensors).await {
            Ok(Answer::Sensors(list)) if json => Outcome::json(
                &list
                    .iter()
                    .map(|(entity, name, area)| {
                        serde_json::json!({"entity": entity, "name": name, "area": area})
                    })
                    .collect::<Vec<_>>(),
            ),
            Ok(Answer::Sensors(list)) => Outcome::out(output::render_sensors(&list)),
            Ok(_) => Outcome::from_call(
                json,
                &CallError::Other(t!("cli-err-unexpected-reply-short")),
            ),
            Err(error) => Outcome::from_call(json, &error),
        },
        Command::Auto(AutoCommand::ResetCurve) => {
            match call(daemon, timeout, Call::ResetCurve).await {
                Ok(_) if json => Outcome::ok(true),
                Ok(_) => Outcome::out(t!("cli-curve-reset") + "\n"),
                Err(error) => Outcome::from_call(json, &error),
            }
        }
        Command::Rescan => match call(daemon, timeout, Call::Rescan).await {
            Ok(_) => Outcome::ok(json),
            Err(error) => Outcome::from_call(json, &error),
        },
        Command::Reload => match call(daemon, timeout, Call::Reload).await {
            Ok(_) => Outcome::ok(json),
            Err(error) => Outcome::from_call(json, &error),
        },
        Command::Doctor { probe } => doctor(json, *probe).await,
    }
}

/// `ha2lux doctor` : autonome, ne passe pas par le démon.
async fn doctor(json: bool, probe: bool) -> Outcome {
    use ha2lux_backends::doctor::{DoctorOptions, run};
    let report = run(&DoctorOptions::system(probe)).await;
    let code = report.exit_code();
    if json {
        return match report.to_json() {
            Ok(text) => Outcome {
                stdout: text + "\n",
                code,
                ..Outcome::default()
            },
            Err(error) => Outcome::fail(true, 3, &t!("cli-err-json", error = error.to_string())),
        };
    }
    Outcome {
        stdout: report.render_text(),
        code,
        ..Outcome::default()
    }
}
