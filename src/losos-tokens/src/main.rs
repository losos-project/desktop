//! Security keys and smart card readers stay plugged into the host, and
//! the machines use them through it (docs/tokens.md).
//!
//!     losos-tokens host [--pcscd PATH] [--loopback-machine NAME] [--forward PORT]...
//!         relay the host's keys and pcscd to the machines (the host)
//!     losos-tokens guest [--pcsc-socket PATH] [--host-cid CID] [--serve PORT]... [--localhost ADDR]
//!         make the host's keys and readers this machine's (a machine)
//!
//! A FIDO key reaches a machine as a copy: `guest` makes a uhid device for
//! each key the host has, and the host passes CTAPHID reports between that
//! copy and the real key's hidraw device. Every program in the machine, a
//! browser, libfido2 or ssh, sees a key on USB. The host reads just enough
//! CTAPHID to keep each machine on the channels it opened, so no machine
//! sees another's credentials or the host's own sign-in.
//!
//! A smart card reader is the host's pcscd's. `guest` listens where
//! programs look for pcscd, and passes each connection over vsock to `host`,
//! which passes it on to pcscd. One machine holds the readers at a time
//! (`Lease`).
//!
//! A program on a machine's localhost that a page hands work to, such as
//! the eID client a sign-in page redirects to, is reachable from the
//! host's localhost too: `guest --serve PORT` tells the host while
//! something listens on PORT, and `host --forward PORT` listens there and
//! passes each connection to the machine that serves it (`Services`).
//!
//! The host knows which machine a connection is from by the vsock address
//! the kernel stamps on it, never by anything the machine says, and asks
//! libvirt which running machine has that address.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;
use vsock::{VsockAddr, VsockListener, VsockStream, VMADDR_CID_ANY, VMADDR_CID_HOST};

use losos_tokens::{
    domain_cid, free_key, is_fido_descriptor, listening, parse_hid_uevent, uhid, Frame, Lease,
    Router, Services, KEYS_PORT, LOCALHOST_PORT, RELAYED_PHYS, REPORT_LEN, SMART_CARDS_PORT,
};

/// Where pcscd listens, on the host and, for programs, in a machine.
const PCSCD_SOCKET: &str = "/run/pcscd/pcscd.comm";
/// How often the host looks for keys plugged in or pulled out. A key is
/// touched by hand, so a second is soon enough.
const SCAN_EVERY: Duration = Duration::from_secs(1);
/// How long a machine waits before it tries the host again.
const RETRY_AFTER: Duration = Duration::from_secs(2);
/// vsock's address for "this kernel", which a test uses for both ends.
const VMADDR_CID_LOCAL: u32 = 1;
/// Reports the router keeps for one machine before it drops them: a slow
/// machine loses its own conversation and nobody else's.
const QUEUE: usize = 256;
/// How often a machine looks at what listens on the ports it serves. An
/// app takes longer than that to start and be clicked through.
const LOOK_EVERY: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    // stderr, which is the journal for a service.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .without_time()
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("host") => host_options(&args[1..]).and_then(host),
        Some("guest") => guest_options(&args[1..]).and_then(|o| guest(&o)),
        _ => Err(miette!(
            "usage: losos-tokens host [--pcscd PATH] [--loopback-machine NAME] [--forward PORT]... | guest [--pcsc-socket PATH] [--host-cid CID] [--serve PORT]... [--localhost ADDR]"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(report) => {
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

struct HostOptions {
    pcscd: PathBuf,
    /// A name for connections from this kernel itself, which only
    /// nixos/tests/tokens.nix gives: it runs both ends on one machine.
    loopback: Option<String>,
    /// Localhost ports passed on to the machine that serves each.
    forward: Vec<u16>,
}

fn host_options(args: &[String]) -> Result<HostOptions> {
    let mut options = HostOptions {
        pcscd: PathBuf::from(PCSCD_SOCKET),
        loopback: None,
        forward: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| miette!("{flag} needs a value"))?;
        match flag.as_str() {
            "--pcscd" => options.pcscd = PathBuf::from(value),
            "--loopback-machine" => options.loopback = Some(value.clone()),
            "--forward" => options.forward.push(port(value)?),
            _ => bail!("unknown option {flag}"),
        }
    }
    Ok(options)
}

struct GuestOptions {
    pcsc_socket: PathBuf,
    /// The host's vsock address: VMADDR_CID_HOST, or this kernel's own
    /// when nixos/tests/tokens.nix runs both ends on one machine.
    host_cid: u32,
    /// Localhost ports the host may reach while something here listens.
    serve: Vec<u16>,
    /// The address those ports are reached on: 127.0.0.1, or another
    /// loopback address where nixos/tests/tokens.nix runs the host's end,
    /// with its own listener on 127.0.0.1, on the same kernel.
    localhost: Ipv4Addr,
}

fn guest_options(args: &[String]) -> Result<GuestOptions> {
    let mut options = GuestOptions {
        pcsc_socket: PathBuf::from(PCSCD_SOCKET),
        host_cid: VMADDR_CID_HOST,
        serve: Vec::new(),
        localhost: Ipv4Addr::LOCALHOST,
    };
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| miette!("{flag} needs a value"))?;
        match flag.as_str() {
            "--pcsc-socket" => options.pcsc_socket = PathBuf::from(value),
            "--host-cid" => {
                options.host_cid = value
                    .parse()
                    .map_err(|_| miette!("--host-cid takes a number"))?
            }
            "--serve" => options.serve.push(port(value)?),
            "--localhost" => {
                options.localhost = value
                    .parse()
                    .ok()
                    .filter(Ipv4Addr::is_loopback)
                    .ok_or_else(|| miette!("--localhost takes a loopback address"))?
            }
            _ => bail!("unknown option {flag}"),
        }
    }
    Ok(options)
}

