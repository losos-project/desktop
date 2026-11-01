//! The LosOS boxes machine's keyring password, kept on the host.
//!
//! Nobody signs in to the machine, so no sign-in password is there to
//! unlock its keyring, where `losos` keeps every box's key and token. The
//! host keeps a password for each LosOS boxes machine instead: `relay` makes
//! one, at random, the first time the machine asks for it on vsock port
//! [`KEYRING_PORT`], and hands the same one over every time after. In the
//! machine, `keyring-password` asks for it and prints it, and the
//! machine's keyring service unlocks the keyring with it, or makes the
//! keyring with it at the first boot (nixos/guest/boxes.nix).
//!
//! It is the machine's libvirt UUID that keys a password, so a machine made
//! again under the same name, with a new disk and so no keyring, gets a new
//! one. The machine's disk holds the keyring encrypted with a password that
//! is only on this host, so a copy of the disk alone opens nothing.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use miette::{bail, IntoDiagnostic, Result, WrapErr};
use tracing::info;

use super::{machine_at, virsh};

/// Where a LosOS boxes machine asks the host for its keyring's password.
pub const KEYRING_PORT: u32 = 7353;
/// Where the host keeps them, one file per machine, named by its UUID: the
/// relay's state directory (modules/losos-boxes.nix), which only its own
/// user can read.
const KEYRING_DIR: &str = "/var/lib/losos-boxes/keyrings";
/// 256 bits, which gnome-keyring stretches into the key it encrypts with.
const PASSWORD_BYTES: usize = 32;

/// Hands the LosOS boxes machine on `stream` its keyring's password, made
/// first if it has none yet, and refuses any other machine.
pub fn serve(mut stream: vsock::VsockStream) -> Result<()> {
    let cid = stream.peer_addr().into_diagnostic()?.cid();
    let Some(name) = machine_at(cid)? else {
        info!(cid, "refused: not a LosOS boxes machine");
        return Ok(());
    };
    let uuid = virsh(&["domuuid", &name])?.trim().to_owned();
    // A UUID is hex and dashes, so it is a file name and nothing else.
    if uuid.is_empty() || !uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        bail!("libvirt gave {name} the UUID {uuid:?}");
    }
    let password = password_for(Path::new(KEYRING_DIR), &uuid)?;
    stream
        .write_all(password.as_bytes())
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot send {name} its keyring's password"))?;
    info!(%name, "handed its keyring's password over");
    Ok(())
}

/// The password kept as `dir/uuid`, made at random if there is none. A new
/// one is written beside it and linked into place, which fails if another
/// connection got there first, so two at once both get the one that won and
/// nobody reads a half-written file.
fn password_for(dir: &Path, uuid: &str) -> Result<String> {
    let path = dir.join(uuid);
    match std::fs::read_to_string(&path) {
        Ok(password) if !password.is_empty() => return Ok(password),
        Ok(_) => bail!("{} is empty", path.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e)
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot read {}", path.display()))
        }
    }
    std::fs::create_dir_all(dir)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot create {}", dir.display()))?;

    let mut bytes = [0u8; PASSWORD_BYTES];
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .into_diagnostic()
        .wrap_err("cannot read /dev/urandom")?;
    let password = hex(&bytes);

    let partial = dir.join(format!(".{uuid}.{}.partial", std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| {
            file.write_all(password.as_bytes())?;
            file.sync_all()
        });
    let linked = written.and_then(|()| std::fs::hard_link(&partial, &path));
    let _ = std::fs::remove_file(&partial);
    match linked {
        Ok(()) => Ok(password),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => std::fs::read_to_string(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot read {}", path.display())),
        Err(e) => Err(e)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot write {}", path.display())),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// In the LosOS boxes machine: asks the host for the keyring's password and
/// prints it, with no newline, since gnome-keyring takes every byte it reads
/// as the password. Nothing printed is an error, so the keyring is never
/// made with an empty password.
pub fn print() -> Result<()> {
    let mut stream =
        vsock::VsockStream::connect(&vsock::VsockAddr::new(vsock::VMADDR_CID_HOST, KEYRING_PORT))
            .into_diagnostic()
            .wrap_err("cannot reach the host's relay")?;
    let mut password = String::new();
    stream
        .read_to_string(&mut password)
        .into_diagnostic()
        .wrap_err("cannot read the keyring's password from the host")?;
    if password.is_empty() {
        bail!("the host did not hand this machine a keyring password");
    }
    std::io::stdout()
        .lock()
        .write_all(password.as_bytes())
        .into_diagnostic()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_is_made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("losos-boxes-keyring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = password_for(&dir, "6f1c").unwrap();
        assert_eq!(first.len(), PASSWORD_BYTES * 2);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(password_for(&dir, "6f1c").unwrap(), first);
        assert_ne!(password_for(&dir, "9a2e").unwrap(), first);
        // Nothing is left beside them.
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["6f1c", "9a2e"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
