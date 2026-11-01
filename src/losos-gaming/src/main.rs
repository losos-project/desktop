//! The gaming machine: a Windows machine with a graphics card of its own.
//!
//!     losos-gaming gpus [--json]
//!     losos-gaming reserve <address>|none        (root)
//!     losos-gaming bind                          (root, at boot)
//!     losos-gaming create --cd <windows.iso> [--cd <more.iso>] [--disk-gib <n>] [--screen looking-glass|monitor]
//!     losos-gaming start | ready | screen <looking-glass|monitor> | show | status
//!     losos-gaming xml [looking-glass|monitor] [--installing]
//!
//! `reserve` records which card the machine gets from the next boot on,
//! and `bind` moves it to vfio-pci at every boot before any host driver
//! sees it (gaming.nix runs both as units). The rest is the person's:
//! `create` defines the machine with Windows' CD in it, `start` defines it
//! again with the keyboards, pointers and game pads plugged in now and
//! starts it, `ready` takes the CD and the installer's emulated screen out
//! once Windows has the card's driver, and `show` opens Looking Glass's
//! window on it.

use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;

use miette::{bail, miette, IntoDiagnostic, WrapErr};
use tracing_subscriber::EnvFilter;

use losos_gaming::domain::{self, Screen, Spec};
use losos_gaming::{gpu, host};

/// Which card goes, kept across boots.
const RESERVED: &str = "/var/lib/losos-gaming/gpu";
/// What `bind` moved this boot, one PCI function a line.
const BOUND: &str = "/run/losos-gaming/functions";
/// The functions the peripherals router keeps for the host.
const KEEP: &str = "/run/losos-passthrough/keep";