fn port(value: &str) -> Result<u16> {
    value
        .parse()
        .ok()
        .filter(|&p| p != 0)
        .ok_or_else(|| miette!("{value} is not a port"))
}

/// A machine, as the host reaches it: its name and vsock address.
#[derive(Clone, Debug)]
struct Machine {
    name: String,
    cid: u32,
}

// The host.

/// What the router thread hears about.
enum Event {
    KeyAdded {
        key: u8,
        vendor: u32,
        product: u32,
        name: String,
        device: File,
    },
    KeyGone(u8),
    FromKey(u8, [u8; REPORT_LEN]),
    MachineJoined(u64, String, Sender<Frame>),
    MachineGone(u64),
    FromMachine(u64, u8, [u8; REPORT_LEN]),
}

fn host(options: HostOptions) -> Result<()> {
    let listen = |port: u32| {
        VsockListener::bind(&VsockAddr::new(VMADDR_CID_ANY, port))
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot listen on vsock port {port}"))
    };
    let (keys, cards) = (listen(KEYS_PORT)?, listen(SMART_CARDS_PORT)?);
    info!(
        keys = KEYS_PORT,
        smart_cards = SMART_CARDS_PORT,
        "relaying security keys and smart card readers to the machines"
    );
    let (events, heard) = bounded::<Event>(QUEUE);
    let lease = Mutex::new(Lease::default());
    let options = &options;
    std::thread::scope(|scope| {
        scope.spawn(move || route(heard));
    let forwards = options
        .forward
        .iter()
        .map(|&port| {
            TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .into_diagnostic()
                .wrap_err_with(|| format!("cannot listen on localhost port {port}"))
                .map(|listener| (port, listener))
        })
        .collect::<Result<Vec<_>>>()?;
    let (events, heard) = bounded::<Event>(QUEUE);
    let lease = Mutex::new(Lease::default());
    let services = Mutex::new(Services::<Machine>::default());
    let (options, services) = (&options, &services);
    std::thread::scope(|scope| {
        scope.spawn(move || route(heard));
        for (port, listener) in forwards {
            info!(
                port,
                "passing localhost connections to the machine that serves them"
            );
            scope.spawn(move || forward(scope, port, listener, services));
        }
        scope.spawn({
            let events = events.clone();
            move || watch_keys(scope, events)
        });
        scope.spawn(|| {
            let mut next = 0u64;
            for stream in keys.incoming() {
                let stream = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "cannot accept");
                        continue;
                    }
                };
                next += 1;
                let (id, events) = (next, events.clone());
                scope.spawn(move || {
                    if let Err(e) = serve_keys(id, stream, events, options, services) {
                        warn!("{e:?}");
                    }
                });
            }
        });
        for stream in cards.incoming() {
            match stream {
                Ok(stream) => {
                    let lease = &lease;
                    scope.spawn(move || {
                        if let Err(e) = serve_cards(stream, lease, options) {
                            warn!("{e:?}");
                        }
                    });
                }
                Err(e) => warn!(error = %e, "cannot accept"),
            }
        }
    });
    Ok(())
}

