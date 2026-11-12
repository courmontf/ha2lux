//! Analyse des arguments (clap) : une seule sous-commande par appel, mêmes
//! validations que le démon (qui reste l'autorité).

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

/// Niveau de 0 à 100.
fn level(text: &str) -> Result<u32, String> {
    bounded(text, 0, 100)
}

/// Pas de 1 à 100.
fn step(text: &str) -> Result<u32, String> {
    bounded(text, 1, 100)
}

/// Règle la luminosité de tous les écrans en parlant au démon `ha2luxd`.
#[derive(Parser, Debug, PartialEq, Eq)]
#[command(
    name = "ha2lux",
    version,
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
pub struct Cli {
    /// Affiche l'aide.
    #[arg(short = 'h', long = "help", global = true, action = clap::ArgAction::Help, help_heading = "global")]
    #[allow(dead_code)]
    help: Option<bool>,
    /// Affiche la version.
    #[arg(short = 'V', long = "version", action = clap::ArgAction::Version)]
    #[allow(dead_code)]
    version: Option<bool>,
    /// Sortie JSON (pour les scripts) ; les erreurs sont aussi renvoyées en JSON.
    #[arg(long, global = true, help_heading = "global")]
    pub json: bool,
    /// Commande à exécuter (une seule par appel).
    #[command(subcommand)]
    pub command: Command,
}

/// Sélecteur d'écran partagé par `set`, `up`, `down` et `get`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct DisplayOpt {
    /// Écran visé : numéro dans `ha2lux list`, nom exact ou sous-chaîne du nom
    /// (insensible à la casse). Sans cette option, la commande agit sur le niveau maître.
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

/// Arguments de `set`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct SetArgs {
    /// Niveau de 0 à 100.
    #[arg(value_name = "NIVEAU", value_parser = level)]
    pub level: u32,
    /// Sélecteur d'écran.
    #[command(flatten)]
    pub target: DisplayOpt,
}

/// Arguments de `up` et `down`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct StepArgs {
    /// Pas de 1 à 100 (défaut : le réglage `general.step` du démon, 5).
    #[arg(value_name = "PAS", value_parser = step)]
    pub step: Option<u32>,
    /// Sélecteur d'écran.
    #[command(flatten)]
    pub target: DisplayOpt,
}

/// Arguments de `get`.
#[derive(Args, Debug, PartialEq, Eq)]
pub struct GetArgs {
    /// Sélecteur d'écran.
    #[command(flatten)]
    pub target: DisplayOpt,
}

/// Interrupteur `on` / `off` (insensible à la casse).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Toggle {
    /// Activer.
    On,
    /// Désactiver.
    Off,
}

impl Toggle {
    /// Vrai pour `on`.
    pub fn is_on(self) -> bool {
        self == Self::On
    }
}

/// Sous-commandes de `auto`.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum AutoCommand {
    /// Active l'auto-luminosité.
    On,
    /// Désactive l'auto-luminosité.
    Off,
    /// Affiche l'état de l'auto-luminosité.
    Status,
    /// Oublie la courbe apprise.
    ResetCurve,
    /// Enregistre le jeton Home Assistant (lu sur l'entrée standard, jamais en argument).
    Token {
        /// Supprime le jeton enregistré au lieu d'en saisir un.
        #[arg(long)]
        clear: bool,
    },
    /// Liste les capteurs d'éclairement de Home Assistant.
    Sensors,
}

/// Commandes de `ha2lux`.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Command {
    /// Définit le niveau (maître, ou celui d'un écran avec --display).
    Set(SetArgs),
    /// Augmente le niveau d'un pas.
    Up(StepArgs),
    /// Diminue le niveau d'un pas.
    Down(StepArgs),
    /// Affiche le niveau maître et ceux des écrans.
    Get(GetArgs),
    /// Liste les écrans, numérotés.
    List,
    /// Applique un preset (insensible à la casse).
    Preset {
        /// Nom du preset.
        name: String,
    },
    /// Active ou désactive le planning horaire.
    Schedule {
        /// `on` ou `off`.
        #[arg(value_enum, ignore_case = true, value_name = "ÉTAT")]
        state: Toggle,
    },
    /// Active ou désactive l'atténuation à l'inactivité.
    IdleDim {
        /// `on` ou `off`.
        #[arg(value_enum, ignore_case = true, value_name = "ÉTAT")]
        state: Toggle,
    },
    /// Auto-luminosité (capteur Home Assistant).
    #[command(subcommand)]
    Auto(AutoCommand),
    /// Relance l'énumération des écrans.
    Rescan,
    /// Recharge la configuration.
    Reload,
    /// Diagnostic de la chaîne DDC/CI et backlight (sans démon).
    Doctor {
        /// Envoie de VRAIES trames DDC/CI (lecture de la luminosité) aux écrans externes.
        #[arg(long)]
        probe: bool,
    },
}

/// Résultat de l'analyse.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Commande à exécuter.
    Run(Cli),
    /// Aide ou version : texte à afficher, succès.
    Output(String),
}

/// Erreur d'usage : texte à afficher sur stderr, code de sortie 1.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

/// Met en minuscules la sous-commande (et la sous-sous-commande de `auto`) pour que
/// `SET 5` fonctionne ; les valeurs, options et noms (preset, écran) sont laissés intacts.
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

/// Modèle d'aide : description, usage, puis arguments.
fn usage_template() -> String {
    format!(
        "{{about}}\n\n{} {{usage}}\n\n{{all-args}}\n",
        t!("cli-usage")
    )
}

/// Texte du catalogue pour `key`, s'il existe.
fn catalog(key: &str) -> Option<String> {
    let text = t!(key);
    (text != key).then_some(text)
}

/// Applique à une commande, et récursivement à ses sous-commandes, les textes du
/// catalogue : `cli-cmd-<chemin>` (description), `cli-arg-<chemin>-<id>` (aide
/// d'un argument) et `cli-val-<chemin>-<id>` (nom de sa valeur). Le chemin de la
/// racine est `root`, celui d'une sous-commande `auto token` est `auto-token`.
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

/// La commande clap complète, dans la langue de l'utilisateur.
pub fn command() -> clap::Command {
    localize(Cli::command())
}

/// Message d'usage traduit à partir d'une erreur de clap.
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

/// Analyse les arguments (sans le nom du programme).
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

    /// Chaque commande et chaque argument de la CLI a son texte dans le catalogue
    /// (sinon l'aide afficherait la description de repli, non traduite).
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
