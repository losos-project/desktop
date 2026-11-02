//! Remembers on the host whether this machine has boxes reachable through
//! their edge, so it can start without waiting for LAN discovery.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};

use super::{machine_at, virsh};

/// Where the machine reports that it has boxes reachable through their edge.
pub const PAIRED_PORT: u32 = 7354;
const PAIRED_DIR: &str = "/var/lib/losos-boxes/paired";

/// Stores whether the connected machine has boxes reachable through their edge.
pub fn serve(mut stream: vsock::VsockStream) -> Result<()> {
    let cid = stream.peer_addr().into_diagnostic()?.cid();
    let Some(name) = machine_at(cid)? else {
        bail!("refused paired-box state from non-LosOS boxes machine at CID {cid}");
    };
    let uuid = machine_uuid(&name)?;
    let mut state = [0];
    stream
        .read_exact(&mut state)
        .into_diagnostic()
        .wrap_err("cannot read paired-box state")?;
    let is_paired = match state[0] {
        0 => false,
        1 => true,
        value => return Err(miette!("invalid paired-box state {value}")),
    };
    set_state(Path::new(PAIRED_DIR), &uuid, is_paired)
}

/// Reports this machine's current away-box state to its host.
pub fn report(is_paired: bool) -> Result<()> {
    let mut stream =
        vsock::VsockStream::connect(&vsock::VsockAddr::new(vsock::VMADDR_CID_HOST, PAIRED_PORT))
            .into_diagnostic()
            .wrap_err("cannot reach the host's relay for paired-box state")?;
    stream
        .write_all(&[u8::from(is_paired)])
        .into_diagnostic()
        .wrap_err("cannot report paired-box state to the host")
}

/// Whether host `machine` was last seen with an away box.
pub fn is_paired(machine: &str) -> Result<bool> {
    let uuid = machine_uuid(machine)?;
    match std::fs::metadata(Path::new(PAIRED_DIR).join(uuid)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e)
            .into_diagnostic()
            .wrap_err("cannot read paired-box state"),
    }
}

fn machine_uuid(machine: &str) -> Result<String> {
    let uuid = virsh(&["domuuid", machine])?;
    let uuid = uuid.trim();
    if uuid.is_empty() || !uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        bail!("libvirt gave {machine} the UUID {uuid:?}");
    }
    Ok(uuid.to_owned())
}

fn set_state(dir: &Path, uuid: &str, is_paired: bool) -> Result<()> {
    let path = dir.join(uuid);
    if is_paired {
        std::fs::create_dir_all(dir)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot create {}", dir.display()))?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file
                .sync_all()
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot sync {}", path.display()))?,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(e)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("cannot create {}", path.display()))
            }
        }
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(e)
                    .into_diagnostic()
                    .wrap_err_with(|| format!("cannot remove {}", path.display()))
            }
        }
    }
    std::fs::File::open(dir)
        .and_then(|directory| directory.sync_all())
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot sync {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_away_box_state_until_it_is_cleared() {
        let dir = std::env::temp_dir().join(format!("losos-boxes-paired-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!state_exists(&dir, "6f1c"));
        set_state(&dir, "6f1c", true).unwrap();
        assert!(state_exists(&dir, "6f1c"));
        set_state(&dir, "6f1c", true).unwrap();
        set_state(&dir, "6f1c", false).unwrap();
        assert!(!state_exists(&dir, "6f1c"));
        set_state(&dir, "6f1c", false).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn state_exists(dir: &Path, uuid: &str) -> bool {
        dir.join(uuid).exists()
    }
}