/// The name of the running machine at vsock address `cid`, which libvirt
/// gave it.
fn machine_at(cid: u32, options: &HostOptions) -> Result<Option<String>> {
    if cid == VMADDR_CID_LOCAL {
        return Ok(options.loopback.clone());
    }
    for name in virsh(&["list", "--name"])?
        .lines()
        .filter(|n| !n.is_empty())
    {
        match virsh(&["dumpxml", name]) {
            Ok(xml) if domain_cid(&xml) == Some(cid) => return Ok(Some(name.to_owned())),
            Ok(_) => {}
            // It stopped between the two calls.
            Err(e) => debug!(%name, error = ?e, "skipped"),
        }
    }
    Ok(None)
}

/// virsh on a read-only connection, which is all naming a machine needs.
fn virsh(args: &[&str]) -> Result<String> {
    let output = Command::new("virsh")
        .args(["-r", "-q"])
        .args(args)
        .output()
        .into_diagnostic()
        .wrap_err("cannot run virsh")?;
    if !output.status.success() {
        bail!(
            "virsh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Decides where every report goes and tells machines about keys. The only
/// thread that writes to a key or to a machine's queue.
fn route(heard: Receiver<Event>) {
    struct Key {
        vendor: u32,
        product: u32,
        name: String,
        device: File,
    }
    let mut keys: BTreeMap<u8, Key> = BTreeMap::new();
    let mut machines: BTreeMap<u64, (String, Sender<Frame>)> = BTreeMap::new();
    let mut router = Router::<u64>::new();
    let send = |machines: &BTreeMap<u64, (String, Sender<Frame>)>, id: u64, frame: Frame| {
        if let Some((name, queue)) = machines.get(&id) {
            if let Err(TrySendError::Full(_)) = queue.try_send(frame) {
                warn!(machine = %name, "dropped a report: the machine is not reading");
            }
        }
    };
    let add = |key: u8, k: &Key| Frame::Add {
        key,
        vendor: k.vendor,
        product: k.product,
        name: k.name.clone(),
    };
    for event in heard {
        match event {
            Event::KeyAdded {
                key,
                vendor,
                product,
                name,
                device,
            } => {
                info!(key, %name, "security key plugged in");
                let k = Key {
                    vendor,
                    product,
                    name,
                    device,
                };
                for &id in machines.keys() {
                    send(&machines, id, add(key, &k));
                }
                keys.insert(key, k);
            }
            Event::KeyGone(key) => {
                if let Some(k) = keys.remove(&key) {
                    info!(key, name = %k.name, "security key pulled out");
                }
                router.key_gone(key);
                for &id in machines.keys() {
                    send(&machines, id, Frame::Remove { key });
                }
            }
            Event::FromKey(key, report) => {
                if let Some(id) = router.from_key(key, &report) {
                    send(&machines, id, Frame::Report { key, report });
                }
            }
            Event::MachineJoined(id, name, queue) => {
                info!(machine = %name, "machine connected for security keys");
                machines.insert(id, (name, queue));
                for (&key, k) in &keys {
                    send(&machines, id, add(key, k));
                }
            }
            Event::MachineGone(id) => {
                if let Some((name, _)) = machines.remove(&id) {
                    info!(machine = %name, "machine hung up");
                }
                router.machine_gone(&id);
            }
            Event::FromMachine(id, key, report) => {
                let Some(k) = keys.get_mut(&key) else {
                    continue;
                };
                if !router.from_machine(&id, key, &report) {
                    if let Some((name, _)) = machines.get(&id) {
                        warn!(machine = %name, key, "refused a report on a channel that is not its own");
                    }
                    continue;
                }
                if let Some((name, _)) = machines.get(&id) {
                    if losos_tokens::ctaphid::init_nonce(&report).is_some() {
                        info!(machine = %name, key = %k.name, "machine is using the security key");
                    }
                }
                // hidraw takes the report number first, 0 on a FIDO key.
                let mut out = [0u8; REPORT_LEN + 1];
                out[1..].copy_from_slice(&report);
                if let Err(e) = k.device.write_all(&out) {
                    warn!(key, error = %e, "cannot write to the security key");
                }
            }
        }
    }
}

/// Watches /sys/class/hidraw for FIDO keys, opens each one that appears and
/// reads its reports on a thread of its own, and says when one goes.
fn watch_keys<'scope>(scope: &'scope std::thread::Scope<'scope, '_>, events: Sender<Event>) {
    let mut open: BTreeMap<String, u8> = BTreeMap::new();
    loop {
        let present = fido_hidraws();
        for gone in open
            .keys()
            .filter(|dev| !present.contains_key(*dev))
            .cloned()
            .collect::<Vec<_>>()
        {
            let key = open.remove(&gone).unwrap();
            let _ = events.send(Event::KeyGone(key));
        }
        for (dev, info) in present {
            if open.contains_key(&dev) {
                continue;
            }
            let used: BTreeSet<u8> = open.values().copied().collect();
            let Some(key) = free_key(&used) else {
                warn!(%dev, "too many security keys");
                continue;
            };
            let path = Path::new("/dev").join(&dev);
            let device = match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(d) => d,
                Err(e) => {
                    // udev may not have made the node yet; next time.
                    debug!(path = %path.display(), error = %e, "cannot open yet");
                    continue;
                }
            };
            let reader = match device.try_clone() {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "cannot clone");
                    continue;
                }
            };
            open.insert(dev.clone(), key);
            let _ = events.send(Event::KeyAdded {
                key,
                vendor: info.vendor,
                product: info.product,
                name: info.name,
                device,
            });
            let events = events.clone();
            scope.spawn(move || read_key(key, reader, events));
        }
        std::thread::sleep(SCAN_EVERY);
    }
}

