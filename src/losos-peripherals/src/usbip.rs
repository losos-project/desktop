//! USB/IP's kernel halves, given vsock streams instead of TCP.
//!
//! USB/IP is a device's USB traffic over a stream: `usbip-host` on the side
//! the device is plugged into, `vhci-hcd`'s virtual controller on the side
//! that uses it. Both take the stream as a socket descriptor written to
//! sysfs, and want only that it is a stream (drivers/usb/usbip,
//! `SOCK_STREAM`), which a vsock socket is. usbip's own programs add a
//! handshake over TCP to find the device and its speed; here the router
//! already knows both from the USB machine's report, so a one-line request
//! stands in for it and the kernel takes the socket straight after.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

const STUB: &str = "/sys/bus/usb/drivers/usbip-host";
const VHCI: &str = "/sys/devices/platform/vhci_hcd.0";

/// Reads one line, a byte at a time so nothing after it is taken from the
/// stream the kernel reads next. A line longer than a request ever is
/// ends the connection.
pub fn read_line(conn: &mut File) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8];
    loop {
        if conn.read(&mut byte)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > 256 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
    String::from_utf8(line).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}

fn write(path: impl AsRef<Path>, value: &str) -> io::Result<()> {
    fs::write(path.as_ref(), value)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.as_ref().display())))
}

/// Gives `busid` to usbip-host, as `usbip bind` does: listed for the stub,
/// taken from whatever driver had it, and bound.
pub fn bind(busid: &str) -> io::Result<()> {
    let dev = Path::new("/sys/bus/usb/devices").join(busid);
    if fs::read_link(dev.join("driver"))
        .ok()
        .is_some_and(|d| d.ends_with("usbip-host"))
    {
        return Ok(());
    }
    write(format!("{STUB}/match_busid"), &format!("add {busid}"))?;
    if dev.join("driver").exists() {
        write(dev.join("driver/unbind"), busid)?;
    }
    write(format!("{STUB}/bind"), busid)
}

/// Exports `busid` over `conn`: binds it, answers `ok <devid> <speed>`,
/// and hands the stream to the stub.
pub fn export(busid: &str, devid: u32, speed: u32, conn: &mut File) -> io::Result<()> {
    bind(busid)?;
    let dev = Path::new("/sys/bus/usb/devices").join(busid);
    writeln!(conn, "ok {devid} {speed}")?;
    write(dev.join("usbip_sockfd"), &conn.as_raw_fd().to_string())
}

/// A free port of the virtual controller for a device of `speed`: its
/// SuperSpeed hub for 5 and up, its high-speed hub for the rest. vhci
/// numbers ports across all its controllers and lists them in `status`,
/// `status.1` and on, each line `hub port sta ...`, 4 being free.
pub fn free_port(vhci: &Path, speed: u32) -> io::Result<u32> {
    let hub = if speed >= 5 { "ss" } else { "hs" };
    let mut files: Vec<_> = fs::read_dir(vhci)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "status" || n.starts_with("status."))
        })
        .collect();
    files.sort();
    for file in files {
        for line in fs::read_to_string(&file)?.lines().skip(1) {
            let mut cols = line.split_whitespace();
            let (Some(h), Some(port), Some(sta)) = (cols.next(), cols.next(), cols.next()) else {
                continue;
            };
            if h == hub && sta.parse::<u32>().ok() == Some(4) {
                if let Ok(port) = port.parse() {
                    return Ok(port);
                }
            }
        }
    }
    Err(io::Error::other(
        "the virtual USB controller has no free port",
    ))
}

/// Plugs the device at the other end of `conn` into this system's virtual
/// controller, as `usbip attach` does once it has shaken hands.
pub fn attach(conn: &File, devid: u32, speed: u32) -> io::Result<u32> {
    // vhci takes SuperSpeed+ on its SuperSpeed hub, as SuperSpeed.
    let speed = speed.min(5);
    let port = free_port(Path::new(VHCI), speed)?;
    write(
        format!("{VHCI}/attach"),
        &format!("{port} {} {devid} {speed}", conn.as_raw_fd()),
    )?;
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_superspeed_device_takes_a_superspeed_port() {
        let dir = std::env::temp_dir().join(format!("losos-vhci-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("status"),
            "hub port sta spd dev      sockfd local_busid\n\
             hs  0000 006 003 00010002 000003 1-2\n\
             hs  0001 004 000 00000000 000000 0-0\n\
             ss  0008 006 005 00010003 000004 1-3\n\
             ss  0009 004 000 00000000 000000 0-0\n",
        )
        .unwrap();
        assert_eq!(free_port(&dir, 3).unwrap(), 1);
        assert_eq!(free_port(&dir, 5).unwrap(), 9);
        fs::remove_dir_all(&dir).unwrap();
    }
}
