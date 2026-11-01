//! The graphics cards a gaming machine can be given, and giving it one.
//!
//! A card goes whole: the GPU and every other function on its slot, which
//! on most cards is the HDMI audio and on some a USB-C controller and its
//! UCSI function, since they share its IOMMU group and with it its DMA.
//! Anything else in the group would go with it, so a card whose group
//! holds anything but its own functions and PCI bridges stays on the
//! host. So does a card the peripherals router keeps for the host
//! (/run/losos-passthrough/keep).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// One PCI function, as sysfs describes it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Function {
    /// `0000:01:00.0`.
    pub address: String,
    /// PCI class, as in sysfs: `0x030000`.
    pub class: u32,
    pub vendor: u16,
    pub device: u16,
    /// The driver bound to it now, if any.
    pub driver: Option<String>,
}

/// A display controller the host has.
#[derive(Clone, Debug, Serialize)]
pub struct Gpu {
    #[serde(flatten)]
    pub function: Function,
    /// The card the firmware drew its screens on, and so the one the
    /// desktop draws on unless told otherwise.
    pub boot_vga: bool,
    /// What giving it to the gaming machine takes, or why it cannot go.
    pub takes: Result<Vec<Function>, String>,
}

/// PCI class 0x03: VGA, 3D (NVIDIA's cards without outputs) and other
/// display controllers.
const DISPLAY: u32 = 0x03;
/// PCI-to-PCI bridges, which an IOMMU group may hold without sharing DMA
/// with the card behind them.
const BRIDGE: u32 = 0x0604;

/// Every display controller under `sys`, in address order.
pub fn gpus(sys: &Path, keep: &[String]) -> Vec<Gpu> {
    let devices = sys.join("bus/pci/devices");
    let mut out: Vec<Gpu> = fs::read_dir(&devices)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| function(&devices, &entry.file_name().to_string_lossy()))
        .filter(|f| f.class >> 16 == DISPLAY)
        .map(|f| Gpu {
            boot_vga: read(&devices.join(&f.address).join("boot_vga")).as_deref() == Some("1"),
            takes: takes(sys, &f.address, keep),
            function: f,
        })
        .collect();
    out.sort_by(|a, b| a.function.address.cmp(&b.function.address));
    out
}

/// The card to offer first: on a PC with two or more, the first that can
/// go and is not the one the firmware drew on, which is where the person's
/// screen is plugged in. A PC with one card has none to spare until it can
/// be handed over and back (docs/gaming-machine.md).
pub fn choose(gpus: &[Gpu]) -> Option<&Gpu> {
    if gpus.len() < 2 {
        return None;
    }
    gpus.iter().find(|g| !g.boot_vga && g.takes.is_ok())
}

/// The functions that go with the card at `address`.
pub fn takes(sys: &Path, address: &str, keep: &[String]) -> Result<Vec<Function>, String> {
    let devices = sys.join("bus/pci/devices");
    let group = devices.join(address).join("iommu_group/devices");
    let members = fs::read_dir(&group).map_err(|_| {
        "the IOMMU is off, so no device can be fenced off for a machine; turn on \
         VT-d or AMD-Vi (often called IOMMU or SVM) in the firmware's settings"
            .to_owned()
    })?;
    let card = slot(address);
    let mut out = Vec::new();
    for member in members.flatten() {
        let name = member.file_name().to_string_lossy().into_owned();
        let Some(f) = function(&devices, &name) else {
            continue;
        };
        if f.class >> 8 == BRIDGE {
            continue;
        }
        if slot(&name) != card {
            return Err(format!(
                "it shares its IOMMU group with {name}, which is not part of the card, \
                 and that would have to go with it; another slot may group it alone"
            ));
        }
        if keep.contains(&name) {
            return Err(format!("{name} is kept on the host"));
        }
        out.push(f);
    }
    out.sort_by(|a, b| a.address.cmp(&b.address));
    Ok(out)
}

/// Binds each function to vfio-pci, so no host driver drives it again this
/// boot: run before udev's coldplug, so none ever has, and done again
/// harmlessly for a function already there. A function some driver took in
/// the initrd (the USB controller of a card with USB-C) is unbound first.
pub fn bind(sys: &Path, functions: &[Function]) -> std::io::Result<()> {
    let devices = sys.join("bus/pci/devices");
    for f in functions {
        let dev = devices.join(&f.address);
        fs::write(dev.join("driver_override"), "vfio-pci")?;
        match driver(&dev).as_deref() {
            Some("vfio-pci") => continue,
            Some(_) => fs::write(dev.join("driver/unbind"), &f.address)?,
            None => {}
        }
        fs::write(sys.join("bus/pci/drivers_probe"), &f.address)?;
        tracing::info!(function = f.address, "bound to vfio-pci");
    }
    Ok(())
}

