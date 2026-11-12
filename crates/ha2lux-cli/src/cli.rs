//! Argument parsing (clap): a single subcommand per call, same
//! validations as the daemon (which remains the authority).

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use ha2lux_i18n::t;

fn bounded(text: &str, low: u32, high: u32) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(value) if (low..=high).contains(&value) => Ok(value),
        _ => Err(t!(
            "cli-err-not-integer-between",
            text = text,
            low = low.to_string(),
            high = high.to_string()
        )),
    }
}

/// Level from 0 to 100.
fn level(text: &str) -> Result<u32, String> {
    bounded(text, 0, 100)
}

/// Step from 1 to 100.
fn step(text: &str) -> Result<u32, String> {
    bounded(text, 1, 100)
}

/// Sets the brightness of all displays by talking to the `ha2luxd` daemon.
#[derive(Parser, Debug, PartialEq, Eq)]
#[command(
    name = "ha2lux",
    version,
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct Cli {
    /// Shows the help.
    #[arg(short = 'h', long = "help", global = true, action = clap::ArgAction::Help, help_heading = "global")]
    #[allow(dead_code)]
    help: Option<bool>,
    /// Shows the version.
    #[arg(short = 'V', long = "version", action = clap::ArgAction::Version)]
    #[allow(dead_code)]
    version: Option<bool>,
    /// JSON output (for scripts); errors are also returned as JSON.
    #[arg(long, global = true, help_heading = "global")]
    pub json: bool,
    /// Command to run (only one per call).
    #[command(subcommand)]
    pub command: Command,
}

/// Display selector shared by `set`, `up`, `down` and `get`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct DisplayOpt {
    /// Target display: number in `ha2lux list`, exact name or substring of the name
    /// (case-insensitive). Without this option, the command acts on the master level.
    #[arg(long, value_name = "ÉCRAN", value_parser = non_empty)]
    pub display: Option<String>,
}

fn non_empty(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err(t!("cli-err-empty-value"))
    } else {
        Ok(value.to_owned())
    }
}

/// Arguments of `set`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct SetArgs {
    /// Level from 0 to 100.
    #[arg(value_name = "NIVEAU", value_parser = level)]
    pub level: u32,
    /// Display selector.
    #[command(flatten)]
    pub target: DisplayOpt,
}

/// Arguments of `up` and `down`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct StepArgs {
    /// Step from 1 to 100 (default: the daemon's `general.step` setting, 5).
    #[arg(value_name = "PAS", value_parser = step)]
    pub step: Option<u32>,
    /// Display selector.
    #[command(flatten)]
    pub target: DisplayOpt,
}

/// Arguments of `get`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct GetArgs {
    /// Display selector.
    #[command(flatten)]
    pub target: DisplayOpt,
}

/// `on` / `off` switch (case-insensitive).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Toggle {
    /// Enable.
    On,
    /// Disable.
    Off,
}

impl Toggle {
    /// True for `on`.
    pub fn is_on(self) -> bool {
        self == Self::On
    }
}

/// Subcommands of `auto`.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum AutoCommand {
    /// Enables auto-brightness.
    On,
    /// Disables auto-brightness.
    Off,
    /// Shows the auto-brightness state.
    Status,
    /// Forgets the learned curve.
    ResetCurve,
    /// Stores the Home Assistant token (read from standard input, never as an argument).
    Token {
        /// Deletes the stored token instead of entering one.
        #[arg(long)]
        clear: bool,
    },
    /// Lists the Home Assistant illuminance sensors.
    Sensors,
}

/// Commands of `ha2lux`.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Command {
    /// Sets the level (master, or that of a display with --display).
    Set(SetArgs),
    /// Raises the level by one step.
    Up(StepArgs),
    /// Lowers the level by one step.
    Down(StepArgs),
    /// Shows the master level and those of the displays.
    Get(GetArgs),
    /// Lists the displays, numbered.
    List,
    /// Applies a preset (case-insensitive).
    Preset {
        /// Preset name.
        name: String,
    },
    /// Enables or disables the time schedule.
    Schedule {
        /// `on` or `off`.
        #[arg(value_enum, ignore_case = true, value_name = "ÉTAT")]
        state: Toggle,
    },
    /// Enables or disables dimming on inactivity.
    IdleDim {
        /// `on` or `off`.
        #[arg(value_enum, ignore_case = true, value_name = "ÉTAT")]
        state: Toggle,
    },
    /// Auto-brightness (Home Assistant sensor).
    #[command(subcommand)]
    Auto(AutoCommand),
    /// Restarts display enumeration.
    Rescan,
    /// Reloads the configuration.
    Reload,
    /// Diagnostics of the DDC/CI and backlight chain (no daemon).
    Doctor {
        /// Sends REAL DDC/CI frames (brightness read) to external displays.
        #[arg(long)]
        probe: bool,
    },
}

/// Result of parsing.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Command to run.
    Run(Cli),
    /// Help or version: text to display, success.
    Output(String),
}

/// Usage error: text to display on stderr, exit code 1.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

/// Lowercases the subcommand (and the sub-subcommand of `auto`) so that
/// `SET 5` works; values, options and names (preset, display) are left untouched.
fn normalize(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut seen_command = false;
    let mut after_auto = false;
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            out.push(arg);
            continue;
        }
        if arg == "--display" {
            skip_value = true;
            out.push(arg);
        } else if arg.starts_with('-') {
            out.push(arg);
        } else if !seen_command {
            seen_command = true;
            let lower = arg.to_lowercase();
            after_auto = lower == "auto";
            out.push(lower);
        } else if after_auto {
            after_auto = false;
            out.push(arg.to_lowercase());
        } else {
            out.push(arg);
        }
    }
    out
}

