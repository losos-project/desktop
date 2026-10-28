//! `losos-report serve`: files a problem on the desktop as a GitHub issue,
//! for derisk, which runs it as a child and talks to it on stdin and stdout
//! (`serve.rs`).
//!
//! Where reports go comes from `/etc/losos/report.json`, which the image
//! writes when it was built with a GitHub App to sign in with
//! (`nixos/modules/report.nix`): `{"repository": "owner/name", "client_id":
//! "Iv…"}`. Without it, `hello` says reports can't be sent and derisk offers
//! none. The flags override it, and point the sign-in and the API at a
//! stand-in in tests.
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use losos_report::Target;
use losos_report::serve::{self, Config};
use miette::{IntoDiagnostic, WrapErr, miette};
use serde_json::Value;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: losos-report serve [--config FILE] [--repo OWNER/NAME] [--client-id ID] \
[--login URL] [--api URL]";

/// The image's own, written by nixos/modules/report.nix.
const CONFIG: &str = "/etc/losos/report.json";

fn parse(mut args: impl Iterator<Item = String>) -> miette::Result<Config> {
    if args.next().as_deref() != Some("serve") {
        return Err(miette!(help = USAGE, "the only command is `serve`"));
    }
    let mut file = PathBuf::from(CONFIG);
    let (mut repo, mut client_id, mut login, mut api) = (None, None, None, None);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| miette!(help = USAGE, "{flag} needs a value"))?;
        match flag.as_str() {
            "--config" => file = PathBuf::from(value),
            "--repo" => repo = Some(value),
            "--client-id" => client_id = Some(value),
            "--login" => login = Some(value),
            "--api" => api = Some(value),
            _ => return Err(miette!(help = USAGE, "unknown flag {flag}")),
        }
    }
    // A missing file is an image built without an App: no reports, not an
    // error. One that is there and unreadable is.
    let saved: Value = match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text)
            .into_diagnostic()
            .wrap_err_with(|| format!("{} is not JSON", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(e) => return Err(miette!("{}: {e}", file.display())),
    };
    let saved = |key: &str| saved[key].as_str().map(str::to_owned);
    let target = match (
        repo.or_else(|| saved("repository")),
        client_id.or_else(|| saved("client_id")),
    ) {
        (Some(repo), Some(client_id)) => Some(Target {
            repo,
            client_id,
            login: login.unwrap_or_else(|| "https://github.com".to_owned()),
            api: api.unwrap_or_else(|| "https://api.github.com".to_owned()),
        }),
        _ => None,
    };
    // Private to this user: curl reads the request bodies from here, and
    // they are the person's to read before anyone else.
    let scratch = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("losos-report-{}", std::process::id()));
    Ok(Config { target, scratch })
}

fn main() -> ExitCode {
    // stderr, never stdout: stdout is the protocol derisk reads.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    match parse(std::env::args().skip(1)) {
        Ok(config) => {
            serve::serve_stdio(&config);
            let _ = std::fs::remove_dir_all(&config.scratch);
            ExitCode::SUCCESS
        }
        // A usage error exits 2, as losos-installer's does.
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::from(2)
        }
    }
}
