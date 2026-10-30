//! Root, encrypted, unlocked by the PC's TPM once the person types a PIN at
//! every start (docs/installer.md, "Asking for a PIN at startup").
//!
//! Without it, the installed system's first boot makes root, /home and swap
//! from its own repart definitions, and root is plain ext4: a person's home
//! is encrypted by systemd-homed, but /var is not, and the machines' disks
//! are in it. With it, the installer makes those three partitions itself,
//! once the download is done and its scratch partition is gone, from the
//! same definitions plus one drop-in that makes root LUKS2 locked to the
//! TPM, and swap from the same losos-swap. First boot then finds every
//! partition it would have made already there and leaves them be, and
//! systemd-gpt-auto-generator unlocks root through the TPM, which asks for
//! the PIN.
//!
//! repart can lock a volume to the TPM but cannot give it a PIN, so its
//! slot is only a step: the installer adds a recovery key, shown to the
//! person once, then a TPM slot with the PIN, and removes repart's. The PIN
//! never reaches a command line or the installer's log; the recovery key
//! never reaches the log.
//!
//! The TPM slot is bound to no PCRs. The PIN is what stands between a
//! stolen PC and root, and the TPM limits how often it can be guessed. PCR
//! binding would also stop a changed boot chain from asking for it, but a
//! PCR the installer's own boot measured is not one the installed system's
//! boot will match, and until the UKIs are signed there is no policy to
//! bind to instead (docs/not-done.md).
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::install::{Event, command, wait_for};

/// The shortest PIN the installer takes. The TPM's lockout makes a short
/// one hard to guess; this keeps it from being a single key.
pub const MIN_PIN: usize = 6;

/// Where the installer's encrypted root comes from: the installed system's
/// own repart definitions, with root's drop-in, and losos-swap.
#[derive(Debug, Clone)]
pub struct Layout {
    pub definitions: PathBuf,
    pub swap: PathBuf,
}

/// Whether this PC can have its root encrypted with a PIN: it has a TPM
/// 2.0 and starts from UEFI, where the installed system's boot measures
/// itself and so unlocks root through the TPM.
pub fn possible(sys: &Path, dev: &Path) -> bool {
    dev.join("tpmrm0").exists() && sys.join("firmware/efi").is_dir()
}

/// Why `pin` cannot be the PIN, if it cannot.
pub fn check_pin(pin: &str) -> Result<(), String> {
    if pin.chars().count() < MIN_PIN {
        return Err(format!("The PIN needs at least {MIN_PIN} characters."));
    }
    if pin.chars().any(char::is_control) {
        return Err("The PIN cannot hold a line break or a tab.".into());
    }
    Ok(())
}

