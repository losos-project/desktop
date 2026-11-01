//! The router: where each USB device goes, and getting it there.
//!
//! Devices come from two places. The USB machine holds the USB controllers
//! it is given and reports what is plugged into them (guest.rs); a device
//! there reaches its machine as a USB/IP stream the router relays between
//! two vsock connections, one to the USB machine and one to the machine it
//! goes to, or plugs into the host's own virtual controller when it is the
//! host's. The host's own controllers (keep.rs) have their ports claimed
//! (ports.rs), so a device there waits, unconfigured, until the router
//! configures it for the host or has libvirt pass it to its machine.
//!
//! Machines are lazy: none runs until a device needs it. The USB machine's
//! controllers are the host's while it is stopped, ports claimed like the
//! rest, and the first device plugged into one starts it; libvirt takes the
//! controllers from the host as it starts, and gives them back when the
//! router stops it again, a minute after its last device went. The
//! Bluetooth machine runs while Bluetooth is on, which Settings' Bluetooth
//! page turns on and off through `losos-peripherals bluetoothctl`; while it
//! is off the radio waits where it is. A disposable machine is made for a
//! device when it comes and destroyed when it goes.
//!
//! Most disposable machines are BusyBox and the device's driver. A sound
//! card needs more than a driver to be any use, so its machine is NixOS,
//! with Kicksecure's hardening and a sound server (nixos/guest/sound.nix),
//! and the router makes a socket on the host for it (a bridge): the host's
//! own sound server reaches the machine's through it, and plays to the
//! card from there. A printer's machine is the same with CUPS
//! (nixos/guest/printer.nix), which the host's CUPS sends jobs to, and a
//! camera's with ffmpeg (nixos/guest/camera.nix), which the host's
//! cameras stream from (camera.rs).
//!
//! What goes where is route.rs's. /run/losos-peripherals/devices.json says
//! where every device is, for Settings.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, select, tick, Sender};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::guest::{Answer, Report, Request, ATTACH, COMMAND, EVENTS, EXPORT};
use crate::keep::mounted_disks;
use crate::machines;
use crate::ports::{self, Claim};
use crate::route::{route, Route};
use crate::splice::splice;
use crate::uevent;
use crate::usb::{is_busid, Device};
use crate::usbip;
use crate::vsock;

/// A service machine: its libvirt name, its vsock address, and the unit
/// that starts and stops it.
#[derive(Clone, Debug, Deserialize)]
pub struct Machine {
    pub name: String,
    pub cid: u32,
    pub unit: String,
}

/// The USB machine, and the PCI functions it is given.
#[derive(Clone, Debug, Deserialize)]
pub struct UsbMachine {
    #[serde(flatten)]
    pub machine: Machine,
    /// Where the passthrough unit writes its hostdev definitions, which
    /// name the controllers.
    pub devices: PathBuf,
}

/// What the NixOS configuration tells the router (nixos/modules/peripherals.nix).
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// The USB machine, when this computer has one.
    pub usb: Option<UsbMachine>,
    pub network: Machine,
    pub bluetooth: Machine,
    pub storage: Machine,
    /// The definition disposable machines are made from (machines.rs).
    pub disposable: PathBuf,
    /// The definitions for routes whose devices need a machine with more
    /// in it, and what of each the host reaches.
    #[serde(default)]
    pub disposables: HashMap<Route, Disposable>,
    /// The vsock addresses disposable machines are given, first to last.
    pub disposable_cids: (u32, u32),
    /// Where the router keeps whether Bluetooth is on, across boots.
    pub state: PathBuf,
    /// The socket `losos-peripherals bluetoothctl` connects to.
    pub control: PathBuf,
}

/// A disposable machine with a stack of its own for its device.
#[derive(Clone, Debug, Deserialize)]
pub struct Disposable {
    /// Its definition, filled in as the BusyBox one's is.
    pub template: PathBuf,
    #[serde(default)]
    pub bridges: Vec<Bridge>,
}

/// A socket on the host for a port of the machine's, one for each machine
/// made: `<dir>/<machine>`, which appears once the machine answers on the
/// port and goes when the machine does.
#[derive(Clone, Debug, Deserialize)]
pub struct Bridge {
    pub dir: PathBuf,
    pub port: u32,
}

/// Where a device is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Source {
    /// On one of the host's own ports.
    Host,
    /// On a controller the USB machine holds.
    UsbMachine,
}

#[derive(Clone, Debug, Serialize)]
struct Placed {
    device: Device,
    route: Route,
    /// The machine it went to; none for the host, or while it waits.
    machine: Option<String>,
    /// The disposable machine's vsock address, when it is one.
    #[serde(skip)]
    disposable: Option<u32>,
    /// Waiting for its machine to start, retried every few seconds.
    pending: bool,
    /// Waiting for its machine to be wanted: a radio while Bluetooth is
    /// off, a device on the USB machine's controller while it starts.
    parked: bool,
    /// In its machine and working: passed through, or its stream relayed
    /// and the machine's controller holding it.
    attached: bool,
    /// A relay is under way for it, so a retry must not start another.
    #[serde(skip)]
    relaying: bool,
}

