//! Moves USB devices into the machines that drive them.
//!
//!     losos-peripherals keep [--out /run/losos-passthrough/keep]
//!     losos-peripherals serve --config <file>
//!     losos-peripherals usb-machine
//!     losos-peripherals receiver
//!     losos-peripherals bluetooth-machine --bus-config <file>
//!     losos-peripherals bluetoothctl [<argument>...]
//!
//! `keep` runs at every boot before any controller is passed through and
//! writes the PCI functions the host must keep. `serve` is the host's
//! router. `usb-machine`, `receiver` and `bluetooth-machine` are the
//! machines' halves, each a machine's PID 1 (lib.rs). `bluetoothctl` is
//! the host's bluetoothctl, which asks the router to run it in the
//! Bluetooth machine.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use miette::{bail, IntoDiagnostic, WrapErr};
use tracing_subscriber::EnvFilter;

use losos_peripherals::{guest, host, keep};

fn main() -> miette::Result<()> {
    // stderr: the journal for the router, the serial console in a machine.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
    };

    match args.first().map(String::as_str) {
        Some("keep") => {
            let out = flag("--out").unwrap_or_else(|| "/run/losos-passthrough/keep".into());
            let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").into_diagnostic()?;
            let kept = keep::kept(Path::new("/sys"), &mountinfo);
            for function in &kept {
                tracing::info!(function, "kept on the host");
            }
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir).into_diagnostic()?;
            }
            let text: String = kept.iter().map(|f| format!("{f}\n")).collect();
            std::fs::write(&out, text)
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot write {}", out.display()))
        }
        Some("serve") => {
            let Some(config) = flag("--config") else {
                bail!("usage: losos-peripherals serve --config <file>");
            };
            let config: host::Config = std::fs::read(&config)
                .into_diagnostic()
                .and_then(|b| serde_json::from_slice(&b).into_diagnostic())
                .wrap_err_with(|| format!("cannot read {}", config.display()))?;
            host::serve(config).into_diagnostic()
        }
        Some("usb-machine") => guest::usb_machine().into_diagnostic(),
        Some("receiver") => guest::receiver().into_diagnostic(),
        Some("bluetooth-machine") => {
            let Some(bus) = flag("--bus-config") else {
                bail!("usage: losos-peripherals bluetooth-machine --bus-config <file>");
            };
            guest::bluetooth_machine(&bus).into_diagnostic()
        }
        Some("bluetoothctl") => {
            let control = std::env::var_os("LOSOS_PERIPHERALS_CONTROL")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/run/losos-peripherals/control".into());
            let status = host::bluetoothctl_client(&control, args[1..].to_vec())
                .into_diagnostic()
                .wrap_err("cannot reach the Bluetooth machine")?;
            std::process::exit(status)
        }
        _ => bail!("usage: losos-peripherals keep|serve|usb-machine|receiver|bluetooth-machine|bluetoothctl"),
    }
}