fn main() -> miette::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let sys = Path::new("/sys");
    let keep: Vec<String> = std::fs::read_to_string(KEEP)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();

    match args.first().map(String::as_str) {
        Some("gpus") => {
            let gpus = gpu::gpus(sys, &keep);
            if args.iter().any(|a| a == "--json") {
                let reserved = reserved();
                let chosen = gpu::choose(&gpus).map(|g| g.function.address.clone());
                let json = serde_json::json!({
                    "reserved": reserved,
                    "suggested": chosen,
                    "gpus": gpus.iter().map(|g| serde_json::json!({
                        "address": g.function.address,
                        "vendor": format!("{:04x}", g.function.vendor),
                        "device": format!("{:04x}", g.function.device),
                        "driver": g.function.driver,
                        "boot_vga": g.boot_vga,
                        "takes": g.takes.as_ref().ok().map(|t| t.iter().map(|f| &f.address).collect::<Vec<_>>()),
                        "refused": g.takes.as_ref().err(),
                    })).collect::<Vec<_>>(),
                });
                println!("{json:#}");
            } else {
                for g in &gpus {
                    let f = &g.function;
                    let note = match (&g.takes, g.boot_vga) {
                        (Err(why), _) => format!("stays: {why}"),
                        (Ok(_), true) => "the firmware's screen is on it".to_owned(),
                        (Ok(t), false) => format!("can go, with {} function(s)", t.len()),
                    };
                    println!(
                        "{} {:04x}:{:04x} {} — {note}",
                        f.address,
                        f.vendor,
                        f.device,
                        f.driver.as_deref().unwrap_or("no driver")
                    );
                }
            }
            Ok(())
        }
        Some("reserve") => {
            let Some(address) = args.get(1) else {
                bail!("usage: losos-gaming reserve <address>|none");
            };
            if address == "none" {
                return match std::fs::remove_file(RESERVED) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).into_diagnostic(),
                    _ => Ok(()),
                };
            }
            let gpus = gpu::gpus(sys, &keep);
            let Some(card) = gpus.iter().find(|g| &g.function.address == address) else {
                bail!("{address} is not a graphics card on this PC");
            };
            if let Err(why) = &card.takes {
                bail!("{address} cannot go to a machine: {why}");
            }
            if gpus.len() < 2 {
                bail!("this PC has one graphics card, which the desktop needs");
            }
            if let Some(dir) = Path::new(RESERVED).parent() {
                std::fs::create_dir_all(dir).into_diagnostic()?;
            }
            std::fs::write(RESERVED, format!("{address}\n")).into_diagnostic()?;
            tracing::info!(address, "goes to the gaming machine from the next boot");
            Ok(())
        }
        Some("bind") => {
            let Some(address) = reserved() else {
                return Ok(());
            };
            // Never fails the boot: the card stays on the host and the
            // machine cannot start, which `status` says.
            let takes = match gpu::takes(sys, &address, &keep) {
                Ok(takes) => takes,
                Err(why) => {
                    tracing::warn!(address, why, "the card stays on the host");
                    return Ok(());
                }
            };
            gpu::bind(sys, &takes)
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot move {address} to vfio-pci"))?;
            if let Some(dir) = Path::new(BOUND).parent() {
                std::fs::create_dir_all(dir).into_diagnostic()?;
            }
            let text: String = takes.iter().map(|f| format!("{}\n", f.address)).collect();
            std::fs::write(BOUND, text).into_diagnostic()
        }
        Some("create") => {
            let mut cds = Vec::new();
            let mut disk_gib = 256u64;
            let mut screen = Screen::LookingGlass;
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                let mut value = || rest.next().ok_or_else(|| miette!("{arg} needs a value"));
                match arg.as_str() {
                    "--cd" => cds.push(value()?.clone()),
                    "--disk-gib" => disk_gib = value()?.parse().into_diagnostic()?,
                    "--screen" => {
                        let v = value()?;
                        screen = Screen::parse(v).ok_or_else(|| miette!("no screen called {v}"))?;
                    }
                    other => return Err(miette!("unknown argument {other}")),
                }
            }
            if cds.is_empty() {
                bail!("usage: losos-gaming create --cd <windows.iso> [--cd <more.iso>] [--disk-gib <n>] [--screen looking-glass|monitor]");
            }
            if virsh(&["dominfo", domain::NAME]).is_ok() {
                bail!("there is a gaming machine already; `virsh undefine --nvram gaming` removes it");
            }
            // Uploaded into libvirt's pool, as pm does with a machine's CDs:
            // QEMU runs as its own user, which cannot read a person's home.
            let mut media = Vec::new();
            for (i, cd) in cds.iter().enumerate() {
                let volume = format!("losos-gaming-cd{i}.iso");
                let size = std::fs::metadata(cd)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("cannot read {cd}"))?
                    .len();
                let _ = virsh(&["vol-delete", "--pool", "default", &volume]);
                virsh(&["vol-create-as", "default", &volume, &size.to_string(), "--format", "raw"])?;
                tracing::info!(cd, volume, "copying the CD into libvirt's pool");
                virsh(&["vol-upload", "--pool", "default", &volume, cd])?;
                media.push(volume);
            }
            if virsh(&["vol-info", "--pool", "default", domain::DISK]).is_err() {
                virsh(&["vol-create-as", "default", domain::DISK, &format!("{disk_gib}G"), "--format", "qcow2"])?;
            }
            define(&spec(sys, screen, true, media)?)?;
            println!("Defined the gaming machine. `losos-gaming start` starts Windows' installer.");
            Ok(())
        }
        Some(command @ ("start" | "ready" | "screen")) => {
            let metadata = virsh(&["metadata", domain::NAME, domain::NAMESPACE])
                .wrap_err("there is no gaming machine; `losos-gaming create` makes one")?;
            let (recorded_screen, recorded_installing) = domain::recorded(&metadata);
            let mut screen = recorded_screen.unwrap_or(Screen::LookingGlass);
            let mut installing = recorded_installing.unwrap_or(false);
            match command {
                "ready" => installing = false,
                "screen" => {
                    let v = args.get(1).ok_or_else(|| miette!("usage: losos-gaming screen looking-glass|monitor"))?;
                    screen = Screen::parse(v).ok_or_else(|| miette!("no screen called {v}"))?;
                }
                _ => {}
            }
            let media = if installing { installed_media() } else { Vec::new() };
            define(&spec(sys, screen, installing, media)?)?;
            if command == "ready" {
                for volume in installed_media() {
                    let _ = virsh(&["vol-delete", "--pool", "default", &volume]);
                }
            }
            if command == "start" {
                virsh(&["start", domain::NAME])?;
                if screen == Screen::Monitor {
                    println!("Started. Both Ctrl keys at once move the keyboard and pointer between the machine and the desktop.");
                }
            }
            Ok(())
        }
        // What `start` would define, for a look before it does, and for
        // the VM test, whose host has no KVM to define it on.
        Some("xml") => {
            let screen = match args.get(1) {
                Some(v) => Screen::parse(v).ok_or_else(|| miette!("no screen called {v}"))?,
                None => Screen::LookingGlass,
            };
            let installing = args.iter().any(|a| a == "--installing");
            let media = if installing {
                vec!["losos-gaming-cd0.iso".to_owned()]
            } else {
                Vec::new()
            };
            print!("{}", domain::xml(&spec(sys, screen, installing, media)?));
            Ok(())
        }
        Some("show") => {
            let display = virsh(&["domdisplay", domain::NAME]).wrap_err("the gaming machine is not running")?;
            // spice://127.0.0.1:5901
            let port = display
                .trim()
                .rsplit(':')
                .next()
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(|| miette!("the gaming machine shows no Looking Glass screen ({display})"))?;
            let client = |program: &str, prefix: &[&str]| {
                Command::new(program)
                    .args(prefix)
                    .args(["-f", domain::SHARED, "-c", "127.0.0.1", "-p", &port.to_string()])
                    .args(["win:title=Gaming", "win:autoResize=yes", "input:escapeKey=KEY_RIGHTCTRL"])
                    .status()
            };
            // Under GameMode, so focus mode (docs/focus-mode.md) holds the
            // host's background work while the window is open: the game is
            // in Windows, but its frames are copied here. Without GameMode
            // the window opens all the same.
            let status = match client("gamemoderun", &["looking-glass-client"]) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    client("looking-glass-client", &[])
                }
                other => other,
            }
            .into_diagnostic()?;
            if !status.success() {
                bail!("looking-glass-client exited with {status}");
            }
            Ok(())
        }
        Some("status") => {
            match reserved() {
                Some(address) => println!("Reserved for the gaming machine: {address}"),
                None => println!("No card is reserved for the gaming machine."),
            }
            match std::fs::read_to_string(BOUND) {
                Ok(bound) => println!("On vfio-pci this boot: {}", bound.split_whitespace().collect::<Vec<_>>().join(", ")),
                Err(_) => println!("Nothing was moved to vfio-pci this boot."),
            }
            match virsh(&["domstate", domain::NAME]) {
                Ok(state) => println!("The gaming machine is {}.", state.trim()),
                Err(_) => println!("There is no gaming machine."),
            }
            Ok(())
        }
        _ => Err(miette!(
            "usage: losos-gaming gpus|reserve|bind|create|start|ready|screen|show|status|xml (see docs/gaming-machine.md)"
        )),
    }
}

