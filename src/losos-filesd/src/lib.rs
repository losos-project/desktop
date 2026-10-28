//! losos-filesd, the files machine's init and its one daemon
//! (docs/files-machine.md): it mounts the storage the machine is given (USB
//! sticks and disks over USB/IP, SD cards in a reader handed through) and
//! serves each to the host, which shows them in Files. The filesystems'
//! code, which parses whatever is on a stranger's stick, runs here, in a
//! machine with no network and nothing the host trusts.
//!
//! The pieces that are pure text, and tested as such, are here.

use std::path::PathBuf;

/// Where each volume is mounted, by its name.
pub const MEDIA: &str = "/media";
/// The volumes, as `losos-filesd list` prints them, kept by the daemon.
pub const VOLUMES: &str = "/run/losos-filesd/volumes";
/// Devices ejected while still plugged in, which the daemon leaves alone.
pub const EJECTED: &str = "/run/losos-filesd/ejected";

/// A filesystem `blkid` found on a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Its device's name under /dev, such as `sda1`.
    pub device: String,
    pub label: String,
    pub uuid: String,
    pub fstype: String,
}

/// One line of BusyBox's `blkid`: `/dev/sda1: LABEL="STICK" UUID="1234-ABCD"
/// TYPE="vfat"`.
pub fn parse_blkid(line: &str) -> Option<Volume> {
    let (device, rest) = line.split_once(": ")?;
    let device = device.strip_prefix("/dev/")?.to_owned();
    let mut volume = Volume {
        device,
        label: String::new(),
        uuid: String::new(),
        fstype: String::new(),
    };
    let mut rest = rest.trim();
    while let Some((key, after)) = rest.split_once("=\"") {
        let (value, after) = after.split_once('"')?;
        match key.trim() {
            "LABEL" => volume.label = value.to_owned(),
            "UUID" => volume.uuid = value.to_owned(),
            "TYPE" => volume.fstype = value.to_owned(),
            _ => {}
        }
        rest = after.trim();
    }
    (!volume.fstype.is_empty()).then_some(volume)
}

/// The kernel filesystem and the mount options for a type `blkid` names, or
/// none for one that is not files (swap, a RAID member, LUKS). Nothing on
/// it runs or is a device; FAT's and exFAT's names are kept as they were
/// written.
pub fn mount_options(fstype: &str) -> Option<(&'static str, &'static str)> {
    const SAFE: &str = "nosuid,nodev,noexec,noatime";
    Some(match fstype {
        "vfat" => (
            "vfat",
            "nosuid,nodev,noexec,noatime,utf8=1,shortname=mixed,flush",
        ),
        "exfat" => ("exfat", "nosuid,nodev,noexec,noatime,iocharset=utf8"),
        "ntfs" | "ntfs3" => ("ntfs3", "nosuid,nodev,noexec,noatime,iocharset=utf8"),
        "ext2" | "ext3" | "ext4" => ("ext4", SAFE),
        "btrfs" => ("btrfs", SAFE),
        "xfs" => ("xfs", SAFE),
        "f2fs" => ("f2fs", SAFE),
        "iso9660" => ("iso9660", "ro,nosuid,nodev,noexec"),
        "udf" => ("udf", "ro,nosuid,nodev,noexec"),
        _ => return None,
    })
}

/// Whether `name` is one a volume could have: a single path component that
/// is not hidden.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name.contains(['/', '\0', '\n', '\t'])
}

/// The name a volume is mounted and shown by: its label, else its UUID,
/// else its device, made a valid name and kept apart from those `taken`.
pub fn mount_name(volume: &Volume, taken: &[String]) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if matches!(c, '/' | '\0' | '\n' | '\t') {
                    '_'
                } else {
                    c
                }
            })
            .collect::<String>()
            .trim()
            .trim_start_matches('.')
            .to_owned()
    };
    let base = [&volume.label, &volume.uuid, &volume.device]
        .into_iter()
        .map(|s| clean(s))
        .find(|s| valid_name(s))
        .unwrap_or_else(|| "disk".to_owned());
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{base} ({n})");
        n += 1;
    }
    name
}