/// The hidraw devices that are FIDO keys, by name (`hidraw3`), and what
/// their HID parent says about them. The machines' own copies, which a host
/// that is itself a machine would see, are left out.
fn fido_hidraws() -> BTreeMap<String, losos_tokens::HidInfo> {
    let mut found = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
        return found;
    };
    for entry in entries.flatten() {
        let device = entry.path().join("device");
        let Ok(descriptor) = std::fs::read(device.join("report_descriptor")) else {
            continue;
        };
        if !is_fido_descriptor(&descriptor) {
            continue;
        }
        let info =
            parse_hid_uevent(&std::fs::read_to_string(device.join("uevent")).unwrap_or_default());
        if info.phys == RELAYED_PHYS {
            continue;
        }
        found.insert(entry.file_name().to_string_lossy().into_owned(), info);
    }
    found
}

/// Reads `key`'s reports until it is pulled out.
fn read_key(key: u8, mut device: File, events: Sender<Event>) {
    let mut report = [0u8; REPORT_LEN];
    loop {
        match device.read(&mut report) {
            Ok(REPORT_LEN) => {
                if events.send(Event::FromKey(key, report)).is_err() {
                    return;
                }
            }
            Ok(n) => debug!(key, n, "short report"),
            // ENODEV once it is pulled out; watch_keys says so.
            Err(e) => {
                debug!(key, error = %e, "stopped reading");
                return;
            }
        }
    }
}

