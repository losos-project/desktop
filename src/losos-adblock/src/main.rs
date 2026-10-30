//! losos-adblock: ad and tracker blocking for every program on the OS, from
//! the same filter lists browsers use (docs/adblock.md).
//!
//! Two blockers share the lists. A DNS forwarder in front of resolved
//! refuses the domains they block, which covers every program, Flatpaks and
//! statically linked ones included, since they all resolve through
//! resolved's stub. A resolver only ever sees a name, though, and most of
//! EasyList is about URLs and page elements, so the same lists are also
//! compiled into WebKit content blockers that Danube loads.
//!
//! Its commands:
//!
//!     losos-adblock update           # fetch the lists and compile both blockers
//!     losos-adblock compile          # compile from the lists already fetched
//!     losos-adblock serve            # the DNS forwarder (socket-activated)
//!     losos-adblock check NAME...    # whether each name is blocked, and by what
//!     losos-adblock encrypted-dns on|strict|off|status
//!                                    # DNS over HTTPS or TLS to the forwarder's upstreams
//!
//! `--config DIR` and `--state DIR` replace /etc/losos-adblock and
//! /var/lib/losos-adblock, and `--listen ADDR` makes `serve` bind its own
//! sockets instead of taking systemd's.

mod dns;
mod encrypted;
mod lists;
mod portal;
mod rules;
mod serve;

use std::io::{IsTerminal, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use miette::IntoDiagnostic;
use tracing_subscriber::EnvFilter;

use lists::Paths;
use rules::{Blocklist, Verdict};
use serve::Sockets;

/// Why a command failed.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error("{0} is neither an http(s) URL nor an absolute path")]
    #[diagnostic(
        code(losos_adblock::source),
        help("each line of /etc/losos-adblock/lists names one filter list")
    )]
    Source(String),
    #[error("cannot fetch {0}: {1}")]
    #[diagnostic(code(losos_adblock::fetch))]
    Fetch(String, String),
    #[error("cannot write {}", .0.display())]
    #[diagnostic(code(losos_adblock::write))]
    Write(PathBuf, #[source] std::io::Error),
    #[error("cannot listen for DNS queries")]
    #[diagnostic(
        code(losos_adblock::listen),
        help("run it from losos-adblock.socket, or pass --listen with a free address")
    )]
    Listen(#[source] std::io::Error),
}

const USAGE: &str = "usage: losos-adblock [--config DIR] [--state DIR] update|compile|serve [--listen ADDR]|check NAME...|encrypted-dns on|strict|off|status";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    match run() {
        Ok(code) => code,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> miette::Result<ExitCode> {
    let mut paths = Paths {
        config: PathBuf::from("/etc/losos-adblock"),
        state: PathBuf::from("/var/lib/losos-adblock"),
    };
    let mut listen: Option<SocketAddr> = None;
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => paths.config = args.next().ok_or_else(|| miette::miette!(USAGE))?.into(),
            "--state" => paths.state = args.next().ok_or_else(|| miette::miette!(USAGE))?.into(),
            "--listen" => {
                listen = Some(
                    args.next()
                        .ok_or_else(|| miette::miette!(USAGE))?
                        .parse()
                        .into_diagnostic()?,
                )
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            _ => words.push(arg),
        }
    }
    match words.first().map(String::as_str) {
        Some("update") => {
            let failed = lists::update(&paths)?;
            // Non-zero when a list could not be fetched, so the unit shows
            // it, though what could be compiled was.
            Ok(if failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Some("compile") => {
            lists::compile(&paths)?;
            Ok(ExitCode::SUCCESS)
        }
        Some("serve") => {
            let sockets = match listen {
                Some(addr) => Sockets::bind(addr).map_err(Error::Listen)?,
                None => Sockets::from_systemd().ok_or_else(|| {
                    Error::Listen(std::io::Error::other("no sockets from systemd"))
                })?,
            };
            serve::run(sockets, &paths);
            Ok(ExitCode::SUCCESS)
        }
        Some("check") if words.len() > 1 => {
            let text = std::fs::read_to_string(paths.dns()).unwrap_or_default();
            let list = Blocklist::read(&text);
            let mut any = false;
            for name in &words[1..] {
                match list.verdict(name) {
                    Verdict::Blocked(rule) => {
                        any = true;
                        println!("{name}: blocked by ||{rule}^");
                    }
                    Verdict::Allowed(rule) => println!("{name}: allowed by @@||{rule}^"),
                    Verdict::Unlisted => println!("{name}: not blocked"),
                }
            }
            Ok(if any {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Some("encrypted-dns") => encrypted_dns(&paths, words.get(1).map(String::as_str)),
        _ => Err(miette::miette!(USAGE)),
    }
}

/// Turns encrypted DNS on or off, or says which it is and through what.
/// The running forwarder sees the switch within a second (serve.rs).
fn encrypted_dns(paths: &Paths, verb: Option<&str>) -> miette::Result<ExitCode> {
    let switch = serve::Switch::new(paths);
    match verb {
        Some("status") => {}
        Some(word) => {
            let mode = serve::Mode::from_word(word).ok_or_else(|| miette::miette!(USAGE))?;
            switch
                .set(mode)
                .map_err(|e| Error::Write(switch.own.clone(), e))?;
        }
        None => return Err(miette::miette!(USAGE)),
    }
    // Written rather than printed: `status | head -n1` closes the pipe
    // early, and println! panics on that.
    let mut out = std::io::stdout().lock();
    let _ = match switch.read() {
        Some(encrypted) => {
            let mut text = format!("{}\n", if encrypted.strict { "strict" } else { "on" });
            for server in encrypted.servers {
                text.push_str(&format!("{server:?}\n"));
            }
            out.write_all(text.as_bytes())
        }
        None if switch.overridden() => {
            out.write_all(b"off\nforced to /run/losos-adblock/upstreams\n")
        }
        None => out.write_all(b"off\n"),
    };
    Ok(ExitCode::SUCCESS)
}
