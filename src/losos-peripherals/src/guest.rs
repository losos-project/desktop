//! The machines' halves.
//!
//! The USB machine (`usb-machine`) holds the controllers passed through to
//! it, and says what is plugged into them: whoever connects to its event
//! port is sent a line for every device there now and every one that comes
//! or goes after. When the router has decided where a device goes, it asks
//! the export port for it and gets the device's USB/IP stream.
//!
//! Every machine that is given devices (`receiver`) plugs them into its
//! virtual controller when the router connects to its attach port with a
//! stream from the USB machine behind it.
//!
//! A disposable machine that is NixOS rather than BusyBox, for a device
//! that needs a whole stack such as a sound card, also lets the host reach
//! that stack (`forward`): each connection to a port of its own is joined
//! to a local socket, such as its sound server's.
//!
//! A camera's machine (`camera-machine`) says its camera's name, and
//! streams it, to the host, which makes a camera of it apps can use
//! (camera.rs).
//!
//! All of them listen on vsock, which only the host can reach, and only the
//! host's root at that (vsock.rs).

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;

use crossbeam_channel::{bounded, select};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::camera;
use crate::splice::splice;
use crate::uevent;
use crate::usb::{is_busid, Device};
use crate::usbip;
use crate::vsock::Listener;

/// The USB machine's ports: devices' streams, and what is plugged in.
pub const EXPORT: u32 = 3240;
pub const EVENTS: u32 = 3241;
/// A receiving machine's.
pub const ATTACH: u32 = 3242;
/// A sound card's machine's sound server.
pub const SOUND: u32 = 3244;
/// A camera's machine's camera (camera.rs).
pub const CAMERA: u32 = 3246;

/// One line on the event port.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Report {
    Add(Device),
    Remove(String),
}

fn devices() -> impl Iterator<Item = Device> {
    fs::read_dir("/sys/bus/usb/devices")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Device::read(&e.path()).ok())
}

fn send(to: &mut File, report: &Report) -> io::Result<()> {
    let mut line = serde_json::to_vec(report).map_err(io::Error::other)?;
    line.push(b'\n');
    to.write_all(&line)
}

/// The USB machine: reports its devices, and exports them on request.
pub fn usb_machine() -> io::Result<()> {
    let events = Listener::bind(EVENTS)?;
    let export = Listener::bind(EXPORT)?;
    let kernel = uevent::Socket::open()?;
    let (reports, reported) = bounded::<Report>(64);
    let (subscribers, subscribed) = bounded::<File>(4);

    thread::scope(|s| {
        // The kernel's events, as reports.
        s.spawn(move || loop {
            let ev = match kernel.next() {
                Ok(ev) => ev,
                Err(e) => {
                    warn!(error = %e, "cannot read device events");
                    return;
                }
            };
            if ev.get("SUBSYSTEM") != "usb" || ev.get("DEVTYPE") != "usb_device" {
                continue;
            }
            let path = Path::new("/sys").join(ev.get("DEVPATH").trim_start_matches('/'));
            let Some(busid) = path
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| is_busid(n))
            else {
                continue;
            };
            match ev.get("ACTION") {
                // Read on a thread of its own: the kernel holds a device's
                // lock while it is being set up, and its name waits for
                // that, so one device slow to come up must not keep the
                // ones after it from being seen.
                "add" => {
                    let reports = reports.clone();
                    let busid = busid.to_owned();
                    s.spawn(move || match Device::read(&path) {
                        Ok(d) => {
                            info!(busid, name = %d.name, "plugged in");
                            let _ = reports.send(Report::Add(d));
                        }
                        Err(e) => warn!(busid, error = %e, "cannot read a new device"),
                    });
                }
                "remove" => {
                    info!(busid, "pulled out");
                    if reports.send(Report::Remove(busid.to_owned())).is_err() {
                        return;
                    }
                }
                _ => {}
            }
        });

        // Who listens: the router, normally once, again after it restarts.
        s.spawn(move || loop {
            match events.accept() {
                Ok(conn) => {
                    if subscribers.send(conn).is_err() {
                        return;
                    }
                }
                Err(e) => warn!(error = %e, "cannot take a connection"),
            }
        });

        s.spawn(move || {
            let mut listening: Vec<File> = Vec::new();
            loop {
                select! {
                    recv(subscribed) -> conn => {
                        let Ok(mut conn) = conn else { return };
                        if devices().try_for_each(|d| send(&mut conn, &Report::Add(d))).is_ok() {
                            info!("the router is listening");
                            listening.push(conn);
                        }
                    }
                    recv(reported) -> report => {
                        let Ok(report) = report else { return };
                        listening.retain_mut(|conn| send(conn, &report).is_ok());
                    }
                }
            }
        });

        // Requests for a device's stream, one connection each.
        loop {
            let mut conn = match export.accept() {
                Ok(conn) => conn,
                Err(e) => {
                    warn!(error = %e, "cannot take a connection");
                    continue;
                }
            };
            s.spawn(move || {
                if let Err(e) = serve_export(&mut conn) {
                    warn!(error = %e, "cannot export a device");
                }
            });
        }
    })
}

