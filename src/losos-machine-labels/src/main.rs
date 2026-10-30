//! losos-machine-labels: machines on libvirt's `default` network kept apart
//! by their colour label (docs/hypervisor.md, "Labels on the network").
//!
//! A red machine, the untrusted one, gets the host's DHCP and DNS and the
//! internet: not the host, the LAN, a LosOS box or any other machine. Any
//! other machine reaches the machines with its own label and the LosOS
//! boxes machine, and not the rest. libvirt runs this as a hook whenever a
//! machine starts or stops, with the machine's definition, its label in
//! pm's metadata, on stdin.
//!
//!     losos-machine-labels load      # load the table, its sets empty
//!     losos-machine-labels sync      # load it, then add every running machine
//!     losos-machine-labels hook DOMAIN OPERATION ...   # libvirt's qemu hook

mod domain;
mod nft;

use std::io::{IsTerminal, Read};
use std::process::{Command, ExitCode};

use miette::IntoDiagnostic;
use tracing_subscriber::EnvFilter;

const USAGE: &str =
    "usage: losos-machine-labels load|sync|hook DOMAIN OPERATION SUB-OPERATION EXTRA";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> miette::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("load") => load(),
        Some("sync") => {
            load()?;
            sync()
        }
        Some("hook") if args.len() >= 3 => {
            hook(&args[1], &args[2]);
            Ok(())
        }
        _ => Err(miette::miette!(USAGE)),
    }
}

/// Replaces the table with an empty one, so every card is red until its
/// machine is added.
fn load() -> miette::Result<()> {
    nft::apply(&(nft::table() + &nft::current_gateways())).into_diagnostic()
}

/// Adds every running machine, as their hooks would have.
fn sync() -> miette::Result<()> {
    let list = Command::new("virsh")
        .args(["-r", "list", "--name"])
        .output()
        .into_diagnostic()?;
    if !list.status.success() {
        return Err(miette::miette!("cannot list libvirt's machines"));
    }
    let mut script = String::new();
    for name in String::from_utf8_lossy(&list.stdout)
        .lines()
        .filter(|l| !l.is_empty())
    {
        let xml = Command::new("virsh")
            .args(["-r", "dumpxml", name])
            .output()
            .into_diagnostic()?;
        match domain::read(&String::from_utf8_lossy(&xml.stdout)) {
            Ok(machine) => script.push_str(&nft::started(&machine)),
            // Left red, which is what a machine nothing can be read of is.
            Err(e) => tracing::warn!(machine = name, "{e}"),
        }
    }
    nft::apply(&script).into_diagnostic()
}

/// libvirt's qemu hook. It runs inside libvirtd, which waits for it, so it
/// never asks libvirt anything, and it never fails the operation: a card
/// it could not add stays red, which is the safe side.
fn hook(name: &str, operation: &str) {
    let started = match operation {
        "started" | "reconnect" | "attach" => true,
        "stopped" | "release" => false,
        _ => return,
    };
    let mut xml = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut xml) {
        tracing::warn!(machine = name, "cannot read the machine's definition: {e}");
        return;
    }
    let machine = match domain::read(&xml) {
        Ok(machine) => machine,
        Err(e) => {
            tracing::warn!(machine = name, "{e}");
            return;
        }
    };
    let script = if started {
        // Someone removed the table, or nftables was flushed: a machine
        // starting puts it back rather than run unfiltered.
        let mut script = if nft::loaded() {
            String::new()
        } else {
            nft::table()
        };
        script.push_str(&nft::current_gateways());
        script.push_str(&nft::started(&machine));
        script
    } else {
        if !nft::loaded() {
            return;
        }
        let mut script = nft::stopped(&machine);
        script.push_str(&nft::forget_gone());
        script
    };
    match nft::apply(&script) {
        Ok(()) => tracing::info!(
            machine = name,
            group = machine.group.set(),
            ports = ?machine.ports,
            "{}",
            if started { "added" } else { "removed" }
        ),
        Err(e) => tracing::warn!(machine = name, "cannot change the table: {e}"),
    }
}
