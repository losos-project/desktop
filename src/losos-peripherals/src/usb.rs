//! A USB device as sysfs describes it, read before any driver has it.
//!
//! The router decides where a device goes before the host configures it,
//! so it cannot ask the device's interfaces, which do not exist yet. What
//! it reads is what the kernel's hub driver fetched while enumerating it:
//! the device descriptor's fields under the device's directory, and
//! `descriptors`, the raw device descriptor followed by every
//! configuration's descriptors, from which the interfaces the device would
//! have are parsed here.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One interface a configuration offers, at its first alternate setting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Interface {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

/// A USB device, by where it is plugged in and what it says it is.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// The kernel's name for its port, such as `1-2.3`: stable while it
    /// stays plugged into the same port, and what USB/IP exports it by.
    pub busid: String,
    pub busnum: u32,
    pub devnum: u32,
    pub vendor: u16,
    pub product: u16,
    /// bcdDevice, its release, which modaliases carry.
    pub release: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// Its first configuration's interfaces, in order.
    pub interfaces: Vec<Interface>,
    /// That configuration's bConfigurationValue, which the router writes
    /// to configure the device on the host.
    pub configuration: u8,
    /// Its speed in Mbit/s as sysfs gives it: `1.5`, `12`, `480`, `5000`...
    pub speed: String,
    /// What it calls itself, manufacturer and product, for the log and
    /// Settings; the device chose these, so nothing decides by them.
    pub name: String,
    /// The PCI address of the controller it hangs off, when it has one.
    pub controller: Option<String>,
}

const CLASS_HUB: u8 = 0x09;

impl Device {
    /// Reads the device whose sysfs directory is `dir`, such as
    /// /sys/bus/usb/devices/1-2. Interfaces (`1-2:1.0`) and root hubs
    /// (`usb1`) are not devices this reads.
    pub fn read(dir: &Path) -> io::Result<Device> {
        let busid = dir
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| is_busid(n))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a USB device"))?
            .to_owned();
        let attr = |name: &str| -> String {
            fs::read_to_string(dir.join(name))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default()
        };
        let hex16 = |name: &str| u16::from_str_radix(&attr(name), 16).unwrap_or(0);
        let raw = fs::read(dir.join("descriptors"))?;
        let parsed = parse_descriptors(&raw)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unreadable descriptors"))?;
        let name = [attr("manufacturer"), attr("product")]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let controller = fs::canonicalize(dir).ok().and_then(|p| controller_of(&p));
        Ok(Device {
            busid,
            busnum: attr("busnum").parse().unwrap_or(0),
            devnum: attr("devnum").parse().unwrap_or(0),
            vendor: hex16("idVendor"),
            product: hex16("idProduct"),
            release: hex16("bcdDevice"),
            class: parsed.class,
            subclass: parsed.subclass,
            protocol: parsed.protocol,
            interfaces: parsed.interfaces,
            configuration: parsed.configuration,
            speed: attr("speed"),
            name,
            controller,
        })
    }

    pub fn is_hub(&self) -> bool {
        self.class == CLASS_HUB || self.interfaces.iter().any(|i| i.class == CLASS_HUB)
    }

    /// The modalias each interface would have once configured, as the
    /// kernel writes them (drivers/usb/core/message.c): what modprobe
    /// resolves to the drivers that would claim it.
    pub fn modaliases(&self) -> impl Iterator<Item = String> + '_ {
        self.interfaces.iter().map(move |i| {
            format!(
                "usb:v{:04X}p{:04X}d{:04X}dc{:02X}dsc{:02X}dp{:02X}ic{:02X}isc{:02X}ip{:02X}in{:02X}",
                self.vendor,
                self.product,
                self.release,
                self.class,
                self.subclass,
                self.protocol,
                i.class,
                i.subclass,
                i.protocol,
                i.number
            )
        })
    }

    /// The usb_device_speed USB/IP's attach takes (include/uapi/linux/usb/ch9.h).
    pub fn speed_code(&self) -> u32 {
        match self.speed.as_str() {
            "1.5" => 1,
            "12" => 2,
            "480" => 3,
            "5000" => 5,
            "10000" | "20000" => 6,
            _ => 0,
        }
    }

    /// The device id USB/IP's attach takes: bus and address.
    pub fn devid(&self) -> u32 {
        (self.busnum << 16) | self.devnum
    }
}

/// `1-2`, `3-1.4.2`: a bus number, a dash, and port numbers joined by dots.
pub fn is_busid(name: &str) -> bool {
    let Some((bus, ports)) = name.split_once('-') else {
        return false;
    };
    !bus.is_empty()
        && bus.bytes().all(|b| b.is_ascii_digit())
        && !ports.is_empty()
        && ports
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `0000:00:14.0`: a PCI function's address.
pub fn is_pci_address(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 12
        && b[4] == b':'
        && b[7] == b':'
        && b[10] == b'.'
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10) || c.is_ascii_hexdigit())
}

