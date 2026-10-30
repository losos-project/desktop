//! The controllers the host must keep: the ones the machine cannot be used
//! without.
//!
//! The USB machine takes every USB controller it can, and with one goes
//! everything plugged into it. A controller that holds the disk the system
//! booted from, or the keyboard or pointer someone is using, stays: without
//! the first the host stops, and without the others nobody can type the
//! disk's password at the next boot or use the desktop at all. That is
//! decided once a boot, before any controller is passed through, from what
//! is plugged in then, and written as the PCI functions to keep, one per
//! line, which the passthrough unit skips with their IOMMU groups.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::usb::pci_functions;

/// Every PCI function above a mounted block device (through device-mapper,
/// to the disks under it) and above a keyboard or pointer, under `sys`.
pub fn kept(sys: &Path, mountinfo: &str) -> BTreeSet<String> {
    let mut keep = BTreeSet::new();
    for path in mounted_disks(sys, mountinfo) {
        keep.extend(pci_functions(&path));
    }
    for path in input_devices(sys) {
        keep.extend(pci_functions(&path));
    }
    keep
}

/// The canonical sysfs paths of every mounted block device and the disks
/// under it, which the router also keeps the USB devices above on the host.
pub fn mounted_disks(sys: &Path, mountinfo: &str) -> Vec<std::path::PathBuf> {
    mounted(mountinfo)
        .flat_map(|dev| disks(sys, &sys.join("dev/block").join(dev)))
        .collect()
}

/// The `major:minor` of every mounted block device in /proc/self/mountinfo.
fn mounted(mountinfo: &str) -> impl Iterator<Item = &str> {
    mountinfo
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2))
        .filter(|d| !d.starts_with("0:"))
}

/// The canonical sysfs paths of a block device and everything under it:
/// the slaves of a device-mapper target such as /usr's verity, and theirs.
fn disks(sys: &Path, dev: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dev.to_path_buf()];
    while let Some(dev) = todo.pop() {
        let Ok(path) = fs::canonicalize(&dev) else {
            continue;
        };
        if out.contains(&path) || !path.starts_with(sys) {
            continue;
        }
        if let Ok(slaves) = fs::read_dir(path.join("slaves")) {
            todo.extend(slaves.flatten().map(|e| e.path()));
        }
        out.push(path);
    }
    out
}

/// The canonical sysfs paths of every input device that is a keyboard or a
/// pointer.
fn input_devices(sys: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    fs::read_dir(sys.join("class/input"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("input"))
        .filter_map(|e| fs::canonicalize(e.path()).ok())
        .filter(|path| {
            let caps = |name: &str| {
                fs::read_to_string(path.join("capabilities").join(name))
                    .map(|s| Bitmap::parse(&s))
                    .unwrap_or_default()
            };
            is_keyboard(&caps("ev"), &caps("key"))
                || is_pointer(&caps("ev"), &caps("key"), &caps("rel"), &caps("abs"))
        })
}

/// A capability bitmap as sysfs prints it: hexadecimal longs, the most
/// significant first.
#[derive(Debug, Default)]
pub struct Bitmap(Vec<u64>);

impl Bitmap {
    pub fn parse(s: &str) -> Bitmap {
        Bitmap(
            s.split_whitespace()
                .rev()
                .map(|w| u64::from_str_radix(w, 16).unwrap_or(0))
                .collect(),
        )
    }

    pub fn has(&self, bit: usize) -> bool {
        self.0
            .get(bit / 64)
            .is_some_and(|w| w & (1 << (bit % 64)) != 0)
    }
}

const EV_KEY: usize = 1;
const EV_REL: usize = 2;
const EV_ABS: usize = 3;
const REL_X: usize = 0;
const REL_Y: usize = 1;
const ABS_X: usize = 0;
const ABS_Y: usize = 1;
const BTN_LEFT: usize = 0x110;
const BTN_TOOL_FINGER: usize = 0x145;
const BTN_TOUCH: usize = 0x14a;

/// udev's own test (input_id): Escape and the first row and a half of
/// letters, keys 1 to 31, all there.
pub fn is_keyboard(ev: &Bitmap, key: &Bitmap) -> bool {
    ev.has(EV_KEY) && (1..32).all(|k| key.has(k))
}

/// A mouse (relative X and Y, a left button), or a touchpad or touch
/// screen (absolute X and Y, touch).
pub fn is_pointer(ev: &Bitmap, key: &Bitmap, rel: &Bitmap, abs: &Bitmap) -> bool {
    let mouse = ev.has(EV_REL) && rel.has(REL_X) && rel.has(REL_Y) && key.has(BTN_LEFT);
    let touch = ev.has(EV_ABS)
        && abs.has(ABS_X)
        && abs.has(ABS_Y)
        && (key.has(BTN_TOUCH) || key.has(BTN_TOOL_FINGER) || key.has(BTN_LEFT));
    mouse || touch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyboard_is_a_keyboard() {
        // An AT keyboard's, from a real /sys/class/input/input3.
        let ev = Bitmap::parse("120013");
        let key = Bitmap::parse("402000000 3803078f800d001 feffffdfffefffff fffffffffffffffe");
        assert!(is_keyboard(&ev, &key));
        assert!(!is_pointer(
            &ev,
            &key,
            &Bitmap::default(),
            &Bitmap::default()
        ));
    }

    #[test]
    fn a_power_button_is_not() {
        let ev = Bitmap::parse("3");
        let key = Bitmap::parse("10000000000000 0");
        assert!(!is_keyboard(&ev, &key));
    }

    #[test]
    fn a_mouse_is_a_pointer() {
        let ev = Bitmap::parse("17");
        let key = Bitmap::parse("ff0000 0 0 0 0");
        let rel = Bitmap::parse("1943");
        assert!(is_pointer(&ev, &key, &rel, &Bitmap::default()));
    }

    #[test]
    fn mounted_devices_skip_virtual_filesystems() {
        let info = "22 1 0:21 / /proc rw - proc proc rw\n\
                    25 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw\n\
                    26 25 254:0 / /usr ro - erofs /dev/mapper/usr ro\n";
        assert_eq!(mounted(info).collect::<Vec<_>>(), ["259:2", "254:0"]);
    }
}