/// One machine's connection for keys: frames in become reports for the
/// router, and the router's frames go out.
fn serve_keys(
    id: u64,
    stream: VsockStream,
    events: Sender<Event>,
    options: &HostOptions,
    services: &Mutex<Services<Machine>>,
) -> Result<()> {
    let cid = stream.peer_addr().into_diagnostic()?.cid();
    let Some(name) = machine_at(cid, options)? else {
        info!(cid, "refused: no running machine has this address");
        return Ok(());
    };
    let (queue, outgoing) = bounded::<Frame>(QUEUE);
    events
        .send(Event::MachineJoined(id, name.clone(), queue))
        .into_diagnostic()?;
    let mut writer = stream.try_clone().into_diagnostic()?;
    let mut reader = stream;
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for frame in outgoing {
                if writer.write_all(&frame.encode()).is_err() {
                    break;
                }
            }
            let _ = writer.shutdown(Shutdown::Both);
        });
        loop {
            match Frame::read(&mut reader) {
                Ok(Some(Frame::Report { key, report })) => {
                    let _ = events.send(Event::FromMachine(id, key, report));
                }
                Ok(Some(Frame::Serves { port })) => {
                    info!(machine = %name, port, "the machine serves a localhost port");
                    let machine = Machine {
                        name: name.clone(),
                        cid,
                    };
                    services.lock().unwrap().serve(id, machine, port);
                }
                Ok(Some(Frame::Stopped { port })) => {
                    info!(machine = %name, port, "the machine no longer serves a localhost port");
                    services.lock().unwrap().stop(id, port);
                }
                Ok(Some(other)) => {
                    warn!(machine = %name, frame = ?other, "a machine cannot say that");
                    break;
                }
                Ok(None) => break,
                Err(e) => {
                    warn!(machine = %name, error = %e, "connection ended");
                    break;
                }
            }
        }
        // Dropping the router's sender ends the writer.
        let _ = events.send(Event::MachineGone(id));
        services.lock().unwrap().gone(id);
    });
    Ok(())
}

/// Passes each connection to the host's localhost `port` to the machine
/// that serves it, which hears the port first and then the connection.
fn forward<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    port: u16,
    listener: TcpListener,
    services: &'scope Mutex<Services<Machine>>,
) {
    for client in listener.incoming() {
        let Ok(client) = client else { continue };
        let Some(machine) = services.lock().unwrap().who(port) else {
            info!(port, "nothing in a machine serves this localhost port");
            continue;
        };
        scope.spawn(move || {
            let reached = VsockStream::connect(&VsockAddr::new(machine.cid, LOCALHOST_PORT))
                .and_then(|mut stream| stream.write_all(&port.to_le_bytes()).map(|()| stream));
            match reached {
                Ok(stream) => {
                    debug!(machine = %machine.name, port, "passing a localhost connection");
                    let _ = splice(client, stream);
                }
                Err(e) => {
                    warn!(machine = %machine.name, port, error = %e, "cannot reach the machine")
                }
            }
        });
    }
}

/// One program's connection to pcscd from a machine, passed on if that
/// machine holds the readers or they are free.
fn serve_cards(stream: VsockStream, lease: &Mutex<Lease>, options: &HostOptions) -> Result<()> {
    let cid = stream.peer_addr().into_diagnostic()?.cid();
    let Some(name) = machine_at(cid, options)? else {
        info!(cid, "refused: no running machine has this address");
        return Ok(());
    };
    {
        let mut lease = lease.lock().unwrap();
        if !lease.take(&name) {
            info!(
                machine = %name,
                holder = lease.holder().unwrap_or_default(),
                "refused the smart card readers: another machine holds them"
            );
            return Ok(());
        }
    }
    let result = UnixStream::connect(&options.pcscd)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot reach pcscd at {}", options.pcscd.display()))
        .and_then(|pcscd| splice(stream, pcscd));
    lease.lock().unwrap().give_back(&name);
    result
}

/// Copies both ways until either side hangs up, then ends both.
fn splice(a: impl Stream, b: impl Stream) -> Result<()> {
    let (mut a_in, mut a_out) = (a.dup()?, a.dup()?);
    let (mut b_in, mut b_out) = (b.dup()?, b.dup()?);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = io::copy(&mut a_in, &mut b_out);
            b_out.end();
            a_in.end();
        });
        let _ = io::copy(&mut b_in, &mut a_out);
        a_out.end();
        b_in.end();
    });
    Ok(())
}

