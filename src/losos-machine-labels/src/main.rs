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
//!     losos-machine-labels sync      # rebuild it from running machines
//!     losos-machine-labels refresh-arp # refresh registered ports' DHCP addresses
//!     losos-machine-labels hook DOMAIN OPERATION ...   # libvirt's qemu hook

mod domain;
mod nft;

use std::io::{IsTerminal, Read, Write};
use std::process::{Command, ExitCode, Stdio};

use miette::IntoDiagnostic;
use tracing_subscriber::EnvFilter;

const USAGE: &str =
    "usage: losos-machine-labels load|sync|refresh-arp|hook DOMAIN OPERATION SUB-OPERATION EXTRA";
const LOCK_FILE: &str = "/run/losos-machine-labels/lock";
const GENERATION_FILE: &str = "/run/losos-machine-labels/generation";
const LEASES_FILE: &str = "/var/lib/libvirt/dnsmasq/default.leases";

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
        Some("load") => with_lock("locked-load", &[], &[]),
        Some("sync") => sync(),
        Some("refresh-arp") => refresh_arp(),
        Some("hook") if args.len() >= 3 => hook(&args[2]),
        Some("locked-load") => locked_load(),
        Some("locked-sync") if args.len() == 3 => locked_sync(&args[1], &args[2]),
        Some("locked-arp") if args.len() == 1 => locked_arp(),
        Some("locked-hook") if args.len() == 2 => locked_hook(&args[1]),
        _ => Err(miette::miette!(USAGE)),
    }
}

/// Replaces the table with an empty one, so every card is red until its
/// machine is added.
fn locked_load() -> miette::Result<()> {
    advance_generation()?;
    let mut script = nft::table();
    script.push_str(&nft::current_gateways());
    nft::apply(&script).into_diagnostic()
}

/// Rebuilds the table from the running machines, as their hooks would have.
fn sync() -> miette::Result<()> {
    let generation = read_generation()?;
    let leases = read_leases()?;
    let list = Command::new("virsh")
        .args(["-r", "list", "--name"])
        .output()
        .into_diagnostic()?;
    if !list.status.success() {
        return Err(miette::miette!("cannot list libvirt's machines"));
    }
    let mut script = nft::table();
    script.push_str(&nft::current_gateways());
    let mut ports = std::collections::BTreeMap::new();
    for name in String::from_utf8_lossy(&list.stdout)
        .lines()
        .filter(|l| !l.is_empty())
    {
        let xml = Command::new("virsh")
            .args(["-r", "dumpxml", name])
            .output()
            .into_diagnostic()?;
        if !xml.status.success() {
            return Err(miette::miette!("cannot read {name}'s definition"));
        }
        match domain::read(&String::from_utf8_lossy(&xml.stdout)) {
            Ok(machine) => {
                script.push_str(&nft::register(&machine));
                ports.extend(
                    machine
                        .macs
                        .iter()
                        .map(|(mac, port)| (port.clone(), mac.clone())),
                );
            }
            // Left red, which is what a machine nothing can be read of is.
            Err(e) => tracing::warn!(machine = name, "{e}"),
        }
    }
    script.push_str(&nft::arp_owners(
        &ports.into_iter().collect::<Vec<_>>(),
        &nft::leases(&String::from_utf8_lossy(&leases)),
    ));

    let mut input = leases;
    let lease_len = input.len().to_string();
    input.extend_from_slice(script.as_bytes());
    with_lock(
        "locked-sync",
        &[&generation.to_string(), &lease_len],
        &input,
    )
}

/// Refreshes DHCP address bindings without deriving labels from live XML.
fn refresh_arp() -> miette::Result<()> {
    let leases = read_leases()?;
    with_lock("locked-arp", &[], &leases)
}

/// Serializes a lifecycle update without making libvirt wait on a lock while
/// the sync command is reading domain definitions.
fn hook(operation: &str) -> miette::Result<()> {
    if !matches!(
        operation,
        "prepare" | "started" | "reconnect" | "attach" | "stopped" | "release"
    ) {
        return Ok(());
    }
    let mut xml = Vec::new();
    std::io::stdin().read_to_end(&mut xml).into_diagnostic()?;
    with_lock("locked-hook", &[operation], &xml)
}

/// Rejects a snapshot if any lifecycle hook or lease update made it stale.
fn locked_sync(expected_generation: &str, lease_len: &str) -> miette::Result<()> {
    let lease_len = lease_len
        .parse::<usize>()
        .map_err(|_| miette::miette!("invalid lease snapshot length"))?;
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).into_diagnostic()?;
    if input.len() < lease_len {
        return Err(miette::miette!("truncated lease snapshot"));
    }
    let (expected_leases, script) = input.split_at(lease_len);
    let expected_generation = expected_generation.parse::<u64>().into_diagnostic()?;
    if read_generation()? != expected_generation || read_leases()? != expected_leases {
        tracing::info!("discarding stale machine snapshot");
        return Ok(());
    }
    advance_generation()?;
    nft::apply(std::str::from_utf8(script).into_diagnostic()?).into_diagnostic()
}