fn serve_export(conn: &mut File) -> io::Result<()> {
    let line = usbip::read_line(conn)?;
    let busid = line
        .strip_prefix("export ")
        .filter(|b| is_busid(b))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, format!("not a request: {line}"))
        })?;
    let device = Device::read(&Path::new("/sys/bus/usb/devices").join(busid))?;
    usbip::export(busid, device.devid(), device.speed_code(), conn)?;
    info!(busid, name = %device.name, "exported");
    Ok(())
}

/// A machine that is given devices: plugs each stream it is sent into its
/// virtual controller.
pub fn receiver() -> io::Result<()> {
    let attach = Listener::bind(ATTACH)?;
    loop {
        let mut conn = match attach.accept() {
            Ok(conn) => conn,
            Err(e) => {
                warn!(error = %e, "cannot take a connection");
                continue;
            }
        };
        let attached = usbip::read_line(&mut conn).and_then(|line| {
            let mut words = line.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (Some("attach"), Some(devid), Some(speed)) => {
                    let parse = |w: &str| {
                        w.parse::<u32>()
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
                    };
                    usbip::attach(&conn, parse(devid)?, parse(speed)?)
                }
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("not a request: {line}"),
                )),
            }
        });
        // The kernel holds the stream now, if it took it; this end closes.
        match attached {
            Ok(port) => info!(port, "plugged in a device"),
            Err(e) => warn!(error = %e, "cannot plug in a device"),
        }
    }
}

/// The Bluetooth machine's command port: `bluetoothctl`, on the host's
/// behalf.
pub const COMMAND: u32 = 3243;

/// A command on the command port: bluetoothctl's arguments.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub args: Vec<String>,
}

/// What it did: its exit status and its output.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Answer {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The Bluetooth machine: BlueZ with its system bus, kept running, the
/// receiver for its radio, and the command port the host's bluetoothctl
/// reaches it through. It is PID 1, so it starts the daemons itself and
/// starts each again if it stops.
pub fn bluetooth_machine(bus_config: &Path) -> io::Result<()> {
    let commands = Listener::bind(COMMAND)?;
    let config = format!("--config-file={}", bus_config.display());
    thread::scope(|s| {
        s.spawn(|| supervise(&["dbus-daemon", "--nofork", "--nopidfile", &config]));
        s.spawn(|| {
            // bluetoothd wants the bus up; it is the first thing it opens.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !Path::new("/run/dbus/system_bus_socket").exists()
                && std::time::Instant::now() < deadline
            {
                thread::sleep(std::time::Duration::from_millis(100));
            }
            supervise(&["bluetoothd", "--nodetach"]);
        });
        s.spawn(|| {
            if let Err(e) = receiver() {
                warn!(error = %e, "the receiver stopped");
            }
        });
        loop {
            let conn = match commands.accept() {
                Ok(conn) => conn,
                Err(e) => {
                    warn!(error = %e, "cannot take a connection");
                    continue;
                }
            };
            s.spawn(move || {
                if let Err(e) = serve_command(conn) {
                    warn!(error = %e, "cannot answer a command");
                }
            });
        }
    })
}

