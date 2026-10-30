//! losos-login-bridge: company accounts from LosOS Login (docs/company.md).
//!
//!     losos-login-bridge status                   # JSON: the joined box, or null
//!     losos-login-bridge fingerprint HOST         # the box's certificate, to compare
//!     losos-login-bridge join HOST FINGERPRINT ADMIN [--public-name NAME] [--admin-group GROUP]...
//!                                                 # ADMIN's password on stdin, then a code if asked
//!     losos-login-bridge leave
//!     losos-login-bridge serve                    # the join service, on its socket
//!     losos-login-bridge sync                     # lock people the box removed (a timer runs it)
//!     losos-login-bridge manage                   # run the company's Mesh CI jobs (a service runs it)
//!
//! `join` and `leave` go through the join service, which asks polkit for
//! an administrator of this computer; the login itself is the PAM module.

use std::{
    io::{self, BufRead, IsTerminal, Write},
    os::{fd::FromRawFd, unix::net::UnixStream},
    path::Path,
    process::ExitCode,
};

use losos_login_bridge::{
    join::Joined,
    service::{self, JoinRequest, Request},
    tls, Error, JOINED_FILE,
};
use miette::{miette, IntoDiagnostic, Result};
use serde_json::json;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: losos-login-bridge status | fingerprint HOST | join HOST FINGERPRINT ADMIN [--public-name NAME] [--admin-group GROUP]... | leave | serve | sync | manage";

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

fn report(e: Error) -> miette::Report {
    miette!("{e}")
}

/// One line of stdin, asked for with `prompt` on a terminal.
fn read_line(prompt: &str) -> Result<Option<String>> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        eprint!("{prompt}");
        io::stderr().flush().into_diagnostic()?;
    }
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).into_diagnostic()? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_owned()))
}

fn main_inner() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["status"] => {
            let joined = Joined::load(Path::new(JOINED_FILE)).map_err(report)?;
            println!("{}", json!({ "joined": joined }));
        }
        ["fingerprint", host] => {
            let pem = tls::fetch_certificate(host).map_err(report)?;
            let der = tls::pem_to_der(&pem).ok_or_else(|| miette!("no certificate"))?;
            println!("{}", tls::fingerprint(&der));
        }
        ["join", host, fingerprint, user, rest @ ..] => {
            let mut request = JoinRequest {
                host: (*host).to_owned(),
                fingerprint: (*fingerprint).to_owned(),
                user: (*user).to_owned(),
                password: String::new(),
                code: None,
                public_name: None,
                admin_groups: Vec::new(),
            };
            let mut rest = rest.iter();
            while let Some(flag) = rest.next() {
                let value = rest.next().ok_or_else(|| miette!("{USAGE}"))?;
                match *flag {
                    "--public-name" => request.public_name = Some((*value).to_owned()),
                    "--admin-group" => request.admin_groups.push((*value).to_owned()),
                    _ => return Err(miette!("{USAGE}")),
                }
            }
            request.password = read_line(&format!("{user}'s LosOS Login password: "))?
                .ok_or_else(|| miette!("no password on stdin"))?;
            let mut answer = service::call(&Request::Join(request.clone())).map_err(report)?;
            if answer["needCode"] == true {
                if let Some(code) = read_line(losos_login_bridge::login::CODE_PROMPT)? {
                    request.code = Some(code);
                    answer = service::call(&Request::Join(request)).map_err(report)?;
                }
            }
            println!("{answer}");
            if answer.get("joined").is_none() {
                return Ok(ExitCode::FAILURE);
            }
        }
        ["leave"] => {
            let answer = service::call(&Request::Leave {}).map_err(report)?;
            println!("{answer}");
            if answer.get("left").is_none() {
                return Ok(ExitCode::FAILURE);
            }
        }
        ["serve"] => {
            // SAFETY: systemd hands the accepted connection over as stdin
            // (Accept=yes, StandardInput=socket), and nothing else here
            // uses fd 0.
            let stream = unsafe { UnixStream::from_raw_fd(0) };
            service::serve(stream).map_err(report)?;
        }
        ["sync"] => {
            for change in losos_login_bridge::sync::run().map_err(report)? {
                println!("{change:?}");
            }
        }
        ["manage"] => losos_login_bridge::manage::serve().map_err(report)?,
        _ => return Err(miette!("{USAGE}")),
    }
    Ok(ExitCode::SUCCESS)
}