/// The PCI function nearest the root hub in a device's sysfs path: the
/// controller, the component just before `usbN`.
pub fn controller_of(path: &Path) -> Option<String> {
    let parts: Vec<&str> = path.iter().filter_map(|c| c.to_str()).collect();
    let root = parts
        .iter()
        .position(|p| p.starts_with("usb") && p[3..].parse::<u32>().is_ok())?;
    parts[..root]
        .iter()
        .rev()
        .find(|p| is_pci_address(p))
        .map(|p| (*p).to_owned())
}

/// Every PCI function on a sysfs path, outermost first: the bridges and
/// the device a block or input device sits under.
pub fn pci_functions(path: &Path) -> impl Iterator<Item = String> + '_ {
    path.iter()
        .filter_map(|c| c.to_str())
        .filter(|c| is_pci_address(c))
        .map(str::to_owned)
}

/// What `descriptors` holds that the router needs.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Descriptors {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub configuration: u8,
    pub interfaces: Vec<Interface>,
}

/// Parses the device descriptor and the first configuration's interfaces
/// out of sysfs's `descriptors`. A descriptor shorter than it says, or one
/// that claims to be shorter than two bytes, ends the walk: the device
/// wrote these, and a loop on them must always move forward.
pub fn parse_descriptors(raw: &[u8]) -> Option<Descriptors> {
    // The device descriptor: 18 bytes, type 1.
    if raw.len() < 18 || raw[0] < 18 || raw[1] != 1 {
        return None;
    }
    let mut out = Descriptors {
        class: raw[4],
        subclass: raw[5],
        protocol: raw[6],
        ..Default::default()
    };
    let mut rest = &raw[raw[0] as usize..];
    // The first configuration: type 2, wTotalLength covering it and every
    // interface and endpoint descriptor under it.
    if rest.len() < 9 || rest[1] != 2 {
        return Some(out);
    }
    let total = u16::from_le_bytes([rest[2], rest[3]]) as usize;
    out.configuration = rest[5];
    rest = &rest[..total.min(rest.len())];
    while rest.len() >= 2 {
        let len = rest[0] as usize;
        if len < 2 || len > rest.len() {
            break;
        }
        // An interface descriptor, at alternate setting 0.
        if rest[1] == 4 && len >= 9 && rest[3] == 0 {
            out.interfaces.push(Interface {
                number: rest[2],
                class: rest[5],
                subclass: rest[6],
                protocol: rest[7],
            });
        }
        rest = &rest[len..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A flash drive: one interface, mass storage, SCSI, bulk-only.
    pub const FLASH_DRIVE: &[u8] = &[
        18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x81, 0x07, 0x81, 0x55, 0x00, 0x01, 1, 2, 3,
        1, // device
        9, 2, 32, 0, 1, 1, 0, 0x80, 50, // configuration 1, 32 bytes
        9, 4, 0, 0, 2, 0x08, 0x06, 0x50, 0, // interface 0
        7, 5, 0x81, 2, 0x00, 0x02, 0, // endpoint
        7, 5, 0x02, 2, 0x00, 0x02, 0, // endpoint
    ];

    #[test]
    fn parses_a_flash_drive() {
        let d = parse_descriptors(FLASH_DRIVE).unwrap();
        assert_eq!(d.configuration, 1);
        assert_eq!(
            d.interfaces,
            vec![Interface {
                number: 0,
                class: 8,
                subclass: 6,
                protocol: 0x50
            }]
        );
    }

    #[test]
    fn a_zero_length_descriptor_ends_the_walk() {
        let mut raw = FLASH_DRIVE.to_vec();
        raw[27] = 0;
        assert!(parse_descriptors(&raw).unwrap().interfaces.is_empty());
    }

    #[test]
    fn busids() {
        assert!(is_busid("1-2"));
        assert!(is_busid("3-1.4.2"));
        assert!(!is_busid("usb1"));
        assert!(!is_busid("1-2:1.0"));
        assert!(!is_busid("1-"));
    }

    #[test]
    fn the_controller_is_the_function_before_the_root_hub() {
        let p = Path::new("/sys/devices/pci0000:00/0000:00:1c.0/0000:03:00.0/usb3/3-1/3-1.2");
        assert_eq!(controller_of(p).as_deref(), Some("0000:03:00.0"));
        assert_eq!(
            pci_functions(p).collect::<Vec<_>>(),
            ["0000:00:1c.0", "0000:03:00.0"]
        );
    }

    #[test]
    fn modalias_matches_the_kernels() {
        let d = Device {
            vendor: 0x0781,
            product: 0x5581,
            release: 0x0100,
            interfaces: parse_descriptors(FLASH_DRIVE).unwrap().interfaces,
            ..Default::default()
        };
        assert_eq!(
            d.modaliases().collect::<Vec<_>>(),
            ["usb:v0781p5581d0100dc00dsc00dp00ic08isc06ip50in00"]
        );
    }
}
