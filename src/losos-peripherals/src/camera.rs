//! Cameras on the host, from the machines that drive them
//! (docs/peripherals.md, "Cameras").
//!
//! A camera's disposable machine has its driver and turns whatever the
//! camera gives into raw frames of one size and format (guest.rs,
//! `camera_machine`). The router makes a socket on the host for each such
//! machine (host.rs, bridges), and `cameras` gives each socket a camera
//! apps can open: a v4l2loopback device, named for the camera, which it
//! writes the machine's frames to. Only while an app is reading it, which
//! v4l2loopback says with its client usage event, is there a stream from
//! the machine at all, so the camera is on, and its light, only then.
//!
//! The device is made with v4l2loopback's exclusive capabilities, so it is
//! a capture device to apps, which is what browsers look for, once this
//! has set its format and written a frame; a black one stands in whenever
//! the camera is off.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing::{info, warn};

/// What every camera machine sends: 1280x720 YUYV at 30 frames a second,
/// a size every app takes and every camera can be scaled to.
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;
pub const FRAME: usize = (WIDTH * HEIGHT * 2) as usize;

/// What the host asks a camera machine, one line per connection.
pub const NAME: &str = "name";
pub const STREAM: &str = "stream";

/// From v4l2loopback.h: _IOW('~', 1, struct v4l2_loopback_config) and
/// _IOW('~', 2, __u32), whose argument is passed by value.
const CTL_ADD: libc::c_ulong = 0x4048_7e01;
const CTL_REMOVE: libc::c_ulong = 0x4004_7e02;
/// From linux/videodev2.h, for this 64-bit layout of their structs:
/// VIDIOC_S_FMT, VIDIOC_SUBSCRIBE_EVENT and VIDIOC_DQEVENT.
const S_FMT: libc::c_ulong = 0xc0d0_5605;
const SUBSCRIBE_EVENT: libc::c_ulong = 0x4020_565a;
const DQEVENT: libc::c_ulong = 0x8088_5659;
const BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const PIX_FMT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");
const FIELD_NONE: u32 = 1;
const COLORSPACE_SRGB: u32 = 8;
const EVENT_SUB_FL_SEND_INITIAL: u32 = 1;
/// v4l2loopback's V4L2_EVENT_PRI_CLIENT_USAGE: V4L2_EVENT_PRIVATE_START
/// plus its offset plus one. Its payload's first word is 1 while an app
/// streams from the device and 0 once none does.
const EVENT_CLIENT_USAGE: u32 = 0x0800_0000 + 0x08e0_0000 + 1;

#[repr(C)]
struct LoopbackConfig {
    output_nr: i32,
    unused: i32,
    card_label: [u8; 32],
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
    max_buffers: i32,
    max_openers: i32,
    debug: i32,
    announce_all_caps: i32,
}

/// struct v4l2_format with the v4l2_pix_format of its union, which is
/// padded to 200 bytes and aligned for the pointers of its other members.
#[repr(C)]
struct Format {
    kind: u32,
    _pad: u32,
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    rest: [u32; 5],
    _union: [u8; 200 - 48],
}

#[repr(C)]
struct Subscription {
    kind: u32,
    id: u32,
    flags: u32,
    reserved: [u32; 5],
}

#[repr(C)]
struct Event {
    kind: u32,
    _pad: u32,
    data: [u32; 16],
    pending: u32,
    sequence: u32,
    timestamp: [i64; 2],
    id: u32,
    reserved: [u32; 8],
    _end: u32,
}

const _: () = assert!(std::mem::size_of::<LoopbackConfig>() == 72);
const _: () = assert!(std::mem::size_of::<Format>() == 208);
const _: () = assert!(std::mem::size_of::<Subscription>() == 32);
const _: () = assert!(std::mem::size_of::<Event>() == 136);

fn ioctl<T>(fd: &impl AsRawFd, request: libc::c_ulong, arg: *mut T) -> io::Result<libc::c_int> {
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// A camera's name, as a machine says it, reduced to what any app shows
/// plainly and to what v4l2loopback keeps of a label.
pub fn label(name: &str) -> String {
    let plain: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || " ._()-".contains(*c))
        .collect();
    let plain = plain.trim();
    let plain = if plain.is_empty() { "Camera" } else { plain };
    plain.chars().take(31).collect()
}

