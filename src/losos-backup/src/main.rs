//! losos-backup: each person's backups to a LosOS box (docs/backup.md).
//!
//! Its commands, which derisk Settings' Backups page runs:
//!
//!     losos-backup status                 # JSON: settings, the last run, the boxes
//!     losos-backup choose BOX             # pin BOX's certificate, print its fingerprint
//!     losos-backup sign-in cloud|git NAME # the password on stdin
//!     losos-backup sign-out cloud|git
//!     losos-backup on|off
//!     losos-backup every HOURS            # how long between backups, 1 to 168
//!     losos-backup run [--if-due]         # back up now, or when one is due
//!
//! `run --if-due` is what the user timer starts every hour.

use std::{
    io::{self, IsTerminal, Read},
    process::ExitCode,
};

use losos_backup::{
    boxes, cloud,
    config::{Account, Paths, Secrets, MAX_HOURS},
    forge, host_name,
    run::{self, Outcome},
    Error,
};
use miette::{miette, IntoDiagnostic, Result};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: losos-backup status | choose BOX | sign-in cloud|git NAME | sign-out cloud|git | on | off | every HOURS | run [--if-due]";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
    match main_inner() {
        Ok(code) => code,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

/// A box error as a diagnostic.
fn report(e: Error) -> miette::Report {
    miette!("{e}")
}

fn main_inner() -> Result<ExitCode> {
    let paths = Paths::from_env().ok_or_else(|| miette!("HOME is not set"))?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["status"] => {
            println!(
                "{}",
                serde_json::to_string(&run::status(&paths)).into_diagnostic()?
            );
        }
        ["choose", name] => choose(&paths, name)?,
        ["sign-in", which @ ("cloud" | "git"), login] => sign_in(&paths, which, login)?,
        ["sign-out", which @ ("cloud" | "git")] => sign_out(&paths, which)?,
        [on @ ("on" | "off")] => {
            let mut config = paths.load_config().into_diagnostic()?;
            config.enabled = *on == "on";
            paths.save_config(&config).into_diagnostic()?;
        }
        ["every", hours] => {
            let hours = hours
                .parse::<u32>()
                .ok()
                .filter(|h| (1..=MAX_HOURS).contains(h))
                .ok_or_else(|| miette!("HOURS is a whole number from 1 to {MAX_HOURS}"))?;
            let mut config = paths.load_config().into_diagnostic()?;
            config.every_hours = hours;
            paths.save_config(&config).into_diagnostic()?;
        }
        ["run", rest @ ..] if rest.iter().all(|a| *a == "--if-due") => {
            match run::run(&paths, !rest.is_empty()).map_err(report)? {
                Some(r) if r.outcome == Outcome::Failed => return Ok(ExitCode::FAILURE),
                _ => {}
            }
        }
        _ => {
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(2));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Pins `name`'s certificate and makes it the box, forgetting the
/// accounts of another box.
fn choose(paths: &Paths, name: &str) -> Result<()> {
    let name = boxes::normalize(name);
    let route = route(&name)?;
    let pem = boxes::fetch_certificate(&route).map_err(report)?;
    let der = boxes::pem_to_der(&pem).ok_or_else(|| miette!("no certificate"))?;
    let mut config = paths.load_config().into_diagnostic()?;
    if config.box_name != name {
        config.cloud = None;
        config.git = None;
        paths.save_secrets(&Secrets::default()).into_diagnostic()?;
    }
    config.box_name = name;
    config.fingerprint = boxes::fingerprint(&der);
    std::fs::create_dir_all(&paths.config).into_diagnostic()?;
    std::fs::write(paths.cert_file(), pem).into_diagnostic()?;
    paths.save_config(&config).into_diagnostic()?;
    println!("{}", config.fingerprint);
    Ok(())
}

/// The way to `name` now, starting a LosOS boxes machine when one is
/// needed; it is left running, as the person is choosing a box and
/// signing in at the screen.
fn route(name: &str) -> Result<boxes::Route> {
    let hosts = run::hosts_dir();
    match boxes::find(name, &hosts) {
        Ok(route) => Ok(route),
        Err(boxes::Missing::NotRouted) => boxes::start_machine_for(name, &hosts)
            .map(|(route, _)| route)
            .map_err(report),
        Err(boxes::Missing::NotFound(why)) => Err(miette!("{name} is not on this network ({why})")),
    }
}

/// The pinned agent for the chosen box.
fn agent(paths: &Paths, name: &str) -> Result<(ureq::Agent, boxes::Route)> {
    let route = route(name)?;
    let pem =
        std::fs::read_to_string(paths.cert_file()).map_err(|_| miette!("choose the box first"))?;
    let der = boxes::pem_to_der(&pem).ok_or_else(|| miette!("the box's certificate is damaged"))?;
    Ok((boxes::pinned(&route, &der).map_err(report)?, route))
}

fn sign_in(paths: &Paths, which: &str, login: &str) -> Result<()> {
    let mut config = paths.load_config().into_diagnostic()?;
    if config.box_name.is_empty() {
        return Err(miette!("choose the box first"));
    }
    let mut password = String::new();
    io::stdin()
        .read_to_string(&mut password)
        .into_diagnostic()?;
    let password = password.trim_end_matches(['\n', '\r']);
    let (agent, route) = agent(paths, &config.box_name)?;
    let mut secrets = paths.load_secrets().into_diagnostic()?;
    let login = login.trim().to_owned();
    if which == "cloud" {
        let (app_password, user) =
            cloud::sign_in(&agent, &cloud::base(&route.name), &login, password).map_err(report)?;
        secrets.cloud = Some(app_password);
        config.cloud = Some(Account { login, user });
    } else {
        let (token, user) = forge::sign_in(
            &agent,
            &forge::base(&route.name),
            &login,
            password,
            &host_name(),
        )
        .map_err(report)?;
        secrets.git = Some(token);
        config.git = Some(Account { login, user });
    }
    paths.save_secrets(&secrets).into_diagnostic()?;
    paths.save_config(&config).into_diagnostic()
}

/// Forgets an account. The cloud's app password is revoked on the box
/// when the box can be reached; a Git token can only be deleted with the
/// password, so it stays listed in the account's settings there.
fn sign_out(paths: &Paths, which: &str) -> Result<()> {
    let mut config = paths.load_config().into_diagnostic()?;
    let mut secrets = paths.load_secrets().into_diagnostic()?;
    if which == "cloud" {
        if let (Some(account), Some(password)) = (&config.cloud, &secrets.cloud) {
            if let Ok((agent, route)) = agent(paths, &config.box_name) {
                let _ =
                    cloud::sign_out(&agent, &cloud::base(&route.name), &account.login, password);
            }
        }
        config.cloud = None;
        secrets.cloud = None;
    } else {
        config.git = None;
        secrets.git = None;
    }
    paths.save_secrets(&secrets).into_diagnostic()?;
    paths.save_config(&config).into_diagnostic()
}