/// The card reserved for the machine, if one is.
fn reserved() -> Option<String> {
    let text = std::fs::read_to_string(RESERVED).ok()?;
    Some(text.trim().to_owned()).filter(|s| !s.is_empty())
}

/// The CD volumes `create` uploaded.
fn installed_media() -> Vec<String> {
    let list = virsh(&["vol-list", "--pool", "default", "--name"]).unwrap_or_default();
    let mut media: Vec<String> = list
        .lines()
        .map(str::trim)
        .filter(|v| v.starts_with("losos-gaming-cd") && v.ends_with(".iso"))
        .map(str::to_owned)
        .collect();
    media.sort();
    media
}

/// The machine as this host and what is plugged in now make it.
fn spec(sys: &Path, screen: Screen, installing: bool, media: Vec<String>) -> miette::Result<Spec> {
    let functions: Vec<String> = std::fs::read_to_string(BOUND)
        .map_err(|_| {
            miette!(
                "no graphics card was moved to vfio-pci this boot: reserve one in Settings, \
                 or with `pm system start losos-gaming-reserve@<address>`, and restart \
                 (`losos-gaming gpus` lists them)"
            )
        })?
        .lines()
        .map(str::to_owned)
        .collect();
    let meminfo = std::fs::read_to_string("/proc/meminfo").into_diagnostic()?;
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").into_diagnostic()?;
    let (cores, threads, amd) = host::processor(sys, &cpuinfo);
    Ok(Spec {
        memory_mib: host::memory_mib(&meminfo),
        cores,
        threads,
        amd,
        functions,
        pads: host::pads(sys),
        inputs: match screen {
            Screen::Monitor => host::inputs(Path::new("/dev/input/by-id")),
            Screen::LookingGlass => Vec::new(),
        },
        screen,
        installing,
        media,
    })
}

/// Defines the machine from `spec`, replacing its definition; a running
/// machine takes it at its next start.
fn define(spec: &Spec) -> miette::Result<()> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| std::env::temp_dir().into());
    let path = Path::new(&dir).join("losos-gaming.xml");
    std::fs::write(&path, domain::xml(spec)).into_diagnostic()?;
    let defined = virsh(&["define", "--validate", &path.to_string_lossy()]);
    let _ = std::fs::remove_file(&path);
    defined.map(drop)
}

/// Runs `virsh` on the system's machines and returns what it printed.
fn virsh(args: &[&str]) -> miette::Result<String> {
    let out = Command::new("virsh")
        .args(["-q", "-c", "qemu:///system"])
        .args(args)
        .output()
        .into_diagnostic()
        .wrap_err("cannot run virsh")?;
    if !out.status.success() {
        bail!(
            "virsh {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
