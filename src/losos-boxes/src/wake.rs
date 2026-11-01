//! Starts the LosOS boxes machine when a box shows up on the host's network.
//!
//! The machine is made at the first boot (modules/losos-boxes.nix) but not
//! started then: most of the time nothing on the network is a box, and a
//! machine that waits for one costs its memory all day. `wake`, a service on
//! the host, browses for boxes through the host's resolved, as the machine
//! itself does through the relay, and starts the machine the first time
//! one answers. Then it ends: if the machine's owner stops it after that,
//! it stays stopped until the next boot.

use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use std::process::Command;
use tracing::info;

use losos_boxes::Change;

use super::run;

/// Browses until a box shows up, then starts `machine`, unless it is gone,
/// which its owner may have done in the Machines app, or already running.
pub fn wake(machine: &str) -> Result<()> {
    match state(machine)? {
        None => {
            info!(%machine, "no such machine; nothing to start");
            return Ok(());
        }
        Some(state) if state != "shut off" => {
            info!(%machine, %state, "already started");
            return Ok(());
        }
        Some(_) => {}
    }
    info!(%machine, "waiting for a LosOS box");
    let mut started = false;
    // run() ends at the first error apply returns, which is how this stops
    // browsing once the machine is up.
    let outcome = run(None, |change| {
        let Change::Write(found) = change else {
            return Ok(());
        };
        info!(host = %found.host, %machine, "a box is here: starting the machine");
        virsh(&["start", machine])?;
        started = true;
        Err(miette!("started"))
    });
    match outcome {
        _ if started => Ok(()),
        Err(e) => Err(e),
        // resolved stopped browsing; the service starts over.
        Ok(_) => bail!("resolved stopped browsing before a box showed up"),
    }
}

/// The machine's state as virsh words it, or None if there is no machine.
fn state(machine: &str) -> Result<Option<String>> {
    let output = Command::new("virsh")
        .args(["-q", "domstate", machine])
        .output()
        .into_diagnostic()
        .wrap_err("cannot run virsh")?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ));
    }
    let error = String::from_utf8_lossy(&output.stderr);
    if error.contains("failed to get domain") {
        return Ok(None);
    }
    bail!("virsh domstate {machine} failed: {}", error.trim())
}

/// virsh on a connection that may change things, which starting needs.
fn virsh(args: &[&str]) -> Result<()> {
    let output = Command::new("virsh")
        .arg("-q")
        .args(args)
        .output()
        .into_diagnostic()
        .wrap_err("cannot run virsh")?;
    if !output.status.success() {
        bail!(
            "virsh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
