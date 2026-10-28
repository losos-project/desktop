//! `losos-portal`: the portal machine's pieces (docs/portal-machine.md).
//!
//!     losos-portal forward [--machine]   the portal backend, on the host or in a machine
//!     losos-portal relay [--machine NAME] [--idle SECONDS] [--boot SECONDS]
//!                                        the host's relay, as root
//!     losos-portal serve                 in the portal machine
//!     losos-portal agent                 in the portal machine, for derisk's portal
//!     losos-portal read|write PATH       the relay's helpers, as the person
//!     losos-portal screenshot

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use losos_portal::{agent, files, forward, relay, serve};
use miette::{miette, IntoDiagnostic, Result};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let helper = matches!(args.first().map(String::as_str), Some("read" | "write" | "screenshot"));
    if !helper {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_writer(std::io::stderr)
            .with_ansi(std::io::stderr().is_terminal())
            .init();
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["forward"] => forward::run(forward::Place::Host),
        ["forward", "--machine"] => forward::run(forward::Place::Machine),
        ["relay", rest @ ..] => relay::run(relay_config(rest)?),
        ["serve"] => serve::run(),
        ["agent"] => agent::run(),
        ["read", path] => files::read(Path::new(path)).into_diagnostic(),
        ["write", path] => files::write(Path::new(path)).into_diagnostic(),
        ["screenshot"] => files::screenshot().into_diagnostic(),
        _ => Err(miette!(
            "usage: losos-portal forward [--machine] | relay [--machine NAME] [--idle SECONDS] [--boot SECONDS] | serve | agent"
        )),
    }
}

fn relay_config(mut rest: &[&str]) -> Result<relay::Config> {
    let mut config = relay::Config {
        machine: "sys-portal".to_owned(),
        idle: Duration::from_secs(600),
        boot: Duration::from_secs(120),
    };
    let seconds = |value: &str| -> Result<Duration> {
        value
            .parse()
            .map(Duration::from_secs)
            .map_err(|_| miette!("not a number of seconds: {value}"))
    };
    while let [flag, value, more @ ..] = rest {
        match *flag {
            "--machine" => config.machine = (*value).to_owned(),
            "--idle" => config.idle = seconds(value)?,
            "--boot" => config.boot = seconds(value)?,
            _ => return Err(miette!("unknown option {flag}")),
        }
        rest = more;
    }
    if !rest.is_empty() {
        return Err(miette!("{} takes a value", rest[0]));
    }
    Ok(config)
}
