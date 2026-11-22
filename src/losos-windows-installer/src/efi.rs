//! The firmware boot entry for Limine.
//!
//! An image of LosOS's own boots without one, from the removable-media path
//! `\EFI\BOOT\BOOTX64.EFI` (limine.nix). On a Windows disk that path is
//! Windows' own fallback, so Limine goes beside it at
//! `\EFI\losos\grubx64.efi` (shim's second-stage name; the file is Limine),
//! and the firmware is told about it the way efibootmgr tells it: a
//! `Boot####` variable holding an `EFI_LOAD_OPTION`, first in `BootOrder`.
//! Limine's menu then lists Windows Boot Manager beside LosOS by itself.
//!
//! This module only builds and reads the bytes (UEFI 2.10, 3.1.3 and 10.3);
//! windows.rs reads and writes the variables.
use crate::guid::Guid;

/// `EFI_GLOBAL_VARIABLE`, the namespace of `Boot####` and `BootOrder`.
pub const GLOBAL_VARIABLE: &str = "{8BE4DF61-93CA-11D2-AA0D-00E098032B8C}";
/// Non-volatile, boot service and runtime access.
pub const ATTRIBUTES: u32 = 0x7;
const LOAD_OPTION_ACTIVE: u32 = 0x1;

/// The ESP, as a hard drive media device path names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    /// The GPT entry's 1-based index.
    pub number: u32,
    pub start_lba: u64,
    pub size_lba: u64,
    pub uuid: Guid,
}

fn ucs2(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// An `EFI_LOAD_OPTION` that starts `path` on `partition`, shown in the
/// firmware's menu as `description`.
pub fn load_option(description: &str, partition: &Partition, path: &str) -> Vec<u8> {
    let mut device_path = Vec::new();
    // Hard drive media path: type 4, subtype 1, 42 bytes.
    device_path.extend_from_slice(&[0x04, 0x01]);
    device_path.extend_from_slice(&42u16.to_le_bytes());
    device_path.extend_from_slice(&partition.number.to_le_bytes());
    device_path.extend_from_slice(&partition.start_lba.to_le_bytes());
    device_path.extend_from_slice(&partition.size_lba.to_le_bytes());
    device_path.extend_from_slice(&partition.uuid.to_bytes_le());
    // Partition format 2: GPT; signature type 2: a GUID.
    device_path.extend_from_slice(&[0x02, 0x02]);
    // File path media path: type 4, subtype 4.
    let file = ucs2(path);
    device_path.extend_from_slice(&[0x04, 0x04]);
    device_path.extend_from_slice(&((4 + file.len()) as u16).to_le_bytes());
    device_path.extend_from_slice(&file);
    // End of the whole device path.
    device_path.extend_from_slice(&[0x7f, 0xff, 0x04, 0x00]);

    let mut option = Vec::new();
    option.extend_from_slice(&LOAD_OPTION_ACTIVE.to_le_bytes());
    option.extend_from_slice(&(device_path.len() as u16).to_le_bytes());
    option.extend_from_slice(&ucs2(description));
    option.extend_from_slice(&device_path);
    option
}

/// A load option's description, if it is a well-formed one.
pub fn description(option: &[u8]) -> Option<String> {
    let text = option.get(6..)?;
    let units: Vec<u16> = text
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&units).ok()
}

/// The file path of a load option's first file path node, if it has one.
pub fn file_path(option: &[u8]) -> Option<String> {
    let node = nodes(option)?.into_iter().find(Node::is_file)?;
    let units: Vec<u16> = node
        .data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&units).ok()
}

/// A device path node: type, subtype and the bytes after its 4-byte header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub kind: u8,
    pub sub: u8,
    pub data: Vec<u8>,
}

impl Node {
    fn is_file(&self) -> bool {
        (self.kind, self.sub) == (0x04, 0x04)
    }

    fn is_end(&self) -> bool {
        self.kind == 0x7f
    }

    /// The partition GUID of a GPT hard drive node.
    pub fn hard_drive_uuid(&self) -> Option<Guid> {
        if (self.kind, self.sub) != (0x04, 0x01) || self.data.len() != 38 || self.data[37] != 2 {
            return None;
        }
        Some(Guid::from_bytes_le(self.data[20..36].try_into().ok()?))
    }
}

/// A load option's device path, as nodes, up to its first end node.
pub fn nodes(option: &[u8]) -> Option<Vec<Node>> {
    let path_len = u16::from_le_bytes([*option.get(4)?, *option.get(5)?]) as usize;
    let description_len = (description(option)?.encode_utf16().count() + 1) * 2;
    let mut bytes = option.get(6 + description_len..6 + description_len + path_len)?;
    let mut out = Vec::new();
    while bytes.len() >= 4 {
        let len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
        if len < 4 || len > bytes.len() {
            return None;
        }
        let node = Node {
            kind: bytes[0],
            sub: bytes[1],
            data: bytes[4..len].to_vec(),
        };
        if node.is_end() {
            return Some(out);
        }
        out.push(node);
        bytes = &bytes[len..];
    }
    None
}

