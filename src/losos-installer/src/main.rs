//! `losos-installer serve`: the installer ISO's backend for `derisk
//! installer`, which runs it as a child and talks to it on stdin and stdout
//! (`serve.rs`).
//!
//! This used to be a terminal interface on tty1 that joined Wi-Fi, picked
//! the disk and showed the install. derisk now draws all of that as the
//! ISO's session, and joins networks itself the way an installed system
//! does, so only what is particular to LosOS stays here: which disks may be
//! installed onto, and repart and sysupdate run in order.
//!
//! `losos-installer encrypt` is an install's last step on its own: root,
//! /home and swap made on a disk whose /usr is already there, root
//! encrypted with the PIN read from stdin, and the recovery key printed.
//! nixos/tests/installer-boot.nix runs it, as the test has no release to
//! download.
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;

use losos_installer::encrypt::{self, Layout};
use losos_installer::install::Event;
use losos_installer::serve::{self, Config};
use miette::miette;
use tracing_subscriber::EnvFilter;

// The name and every path are required: the one caller, the ISO's
// (nixos/installer/default.nix), always passes them, so a default here could
// only drift from it.
const USAGE: &str = "usage: losos-installer serve --name NAME --repart-definitions DIR --sysupdate-definitions DIR \
--esp DIR --medium DIR --work DIR [--source URL] [--index URL] [--local-source DIR] [--release-repo OWNER/NAME --release-version VERSION [--release-channel CHANNEL]] \
[--bios-boot PROGRAM] [--root-definitions DIR --swap PROGRAM] [--report-client-id ID [--report-repo OWNER/NAME] [--report-login URL] [--report-api URL]]
       losos-installer encrypt --root-definitions DIR --swap PROGRAM --work DIR DISK < PIN";

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
    let mut root_definitions = None;
    let mut swap = None;
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
        encryption: None,
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
            "--root-definitions" => root_definitions = Some(PathBuf::from(value)),
            "--swap" => swap = Some(PathBuf::from(value)),
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
    config.encryption = layout(root_definitions, swap)?;
    Ok(config)
}

/// Both of encryption's paths, or neither.
fn layout(definitions: Option<PathBuf>, swap: Option<PathBuf>) -> miette::Result<Option<Layout>> {
    match (definitions, swap) {
        (Some(definitions), Some(swap)) => Ok(Some(Layout { definitions, swap })),
        (None, None) => Ok(None),
        _ => Err(miette!(
            help = USAGE,
            "--root-definitions and --swap go together"
        )),
    }
}

/// `encrypt`: its flags, then the disk.
fn encrypt_command(mut args: impl Iterator<Item = String>) -> miette::Result<()> {
    let (mut definitions, mut swap, mut work, mut disk) = (None, None, None, None);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| miette!(help = USAGE, "{arg} needs a value"))
        };
        match arg.as_str() {
            "--root-definitions" => definitions = Some(PathBuf::from(value()?)),
            "--swap" => swap = Some(PathBuf::from(value()?)),
            "--work" => work = Some(PathBuf::from(value()?)),
            _ if disk.is_none() && arg.starts_with("/dev/") => disk = Some(arg),
            _ => return Err(miette!(help = USAGE, "unknown argument {arg}")),
        }
    }
    let layout = layout(definitions, swap)?
        .ok_or_else(|| miette!(help = USAGE, "--root-definitions and --swap are required"))?;
    let work = work.ok_or_else(|| miette!(help = USAGE, "--work is required"))?;
    let disk = disk.ok_or_else(|| miette!(help = USAGE, "which disk?"))?;
    let mut pin = String::new();
    std::io::stdin()
        .read_to_string(&mut pin)
        .map_err(|e| miette!("cannot read the PIN: {e}"))?;
    let pin = pin.strip_suffix('\n').unwrap_or(&pin);

    // The steps' output to stderr, as the installer's log would show it.
    let (tx, rx) = mpsc::channel();
    let shown = std::thread::spawn(move || {
        for event in rx {
            if let Event::Line(line) = event {
                eprintln!("{line}");
            }
        }
    });
    let result = encrypt::run(&layout, &disk, Path::new(&work), pin, &tx);
    drop(tx);
    let _ = shown.join();
    let key = result.map_err(|e| miette!("{e}"))?;
    println!("{key}");
    Ok(())
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
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("encrypt") {
        args.next();
        return match encrypt_command(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(report) => {
                eprintln!("{report:?}");
                ExitCode::FAILURE
            }
        };
    }
    match parse(args) {
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