/// A frame of black, which apps see while the camera is off.
pub fn black() -> Vec<u8> {
    [0x10u8, 0x80].repeat(FRAME / 2)
}

/// One v4l2loopback device, removed when this is dropped.
struct Loopback {
    control: File,
    nr: i32,
}

impl Loopback {
    fn add(label: &str) -> io::Result<Loopback> {
        let control = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/v4l2loopback")?;
        let mut config = LoopbackConfig {
            output_nr: -1,
            unused: -1,
            card_label: [0; 32],
            min_width: 0,
            max_width: 0,
            min_height: 0,
            max_height: 0,
            max_buffers: 0,
            max_openers: 0,
            debug: 0,
            // Exclusive capabilities: a capture device once it has a frame.
            announce_all_caps: 0,
        };
        config.card_label[..label.len()].copy_from_slice(label.as_bytes());
        let nr = ioctl(&control, CTL_ADD, &mut config)?;
        Ok(Loopback { control, nr })
    }

    fn path(&self) -> PathBuf {
        format!("/dev/video{}", self.nr).into()
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        let r = unsafe { libc::ioctl(self.control.as_raw_fd(), CTL_REMOVE as _, self.nr) };
        if r < 0 {
            warn!(nr = self.nr, error = %io::Error::last_os_error(), "cannot remove a camera");
        }
    }
}

fn set_format(video: &File) -> io::Result<()> {
    let mut format = Format {
        kind: BUF_TYPE_VIDEO_OUTPUT,
        _pad: 0,
        width: WIDTH,
        height: HEIGHT,
        pixelformat: PIX_FMT_YUYV,
        field: FIELD_NONE,
        bytesperline: WIDTH * 2,
        sizeimage: FRAME as u32,
        colorspace: COLORSPACE_SRGB,
        rest: [0; 5],
        _union: [0; 200 - 48],
    };
    ioctl(video, S_FMT, &mut format).map(drop)
}

fn subscribe(video: &File) -> io::Result<()> {
    let mut sub = Subscription {
        kind: EVENT_CLIENT_USAGE,
        id: 0,
        flags: EVENT_SUB_FL_SEND_INITIAL,
        reserved: [0; 5],
    };
    ioctl(video, SUBSCRIBE_EVENT, &mut sub).map(drop)
}

/// Writes one frame to the device, which is one write: v4l2loopback takes
/// each as a frame, cut to a frame's size.
fn show(mut video: &File, frame: &[u8]) -> io::Result<()> {
    video.write(frame).map(drop)
}

/// Waits up to `wait` for the usage events, and says whether an app now
/// streams from the device, or nothing if no event came.
fn used(video: &File, wait: Duration) -> io::Result<Option<bool>> {
    let mut poll = libc::pollfd {
        fd: video.as_raw_fd(),
        events: libc::POLLPRI,
        revents: 0,
    };
    let n = unsafe { libc::poll(&mut poll, 1, wait.as_millis() as libc::c_int) };
    if n < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted {
            Ok(None)
        } else {
            Err(e)
        };
    }
    let mut last = None;
    if poll.revents & libc::POLLPRI != 0 {
        // Every event queued, the newest count being the one that holds.
        // DQEVENT waits for one on a blocking file, so it stops at the
        // last, which says none are left.
        loop {
            let mut event: Event = unsafe { std::mem::zeroed() };
            ioctl(video, DQEVENT, &mut event)?;
            if event.kind == EVENT_CLIENT_USAGE {
                last = Some(event.data[0] > 0);
            }
            if event.pending == 0 {
                break;
            }
        }
    }
    Ok(last)
}

/// One request to a camera machine through its socket.
fn ask(socket: &Path, request: &str) -> io::Result<UnixStream> {
    let mut conn = UnixStream::connect(socket)?;
    conn.write_all(format!("{request}\n").as_bytes())?;
    // A machine that stops sending is gone, or its camera is.
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    Ok(conn)
}