/// A connection `splice` can copy between: vsock or a Unix socket.
trait Stream: Read + Write + Send + Sized {
    fn dup(&self) -> Result<Self>;
    fn end(&self);
}

impl Stream for VsockStream {
    fn dup(&self) -> Result<Self> {
        self.try_clone().into_diagnostic()
    }
    fn end(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

impl Stream for UnixStream {
    fn dup(&self) -> Result<Self> {
        self.try_clone().into_diagnostic()
    }
    fn end(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

impl Stream for TcpStream {
    fn dup(&self) -> Result<Self> {
        self.try_clone().into_diagnostic()
    }
    fn end(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

// A machine.

fn guest(options: &GuestOptions) -> Result<()> {
    std::thread::scope(|scope| {
        scope.spawn(|| loop {
            if let Err(e) = keys_from_host(options.host_cid) {
            if let Err(e) = keys_from_host(options) {
                debug!("{e:?}");
            }
            std::thread::sleep(RETRY_AFTER);
        });
        if !options.serve.is_empty() {
            scope.spawn(|| {
                if let Err(e) = localhost_for_host(options) {
                    warn!("{e:?}");
                }
            });
        }
        cards_from_host(&options.pcsc_socket, options.host_cid)
    })
}

/// Holds a connection to the host's keys, and a copy of each key while it
/// lasts. Every copy goes when the connection does. The same connection
/// tells the host which of the served ports something listens on.
fn keys_from_host(options: &GuestOptions) -> Result<()> {
    let host_cid = options.host_cid;
    let stream = VsockStream::connect(&VsockAddr::new(host_cid, KEYS_PORT))
        .into_diagnostic()
        .wrap_err("cannot reach the host's security keys")?;
    info!("connected to the host's security keys");
    let to_host = Mutex::new(stream.try_clone().into_diagnostic()?);
    let mut from_host = stream;
    let mut copies: BTreeMap<u8, File> = BTreeMap::new();
    let to_host = &to_host;
    // Nothing is ever sent: dropping it stops the thread that looks at
    // the served ports.
    let (_looking, stop) = bounded::<()>(0);
    let result = std::thread::scope(|scope| -> Result<()> {
        if !options.serve.is_empty() {
            scope.spawn(move || announce(options, to_host, stop));
        }
        let result = (|| -> Result<()> {
            while let Some(frame) = Frame::read(&mut from_host).into_diagnostic()? {
                match frame {
                    Frame::Add {
                        key,
                        vendor,
                        product,
                        name,
                    } => {
                        let device = make_copy(&name, vendor, product)?;
                        let events = device.try_clone().into_diagnostic()?;
                        info!(key, %name, "the host's security key is here");
                        if let Some(old) = copies.insert(key, device) {
                            destroy(old);
                        }
                        scope.spawn(move || read_copy(key, events, to_host));
                    }
                    Frame::Remove { key } => {
                        if let Some(old) = copies.remove(&key) {
                            info!(key, "the host's security key is gone");
                            destroy(old);
                        }
                    }
                    Frame::Report { key, report } => {
                        if let Some(copy) = copies.get_mut(&key) {
                            copy.write_all(&uhid::input(&report)).into_diagnostic()?;
                        }
                    }
                    Frame::Serves { .. } | Frame::Stopped { .. } => {
                        bail!("the host cannot say that")
                    }
                }
            }
            Ok(())
        })();
        // The copies' readers end when their devices do.
        for (_, copy) in std::mem::take(&mut copies) {
            destroy(copy);
        }
        let _ = to_host.lock().unwrap().shutdown(Shutdown::Both);
        drop(_looking);
        result
    });
    info!("lost the host's security keys");
    result
}

/// Tells the host whenever something starts or stops listening on one of
/// the served ports, until `stop` disconnects.
fn announce(options: &GuestOptions, to_host: &Mutex<VsockStream>, stop: Receiver<()>) {
    let mut served = BTreeSet::new();
    loop {
        let tables = ["/proc/net/tcp", "/proc/net/tcp6"]
            .map(|t| std::fs::read_to_string(t).unwrap_or_default());
        for &port in &options.serve {
            let now = tables.iter().any(|t| listening(t, options.localhost, port));
            let frame = match (now, served.contains(&port)) {
                (true, false) => Frame::Serves { port },
                (false, true) => Frame::Stopped { port },
                _ => continue,
            };
            if to_host.lock().unwrap().write_all(&frame.encode()).is_err() {
                return;
            }
            if now {
                info!(port, "serving a localhost port to the host");
                served.insert(port);
            } else {
                info!(port, "no longer serving a localhost port");
                served.remove(&port);
            }
        }
        if stop.recv_timeout(LOOK_EVERY) != Err(crossbeam_channel::RecvTimeoutError::Timeout) {
            return;
        }
    }
}

/// Takes the host's connections for this machine's localhost: each names
/// its port first, and only a port this machine serves is reached.
fn localhost_for_host(options: &GuestOptions) -> Result<()> {
    let listener = VsockListener::bind(&VsockAddr::new(VMADDR_CID_ANY, LOCALHOST_PORT))
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot listen on vsock port {LOCALHOST_PORT}"))?;
    std::thread::scope(|scope| {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // Only the host forwards: no other address may reach in.
            if stream.peer_addr().map(|a| a.cid()).ok() != Some(options.host_cid) {
                continue;
            }
            scope.spawn(move || {
                let mut port = [0u8; 2];
                if stream.read_exact(&mut port).is_err() {
                    return;
                }
                let port = u16::from_le_bytes(port);
                if !options.serve.contains(&port) {
                    warn!(
                        port,
                        "the host asked for a port this machine does not serve"
                    );
                    return;
                }
                match TcpStream::connect((options.localhost, port)) {
                    Ok(local) => {
                        let _ = splice(stream, local);
                    }
                    Err(e) => warn!(port, error = %e, "nothing answers on the served port"),
                }
            });
        }
    });
    Ok(())
}

fn make_copy(name: &str, vendor: u32, product: u32) -> Result<File> {
    let mut device = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uhid")
        .into_diagnostic()
        .wrap_err("cannot open /dev/uhid")?;
    device
        .write_all(&uhid::create(name, RELAYED_PHYS, vendor, product))
        .into_diagnostic()
        .wrap_err("cannot make a copy of the security key")?;
    Ok(device)
}

fn destroy(mut copy: File) {
    let _ = copy.write_all(&uhid::destroy());
}

/// Passes what programs here write to copy `key` on to the host, and turns
/// down the report requests a FIDO key never answers.
fn read_copy(key: u8, mut device: File, to_host: &Mutex<VsockStream>) {
    let mut event = vec![0u8; uhid::EVENT_LEN];
    loop {
        match device.read(&mut event) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        match uhid::kind(&event) {
            Some(uhid::OUTPUT) => {
                if let Some(report) = uhid::output(&event) {
                    let frame = Frame::Report { key, report }.encode();
                    if to_host.lock().unwrap().write_all(&frame).is_err() {
                        return;
                    }
                }
            }
            Some(uhid::GET_REPORT | uhid::SET_REPORT) => {
                if let Some(reply) = uhid::refuse(&event) {
                    let _ = device.write_all(&reply);
                }
            }
            // The kernel's last word once the copy is destroyed.
            Some(uhid::STOP) => return,
            _ => {}
        }
    }
}

/// Listens where programs look for pcscd and passes each connection to the
/// host's.
fn cards_from_host(socket: &Path, host_cid: u32) -> Result<()> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).into_diagnostic()?;
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot listen on {}", socket.display()))?;
    // Every program may ask, as with pcscd's own socket; what a card does
    // is up to its PIN.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666)).into_diagnostic()?;
    info!(socket = %socket.display(), "passing smart card connections to the host");
    std::thread::scope(|scope| {
        for client in listener.incoming() {
            let Ok(client) = client else { continue };
            scope.spawn(move || {
                match VsockStream::connect(&VsockAddr::new(host_cid, SMART_CARDS_PORT)) {
                    Ok(host) => {
                        let _ = splice(client, host);
                    }
                    Err(e) => warn!(error = %e, "cannot reach the host's smart card readers"),
                }
            });
        }
    });
    Ok(())
}