/// `0000:01:00` of `0000:01:00.1`: the slot, whose functions are one card.
fn slot(address: &str) -> &str {
    address.rsplit_once('.').map_or(address, |(slot, _)| slot)
}

fn function(devices: &Path, address: &str) -> Option<Function> {
    let dev = devices.join(address);
    let hex =
        |name: &str| u32::from_str_radix(read(&dev.join(name))?.trim_start_matches("0x"), 16).ok();
    Some(Function {
        address: address.to_owned(),
        class: hex("class")?,
        vendor: u16::try_from(hex("vendor")?).ok()?,
        device: u16::try_from(hex("device")?).ok()?,
        driver: driver(&dev),
    })
}

fn driver(dev: &Path) -> Option<String> {
    let link: PathBuf = fs::read_link(dev.join("driver")).ok()?;
    Some(link.file_name()?.to_string_lossy().into_owned())
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A sysfs with `devices`, each `(address, class, group, driver)`.
    fn sysfs(devices: &[(&str, &str, u32, Option<&str>)]) -> tempdir::Dir {
        let dir = tempdir::Dir::new();
        let pci = dir.0.join("bus/pci/devices");
        for (address, class, group, driver) in devices {
            let dev = dir.0.join("devices/pci").join(address);
            fs::create_dir_all(&dev).unwrap();
            fs::write(dev.join("class"), format!("{class}\n")).unwrap();
            fs::write(dev.join("vendor"), "0x1002\n").unwrap();
            fs::write(dev.join("device"), "0x744c\n").unwrap();
            fs::write(
                dev.join("boot_vga"),
                if *address == "0000:00:02.0" {
                    "1\n"
                } else {
                    "0\n"
                },
            )
            .unwrap();
            if let Some(driver) = driver {
                let d = dir.0.join("bus/pci/drivers").join(driver);
                fs::create_dir_all(&d).unwrap();
                symlink(&d, dev.join("driver")).unwrap();
            }
            let g = dir
                .0
                .join("kernel/iommu_groups")
                .join(group.to_string())
                .join("devices");
            fs::create_dir_all(&g).unwrap();
            symlink(&dev, g.join(address)).unwrap();
            symlink(
                dir.0.join("kernel/iommu_groups").join(group.to_string()),
                dev.join("iommu_group"),
            )
            .unwrap();
            fs::create_dir_all(&pci).unwrap();
            symlink(&dev, pci.join(address)).unwrap();
        }
        dir
    }

    #[test]
    fn a_card_goes_with_its_audio_and_its_bridge_stays_out() {
        let sys = sysfs(&[
            ("0000:00:02.0", "0x030000", 0, Some("i915")),
            ("0000:00:01.0", "0x060400", 1, Some("pcieport")),
            ("0000:01:00.0", "0x030000", 1, None),
            ("0000:01:00.1", "0x040300", 1, None),
        ]);
        let gpus = gpus(&sys.0, &[]);
        assert_eq!(gpus.len(), 2);
        let chosen = choose(&gpus).unwrap();
        assert_eq!(chosen.function.address, "0000:01:00.0");
        let takes: Vec<_> = chosen
            .takes
            .as_ref()
            .unwrap()
            .iter()
            .map(|f| f.address.as_str())
            .collect();
        assert_eq!(takes, ["0000:01:00.0", "0000:01:00.1"]);
    }

    #[test]
    fn a_group_shared_with_another_device_keeps_the_card() {
        let sys = sysfs(&[
            ("0000:00:02.0", "0x030000", 0, Some("i915")),
            ("0000:01:00.0", "0x030000", 1, None),
            ("0000:02:00.0", "0x010802", 1, Some("nvme")),
        ]);
        let gpus = gpus(&sys.0, &[]);
        assert!(gpus[1].takes.as_ref().unwrap_err().contains("0000:02:00.0"));
        assert!(choose(&gpus).is_none());
    }

    #[test]
    fn one_card_is_not_offered() {
        let sys = sysfs(&[("0000:00:02.0", "0x030000", 0, Some("i915"))]);
        assert!(choose(&gpus(&sys.0, &[])).is_none());
    }

    #[test]
    fn a_kept_function_keeps_the_card() {
        let sys = sysfs(&[
            ("0000:00:02.0", "0x030000", 0, Some("i915")),
            ("0000:01:00.0", "0x030000", 1, None),
            ("0000:01:00.2", "0x0c0330", 1, Some("xhci_hcd")),
        ]);
        let keep = vec!["0000:01:00.2".to_owned()];
        assert!(gpus(&sys.0, &keep)[1].takes.is_err());
    }

    /// A directory removed when the test ends, without a crate for it.
    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let path = std::env::temp_dir().join(format!(
                    "losos-gaming-test-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