/// One camera: its device for as long as its machine's socket is there,
/// streaming while an app uses it.
fn camera(socket: &Path, stop: &AtomicBool) -> io::Result<()> {
    let mut name = String::new();
    BufReader::new(ask(socket, NAME)?).read_line(&mut name)?;
    let label = label(&name);
    let device = Loopback::add(&label)?;
    let video = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device.path())?;
    set_format(&video)?;
    let black = black();
    show(&video, &black)?;
    subscribe(&video)?;
    info!(socket = %socket.display(), device = %device.path().display(), label, "a camera");

    let mut wanted = false;
    let mut stream: Option<UnixStream> = None;
    let mut frame = vec![0u8; FRAME];
    while !stop.load(Ordering::Relaxed) {
        // While streaming, only a look at the events between frames.
        let wait = if stream.is_some() {
            Duration::ZERO
        } else {
            Duration::from_millis(500)
        };
        if let Some(now) = used(&video, wait)? {
            wanted = now;
        }
        match (&mut stream, wanted) {
            (None, true) => match ask(socket, STREAM) {
                Ok(conn) => {
                    info!(label, "the camera is on");
                    stream = Some(conn);
                }
                Err(e) => {
                    warn!(label, error = %e, "cannot reach the camera");
                    thread::sleep(Duration::from_secs(1));
                }
            },
            (Some(_), false) => {
                // Hanging up is what turns the camera off in its machine.
                stream = None;
                show(&video, &black)?;
                info!(label, "the camera is off");
            }
            (Some(conn), true) => match conn.read_exact(&mut frame) {
                Ok(()) => show(&video, &frame)?,
                Err(e) => {
                    warn!(label, error = %e, "the camera's stream ended");
                    stream = None;
                    show(&video, &black)?;
                    thread::sleep(Duration::from_secs(1));
                }
            },
            (None, false) => {}
        }
    }
    info!(label, "the camera is gone");
    Ok(())
}

/// A camera's thread, stopped and waited for when this is dropped.
struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    fn start(socket: PathBuf) -> Running {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            thread::spawn(move || {
                if let Err(e) = camera(&socket, &stop) {
                    warn!(socket = %socket.display(), error = %e, "no camera for this machine");
                }
            })
        };
        Running {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn sockets(dir: &Path) -> HashSet<String> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_socket()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

/// The host's cameras: one for each camera machine's socket in `dir`, made
/// as the socket comes and removed as it goes. The router clears and makes
/// `dir` again as it starts, so it is looked at every second rather than
/// watched.
pub fn cameras(dir: &Path) -> io::Result<()> {
    // Devices left from before a restart, which only this makes, as the
    // module makes none: v4l2loopback's are the virtual video devices.
    let control = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/v4l2loopback")?;
    for e in fs::read_dir("/sys/devices/virtual/video4linux")
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name();
        let Some(nr) = name
            .to_str()
            .and_then(|n| n.strip_prefix("video")?.parse::<i32>().ok())
        else {
            continue;
        };
        drop(Loopback {
            control: control.try_clone()?,
            nr,
        });
    }
    let mut running: HashMap<String, Running> = HashMap::new();
    loop {
        let present = sockets(dir);
        running.retain(|name, _| present.contains(name));
        for name in present {
            running
                .entry(name)
                .or_insert_with_key(|name| Running::start(dir.join(name)));
        }
        thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_plain_and_fit_v4l2loopback() {
        assert_eq!(label("LosOS Test Camera\n"), "LosOS Test Camera");
        assert_eq!(label("Caméra\tHD\u{1b}\n"), "CamraHD");
        assert_eq!(label("\n"), "Camera");
        assert_eq!(label(&"x".repeat(40)).len(), 31);
    }

    #[test]
    fn a_black_frame_is_one_frame_of_yuyv_black() {
        let black = black();
        assert_eq!(black.len(), FRAME);
        assert_eq!(&black[..4], &[0x10, 0x80, 0x10, 0x80]);
    }
}
