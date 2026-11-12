//! Local demonstration of the backlight backend, without the daemon.
//!
//! ```text
//! cargo run -p ha2lux-backends --example backlight -- list
//! cargo run -p ha2lux-backends --example backlight -- get  [--device NOM]
//! cargo run -p ha2lux-backends --example backlight -- set 40 [--device NOM] [--floor N]
//! ```
//!
//! `list` and `get` are read-only and do not touch the D-Bus bus.
//! `set` really writes the brightness through logind (`SetBrightness`), without root.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use ha2lux_backends::backlight::{
    BacklightBackend, BacklightInfo, DEFAULT_SYSFS, LogindSetter, preferred, scan,
};
use ha2lux_backends::task::{DisplayEvent, TaskConfig, spawn_display_task};
use ha2lux_core::DisplayBackend;
use ha2lux_core::hw::{backlight_raw_from_level, level_from_raw};

const USAGE: &str =
    "usage : backlight list | get [--device NOM] | set <0-100> [--device NOM] [--floor N]";

struct Options {
    command: String,
    level: Option<i64>,
    device: Option<String>,
    floor: u32,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut iter = args.iter();
    let command = iter.next().ok_or(USAGE)?.clone();
    let mut options = Options {
        command,
        level: None,
        device: None,
        floor: 1,
    };
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--device" => {
                options.device = Some(iter.next().ok_or("--device attend un nom")?.clone())
            }
            "--floor" => {
                options.floor = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--floor attend un entier")?;
            }
            value if options.level.is_none() && !value.starts_with("--") => {
                options.level = Some(
                    value
                        .parse()
                        .map_err(|_| format!("niveau invalide : {value}"))?,
                );
            }
            other => return Err(format!("argument inattendu : {other}")),
        }
    }
    match options.command.as_str() {
        "list" | "get" => {}
        "set" => match options.level {
            Some(level) if (0..=100).contains(&level) => {}
            Some(level) => return Err(format!("niveau hors bornes : {level} (attendu 0 à 100)")),
            None => return Err(USAGE.to_owned()),
        },
        other => return Err(format!("commande inconnue : {other}\n{USAGE}")),
    }
    Ok(options)
}

fn select<'a>(
    list: &'a [BacklightInfo],
    wanted: Option<&str>,
) -> Result<&'a BacklightInfo, String> {
    match wanted {
        Some(name) => list
            .iter()
            .find(|b| b.name == name)
            .ok_or_else(|| format!("rétroéclairage inconnu : {name}")),
        None => preferred(list)
            .ok_or_else(|| "aucun rétroéclairage dans /sys/class/backlight".to_owned()),
    }
}

async fn run(options: Options) -> Result<(), String> {
    let devices = scan(Path::new(DEFAULT_SYSFS)).await;
    match options.command.as_str() {
        "list" => {
            if devices.is_empty() {
                println!("aucun rétroéclairage dans {DEFAULT_SYSFS}");
            }
            let best = preferred(&devices).map(|b| b.name.clone());
            for device in &devices {
                let mut backend = BacklightBackend::new(device, LogindSetter::new());
                let current = backend
                    .read()
                    .await
                    .map_or("?".to_owned(), |v| v.to_string());
                let mark = if best.as_deref() == Some(device.name.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{mark} {} type={:?} max={} courant={current}",
                    device.name, device.kind, device.max
                );
            }
            Ok(())
        }
        "get" => {
            let device = select(&devices, options.device.as_deref())?;
            let mut backend = BacklightBackend::new(device, LogindSetter::new());
            let raw = backend.read().await.map_err(|e| e.to_string())?;
            println!(
                "{} : {raw}/{} = {} %",
                device.name,
                device.max,
                level_from_raw(raw, device.max)
            );
            Ok(())
        }
        "set" => {
            let level = options.level.ok_or(USAGE)?;
            let device = select(&devices, options.device.as_deref())?;
            let raw = backlight_raw_from_level(level, device.max, options.floor);
            println!(
                "{} : niveau {level} % → brut {raw}/{} (plancher {})",
                device.name, device.max, options.floor
            );

            let backend = BacklightBackend::new(device, LogindSetter::new());
            let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
            let handle = spawn_display_task(backend, TaskConfig::backlight(), events);
            handle.set_target(raw);
            let outcome = tokio::time::timeout(Duration::from_secs(10), received.recv()).await;
            let verdict = match outcome {
                Ok(Some(DisplayEvent::Applied { raw, .. })) => {
                    let back = handle.read().await.map_err(|e| e.to_string())?;
                    println!("écrit : {raw} ; relu dans sysfs : {back}");
                    Ok(())
                }
                Ok(Some(DisplayEvent::WriteFailed { error, fatal, .. })) => Err(format!(
                    "échec de l'écriture ({}) : {error}",
                    if fatal {
                        "définitif"
                    } else {
                        "après retries"
                    }
                )),
                Ok(Some(DisplayEvent::CoolingDown { .. })) | Ok(None) => {
                    Err("la tâche s'est arrêtée".to_owned())
                }
                Err(_) => Err("aucune réponse en 10 s".to_owned()),
            };
            let stats = handle.stats();
            println!(
                "compteurs : ok={} échecs={} retries={} abandonnées={}",
                stats.writes_ok, stats.writes_failed, stats.retries, stats.abandoned_ops
            );
            handle.shutdown().await;
            verdict
        }
        _ => Err(USAGE.to_owned()),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse(&args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(1);
        }
    };
    match run(options).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("erreur : {message}");
            ExitCode::from(3)
        }
    }
}