/// A line of the volumes file: name, type, size in bytes and device,
/// tab-separated.
pub fn volume_line(name: &str, volume: &Volume, size: u64) -> String {
    format!("{name}\t{}\t{size}\t{}\n", volume.fstype, volume.device)
}

/// The names in the volumes file.
pub fn names(volumes: &str) -> Vec<String> {
    volumes
        .lines()
        .filter_map(|line| line.split('\t').next())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The device a name in the volumes file is on.
pub fn device_of(volumes: &str, name: &str) -> Option<String> {
    volumes.lines().find_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        (fields.first() == Some(&name)).then(|| fields.get(3).map(|d| (*d).to_owned()))?
    })
}

/// The machine's configuration: `key value...` lines, `#` comments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// OpenSSH's sftp-server, which serves a volume to the host.
    pub sftp_server: Option<PathBuf>,
    /// How long the machine waits with nothing plugged in before it
    /// powers off, in seconds; none to stay up.
    pub idle: Option<u64>,
    /// Programs kept running, each a command line.
    pub run: Vec<Vec<String>>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut config = Config::default();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let bad = || format!("line {}: {line}", number + 1);
            match words.as_slice() {
                ["sftp-server", path] => config.sftp_server = Some(path.into()),
                ["idle", seconds] => config.idle = Some(seconds.parse().map_err(|_| bad())?),
                ["run", command @ ..] if !command.is_empty() => config
                    .run
                    .push(command.iter().map(|w| (*w).to_owned()).collect()),
                _ => return Err(bad()),
            }
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stick() -> Volume {
        Volume {
            device: "sda1".into(),
            label: "STICK".into(),
            uuid: "1234-ABCD".into(),
            fstype: "vfat".into(),
        }
    }

    #[test]
    fn reads_blkid() {
        assert_eq!(
            parse_blkid(r#"/dev/sda1: LABEL="STICK" UUID="1234-ABCD" TYPE="vfat""#),
            Some(stick())
        );
        let no_label = parse_blkid(r#"/dev/mmcblk0p1: UUID="e1f0" TYPE="ext4""#).unwrap();
        assert_eq!(no_label.device, "mmcblk0p1");
        assert_eq!(no_label.label, "");
        assert_eq!(parse_blkid(r#"/dev/sdb: UUID="x""#), None);
        assert_eq!(parse_blkid("garbage"), None);
    }

    #[test]
    fn names_volumes() {
        assert_eq!(mount_name(&stick(), &[]), "STICK");
        assert_eq!(mount_name(&stick(), &["STICK".into()]), "STICK (2)");
        let odd = Volume {
            label: "../etc".into(),
            ..stick()
        };
        assert_eq!(mount_name(&odd, &[]), "_etc");
        let blank = Volume {
            label: " ".into(),
            uuid: String::new(),
            ..stick()
        };
        assert_eq!(mount_name(&blank, &[]), "sda1");
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
    }

    #[test]
    fn mounts_only_files() {
        assert_eq!(mount_options("ntfs").unwrap().0, "ntfs3");
        assert!(mount_options("vfat").unwrap().1.contains("noexec"));
        assert_eq!(mount_options("swap"), None);
        assert_eq!(mount_options("crypto_LUKS"), None);
    }

    #[test]
    fn keeps_the_volume_list() {
        let text = volume_line("STICK", &stick(), 8_000_000) + &volume_line("Photos", &stick(), 1);
        assert_eq!(names(&text), ["STICK", "Photos"]);
        assert_eq!(device_of(&text, "Photos").as_deref(), Some("sda1"));
        assert_eq!(device_of(&text, "nope"), None);
    }

    #[test]
    fn reads_the_configuration() {
        let config = Config::parse(
            "sftp-server /bin/sftp-server\nidle 300\nrun losos-vm-call serve 5100 losos-filesd list\n",
        )
        .unwrap();
        assert_eq!(config.idle, Some(300));
        assert_eq!(config.run.len(), 1);
        assert!(Config::parse("idle soon").is_err());
    }
}
