//! losos-backup: each person's backups to a LosOS box (docs/backup.md).
//!
//! Its commands, which derisk Settings' Backups page runs:
//!
//!     losos-backup status                 # JSON: settings, the last run, the boxes
//!     losos-backup choose BOX             # pin BOX's certificate, print its fingerprint
//!     losos-backup sign-in box            # both, through LosOS Login; prints the link and code
//!     losos-backup sign-in cloud NAME     # the password on stdin
//!     losos-backup sign-in git            # a LosOS Git token on stdin
//!     losos-backup sign-out cloud|git
//!     losos-backup on|off
//!     losos-backup run [--if-due]         # back up now, or when a day has passed
//!
//! `run --if-due` is what the user timer starts every hour.

use std::{
    io::{self, IsTerminal, Read, Write},
    process::ExitCode,
};

use losos_backup::{
    boxes, cloud,
    config::{Account, Paths, Secrets},
    forge, host_name, login,
    run::{self, Outcome},
    Error,
};
use miette::{miette, IntoDiagnostic, Result};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: losos-backup status | choose BOX | sign-in box | sign-in cloud NAME | sign-in git | sign-out cloud|git | on | off | run [--if-due]";

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
        ["sign-in", "box"] => sign_in_box(&paths)?,
        ["sign-in", "cloud", login] => sign_in(&paths, "cloud", login)?,
        ["sign-in", "git"] => sign_in(&paths, "git", "")?,
        ["sign-out", which @ ("cloud" | "git")] => sign_out(&paths, which)?,
        [on @ ("on" | "off")] => {
            let mut config = paths.load_config().into_diagnostic()?;
            config.enabled = *on == "on";
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

/// Both accounts at once, through LosOS Login (`login`): the link and the
/// code go to standard output as one JSON line for Settings to show, then
/// this waits for the person to approve them in a browser, and prints
/// which accounts it signed in to.
fn sign_in_box(paths: &Paths) -> Result<()> {
    let mut config = paths.load_config().into_diagnostic()?;
    if config.box_name.is_empty() {
        return Err(miette!("choose the box first"));
    }
    let (agent, route) = agent(paths, &config.box_name)?;
    let code = login::start(&agent, &route.name).map_err(report)?;
    let mut out = io::stdout().lock();
    writeln!(out, "{}", code.prompt()).into_diagnostic()?;
    out.flush().into_diagnostic()?;
    drop(out);
    if io::stderr().is_terminal() {
        eprintln!("Open {} and enter {}.", code.link(), code.user_code);
    }
    let token = login::finish(&agent, &route.name, &code).map_err(report)?;
    let made = login::credentials(&agent, &route.name, &token, &host_name()).map_err(report)?;
    let mut secrets = paths.load_secrets().into_diagnostic()?;
    // An app that made nothing this time keeps what it had.
    if let Some(cloud) = &made.cloud {
        secrets.cloud = Some(cloud.secret.clone());
        config.cloud = Some(Account {
            login: cloud.user.clone(),
            user: cloud.user.clone(),
        });
    }
    if let Some(git) = &made.git {
        secrets.git = Some(git.secret.clone());
        config.git = Some(Account {
            login: git.user.clone(),
            user: git.user.clone(),
        });
    } else if made.git_off {
        config.git = None;
        secrets.git = None;
    }
    paths.save_secrets(&secrets).into_diagnostic()?;
    paths.save_config(&config).into_diagnostic()?;
    for problem in &made.problems {
        tracing::warn!("{problem}");
    }
    println!(
        "{}",
        serde_json::json!({
            "cloud": made.cloud.map(|c| c.user),
            "git": made.git.map(|g| g.user),
            "gitOff": made.git_off,
            "problems": made.problems,
        })
    );
    Ok(())
}

/// One account with what is on standard input: LosOS cloud with the
/// account's password, which buys an app password of this computer's,
/// or LosOS Git with a token made in its settings, kept as it is.
fn sign_in(paths: &Paths, which: &str, login: &str) -> Result<()> {
    let mut config = paths.load_config().into_diagnostic()?;
    if config.box_name.is_empty() {
        return Err(miette!("choose the box first"));
    }
    let mut secret = String::new();
    io::stdin().read_to_string(&mut secret).into_diagnostic()?;
    let secret = secret.trim_end_matches(['\n', '\r']);
    let (agent, route) = agent(paths, &config.box_name)?;
    let mut secrets = paths.load_secrets().into_diagnostic()?;
    if which == "cloud" {
        let login = login.trim().to_owned();
        let (app_password, user) =
            cloud::sign_in(&agent, &cloud::base(&route.name), &login, secret).map_err(report)?;
        secrets.cloud = Some(app_password);
        config.cloud = Some(Account { login, user });
    } else {
        let token = secret.trim();
        let user = forge::check_token(&agent, &forge::base(&route.name), token).map_err(report)?;
        secrets.git = Some(token.to_owned());
        config.git = Some(Account {
            login: user.clone(),
            user,
        });
    }
    paths.save_secrets(&secrets).into_diagnostic()?;
    paths.save_config(&config).into_diagnostic()
}

/// Forgets an account. The cloud's app password is revoked on the box
/// when the box can be reached; a Git token stays listed in LosOS Git's
/// settings, where the person removes it, and a new sign-in from this
/// computer replaces it.
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
