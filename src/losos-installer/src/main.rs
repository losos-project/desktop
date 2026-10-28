//! `losos-installer serve`: the installer ISO's backend for `derisk
//! installer`, which runs it as a child and talks to it on stdin and stdout
//! (`serve.rs`).
//!
//! This used to be a terminal interface on tty1 that joined Wi-Fi, picked
//! the disk and showed the install. derisk now draws all of that as the
//! ISO's session, and joins networks itself the way an installed system
//! does, so only what is particular to LosOS stays here: which disks may be
//! installed onto, and repart and sysupdate run in order.
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use losos_installer::serve::{self, Config};
use miette::miette;
use tracing_subscriber::EnvFilter;

// The name and every path are required: the one caller, the ISO's
// (nixos/installer/default.nix), always passes them, so a default here could
// only drift from it.
const USAGE: &str = "usage: losos-installer serve --name NAME --repart-definitions DIR --sysupdate-definitions DIR \
--esp DIR --medium DIR --work DIR [--source URL] [--index URL] [--local-source DIR] [--release-repo OWNER/NAME --release-version VERSION [--release-channel CHANNEL]] \
[--bios-boot PROGRAM] [--report-client-id ID [--report-repo OWNER/NAME] [--report-login URL] [--report-api URL]]";

fn parse(mut args: impl Iterator<Item = String>) -> miette::Result<Config> {
    if args.next().as_deref() != Some("serve") {
        return Err(miette!(help = USAGE, "the only command is `serve`"));
    }
    let mut repart = None;
    let mut sysupdate = None;
    let mut esp = None;
    let mut medium = None;
    let mut work = None;
    let mut name = None;
    let mut config = Config {
        name: String::new(),
        repart_definitions: PathBuf::new(),
        sysupdate_templates: PathBuf::new(),
        esp: PathBuf::new(),
        medium: String::new(),
        work: PathBuf::new(),
        source: None,
        index: None,
        local_source: None,
        bios_boot: None,
        release_repo: None,
        release_version: None,
        release_channel: None,
        report_repo: None,
        report_client_id: None,
        report_login: None,
        report_api: None,
    };
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| miette!(help = USAGE, "{flag} needs a value"))?;
        match flag.as_str() {
            "--repart-definitions" => repart = Some(value),
            "--sysupdate-definitions" => sysupdate = Some(value),
            "--esp" => esp = Some(value),
            "--medium" => medium = Some(value),
            "--work" => work = Some(value),
            "--source" => config.source = Some(value),
            "--index" => config.index = Some(value),
            "--local-source" => config.local_source = Some(PathBuf::from(value)),
            "--bios-boot" => config.bios_boot = Some(PathBuf::from(value)),
            "--release-repo" => config.release_repo = Some(value),
            "--release-version" => config.release_version = Some(value),
            "--release-channel" => config.release_channel = Some(value),
            "--report-repo" => config.report_repo = Some(value),
            "--report-client-id" => config.report_client_id = Some(value),
            // GitHub's own unless a test stands something in for it.
            "--report-login" => config.report_login = Some(value),
            "--report-api" => config.report_api = Some(value),
            "--name" => name = Some(value),
            _ => return Err(miette!(help = USAGE, "unknown argument {flag}")),
        }
    }
    let required = |value: Option<String>, flag: &str| {
        value.ok_or_else(|| miette!(help = USAGE, "{flag} is required"))
    };
    config.name = required(name, "--name")?;
    config.repart_definitions = required(repart, "--repart-definitions")?.into();
    config.sysupdate_templates = required(sysupdate, "--sysupdate-definitions")?.into();
    config.esp = required(esp, "--esp")?.into();
    config.medium = required(medium, "--medium")?;
    config.work = required(work, "--work")?.into();
    Ok(config)
}

fn main() -> ExitCode {
    // stderr, never stdout: stdout is the protocol `derisk installer` reads.
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
            ExitCode::SUCCESS
        }
        // Printed here rather than returned from `main`: a usage error exits
        // 2, as it always has, and `main() -> miette::Result` can only exit 1.
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::from(2)
        }
    }
}