/// Help template: description, usage, then arguments.
fn usage_template() -> String {
    format!(
        "{{about}}\n\n{} {{usage}}\n\n{{all-args}}\n",
        t!("cli-usage")
    )
}

/// Catalog text for `key`, if any.
fn catalog(key: &str) -> Option<String> {
    let text = t!(key);
    (text != key).then_some(text)
}

/// Applies to a command, and recursively to its subcommands, the texts of the
/// catalog: `cli-cmd-<path>` (description), `cli-arg-<path>-<id>` (help
/// of an argument) and `cli-val-<path>-<id>` (name of its value). The path of the
/// root is `root`, that of an `auto token` subcommand is `auto-token`.
fn localize(command: clap::Command) -> clap::Command {
    localize_node(command, "root")
}

fn localize_node(mut command: clap::Command, path: &str) -> clap::Command {
    command = command
        .help_template(usage_template())
        .subcommand_help_heading(t!("cli-heading-commands"))
        .subcommand_value_name(t!("cli-value-command"));
    if let Some(about) = catalog(&format!("cli-cmd-{path}")) {
        command = command.about(about);
    }
    let ids: Vec<String> = command
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .collect();
    for id in ids {
        let help = catalog(&format!("cli-arg-{path}-{id}"));
        let value_name = catalog(&format!("cli-val-{path}-{id}"));
        command = command.mut_arg(id.as_str(), |mut arg| {
            if let Some(help) = help {
                arg = arg.help(help);
            }
            if let Some(value_name) = value_name {
                arg = arg.value_name(value_name);
            }
            if arg.get_help_heading().is_some() {
                arg = arg.help_heading(t!("cli-heading-global"));
            }
            arg
        });
    }
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    for name in names {
        let child = if path == "root" {
            name.clone()
        } else {
            format!("{path}-{name}")
        };
        command = command.mut_subcommand(name.as_str(), |sub| localize_node(sub, &child));
    }
    command
}

/// The complete clap command, in the user's language.
pub fn command() -> clap::Command {
    localize(Cli::command())
}

/// Translated usage message built from a clap error.
fn friendly(error: &clap::Error) -> String {
    use clap::error::{ContextKind, ErrorKind};
    use std::error::Error;
    let get = |kind| error.get(kind).map(ToString::to_string).unwrap_or_default();
    let body = match error.kind() {
        ErrorKind::InvalidSubcommand => t!(
            "cli-usage-unknown-command",
            value = get(ContextKind::InvalidSubcommand)
        ),
        ErrorKind::UnknownArgument => t!(
            "cli-usage-unknown-argument",
            arg = get(ContextKind::InvalidArg)
        ),
        ErrorKind::MissingRequiredArgument => t!(
            "cli-usage-missing-argument",
            arg = get(ContextKind::InvalidArg)
        ),
        ErrorKind::MissingSubcommand => t!("cli-usage-missing-command"),
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
            let mut reason = error
                .source()
                .map(|s| t!("cli-usage-reason", reason = s.to_string()))
                .unwrap_or_default();
            let valid = get(ContextKind::ValidValue);
            if !valid.is_empty() {
                reason.push_str(&t!("cli-usage-possible-values", values = valid));
            }
            t!(
                "cli-usage-invalid-value",
                value = get(ContextKind::InvalidValue),
                arg = get(ContextKind::InvalidArg),
                reason = reason
            )
        }
        ErrorKind::ArgumentConflict => {
            t!("cli-usage-conflict", arg = get(ContextKind::InvalidArg))
        }
        ErrorKind::TooManyValues | ErrorKind::WrongNumberOfValues => {
            t!("cli-usage-too-many", arg = get(ContextKind::InvalidArg))
        }
        ErrorKind::NoEquals | ErrorKind::TooFewValues => {
            t!(
                "cli-usage-missing-value",
                arg = get(ContextKind::InvalidArg)
            )
        }
        _ => error
            .to_string()
            .trim_start_matches("error: ")
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
    };
    format!(
        "{}\n{}\n",
        t!("cli-error", message = body),
        t!("cli-usage-hint")
    )
}

/// Parses the arguments (without the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, UsageError> {
    let args: Vec<String> = args.into_iter().collect();
    if args.is_empty() {
        return Ok(Parsed::Output(command().render_help().to_string()));
    }
    let argv = std::iter::once("ha2lux".to_owned()).chain(normalize(args));
    let parsed = command()
        .try_get_matches_from(argv)
        .and_then(|matches| <Cli as clap::FromArgMatches>::from_arg_matches(&matches));
    match parsed {
        Ok(cli) => Ok(Parsed::Run(cli)),
        Err(error) => match error.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                Ok(Parsed::Output(error.render().to_string()))
            }
            _ => Err(UsageError(friendly(&error))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command and argument of the CLI has its text in the catalog
    /// (otherwise the help would show the untranslated fallback description).
    #[test]
    fn every_command_and_argument_has_a_catalog_entry() {
        fn walk(command: &clap::Command, path: &str, missing: &mut Vec<String>) {
            if catalog(&format!("cli-cmd-{path}")).is_none() {
                missing.push(format!("cli-cmd-{path}"));
            }
            for arg in command.get_arguments() {
                let id = arg.get_id();
                if catalog(&format!("cli-arg-{path}-{id}")).is_none() {
                    missing.push(format!("cli-arg-{path}-{id}"));
                }
            }
            for sub in command.get_subcommands() {
                let name = sub.get_name();
                let child = if path == "root" {
                    name.to_owned()
                } else {
                    format!("{path}-{name}")
                };
                walk(sub, &child, missing);
            }
        }
        let mut missing = Vec::new();
        walk(&Cli::command(), "root", &mut missing);
        assert!(missing.is_empty(), "clés manquantes : {missing:?}");
    }
}