/// What the router shares with the threads answering bluetoothctl.
#[derive(Clone, Debug, Default)]
struct Bluetooth {
    on: bool,
    /// The radio's name, when there is one.
    radio: Option<String>,
}

enum Msg {
    Kernel(uevent::Event),
    Usb(Report),
    UsbUp,
    UsbGone,
    SetBluetooth(bool),
    /// A device from the USB machine is in its machine now.
    Relayed(String),
    /// A device's relay ended: pulled out, or its machine went away or
    /// never listened, so it is tried again while it is still plugged in.
    RelayEnded(String),
    /// The kernel's events stopped, which ends the router.
    KernelGone,
}

struct Router {
    config: Config,
    /// For the threads the router starts, to say how they went.
    events: Sender<Msg>,
    claims: HashMap<String, Claim>,
    disks: Vec<PathBuf>,
    placed: HashMap<(Source, String), Placed>,
    free_cids: BTreeSet<u32>,
    modules: HashMap<String, Vec<String>>,
    /// The USB machine's controllers, and whether it is up and since when
    /// it has had nothing on it.
    usb_controllers: BTreeSet<String>,
    usb_up: bool,
    /// Since when it has been asked to start, until it listens.
    usb_starting: Option<Instant>,
    usb_idle: Option<Instant>,
    bluetooth: Arc<Mutex<Bluetooth>>,
    /// Each disposable machine's bridges, by its vsock address; dropping
    /// them closes their sockets.
    bridges: HashMap<u32, Vec<Bridged>>,
}

const STATE: &str = "/run/losos-peripherals/devices.json";
const DISPOSABLE: &str = "disposable-";
/// How long the USB machine runs with nothing plugged into it.
const IDLE: Duration = Duration::from_secs(60);

/// Runs the router until the kernel's events stop.
pub fn serve(config: Config) -> io::Result<()> {
    let kernel = uevent::Socket::open()?;
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let (first, last) = config.disposable_cids;
    let on = fs::read_to_string(config.state.join("bluetooth")).is_ok_and(|s| s.trim() == "on");
    let (events, received) = bounded::<Msg>(256);
    let mut router = Router {
        events: events.clone(),
        disks: mounted_disks(Path::new("/sys"), &mountinfo),
        claims: HashMap::new(),
        placed: HashMap::new(),
        free_cids: (first..=last).collect(),
        modules: HashMap::new(),
        usb_controllers: config
            .usb
            .as_ref()
            .map(|u| controllers(&u.devices))
            .unwrap_or_default(),
        usb_up: false,
        usb_starting: None,
        usb_idle: None,
        bluetooth: Arc::new(Mutex::new(Bluetooth { on, radio: None })),
        bridges: HashMap::new(),
        config,
    };

    // Sockets a router before this one left are for machines destroyed
    // below. Anyone signed in may look in the directories, as at the
    // control socket.
    for bridge in router.config.disposables.values().flat_map(|d| &d.bridges) {
        let _ = fs::remove_dir_all(&bridge.dir);
        fs::create_dir_all(&bridge.dir)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bridge.dir, fs::Permissions::from_mode(0o755))?;
    }

    // Disposable machines a router before this one made are for devices it
    // no longer knows of.
    for name in machines::running(DISPOSABLE).unwrap_or_default() {
        info!(machine = %name, "destroying a disposable machine left from before");
        let _ = machines::destroy(&name);
    }

    // The ports first, so nothing plugged in from now on is configured
    // behind the router's back; then what was there before.
    // A machine with no USB at all has no such directory, and still has
    // Bluetooth's page to answer.
    let mut present: Vec<PathBuf> = fs::read_dir("/sys/bus/usb/devices")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    present.sort_by_key(|p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (!name.starts_with("usb"), name.matches('.').count(), name)
    });
    for dir in &present {
        router.host_add(dir);
    }
    // Controllers bound for passthrough at boot are no use to the host:
    // their machine runs from the start.
    if router.usb_controllers.iter().any(|f| bound_to_vfio(f)) || on {
        router.start_usb();
    }
    if on {
        router.start(&router.config.bluetooth.unit.clone());
    }

    let k = events.clone();
    thread::spawn(move || loop {
        match kernel.next() {
            Ok(ev) => {
                if k.send(Msg::Kernel(ev)).is_err() {
                    return;
                }
            }
            Err(e) => {
                warn!(error = %e, "cannot read device events");
                let _ = k.send(Msg::KernelGone);
                return;
            }
        }
    });
    if let Some(usb) = router.config.usb.clone() {
        let u = events.clone();
        thread::spawn(move || listen_to_usb_machine(usb.machine.cid, u));
    }
    {
        let listener = control_socket(&router.config.control)?;
        let c = events.clone();
        let shared = router.bluetooth.clone();
        let cid = router.config.bluetooth.cid;
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let c = c.clone();
                let shared = shared.clone();
                thread::spawn(move || {
                    if let Err(e) = bluetoothctl(conn, cid, &shared, &c) {
                        warn!(error = %e, "cannot answer bluetoothctl");
                    }
                });
            }
        });
    }
    drop(events);

    let every = tick(Duration::from_secs(5));
    router.save();
    loop {
        select! {
            recv(received) -> msg => {
                match msg {
                    Ok(Msg::Kernel(ev)) => router.kernel(&ev),
                    Ok(Msg::Usb(Report::Add(device))) => router.usb_add(device),
                    Ok(Msg::Usb(Report::Remove(busid))) => router.remove(Source::UsbMachine, &busid),
                    Ok(Msg::UsbUp) => router.usb_up(),
                    Ok(Msg::UsbGone) => router.usb_gone(),
                    Ok(Msg::SetBluetooth(on)) => router.set_bluetooth(on),
                    Ok(Msg::Relayed(busid)) => router.relayed(&busid),
                    Ok(Msg::RelayEnded(busid)) => router.relay_ended(&busid),
                    Ok(Msg::KernelGone) | Err(_) => return Ok(()),
                }
                router.save();
            }
            recv(every) -> _ => {
                router.retry();
                router.stop_usb_if_idle();
                router.save();
            }
        }
    }
}

