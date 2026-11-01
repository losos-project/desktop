//! The machines devices go to, through libvirt's virsh.
//!
//! The service machines (the USB, Bluetooth, network and files machines)
//! are libvirt's, defined at boot from the NixOS configuration and hidden
//! from the Machines app. A disposable machine is a transient copy of the
//! template definition the NixOS configuration writes, under a name and
//! vsock address of its own: libvirt forgets a transient machine when it
//! stops, and it has no disk to keep anything on, so a device's machine is
//! gone, and all it saw, once the device is.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::usb::Device;

fn virsh(args: &[&str], input: Option<&str>) -> io::Result<String> {
    let mut child = Command::new("virsh")
        .args(["--quiet"])
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(input.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "virsh {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A device on one of the host's own ports, as libvirt passes it through:
/// QEMU opens it through usbfs and the machine configures it.
pub fn hostdev(device: &Device) -> String {
    format!(
        "<hostdev mode='subsystem' type='usb' managed='yes'>\
         <source startupPolicy='optional'><address bus='{}' device='{}'/></source>\
         </hostdev>",
        device.busnum, device.devnum
    )
}

/// Plugs a host port's device into a running machine.
pub fn attach(machine: &str, device: &Device) -> io::Result<()> {
    virsh(
        &["attach-device", machine, "/dev/stdin", "--live"],
        Some(&hostdev(device)),
    )
    .map(drop)
}

/// Unplugs it again. A device that was pulled out is unplugged by QEMU
/// already, and libvirt then says so; that is not a failure.
pub fn detach(machine: &str, device: &Device) -> io::Result<()> {
    match virsh(
        &["detach-device", machine, "/dev/stdin", "--live"],
        Some(&hostdev(device)),
    ) {
        Err(e) if e.to_string().contains("not found") => Ok(()),
        r => r.map(drop),
    }
}

/// Starts a disposable machine from `template`, a domain definition with
/// @NAME@, @CID@, @TYPE@ and @CPU@ left to fill: KVM and the host's CPU
/// when the host has KVM, QEMU's emulation of its own otherwise.
pub fn start_disposable(template: &Path, name: &str, cid: u32) -> io::Result<()> {
    let (kind, cpu) = if Path::new("/dev/kvm").exists() {
        ("kvm", "<cpu mode='host-passthrough'/>")
    } else {
        ("qemu", "<cpu mode='maximum'/>")
    };
    let xml = fs::read_to_string(template)?
        .replace("@NAME@", name)
        .replace("@CID@", &cid.to_string())
        .replace("@TYPE@", kind)
        .replace("@CPU@", cpu);
    virsh(&["create", "/dev/stdin"], Some(&xml)).map(drop)
}

/// Starts or stops a service machine's unit, without waiting for it.
pub fn unit(action: &str, unit: &str) -> io::Result<()> {
    let status = Command::new("systemctl")
        .args([action, "--no-block", unit])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "systemctl {action} {unit} failed"
        )))
    }
}

/// Stops one, which libvirt then forgets.
pub fn destroy(name: &str) -> io::Result<()> {
    virsh(&["destroy", name], None).map(drop)
}

/// Disposable machines left from a router that stopped, by name.
pub fn running(prefix: &str) -> io::Result<Vec<String>> {
    Ok(virsh(&["list", "--name", "--transient"], None)?
        .lines()
        .map(str::trim)
        .filter(|n| n.starts_with(prefix))
        .map(str::to_owned)
        .collect())
}