/// Makes root, /home and swap on `disk`, root encrypted with `pin`, and
/// returns the recovery key.
pub fn run(
    layout: &Layout,
    disk: &str,
    work: &Path,
    pin: &str,
    events: &Sender<Event>,
) -> Result<String, String> {
    check_pin(pin)?;
    // losos-swap's definition, for this PC's RAM, as first boot would write
    // it into /run/repart.d.
    let swap_dir = work.join("repart.d");
    let _ = fs::remove_dir_all(&swap_dir);
    fs::create_dir_all(&swap_dir).map_err(|e| format!("{}: {e}", swap_dir.display()))?;
    let swap_dir_text = swap_dir.to_string_lossy().into_owned();
    command(
        &layout.swap.to_string_lossy(),
        &[&swap_dir_text],
        false,
        events,
    )?;

    let definitions = format!("--definitions={}", layout.definitions.display());
    let swap_definitions = format!("--definitions={swap_dir_text}");
    // No --empty: the ESP and the /usr slots stay as they are, and repart
    // adds what is missing in the free space behind them.
    let table = command(
        "systemd-repart",
        &[
            "--dry-run=no",
            &definitions,
            &swap_definitions,
            "--tpm2-device=auto",
            "--json=short",
            disk,
        ],
        true,
        events,
    )?;
    let root = root_node(&table)?;
    command("udevadm", &["settle"], false, events)?;
    wait_for(&root, Duration::from_secs(10))?;
    let root_text = root.to_string_lossy().into_owned();

    let recovery = recovery_key(&root_text, events)?;
    let _ = events.send(Event::Line(format!(
        "$ systemd-cryptenroll --unlock-tpm2-device=auto --tpm2-device=auto --tpm2-with-pin=yes --tpm2-pcrs= --wipe-slot=tpm2 {root_text}"
    )));
    let enrolled = Command::new("systemd-cryptenroll")
        .args([
            "--unlock-tpm2-device=auto",
            "--tpm2-device=auto",
            "--tpm2-with-pin=yes",
            "--tpm2-pcrs=",
            // repart's slot, which needs no PIN: the new one is kept.
            "--wipe-slot=tpm2",
            &root_text,
        ])
        .env("NEWPIN", pin)
        .env("SYSTEMD_COLORS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("systemd-cryptenroll: {e}"))?;
    for line in String::from_utf8_lossy(&enrolled.stderr).lines() {
        let _ = events.send(Event::Line(line.to_owned()));
    }
    if !enrolled.status.success() {
        return Err(format!(
            "systemd-cryptenroll could not add the PIN ({})",
            enrolled.status
        ));
    }
    Ok(recovery)
}

/// Adds a recovery key to the volume at `node`, unlocked through repart's
/// TPM slot, and returns it. Its output stays out of the log: the key is on
/// stdout and again, as a QR code, on stderr.
fn recovery_key(node: &str, events: &Sender<Event>) -> Result<String, String> {
    let _ = events.send(Event::Line(format!(
        "$ systemd-cryptenroll --unlock-tpm2-device=auto --recovery-key {node}"
    )));
    let mut child = Command::new("systemd-cryptenroll")
        .args(["--unlock-tpm2-device=auto", "--recovery-key", node])
        .env("SYSTEMD_COLORS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("systemd-cryptenroll: {e}"))?;
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("piped")
        .read_to_string(&mut out)
        .map_err(|e| format!("systemd-cryptenroll: {e}"))?;
    let status = child
        .wait()
        .map_err(|e| format!("systemd-cryptenroll: {e}"))?;
    let key = out.trim().to_owned();
    if !status.success() || !is_recovery_key(&key) {
        return Err(format!(
            "systemd-cryptenroll could not add a recovery key ({status})"
        ));
    }
    Ok(key)
}

/// Whether `key` looks like systemd's recovery keys: eight groups of eight
/// modhex characters.
pub fn is_recovery_key(key: &str) -> bool {
    let groups: Vec<&str> = key.split('-').collect();
    groups.len() == 8
        && groups
            .iter()
            .all(|g| g.len() == 8 && g.chars().all(|c| "cbdefghijklnrtuv".contains(c)))
}

/// Root's device node, from repart's JSON summary of the table.
pub fn root_node(json: &str) -> Result<PathBuf, String> {
    let table: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("systemd-repart's output: {e}"))?;
    table
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["type"].as_str().is_some_and(|t| t.starts_with("root")))
        .and_then(|p| p["node"].as_str())
        .map(PathBuf::from)
        .ok_or_else(|| "systemd-repart reported no root partition".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins() {
        assert!(check_pin("12345").is_err());
        assert!(check_pin("123456").is_ok());
        assert!(check_pin("correct horse").is_ok());
        assert!(check_pin("1234\n567").is_err());
    }

    #[test]
    fn recovery_keys() {
        assert!(is_recovery_key(
            "fgbjnkrc-ccjhnveb-nhtfjlbh-hddrcckj-ihlukeen-tjbhvjfh-gdkrlfuf-lrrrrhbj"
        ));
        assert!(!is_recovery_key("fgbjnkrc-ccjhnveb"));
        assert!(!is_recovery_key(""));
    }

    #[test]
    fn finds_root() {
        let json = r#"[{"type":"esp","node":"/dev/vda2"},{"type":"root-x86-64","node":"/dev/vda7"},{"type":"home","node":"/dev/vda8"}]"#;
        assert_eq!(root_node(json).unwrap(), PathBuf::from("/dev/vda7"));
        assert!(root_node("[]").is_err());
    }

    #[test]
    fn needs_a_tpm_and_uefi() {
        let dir = std::env::temp_dir().join(format!("losos-encrypt-{}", std::process::id()));
        let (sys, dev) = (dir.join("sys"), dir.join("dev"));
        fs::create_dir_all(sys.join("firmware")).unwrap();
        fs::create_dir_all(&dev).unwrap();
        assert!(!possible(&sys, &dev));
        fs::write(dev.join("tpmrm0"), "").unwrap();
        assert!(!possible(&sys, &dev));
        fs::create_dir_all(sys.join("firmware/efi")).unwrap();
        assert!(possible(&sys, &dev));
        fs::remove_dir_all(&dir).unwrap();
    }
}