/// Runs `argv` again whenever it stops, a second later.
fn supervise(argv: &[&str]) {
    loop {
        match std::process::Command::new(argv[0])
            .args(&argv[1..])
            .status()
        {
            Ok(status) => warn!(program = argv[0], %status, "stopped; starting it again"),
            Err(e) => warn!(program = argv[0], error = %e, "cannot start; trying again"),
        }
        thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn serve_command(conn: File) -> io::Result<()> {
    let mut reader = io::BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    io::BufRead::read_line(&mut reader, &mut line)?;
    let request: Request = serde_json::from_str(&line).map_err(io::Error::other)?;
    let answer = match std::process::Command::new("bluetoothctl")
        .args(&request.args)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(out) => Answer {
            status: out.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => Answer {
            status: 127,
            stderr: format!("bluetoothctl: {e}\n"),
            ..Default::default()
        },
    };
    let mut conn = conn;
    let mut out = serde_json::to_vec(&answer).map_err(io::Error::other)?;
    out.push(b'\n');
    conn.write_all(&out)
}

/// A service of a disposable machine's that the host reaches: every
/// connection to `port` joined to the local socket `to` for as long as
/// both ends keep it.
pub fn forward(port: u32, to: &Path) -> io::Result<()> {
    let listener = Listener::bind(port)?;
    info!(port, to = %to.display(), "forwarding");
    thread::scope(|s| loop {
        let conn = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                warn!(error = %e, "cannot take a connection");
                continue;
            }
        };
        s.spawn(move || match UnixStream::connect(to) {
            Ok(local) => splice(&conn, &local),
            Err(e) => warn!(to = %to.display(), error = %e, "cannot reach the service"),
        });
    })
}

/// The camera a camera's machine has: the first video device that is its
/// driver's first, uvcvideo's second being the camera's metadata.
fn capture_device() -> Option<(String, String)> {
    let mut nodes: Vec<_> = fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    nodes.sort();
    nodes.into_iter().find_map(|node| {
        let index = fs::read_to_string(node.join("index")).ok()?;
        (index.trim() == "0").then_some(())?;
        // The camera's own product name; uvcvideo's for the node adds
        // the name of the camera's entity to it.
        let name = fs::read_to_string(node.join("device/../product"))
            .or_else(|_| fs::read_to_string(node.join("name")))
            .ok()?;
        let dev = format!("/dev/{}", node.file_name()?.to_str()?);
        Some((dev, name.trim().to_owned()))
    })
}

/// A camera's machine: once its camera is plugged in, says the camera's
/// name to whoever asks, and streams it to whoever asks for that until they
/// hang up, ffmpeg turning whatever the camera gives into camera.rs' one
/// size and format. Nothing reads the camera otherwise.
pub fn camera_machine() -> io::Result<()> {
    while capture_device().is_none() {
        thread::sleep(std::time::Duration::from_secs(1));
    }
    let listener = Listener::bind(CAMERA)?;
    info!(port = CAMERA, "a camera to stream");
    thread::scope(|s| loop {
        let conn = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                warn!(error = %e, "cannot take a connection");
                continue;
            }
        };
        s.spawn(move || {
            if let Err(e) = answer_camera(conn) {
                warn!(error = %e, "cannot answer for the camera");
            }
        });
    })
}

fn answer_camera(mut conn: File) -> io::Result<()> {
    let Some((dev, name)) = capture_device() else {
        return Err(io::Error::other("the camera is gone"));
    };
    // The host sends its one line and waits, so nothing past it is lost.
    let mut line = String::new();
    io::BufRead::read_line(&mut io::BufReader::new(&conn), &mut line)?;
    match line.trim() {
        camera::NAME => conn.write_all(format!("{name}\n").as_bytes()),
        camera::STREAM => {
            let (w, h) = (camera::WIDTH, camera::HEIGHT);
            // Scaled to fit, the rest black, at a steady rate.
            let filter = format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,format=yuyv422"
            );
            info!(dev, "streaming");
            // ffmpeg writes the connection itself, and stops when the host
            // hangs up.
            let status = std::process::Command::new("ffmpeg")
                .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
                .args(["-f", "v4l2", "-i", &dev, "-vf", &filter])
                .args(["-r", &camera::FPS.to_string(), "-f", "rawvideo", "-"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::from(std::os::fd::OwnedFd::from(conn)))
                .status()?;
            info!(dev, %status, "the stream ended");
            Ok(())
        }
        other => Err(io::Error::other(format!("no such request: {other}"))),
    }
}
