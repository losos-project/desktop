//! The host's own USB ports, held so nothing plugged into them is driven
//! before the router says so.
//!
//! A controller the host keeps (keep.rs) still has ports anything can be
//! plugged into. The kernel would configure each new device at once and
//! bind its drivers: usb-storage, then udev's blkid reading the drive's
//! filesystem, which is what the files machine is there to do instead. So
//! the router claims every port of every hub the host has
//! (USBDEVFS_CLAIM_PORT): the kernel still enumerates what is plugged in,
//! reading its descriptors, but leaves it unconfigured, with no interfaces
//! and no drivers, for the router to hand to a machine as it is, which
//! configures it through usbfs with no host driver binding (an owned
//! device's interfaces take none), or to give back to the kernel with its
//! port when the host keeps it. The claims are held by open
//! files, so if the router stops they go with it and the ports behave as
//! they did without it.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

/// _IOR('U', 24, unsigned int) and _IOR('U', 25, unsigned int), from
/// linux/usbdevice_fs.h.
const USBDEVFS_CLAIM_PORT: libc::c_ulong = 0x8004_5518;
const USBDEVFS_RELEASE_PORT: libc::c_ulong = 0x8004_5519;

/// One hub's claimed ports, by the file that holds them.
pub struct Claim {
    hub: File,
}

fn port_ioctl(hub: &File, request: libc::c_ulong, port: u32) -> io::Result<()> {
    let mut n: libc::c_uint = port;
    if unsafe { libc::ioctl(hub.as_raw_fd(), request as _, &mut n) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Claims every port of the hub at sysfs `dir`, such as
/// /sys/bus/usb/devices/usb1 or /sys/bus/usb/devices/1-2.
pub fn claim(dir: &Path) -> io::Result<Claim> {
    let attr = |name: &str| fs::read_to_string(dir.join(name)).map(|s| s.trim().to_owned());
    let ports: u32 = attr("maxchild")?.parse().unwrap_or(0);
    let node = format!(
        "/dev/bus/usb/{:03}/{:03}",
        attr("busnum")?.parse::<u32>().unwrap_or(0),
        attr("devnum")?.parse::<u32>().unwrap_or(0)
    );
    let claim = Claim {
        hub: OpenOptions::new().read(true).write(true).open(&node)?,
    };
    for port in 1..=ports {
        claim
            .take(port)
            .map_err(|e| io::Error::new(e.kind(), format!("{node} port {port}: {e}")))?;
    }
    Ok(claim)
}

impl Claim {
    /// Claims one port again, once what the host kept on it is gone.
    pub fn take(&self, port: u32) -> io::Result<()> {
        match port_ioctl(&self.hub, USBDEVFS_CLAIM_PORT, port) {
            // Claimed already, by this claim.
            Err(e) if e.raw_os_error() == Some(libc::EBUSY) => Ok(()),
            r => r,
        }
    }

    /// Gives one port back to the kernel, for a device the host keeps: an
    /// interface driver will not bind to a device on a claimed port
    /// (usb_probe_interface), not even after a suspend.
    pub fn give_back(&self, port: u32) -> io::Result<()> {
        match port_ioctl(&self.hub, USBDEVFS_RELEASE_PORT, port) {
            // Not claimed.
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
            r => r,
        }
    }
}

/// The hub a device hangs off, by its sysfs name, and its port there:
/// `1-2.3` is port 3 of `1-2`, and `1-2` port 2 of the root hub `usb1`.
pub fn parent_port(busid: &str) -> Option<(String, u32)> {
    let (bus, ports) = busid.split_once('-')?;
    match ports.rsplit_once('.') {
        Some((_, port)) => Some((
            busid[..busid.len() - port.len() - 1].to_owned(),
            port.parse().ok()?,
        )),
        None => Some((format!("usb{bus}"), ports.parse().ok()?)),
    }
}

/// Configures a device on a claimed port, as the kernel would have:
/// interfaces appear and their drivers bind.
pub fn configure(dir: &Path, configuration: u8) -> io::Result<()> {
    let current = fs::read_to_string(dir.join("bConfigurationValue")).unwrap_or_default();
    if current.trim().parse::<u8>().unwrap_or(0) != 0 {
        return Ok(());
    }
    fs::write(
        dir.join("bConfigurationValue"),
        configuration.max(1).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parents_and_ports() {
        assert_eq!(parent_port("1-2"), Some(("usb1".into(), 2)));
        assert_eq!(parent_port("3-1.4.2"), Some(("3-1.4".into(), 2)));
        assert_eq!(parent_port("usb1"), None);
    }
}