/// Updates only the IPv4 ownership map for ports already registered by hooks
/// or a full reload.
fn locked_arp() -> miette::Result<()> {
    let mut leases = Vec::new();
    std::io::stdin()
        .read_to_end(&mut leases)
        .into_diagnostic()?;
    if read_leases()? != leases {
        tracing::info!("discarding stale DHCP lease snapshot");
        return Ok(());
    }
    advance_generation()?;
    // Ports gone since the last time drop their bindings here too, so a
    // card named the same as a gone one does not inherit them.
    let mut script = nft::forget_gone();
    script.push_str(&nft::arp_owners(
        &nft::current_port_macs().into_diagnostic()?,
        &nft::leases(&String::from_utf8_lossy(&leases)),
    ));
    nft::apply(&script).into_diagnostic()
}

/// Ensures every hook sees a baseline filter before it lets a machine start.
fn locked_hook(operation: &str) -> miette::Result<()> {
    advance_generation()?;
    let mut xml = String::new();
    std::io::stdin()
        .read_to_string(&mut xml)
        .into_diagnostic()?;
    let baseline_missing = !nft::loaded();
    if baseline_missing {
        let mut baseline = nft::table();
        baseline.push_str(&nft::current_gateways());
        nft::apply(&baseline).into_diagnostic()?;
    }
    let started = matches!(operation, "started" | "reconnect" | "attach");
    if matches!(operation, "prepare") {
        let machine = match domain::read(&xml) {
            Ok(machine) => machine,
            Err(error) => {
                tracing::warn!("{error}");
                return Ok(());
            }
        };
        nft::apply(&nft::unassign(&machine)).into_diagnostic()?;
        let mut script = nft::mac_bindings(&machine);
        script.push_str(&nft::arp_bindings(&machine, &hook_leases()));
        match nft::apply(&script) {
            Ok(()) => {}
            Err(error) => tracing::warn!("cannot register machine ports: {error}"),
        }
        return Ok(());
    }

    let machine = match domain::read(&xml) {
        Ok(machine) => machine,
        Err(error) => {
            tracing::warn!("{error}");
            return Ok(());
        }
    };
    if started {
        nft::apply(&nft::unassign(&machine)).into_diagnostic()?;
        let mut script = nft::current_gateways();
        script.push_str(&nft::started(&machine));
        script.push_str(&nft::arp_bindings(&machine, &hook_leases()));
        match nft::apply(&script) {
            Ok(()) => tracing::info!(
                group = machine.group.set(),
                ports = ?machine.ports,
                "added"
            ),
            Err(error) => tracing::warn!("cannot change the table: {error}"),
        }
    } else {
        let mut script = nft::stopped(&machine);
        script.push_str(&nft::forget_gone());
        match nft::apply(&script) {
            Ok(()) => tracing::info!(
                group = machine.group.set(),
                ports = ?machine.ports,
                "removed"
            ),
            Err(error) => tracing::warn!("cannot change the table: {error}"),
        }
    }
    Ok(())
}

fn with_lock(mode: &str, arguments: &[&str], input: &[u8]) -> miette::Result<()> {
    let executable = std::env::current_exe().into_diagnostic()?;
    let mut child = Command::new("flock")
        .args(["--exclusive", LOCK_FILE])
        .arg(executable)
        .arg(mode)
        .args(arguments)
        .stdin(Stdio::piped())
        .spawn()
        .into_diagnostic()?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input)
        .into_diagnostic()?;
    let status = child.wait().into_diagnostic()?;
    if status.success() {
        Ok(())
    } else {
        Err(miette::miette!("{mode} exited with {status}"))
    }
}

fn read_generation() -> miette::Result<u64> {
    match std::fs::read_to_string(GENERATION_FILE) {
        Ok(generation) => generation.parse().into_diagnostic(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error).into_diagnostic(),
    }
}

fn advance_generation() -> miette::Result<()> {
    let generation = read_generation()?
        .checked_add(1)
        .ok_or_else(|| miette::miette!("machine generation counter overflow"))?;
    let temporary = format!("{GENERATION_FILE}.tmp");
    std::fs::write(&temporary, generation.to_string()).into_diagnostic()?;
    std::fs::rename(temporary, GENERATION_FILE).into_diagnostic()
}

fn read_leases() -> miette::Result<Vec<u8>> {
    match std::fs::read(LEASES_FILE) {
        Ok(leases) => Ok(leases),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).into_diagnostic(),
    }
}

fn hook_leases() -> Vec<(String, std::net::Ipv4Addr)> {
    match read_leases() {
        Ok(leases) => nft::leases(&String::from_utf8_lossy(&leases)),
        Err(error) => {
            tracing::warn!("cannot read DHCP leases: {error}");
            Vec::new()
        }
    }
}