/// The PCI functions in the passthrough unit's hostdev definitions:
/// `<address domain='0x0000' bus='0x00' slot='0x14' function='0x0'/>`.
fn controllers(dir: &Path) -> BTreeSet<String> {
    let hex = |s: &str, key: &str| -> Option<u32> {
        let at = s.find(&format!("{key}='0x"))? + key.len() + 4;
        let end = s[at..].find('\'')? + at;
        u32::from_str_radix(&s[at..end], 16).ok()
    };
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .flat_map(|xml| {
            xml.split("<address ")
                .skip(1)
                .filter_map(|a| {
                    Some(format!(
                        "{:04x}:{:02x}:{:02x}.{:x}",
                        hex(a, "domain")?,
                        hex(a, "bus")?,
                        hex(a, "slot")?,
                        hex(a, "function")?
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn bound_to_vfio(function: &str) -> bool {
    fs::read_link(format!("/sys/bus/pci/devices/{function}/driver"))
        .is_ok_and(|d| d.ends_with("vfio-pci"))
}

/// The control socket: anyone signed in may use it, as BlueZ lets anyone
/// on the system bus use Bluetooth.
fn control_socket(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// Follows the USB machine's reports, reconnecting whenever it restarts.
fn listen_to_usb_machine(cid: u32, to: Sender<Msg>) {
    loop {
        if let Ok(conn) = vsock::connect(cid, EVENTS) {
            info!("listening to the USB machine");
            if to.send(Msg::UsbUp).is_err() {
                return;
            }
            for line in BufReader::new(conn).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<Report>(&line) {
                    Ok(report) => {
                        if to.send(Msg::Usb(report)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "the USB machine sent something that is not a report")
                    }
                }
            }
            if to.send(Msg::UsbGone).is_err() {
                return;
            }
            info!("the USB machine stopped");
        }
        thread::sleep(Duration::from_secs(1));
    }
}

impl Router {
    fn kernel(&mut self, ev: &uevent::Event) {
        if ev.get("SUBSYSTEM") != "usb" || ev.get("DEVTYPE") != "usb_device" {
            return;
        }
        let dir = Path::new("/sys").join(ev.get("DEVPATH").trim_start_matches('/'));
        match ev.get("ACTION") {
            "add" => self.host_add(&dir),
            "remove" => {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.claims.remove(&name);
                self.remove(Source::Host, &name);
                // Whatever comes next into that port waits for the router.
                if let Some((hub, port)) = ports::parent_port(&name) {
                    if let Some(claim) = self.claims.get(&hub) {
                        if let Err(e) = claim.take(port) {
                            warn!(hub, port, error = %e, "cannot hold a port again");
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// A device on the host's own ports, new or there since before the
    /// router started.
    fn host_add(&mut self, dir: &Path) {
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            return;
        };
        if name.starts_with("usb") {
            self.claim(dir, &name);
            return;
        }
        if !is_busid(&name) {
            return;
        }
        let device = match Device::read(dir) {
            Ok(d) => d,
            Err(e) => {
                warn!(busid = %name, error = %e, "cannot read a device");
                return;
            }
        };
        let canonical = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let route = if self.disks.iter().any(|d| d.starts_with(&canonical)) {
            // It holds a mounted disk, which the host booted from.
            Route::Host
        } else {
            self.route(&device)
        };
        let on_usb_machine = device
            .controller
            .as_ref()
            .is_some_and(|c| self.usb_controllers.contains(c));
        if matches!(route, Route::Hub | Route::Host) {
            self.give_back(&name);
        }
        if route == Route::Hub {
            if let Err(e) = ports::configure(dir, device.configuration) {
                warn!(busid = %name, error = %e, "cannot configure a hub");
                return;
            }
            // Its ports, once its driver has it.
            let deadline = Instant::now() + Duration::from_secs(2);
            while !dir.join("maxchild").exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            self.claim(dir, &name);
            return;
        }
        if on_usb_machine && route != Route::Host {
            // Its controller is the USB machine's, which takes it, and the
            // device with it, as it starts.
            info!(busid = %name, name = %device.name, "waking the USB machine for a device");
            self.start_usb();
            self.placed.insert(
                (Source::Host, name),
                Placed {
                    device,
                    route,
                    machine: None,
                    disposable: None,
                    pending: false,
                    parked: true,
                    attached: false,
                    relaying: false,
                },
            );
            return;
        }
        if route == Route::Host {
            if let Err(e) = ports::configure(dir, device.configuration) {
                warn!(busid = %name, error = %e, "cannot configure a device for the host");
            }
            info!(busid = %name, name = %device.name, "kept on the host");
            self.placed.insert(
                (Source::Host, name),
                Placed {
                    device,
                    route,
                    machine: None,
                    disposable: None,
                    pending: false,
                    parked: false,
                    attached: true,
                    relaying: false,
                },
            );
            return;
        }
        self.place(Source::Host, device, route);
    }

    /// Gives a device's port back to the kernel for a device the host
    /// keeps.
    fn give_back(&self, busid: &str) {
        let Some((hub, port)) = ports::parent_port(busid) else {
            return;
        };
        if let Some(claim) = self.claims.get(&hub) {
            if let Err(e) = claim.give_back(port) {
                warn!(hub, port, error = %e, "cannot give a port back");
            }
        }
    }

    fn claim(&mut self, dir: &Path, name: &str) {
        match ports::claim(dir) {
            Ok(claim) => {
                self.claims.insert(name.to_owned(), claim);
            }
            Err(e) => warn!(hub = %name, error = %e, "cannot hold a hub's ports"),
        }
    }

    /// A device the USB machine reports. What it says is the USB machine's
    /// word, and a device there may have taken the machine over, so nothing
    /// in it is trusted beyond where the device goes: a busid that is not
    /// one is dropped, and the name is only logged.
    fn usb_add(&mut self, device: Device) {
        if !is_busid(&device.busid) {
            warn!(busid = %device.busid, "the USB machine named a device that cannot exist");
            return;
        }
        let route = self.route(&device);
        if route != Route::Hub {
            info!(busid = %device.busid, name = %device.name, ?route, "a device in the USB machine");
            self.usb_idle = None;
            self.place(Source::UsbMachine, device, route);
        }
    }

    fn start(&self, unit: &str) {
        if let Err(e) = machines::unit("start", unit) {
            warn!(unit, error = %e, "cannot start a machine");
        }
    }

    fn start_usb(&mut self) {
        if self.usb_up || self.usb_starting.is_some() {
            return;
        }
        if let Some(usb) = &self.config.usb {
            self.usb_starting = Some(Instant::now());
            self.start(&usb.machine.unit);
        }
    }

    fn usb_up(&mut self) {
        self.usb_up = true;
        self.usb_starting = None;
        self.usb_idle = Some(Instant::now());
    }

    fn usb_gone(&mut self) {
        self.usb_up = false;
        self.usb_starting = None;
        self.usb_idle = None;
        let gone: Vec<String> = self
            .placed
            .keys()
            .filter(|(s, _)| *s == Source::UsbMachine)
            .map(|(_, b)| b.clone())
            .collect();
        for busid in gone {
            self.remove(Source::UsbMachine, &busid);
        }
    }

    /// Stops the USB machine once it has had nothing plugged into it, but
    /// hubs, for a minute.
    fn stop_usb_if_idle(&mut self) {
        if !self.usb_up {
            return;
        }
        let busy = self.placed.keys().any(|(s, _)| *s == Source::UsbMachine);
        if busy {
            self.usb_idle = None;
            return;
        }
        let since = *self.usb_idle.get_or_insert_with(Instant::now);
        if since.elapsed() >= IDLE {
            if let Some(usb) = &self.config.usb {
                info!("stopping the USB machine: nothing is plugged into it");
                if let Err(e) = machines::unit("stop", &usb.machine.unit) {
                    warn!(error = %e, "cannot stop the USB machine");
                }
            }
            self.usb_idle = None;
        }
    }

    fn set_bluetooth(&mut self, on: bool) {
        self.bluetooth.lock().expect("not poisoned").on = on;
        let write = fs::create_dir_all(&self.config.state).and_then(|()| {
            fs::write(
                self.config.state.join("bluetooth"),
                if on { "on\n" } else { "off\n" },
            )
        });
        if let Err(e) = write {
            warn!(error = %e, "cannot keep whether Bluetooth is on");
        }
        let unit = self.config.bluetooth.unit.clone();
        if on {
            info!("Bluetooth on");
            self.start(&unit);
            // A laptop's radio is on its own USB controller, inside.
            self.start_usb();
            let parked: Vec<_> = self
                .placed
                .iter()
                .filter(|(_, p)| p.route == Route::Bluetooth && p.parked)
                .map(|(k, _)| k.clone())
                .collect();
            for key in parked {
                if let Some(p) = self.placed.remove(&key) {
                    self.place(key.0, p.device, p.route);
                }
            }
        } else {
            info!("Bluetooth off");
            if let Err(e) = machines::unit("stop", &unit) {
                warn!(error = %e, "cannot stop the Bluetooth machine");
            }
            for p in self
                .placed
                .values_mut()
                .filter(|p| p.route == Route::Bluetooth)
            {
                p.machine = None;
                p.pending = false;
                p.parked = true;
            }
        }
    }

    /// The machine for `route`: a service machine's, started if it is
    /// not running, or a new disposable one's.
    fn machine_for(&mut self, route: Route) -> io::Result<(String, u32, Option<u32>)> {
        let c = &self.config;
        let fixed = match route {
            Route::Network => Some(&c.network),
            Route::Bluetooth => Some(&c.bluetooth),
            Route::Storage => Some(&c.storage),
            _ => None,
        };
        if let Some(m) = fixed {
            let m = m.clone();
            self.start(&m.unit);
            return Ok((m.name, m.cid, None));
        }
        let cid = self
            .free_cids
            .pop_first()
            .ok_or_else(|| io::Error::other("every disposable machine's address is taken"))?;
        let name = format!("{DISPOSABLE}{cid}");
        let stack = self.config.disposables.get(&route);
        let template = stack.map_or(&self.config.disposable, |d| &d.template);
        if let Err(e) = machines::start_disposable(template, &name, cid) {
            self.free_cids.insert(cid);
            return Err(e);
        }
        info!(machine = %name, ?route, "started a disposable machine");
        if let Some(stack) = stack {
            let bridged = stack
                .bridges
                .iter()
                .map(|b| Bridged::open(cid, b.port, b.dir.join(&name)))
                .collect();
            self.bridges.insert(cid, bridged);
        }
        Ok((name, cid, Some(cid)))
    }

    fn place(&mut self, source: Source, device: Device, route: Route) {
        let busid = device.busid.clone();
        let mut placed = Placed {
            device,
            route,
            machine: None,
            disposable: None,
            pending: false,
            parked: false,
            attached: false,
            relaying: false,
        };
        if route == Route::Bluetooth {
            let mut bt = self.bluetooth.lock().expect("not poisoned");
            bt.radio = Some(placed.device.name.clone());
            if !bt.on {
                info!(busid = %busid, name = %placed.device.name, "a Bluetooth radio, waiting for Bluetooth to be turned on");
                placed.parked = true;
                drop(bt);
                self.placed.insert((source, busid), placed);
                return;
            }
        }
        if route == Route::Host {
            // From the USB machine to the host's own virtual controller.
            let usb = self.config.usb.as_ref().map(|m| m.machine.cid);
            let device = placed.device.clone();
            let events = self.events.clone();
            thread::spawn(move || {
                let Some(usb) = usb else { return };
                match export(usb, &device.busid)
                    .and_then(|(conn, devid, speed)| usbip::attach(&conn, devid, speed))
                {
                    Ok(port) => {
                        info!(busid = %device.busid, name = %device.name, port, "plugged into the host");
                        let _ = events.send(Msg::Relayed(device.busid.clone()));
                    }
                    Err(e) => {
                        warn!(busid = %device.busid, error = %e, "cannot plug a device into the host")
                    }
                }
            });
            self.placed.insert((source, busid), placed);
            return;
        }
        match self.machine_for(route) {
            Ok((machine, cid, disposable)) => {
                placed.machine = Some(machine.clone());
                placed.disposable = disposable;
                if self.deliver(source, &placed.device, &machine, cid) {
                    placed.attached = source == Source::Host;
                    placed.relaying = source == Source::UsbMachine;
                } else {
                    placed.pending = true;
                }
            }
            Err(e) => {
                warn!(busid = %busid, error = %e, "no machine for a device; it stays unconfigured");
            }
        }
        self.placed.insert((source, busid), placed);
    }

    /// Gets a device to its machine; false when the machine is not up to
    /// take it yet.
    fn deliver(&self, source: Source, device: &Device, machine: &str, cid: u32) -> bool {
        match source {
            Source::Host => match machines::attach(machine, device) {
                Ok(()) => {
                    info!(busid = %device.busid, name = %device.name, machine, "passed through");
                    true
                }
                Err(e) => {
                    info!(busid = %device.busid, machine, error = %e, "cannot pass a device through yet");
                    false
                }
            },
            Source::UsbMachine => {
                let Some(usb) = self.config.usb.as_ref().map(|m| m.machine.cid) else {
                    return true;
                };
                let device = device.clone();
                let machine = machine.to_owned();
                let events = self.events.clone();
                thread::spawn(move || relay(usb, cid, &device, &machine, &events));
                true
            }
        }
    }

    fn retry(&mut self) {
        // A start that failed, such as one that came while libvirt was
        // still letting the machine go, is asked for again. Starting a
        // machine that is starting or up does nothing.
        if let (Some(since), Some(usb)) = (self.usb_starting, &self.config.usb) {
            if since.elapsed() >= Duration::from_secs(30) {
                let unit = usb.machine.unit.clone();
                self.usb_starting = Some(Instant::now());
                self.start(&unit);
            }
        }
        let pending: Vec<(Source, String)> = self
            .placed
            .iter()
            .filter(|(_, p)| p.pending && !p.relaying)
            .map(|(k, _)| k.clone())
            .collect();
        for key in pending {
            let p = &self.placed[&key];
            let (Some(machine), device) = (p.machine.clone(), p.device.clone()) else {
                continue;
            };
            // A machine that stopped since, such as the files machine gone
            // idle, starts again for it.
            if let Some(unit) = self.unit_of(&machine) {
                self.start(&unit);
            }
            let cid = self.cid_of(&machine).unwrap_or(0);
            if self.deliver(key.0, &device, &machine, cid) {
                if let Some(p) = self.placed.get_mut(&key) {
                    p.pending = false;
                    p.attached = key.0 == Source::Host;
                    p.relaying = key.0 == Source::UsbMachine;
                }
            }
        }
    }

    fn relayed(&mut self, busid: &str) {
        if let Some(p) = self.placed.get_mut(&(Source::UsbMachine, busid.to_owned())) {
            p.attached = true;
        }
    }

    /// The device is still plugged in, or its report of going is on its
    /// way: either way it waits for the next retry, which a removal
    /// cancels.
    fn relay_ended(&mut self, busid: &str) {
        if let Some(p) = self.placed.get_mut(&(Source::UsbMachine, busid.to_owned())) {
            p.attached = false;
            p.relaying = false;
            p.pending = true;
        }
    }

    fn unit_of(&self, machine: &str) -> Option<String> {
        let c = &self.config;
        [&c.network, &c.bluetooth, &c.storage]
            .into_iter()
            .find(|m| m.name == machine)
            .map(|m| m.unit.clone())
    }

    fn cid_of(&self, machine: &str) -> Option<u32> {
        let c = &self.config;
        [&c.network, &c.bluetooth, &c.storage]
            .into_iter()
            .find(|m| m.name == machine)
            .map(|m| m.cid)
            .or_else(|| machine.strip_prefix(DISPOSABLE)?.parse().ok())
    }

    fn remove(&mut self, source: Source, busid: &str) {
        let Some(placed) = self.placed.remove(&(source, busid.to_owned())) else {
            return;
        };
        if placed.route == Route::Bluetooth {
            self.bluetooth.lock().expect("not poisoned").radio = None;
        }
        if let Some(cid) = placed.disposable {
            if let Some(machine) = &placed.machine {
                info!(busid, machine = %machine, "unplugged; destroying its disposable machine");
                if let Err(e) = machines::destroy(machine) {
                    warn!(machine = %machine, error = %e, "cannot destroy a disposable machine");
                }
            }
            self.bridges.remove(&cid);
            self.free_cids.insert(cid);
        } else if let (Source::Host, Some(machine)) = (source, &placed.machine) {
            let _ = machines::detach(machine, &placed.device);
            info!(busid, machine = %machine, "unplugged");
        } else {
            info!(busid, "unplugged");
        }
    }

    fn route(&mut self, device: &Device) -> Route {
        let modules: Vec<String> = device
            .modaliases()
            .flat_map(|alias| {
                self.modules
                    .entry(alias.clone())
                    .or_insert_with(|| modules_for(&alias))
                    .clone()
            })
            .collect();
        route(device, modules.iter().map(String::as_str))
    }

    fn save(&self) {
        let devices: Vec<serde_json::Value> = self
            .placed
            .iter()
            .map(|((source, _), p)| {
                serde_json::json!({
                    "source": source,
                    "busid": p.device.busid,
                    "name": p.device.name,
                    "vendor": format!("{:04x}", p.device.vendor),
                    "product": format!("{:04x}", p.device.product),
                    "route": p.route,
                    "machine": p.machine,
                    "pending": p.pending,
                    "parked": p.parked,
                    "attached": p.attached,
                })
            })
            .collect();
        let write = || -> io::Result<()> {
            fs::create_dir_all(Path::new(STATE).parent().expect("has a parent"))?;
            let tmp = format!("{STATE}.new");
            let mut f = File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(&devices).map_err(io::Error::other)?)?;
            fs::rename(tmp, STATE)
        };
        if let Err(e) = write() {
            warn!(error = %e, "cannot write {STATE}");
        }
    }
}

/// The kernel-tree paths of the modules that would claim an interface with
/// this modalias, as modprobe and modinfo find them.
fn modules_for(alias: &str) -> Vec<String> {
    let run = |args: &[&str]| -> Vec<String> {
        Command::new(args[0])
            .args(&args[1..])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    run(&["modprobe", "--resolve-alias", alias])
        .into_iter()
        .flat_map(|m| run(&["modinfo", "-n", &m]))
        .collect()
}

/// Asks the USB machine for a device's stream: the connection, and the
/// device's id and speed for the controller it is plugged into.
fn export(usb: u32, busid: &str) -> io::Result<(File, u32, u32)> {
    let mut conn = vsock::connect(usb, EXPORT)?;
    writeln!(conn, "export {busid}")?;
    let line = usbip::read_line(&mut conn)?;
    let mut words = line.split_whitespace();
    match (
        words.next(),
        words.next().and_then(|w| w.parse().ok()),
        words.next().and_then(|w| w.parse().ok()),
    ) {
        (Some("ok"), Some(devid), Some(speed)) => Ok((conn, devid, speed)),
        _ => Err(io::Error::other(format!("the USB machine refused: {line}"))),
    }
}

/// Waits up to `timeout` for machine `cid` to listen on `port`.
fn connect_when_up(cid: u32, port: u32, timeout: Duration) -> io::Result<File> {
    let deadline = Instant::now() + timeout;
    loop {
        match vsock::connect(cid, port) {
            Ok(conn) => return Ok(conn),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(250)),
            Err(e) => return Err(e),
        }
    }
}

/// Carries a device's stream from the USB machine to machine `cid`, until
/// either end closes it: the device is pulled out, or the machine stops.
/// A machine just started takes a while to listen, so the connection to it
/// is tried for up to three minutes.
fn relay(usb: u32, cid: u32, device: &Device, machine: &str, events: &Sender<Msg>) {
    let busid = &device.busid;
    // However it ends, the router hears of it.
    struct Ended<'a>(&'a Sender<Msg>, &'a str);
    impl Drop for Ended<'_> {
        fn drop(&mut self) {
            let _ = self.0.send(Msg::RelayEnded(self.1.to_owned()));
        }
    }
    let _ended = Ended(events, busid);
    let mut to = match connect_when_up(cid, ATTACH, Duration::from_secs(180)) {
        Ok(conn) => conn,
        Err(e) => {
            warn!(busid, machine, error = %e, "the machine never listened for its device");
            return;
        }
    };
    let (from, devid, speed) = match export(usb, busid) {
        Ok(x) => x,
        Err(e) => {
            warn!(busid, error = %e, "cannot export a device");
            return;
        }
    };
    if let Err(e) = writeln!(to, "attach {devid} {speed}") {
        warn!(busid, machine, error = %e, "cannot hand a device over");
        return;
    }
    info!(busid, name = %device.name, machine, "relaying");
    let _ = events.send(Msg::Relayed(busid.clone()));
    splice(&from, &to);
    info!(busid, machine, "the relay ended");
}

/// One bridge's socket for one machine.
struct Bridged {
    path: PathBuf,
    /// Set as the machine goes, under the lock the socket is made under,
    /// so the socket is either never made or is there to be removed.
    closed: Arc<Mutex<bool>>,
}

impl Bridged {
    fn open(cid: u32, port: u32, path: PathBuf) -> Bridged {
        let closed = Arc::new(Mutex::new(false));
        let bridged = Bridged {
            path: path.clone(),
            closed: closed.clone(),
        };
        thread::spawn(move || bridge(cid, port, &path, &closed));
        bridged
    }
}

impl Drop for Bridged {
    fn drop(&mut self) {
        *self.closed.lock().expect("not poisoned") = true;
        // Wakes the listener, which sees it is closed and stops.
        let _ = UnixStream::connect(&self.path);
        let _ = fs::remove_file(&self.path);
    }
}

/// Makes `path` once machine `cid` answers on `port`, a NixOS machine
/// taking a while to boot, so whatever watches for the socket finds one
/// it can use; then joins every connection to it to a connection to the
/// port, until the machine goes.
fn bridge(cid: u32, port: u32, path: &Path, closed: &Mutex<bool>) {
    if let Err(e) = connect_when_up(cid, port, Duration::from_secs(300)) {
        warn!(cid, port, error = %e, "the machine never answered; no socket for it");
        return;
    }
    let listener = {
        let closed = closed.lock().expect("not poisoned");
        if *closed {
            return;
        }
        let _ = fs::remove_file(path);
        match UnixListener::bind(path) {
            Ok(l) => {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o666));
                l
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cannot make a bridge's socket");
                return;
            }
        }
    };
    info!(path = %path.display(), cid, port, "bridged");
    for conn in listener.incoming() {
        if *closed.lock().expect("not poisoned") {
            return;
        }
        let Ok(conn) = conn else { continue };
        thread::spawn(move || match vsock::connect(cid, port) {
            Ok(to) => splice(&conn, &to),
            Err(e) => warn!(cid, port, error = %e, "cannot reach the machine"),
        });
    }
}

/// Runs bluetoothctl in the Bluetooth machine, for one caller of the
/// control socket.
fn run_in_machine(cid: u32, args: &[String], wait: Duration) -> io::Result<Answer> {
    let mut conn = connect_when_up(cid, COMMAND, wait)?;
    let mut line = serde_json::to_vec(&Request {
        args: args.to_vec(),
    })
    .map_err(io::Error::other)?;
    line.push(b'\n');
    conn.write_all(&line)?;
    let mut answer = String::new();
    BufReader::new(conn).read_line(&mut answer)?;
    serde_json::from_str(&answer).map_err(io::Error::other)
}

/// Answers one `losos-peripherals bluetoothctl`: a Request line in, an
/// Answer line out. Turning the radio on turns Bluetooth on first, which
/// starts its machine and hands it the radio, and waits for BlueZ to see
/// it; turning it off turns Bluetooth off after. While Bluetooth is off
/// the machine is not running, so what Settings reads is answered here:
/// the radio, off, and no devices.
fn bluetoothctl(
    conn: UnixStream,
    cid: u32,
    shared: &Mutex<Bluetooth>,
    router: &Sender<Msg>,
) -> io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line).map_err(io::Error::other)?;
    let words: Vec<&str> = request
        .args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with("--") && a.parse::<u32>().is_err())
        .collect();
    let state = shared.lock().expect("not poisoned").clone();
    let answer = match words.as_slice() {
        ["power", "on"] => {
            if !state.on {
                router.send(Msg::SetBluetooth(true)).map_err(io::Error::other)?;
            }
            // Until BlueZ has the radio, `power on` would find none.
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let show = run_in_machine(cid, &["show".into()], Duration::from_secs(60));
                if show.as_ref().is_ok_and(|a| a.stdout.contains("Controller ")) || Instant::now() > deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }
            run_in_machine(cid, &request.args, Duration::from_secs(5))?
        }
        ["power", "off"] => {
            let answer = run_in_machine(cid, &request.args, Duration::from_secs(1)).unwrap_or_default();
            router.send(Msg::SetBluetooth(false)).map_err(io::Error::other)?;
            answer
        }
        _ if !state.on => match (words.first(), &state.radio) {
            (Some(&"show"), Some(name)) => Answer {
                status: 0,
                stdout: format!(
                    "Controller 00:00:00:00:00:00 (public)\n\tName: {name}\n\tAlias: {name}\n\tPowered: no\n"
                ),
                ..Default::default()
            },
            (Some(&"show"), None) => Answer {
                status: 1,
                stdout: "No default controller available\n".into(),
                ..Default::default()
            },
            (Some(&"devices"), _) => Answer::default(),
            _ => Answer {
                status: 1,
                stderr: "Bluetooth is off\n".into(),
                ..Default::default()
            },
        },
        _ => run_in_machine(cid, &request.args, Duration::from_secs(60))?,
    };
    let mut out = serde_json::to_vec(&answer).map_err(io::Error::other)?;
    out.push(b'\n');
    let mut conn = conn;
    conn.write_all(&out)
}

/// The client: what the host's `bluetoothctl` is, so Settings' Bluetooth
/// page reaches BlueZ in its machine unchanged.
pub fn bluetoothctl_client(control: &Path, args: Vec<String>) -> io::Result<i32> {
    let mut conn = UnixStream::connect(control)?;
    let mut line = serde_json::to_vec(&Request { args }).map_err(io::Error::other)?;
    line.push(b'\n');
    conn.write_all(&line)?;
    let mut answer = String::new();
    BufReader::new(conn).read_line(&mut answer)?;
    let answer: Answer = serde_json::from_str(&answer).map_err(io::Error::other)?;
    print!("{}", answer.stdout);
    eprint!("{}", answer.stderr);
    Ok(answer.status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controllers_come_from_hostdev_definitions() {
        let dir = std::env::temp_dir().join(format!("losos-hostdevs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("0000:00:14.0.xml"),
            "<hostdev mode='subsystem' type='pci' managed='yes'><source>\
             <address domain='0x0000' bus='0x00' slot='0x14' function='0x0'/></source></hostdev>",
        )
        .unwrap();
        assert_eq!(
            controllers(&dir).into_iter().collect::<Vec<_>>(),
            ["0000:00:14.0"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn routes_name_their_disposable_machines() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {"name": "sys-net", "cid": 4100, "unit": "n"},
                "storage": {"name": "sys-files", "cid": 4101, "unit": "f"},
                "bluetooth": {"name": "sys-bluetooth", "cid": 4111, "unit": "b"},
                "disposable": "/busybox.xml",
                "disposables": {
                    "audio": {
                        "template": "/sound.xml",
                        "bridges": [{"dir": "/run/losos-peripherals/sound", "port": 3244}]
                    }
                },
                "disposable_cids": [4120, 4199],
                "state": "/var/lib/losos-peripherals",
                "control": "/run/losos-peripherals/control"
            }"#,
        )
        .unwrap();
        let sound = &config.disposables[&Route::Audio];
        assert_eq!(sound.template, Path::new("/sound.xml"));
        assert_eq!(sound.bridges[0].port, 3244);
        assert!(!config.disposables.contains_key(&Route::Camera));
    }

    #[test]
    fn a_bridge_joins_its_socket_to_the_other_end() {
        let dir = std::env::temp_dir().join(format!("losos-splice-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server");
        let server = UnixListener::bind(&path).unwrap();
        let echo = thread::spawn(move || {
            let (conn, _) = server.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&conn).read_line(&mut line).unwrap();
            (&conn).write_all(line.to_uppercase().as_bytes()).unwrap();
        });
        let (near, far) = UnixStream::pair().unwrap();
        let server = UnixStream::connect(&path).unwrap();
        let joined = thread::spawn(move || splice(&far, &server));
        (&near).write_all(b"hello\n").unwrap();
        let mut answer = String::new();
        BufReader::new(&near).read_line(&mut answer).unwrap();
        assert_eq!(answer, "HELLO\n");
        echo.join().unwrap();
        joined.join().unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