/// Windows Boot Manager's own load option with its file swapped for
/// `path`: everything that leads the firmware to the ESP is kept exactly as
/// Windows registered it, which is the form this firmware is known to boot.
/// None if `option` does not name a file on a GPT partition that is `esp`.
pub fn beside(option: &[u8], esp: Guid, description: &str, path: &str) -> Option<Vec<u8>> {
    let nodes = nodes(option)?;
    let file = nodes.iter().position(Node::is_file)?;
    let prefix = &nodes[..file];
    if !prefix.iter().any(|n| n.hard_drive_uuid() == Some(esp)) {
        return None;
    }
    let mut device_path = Vec::new();
    for node in prefix {
        device_path.extend_from_slice(&[node.kind, node.sub]);
        device_path.extend_from_slice(&((4 + node.data.len()) as u16).to_le_bytes());
        device_path.extend_from_slice(&node.data);
    }
    let file = ucs2(path);
    device_path.extend_from_slice(&[0x04, 0x04]);
    device_path.extend_from_slice(&((4 + file.len()) as u16).to_le_bytes());
    device_path.extend_from_slice(&file);
    device_path.extend_from_slice(&[0x7f, 0xff, 0x04, 0x00]);
    let mut out = Vec::new();
    out.extend_from_slice(&LOAD_OPTION_ACTIVE.to_le_bytes());
    out.extend_from_slice(&(device_path.len() as u16).to_le_bytes());
    out.extend_from_slice(&ucs2(description));
    out.extend_from_slice(&device_path);
    Some(out)
}

pub fn boot_variable(number: u16) -> String {
    format!("Boot{number:04X}")
}

pub fn parse_order(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

pub fn encode_order(order: &[u16]) -> Vec<u8> {
    order.iter().flat_map(|n| n.to_le_bytes()).collect()
}

/// `order` with `first` moved, or added, to the front.
pub fn with_first(order: &[u16], first: u16) -> Vec<u16> {
    std::iter::once(first)
        .chain(order.iter().copied().filter(|&n| n != first))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esp() -> Partition {
        Partition {
            number: 1,
            start_lba: 2048,
            size_lba: 204800,
            uuid: Guid::parse_str("e41f8688-063e-4898-bf2a-d33537e9ecb3").unwrap(),
        }
    }

    #[test]
    fn builds_what_efibootmgr_builds() {
        let option = load_option("LosOS", &esp(), r"\EFI\systemd\systemd-bootx64.efi");
        // Attributes, then the device path's length: 42 (disk) + 4 + 66
        // (file, 32 characters and a NUL) + 4 (end).
        assert_eq!(&option[..4], &[1, 0, 0, 0]);
        assert_eq!(u16::from_le_bytes([option[4], option[5]]), 42 + 4 + 66 + 4);
        assert_eq!(&option[6..18], &ucs2("LosOS")[..]);
        let path = &option[18..];
        assert_eq!(&path[..4], &[0x04, 0x01, 42, 0]);
        assert_eq!(&path[4..8], &1u32.to_le_bytes());
        assert_eq!(&path[8..16], &2048u64.to_le_bytes());
        assert_eq!(&path[16..24], &204800u64.to_le_bytes());
        assert_eq!(
            &path[24..40],
            &[
                0x88, 0x86, 0x1f, 0xe4, 0x3e, 0x06, 0x98, 0x48, 0xbf, 0x2a, 0xd3, 0x35, 0x37, 0xe9,
                0xec, 0xb3
            ]
        );
        assert_eq!(&path[40..42], &[2, 2]);
        assert_eq!(&path[42..46], &[0x04, 0x04, 70, 0]);
        assert_eq!(&path[path.len() - 4..], &[0x7f, 0xff, 4, 0]);
        assert_eq!(description(&option).as_deref(), Some("LosOS"));
        assert_eq!(
            file_path(&option).as_deref(),
            Some(r"\EFI\systemd\systemd-bootx64.efi")
        );
    }

    #[test]
    fn puts_an_entry_first_once() {
        assert_eq!(with_first(&[3, 0, 7], 7), vec![7, 3, 0]);
        assert_eq!(with_first(&[3, 0], 9), vec![9, 3, 0]);
        assert_eq!(parse_order(&encode_order(&[0x10, 0x2])), vec![0x10, 0x2]);
        assert_eq!(boot_variable(0x1a), "Boot001A");
    }

    #[test]
    fn swaps_the_file_in_windows_boot_managers_entry() {
        // A firmware's entry: a PCI and NVMe hardware path before the
        // partition, then Windows' boot manager.
        let mut windows = load_option(
            "Windows Boot Manager",
            &esp(),
            r"\EFI\Microsoft\Boot\bootmgfw.efi",
        );
        let hardware = [0x02, 0x01, 0x0c, 0x00, 0xd0, 0x41, 0x03, 0x0a, 0, 0, 0, 0];
        let description_end = 6 + ucs2("Windows Boot Manager").len();
        windows.splice(description_end..description_end, hardware);
        let len = u16::from_le_bytes([windows[4], windows[5]]) + hardware.len() as u16;
        windows[4..6].copy_from_slice(&len.to_le_bytes());

        let ours = beside(
            &windows,
            esp().uuid,
            "LosOS",
            r"\EFI\systemd\systemd-bootx64.efi",
        )
        .unwrap();
        let n = nodes(&ours).unwrap();
        assert_eq!(n.len(), 3);
        assert_eq!((n[0].kind, n[0].sub), (0x02, 0x01));
        assert_eq!(n[1].hard_drive_uuid(), Some(esp().uuid));
        assert_eq!(description(&ours).as_deref(), Some("LosOS"));
        assert_eq!(
            file_path(&ours).as_deref(),
            Some(r"\EFI\systemd\systemd-bootx64.efi")
        );
        // Not an entry for this ESP: refused rather than guessed at.
        let other = Guid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        assert!(beside(&windows, other, "LosOS", r"\x.efi").is_none());
    }

    #[test]
    fn reads_nothing_from_garbage() {
        assert_eq!(file_path(&[1, 0, 0, 0, 200, 0, 65, 0, 0, 0]), None);
        assert_eq!(description(&[1, 0]), None);
    }
}
