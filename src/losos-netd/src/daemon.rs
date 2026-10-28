//! The daemon: the network cards, their addresses and Wi-Fi, the routing
//! and firewall, the programs the machine keeps running, and the answers to
//! nmcli. One thread looks at everything once a second and acts; the
//! control socket's connections ask it questions under the same lock.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use losos_netd::nmcli::{self, Add, Key, Request, Row};
use losos_netd::saved::{self, Saved};
use losos_netd::wpa::{self, AccessPoint, Control};
use losos_netd::{nameservers, Config, CONTROL};

/// How often everything is looked at.
const TICK: Duration = Duration::from_secs(1);
/// How long a Wi-Fi join gets before it counts as failed: association, the
/// handshake, DHCP. A wrong passphrase is a handshake that never ends.
const JOIN_TIME: Duration = Duration::from_secs(40);
/// How long a network that failed to join is left out of joining by
/// itself, so a wrong saved passphrase is not retried in a loop.
const FAILED_PAUSE: Duration = Duration::from_secs(120);
/// How often a Wi-Fi card that is not connected is asked to scan.
const SCAN_EVERY: Duration = Duration::from_secs(30);
/// Where udhcpc's leases are written, one file per card.
const DHCP_DIR: &str = "/run/losos-netd/dhcp";
/// The forwarder's servers (losos-adblock serve's override).
const UPSTREAMS: &str = "/run/losos-adblock/upstreams";
/// There while those servers must be the only ones asked: the gateway in
/// schizo mode, or the resolvers through the edge's tunnel. The forwarder's
/// encrypted servers stand aside meanwhile.
const FORCED: &str = "/run/losos-adblock/upstreams-forced";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ethernet,
    Wifi,
    /// A link to the host or the gateway, by its role in the configuration.
    Static,
}

struct Link {
    kind: Kind,
    /// Its own address, before schizo mode gave it a random one.
    mac: String,
    dhcp: Option<Child>,
    wpa: Option<Child>,
    /// Ethernet: whether the cable is in. Wi-Fi: whether associated.
    carrier: bool,
    /// The address DHCP gave it, and its servers.
    ipv4: Option<String>,
    /// Wi-Fi: the saved network it is on or joining, and since when.
    wifi: Option<(String, Instant, bool)>,
    /// Wi-Fi: when the joined network went, while wpa_supplicant looks for
    /// it again.
    lost: Option<Instant>,
    /// Wi-Fi: the last scan's access points.
    scan: Vec<AccessPoint>,
    last_scan: Option<Instant>,
    /// Set by `nmcli device disconnect`: no joining by itself until a
    /// `connect` or an `up`.
    held: bool,
    /// Whether schizo mode's random MAC address is on it.
    random_mac: bool,
}

impl Link {
    fn new(kind: Kind, mac: String) -> Self {
        Self {
            kind,
            mac,
            dhcp: None,
            wpa: None,
            carrier: false,
            ipv4: None,
            wifi: None,
            lost: None,
            scan: Vec::new(),
            last_scan: None,
            held: false,
            random_mac: false,
        }
    }
}

pub struct State {
    config: Config,
    links: BTreeMap<String, Link>,
    saved: Vec<Saved>,
    /// Networks added with `save no`: here until the machine restarts.
    unsaved: Vec<Saved>,
    failed: BTreeMap<String, Instant>,
    radio: bool,
    schizo: Option<bool>,
    /// The edge's tunnel is up (edge_run.rs): names go through it too.
    edge_dns: bool,
    upstreams: String,
    running: Vec<(Vec<String>, Option<Child>, Instant)>,
    /// Bumped on every change `nmcli monitor` would print.
    generation: u64,
    changes: Vec<String>,
}

pub struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    /// The edge thread's answers (edge_run.rs).
    pub edge: Arc<crate::edge_run::Edge>,
}

/// Runs a program to its end, and says whether it succeeded; why not is
/// logged at debug, since a failure here is often an answer (no such route).
pub fn run_quiet(program: &str, args: &[&str]) -> bool {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            debug!(
                program,
                ?args,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "failed"
            );
            false
        }
        Err(e) => {
            warn!(program, error = %e, "cannot run");
            false
        }
    }
}

fn ip(args: &[&str]) -> bool {
    run_quiet("ip", args)
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
}

fn state_dir(config: &Config) -> PathBuf {
    config
        .state_disk
        .as_ref()
        .map(|(_, path)| path.join("connections"))
        .unwrap_or_else(|| PathBuf::from("/run/losos-netd/connections"))
}

/// A random, locally administered unicast MAC address.
fn random_mac() -> String {
    let mut bytes = [0u8; 6];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    bytes[0] = (bytes[0] & 0xfc) | 0x02;
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn new_uuid() -> String {
    read("/proc/sys/kernel/random/uuid").unwrap_or_else(|| format!("{:x}", rand_u64()))
}

fn rand_u64() -> u64 {
    let mut bytes = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    }
    u64::from_ne_bytes(bytes)
}

impl State {
    pub fn new(config: Config) -> Self {
        let saved = saved::load(&state_dir(&config));
        Self {
            running: config
                .run
                .iter()
                .map(|c| (c.clone(), None, Instant::now()))
                .collect(),
            config,
            links: BTreeMap::new(),
            saved,
            unsaved: Vec::new(),
            failed: BTreeMap::new(),
            radio: true,
            schizo: None,
            edge_dns: false,
            upstreams: String::new(),
            generation: 0,
            changes: Vec::new(),
        }
    }

    fn changed(&mut self, what: String) {
        info!("{what}");
        self.generation += 1;
        self.changes.push(what);
        if self.changes.len() > 64 {
            self.changes.remove(0);
        }
    }

    fn all_saved(&self) -> impl Iterator<Item = &Saved> {
        self.saved.iter().chain(self.unsaved.iter())
    }

    fn find(&self, key: &Key) -> Option<Saved> {
        self.all_saved()
            .find(|s| match key {
                Key::Uuid(u) => &s.uuid == u,
                Key::Id(id) => &s.name == id || &s.uuid == id,
            })
            .cloned()
    }

    fn schizo_on(&self) -> bool {
        self.schizo == Some(true)
    }

    /// Finds the cards and links, names the links by MAC, and brings each
    /// up once.
    fn discover(&mut self) {
        let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
            return;
        };
        let mut present = Vec::new();
        for entry in entries.flatten() {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if name == "lo" {
                continue;
            }
            let sys = entry.path();
            let Some(mac) = read(sys.join("address")).map(|m| m.to_lowercase()) else {
                continue;
            };
            if let Some(link) = self.config.links.iter().find(|l| l.mac == mac).cloned() {
                if name != link.name {
                    ip(&["link", "set", "dev", &name, "down"]);
                    if !ip(&["link", "set", "dev", &name, "name", &link.name]) {
                        continue;
                    }
                    name = link.name.clone();
                }
                present.push(name.clone());
                if !self.links.contains_key(&name) {
                    ip(&["link", "set", "dev", &name, "up"]);
                    ip(&["-4", "addr", "replace", &link.address, "dev", &name]);
                    if let Some(router) = &link.router {
                        ip(&[
                            "-4", "route", "replace", "default", "via", router, "dev", &name,
                            "metric", "900",
                        ]);
                    }
                    self.links
                        .insert(name.clone(), Link::new(Kind::Static, mac));
                    self.changed(format!("{name}: the link to the {}", link.role));
                }
                continue;
            }
            // Only devices, not anything virtual a program made here.
            if !sys.join("device").exists() {
                continue;
            }
            present.push(name.clone());
            if self.links.contains_key(&name) {
                continue;
            }
            let kind = if sys.join("wireless").exists() || sys.join("phy80211").exists() {
                Kind::Wifi
            } else {
                Kind::Ethernet
            };
            ip(&["link", "set", "dev", &name, "up"]);
            self.links.insert(name.clone(), Link::new(kind, mac));
            self.changed(format!(
                "{name}: {} card found",
                if kind == Kind::Wifi {
                    "Wi-Fi"
                } else {
                    "Ethernet"
                }
            ));
        }
        let gone: Vec<String> = self
            .links
            .keys()
            .filter(|n| !present.contains(n))
            .cloned()
            .collect();
        for name in gone {
            if let Some(mut link) = self.links.remove(&name) {
                stop(&mut link.dhcp);
                stop(&mut link.wpa);
            }
            let _ = std::fs::remove_file(Path::new(DHCP_DIR).join(&name));
            self.changed(format!("{name}: gone"));
        }
    }

    /// What each card's lease says, from the DHCP hook's files.
    fn read_leases(&mut self) {
        let mut changes = Vec::new();
        for (name, link) in &mut self.links {
            if link.kind == Kind::Static {
                continue;
            }
            let lease = read(Path::new(DHCP_DIR).join(name));
            let address = lease.as_deref().and_then(|l| {
                l.lines()
                    .find_map(|line| line.strip_prefix("ip="))
                    .map(str::to_owned)
            });
            if address != link.ipv4 {
                changes.push(match &address {
                    Some(a) => format!("{name}: connected, {a}"),
                    None => format!("{name}: no address"),
                });
                link.ipv4 = address;
            }
        }
        for change in changes {
            self.changed(change);
        }
    }

    fn ethernet(&mut self, name: &str) {
        let schizo = self.schizo_on();
        let Some(link) = self.links.get_mut(name) else {
            return;
        };
        if link.random_mac != schizo {
            stop(&mut link.dhcp);
            ip(&["link", "set", "dev", name, "down"]);
            let mac = if schizo {
                random_mac()
            } else {
                link.mac.clone()
            };
            ip(&["link", "set", "dev", name, "address", &mac]);
            ip(&["link", "set", "dev", name, "up"]);
            link.random_mac = schizo;
        }
        let carrier = read(format!("/sys/class/net/{name}/carrier")).as_deref() == Some("1");
        let want = carrier && !link.held;
        if want && link.dhcp.is_none() {
            link.dhcp = spawn_dhcp(name);
        } else if !want && link.dhcp.is_some() {
            stop(&mut link.dhcp);
            ip(&["-4", "addr", "flush", "dev", name]);
            let _ = std::fs::remove_file(Path::new(DHCP_DIR).join(name));
        }
        if carrier != link.carrier {
            link.carrier = carrier;
            let what = format!("{name}: {}", if carrier { "cable in" } else { "no cable" });
            self.changed(what);
        }
    }

    fn wifi(&mut self, name: &str) {
        let radio = self.radio;
        let schizo = self.schizo_on();
        let saved: Vec<Saved> = self.all_saved().cloned().collect();
        let now = Instant::now();
        self.failed
            .retain(|_, at| now.duration_since(*at) < FAILED_PAUSE);
        let failed: Vec<String> = self.failed.keys().cloned().collect();
        let Some(link) = self.links.get_mut(name) else {
            return;
        };
        if !radio {
            if link.wpa.is_some() {
                stop(&mut link.dhcp);
                stop(&mut link.wpa);
                ip(&["-4", "addr", "flush", "dev", name]);
                ip(&["link", "set", "dev", name, "down"]);
                link.wifi = None;
                link.scan.clear();
            }
            return;
        }
        if link.wpa.is_none() {
            ip(&["link", "set", "dev", name, "up"]);
            link.wpa = spawn_wpa(name);
            return;
        }
        let Ok(control) = Control::open(name) else {
            return;
        };
        if link.random_mac != schizo {
            let flag = if schizo { "1" } else { "0" };
            let _ = control.ok(&format!("SET preassoc_mac_addr {flag}"));
            let _ = control.ok(&format!("SET mac_addr {flag}"));
            link.random_mac = schizo;
            // The join in progress, if any, starts again with the address.
            if link.wifi.is_some() {
                let _ = control.ok("DISCONNECT");
                let _ = control.ok("RECONNECT");
            }
        }
        if let Ok(results) = control.request("SCAN_RESULTS") {
            link.scan = wpa::parse_scan(&results);
        }
        let status = control.request("STATUS").unwrap_or_default();
        let completed = wpa::status_value(&status, "wpa_state") == Some("COMPLETED");
        let mut events = Vec::new();
        if completed != link.carrier {
            link.carrier = completed;
            if completed {
                link.dhcp = spawn_dhcp(name);
            } else {
                stop(&mut link.dhcp);
                ip(&["-4", "addr", "flush", "dev", name]);
                let _ = std::fs::remove_file(Path::new(DHCP_DIR).join(name));
            }
            events.push(format!(
                "{name}: {}",
                if completed {
                    "associated"
                } else {
                    "disassociated"
                }
            ));
        }
        match &mut link.wifi {
            Some((uuid, since, joined)) => {
                if completed && link.ipv4.is_some() {
                    link.lost = None;
                    if !*joined {
                        *joined = true;
                        events.push(format!("{name}: joined"));
                    }
                } else if !*joined && since.elapsed() > JOIN_TIME {
                    let uuid = uuid.clone();
                    let _ = control.ok("DISCONNECT");
                    let _ = control.request("REMOVE_NETWORK all");
                    link.wifi = None;
                    self.failed.insert(uuid, Instant::now());
                    events.push(format!("{name}: could not join"));
                } else if *joined && !completed {
                    // Lost: wpa_supplicant looks for it again by itself, and
                    // if it has not found it in a join's time, the next look
                    // picks the best network there is. Not a failure, so it
                    // is not left out.
                    if link.lost.get_or_insert_with(Instant::now).elapsed() > JOIN_TIME {
                        let _ = control.request("REMOVE_NETWORK all");
                        link.wifi = None;
                        link.lost = None;
                        events.push(format!("{name}: lost the network"));
                    }
                }
            }
            None if !link.held => {
                // Joins the strongest saved network in reach by itself.
                let best = link
                    .scan
                    .iter()
                    .filter_map(|ap| {
                        let s = saved
                            .iter()
                            .find(|s| s.ssid == ap.ssid && !failed.contains(&s.uuid))?;
                        Some((ap.signal, s.clone()))
                    })
                    .max_by_key(|(signal, _)| *signal);
                if let Some((_, network)) = best {
                    if join(&control, &network, schizo).is_ok() {
                        link.wifi = Some((network.uuid.clone(), Instant::now(), false));
                        events.push(format!("{name}: joining {}", network.name));
                    }
                }
            }
            None => {}
        }
        if link.wifi.as_ref().is_none_or(|(_, _, joined)| !*joined)
            && link.last_scan.is_none_or(|t| t.elapsed() > SCAN_EVERY)
        {
            let _ = control.request("SCAN");
            link.last_scan = Some(Instant::now());
        }
        for event in events {
            self.changed(event);
        }
    }

    /// Follows the host: schizo mode, and where the forwarder sends names.
    fn follow_host(&mut self) {
        let host = self.config.host.clone();
        let schizo = host.as_ref().is_some_and(|h| h.join("schizo").exists());
        if self.schizo != Some(schizo) && self.apply_schizo(schizo) {
            self.schizo = Some(schizo);
            self.changed(format!("schizo mode {}", if schizo { "on" } else { "off" }));
        }
        let host_upstreams = host
            .as_ref()
            .and_then(|h| read(h.join("upstreams")))
            .map(|t| nameservers(&t))
            .unwrap_or_default();
        let servers: Vec<String> = if schizo {
            self.config.gateway.iter().cloned().collect()
        } else if self.edge_dns {
            losos_netd::edge::DNS
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        } else if !host_upstreams.is_empty() {
            host_upstreams
        } else {
            let mut all = Vec::new();
            for (name, link) in &self.links {
                if link.kind == Kind::Static || link.ipv4.is_none() {
                    continue;
                }
                if let Some(lease) = read(Path::new(DHCP_DIR).join(name)) {
                    for line in lease.lines() {
                        if let Some(dns) = line.strip_prefix("dns=") {
                            all.extend(dns.split_whitespace().map(str::to_owned));
                        }
                    }
                }
            }
            all
        };
        let text: String = servers
            .iter()
            .map(|s| format!("nameserver {s}\n"))
            .collect();
        if text != self.upstreams {
            let _ = std::fs::create_dir_all("/run/losos-adblock");
            let partial = format!("{UPSTREAMS}.new");
            if std::fs::write(&partial, &text)
                .and_then(|()| std::fs::rename(&partial, UPSTREAMS))
                .is_ok()
            {
                self.upstreams = text;
                info!(?servers, "DNS servers");
            }
        }
        let forced = schizo || self.edge_dns;
        if forced != Path::new(FORCED).exists() {
            let _ = if forced {
                std::fs::write(FORCED, "")
            } else {
                std::fs::remove_file(FORCED)
            };
        }
    }

    /// The firewall and routing for schizo mode on or off. False when it
    /// could not all be done yet, such as the gateway's link not being
    /// plugged in; the firewall half is done first, so until then the
    /// host's traffic goes nowhere rather than around Tor.
    fn apply_schizo(&mut self, on: bool) -> bool {
        let rules = if on {
            &self.config.nft_on
        } else {
            &self.config.nft_off
        };
        if let Some(rules) = rules {
            if !run_quiet("nft", &["-f", &rules.to_string_lossy()]) {
                return false;
            }
        }
        let table = self.config.schizo_table.to_string();
        let up = self.config.link("up").map(|l| l.name.clone());
        let tor = self.config.link("tor").map(|l| l.name.clone());
        ip(&["-4", "rule", "del", "pref", &table]);
        if on {
            // The edge's rules come ahead of these and would send the
            // host's traffic around Tor; its thread sees the mode next and
            // keeps them off (the firewall drops that path meanwhile too).
            crate::edge_run::teardown(&up);
            let (Some(gateway), Some(up), Some(tor)) = (self.config.gateway.clone(), up, tor)
            else {
                return false;
            };
            if !self.links.contains_key(&tor)
                || !ip(&[
                    "-4", "route", "replace", "default", "via", &gateway, "dev", &tor, "table",
                    &table,
                ])
            {
                return false;
            }
            ip(&[
                "-4", "rule", "add", "pref", &table, "iif", &up, "lookup", &table,
            ]);
        } else {
            ip(&["-4", "route", "flush", "table", &table]);
        }
        true
    }

    /// Keeps the configuration's programs running.
    fn supervise(&mut self) {
        for (command, child, since) in &mut self.running {
            if let Some(c) = child {
                if let Ok(Some(status)) = c.try_wait() {
                    warn!(?command, %status, "exited");
                    *child = None;
                    *since = Instant::now();
                }
            }
            if child.is_none() && since.elapsed() >= Duration::from_secs(1) {
                match Command::new(&command[0])
                    .args(&command[1..])
                    .stdin(Stdio::null())
                    .spawn()
                {
                    Ok(c) => *child = Some(c),
                    Err(e) => {
                        warn!(?command, error = %e, "cannot start");
                        *since = Instant::now();
                    }
                }
            }
        }
    }

    pub fn tick(&mut self) {
        self.discover();
        self.read_leases();
        let names: Vec<(String, Kind)> = self
            .links
            .iter()
            .map(|(n, l)| (n.clone(), l.kind))
            .collect();
        for (name, kind) in names {
            match kind {
                Kind::Ethernet => self.ethernet(&name),
                Kind::Wifi => self.wifi(&name),
                Kind::Static => {}
            }
        }
        self.follow_host();
        self.supervise();
    }

    pub fn stop_all(&mut self) {
        for link in self.links.values_mut() {
            stop(&mut link.dhcp);
            stop(&mut link.wpa);
        }
        for (_, child, _) in &mut self.running {
            stop(child);
        }
    }

    fn device_state(&self, link: &Link) -> &'static str {
        match link.kind {
            Kind::Ethernet if !link.carrier => "unavailable",
            Kind::Ethernet if link.ipv4.is_some() => "connected",
            Kind::Ethernet if link.held => "disconnected",
            Kind::Ethernet => "connecting (getting IP configuration)",
            Kind::Wifi if !self.radio || link.wpa.is_none() => "unavailable",
            Kind::Wifi => match &link.wifi {
                Some((_, _, true)) => "connected",
                Some(_) if link.carrier => "connecting (getting IP configuration)",
                Some(_) => "connecting (configuring)",
                None => "disconnected",
            },
            Kind::Static => "unmanaged",
        }
    }

    fn general_state(&self) -> &'static str {
        let cards = self.links.values().filter(|l| l.kind != Kind::Static);
        let mut connecting = false;
        for link in cards {
            match self.device_state(link) {
                "connected" => return "connected",
                s if s.starts_with("connecting") => connecting = true,
                _ => {}
            }
        }
        if connecting {
            "connecting"
        } else {
            "disconnected"
        }
    }

    fn active_on(&self, uuid: &str) -> Option<&str> {
        self.links.iter().find_map(|(name, link)| {
            matches!(&link.wifi, Some((u, _, _)) if u == uuid).then_some(name.as_str())
        })
    }

    fn first_wifi(&self, ifname: Option<&str>) -> Result<String, String> {
        self.links
            .iter()
            .find(|(name, link)| {
                link.kind == Kind::Wifi && ifname.is_none_or(|i| i == name.as_str())
            })
            .map(|(name, _)| name.clone())
            .ok_or_else(|| match ifname {
                Some(i) => format!("Device '{i}' not found."),
                None => "No Wi-Fi device found.".into(),
            })
    }
}

fn stop(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn spawn_dhcp(name: &str) -> Option<Child> {
    // udhcpc runs the hook with the event as its one argument; the hook is
    // this program, which writes the lease where the daemon reads it.
    // No host name is sent, in or out of schizo mode.
    Command::new("udhcpc")
        .args([
            "-f",
            "-i",
            name,
            "-s",
            "/run/losos-netd/dhcp-hook",
            "-t",
            "4",
            "-A",
            "3",
        ])
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| warn!(name, error = %e, "cannot start udhcpc"))
        .ok()
}

fn spawn_wpa(name: &str) -> Option<Child> {
    Command::new("wpa_supplicant")
        .args([
            "-i",
            name,
            "-D",
            "nl80211,wext",
            "-c",
            "/run/losos-netd/wpa_supplicant.conf",
        ])
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| warn!(name, error = %e, "cannot start wpa_supplicant"))
        .ok()
}

/// Has wpa_supplicant join `network`, and nothing else.
fn join(control: &Control, network: &Saved, schizo: bool) -> io::Result<()> {
    let _ = control.request("REMOVE_NETWORK all");
    let id = control.request("ADD_NETWORK")?.trim().to_owned();
    if id.parse::<u32>().is_err() {
        return Err(io::Error::other(format!("wpa_supplicant: {id}")));
    }
    for (key, value) in network.wpa_settings() {
        control.ok(&format!("SET_NETWORK {id} {key} {value}"))?;
    }
    if schizo {
        control.ok(&format!("SET_NETWORK {id} mac_addr 1"))?;
    }
    control.ok(&format!("SELECT_NETWORK {id}"))
}

/// The files and setup the daemon needs before its first look.
pub fn prepare() -> io::Result<()> {
    for dir in [
        "/run/losos-netd",
        DHCP_DIR,
        wpa::CONTROL_DIR,
        "/run/losos-adblock",
    ] {
        std::fs::create_dir_all(dir)?;
    }
    let exe = std::env::current_exe()?;
    let hook = "/run/losos-netd/dhcp-hook";
    std::fs::write(
        hook,
        format!("#!/bin/sh\nexec {} dhcp \"$@\"\n", exe.display()),
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(hook, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(
        "/run/losos-netd/wpa_supplicant.conf",
        format!("ctrl_interface={}\nupdate_config=0\n", wpa::CONTROL_DIR),
    )?;
    Ok(())
}

/// Takes in the networks the host had saved, once.
pub fn import(config: &Config) {
    let Some(host) = &config.host else { return };
    let dir = state_dir(config);
    let marker = dir.join(".imported");
    if marker.exists() {
        return;
    }
    let known = saved::load(&dir);
    for entry in std::fs::read_dir(host.join("import"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Some(network) = Saved::from_keyfile(&text) {
            if known
                .iter()
                .any(|k| k.uuid == network.uuid || k.ssid == network.ssid)
            {
                continue;
            }
            match saved::store(&dir, &network) {
                Ok(()) => info!(name = network.name, "took in a network the host had saved"),
                Err(e) => warn!(error = %e, "cannot save"),
            }
        }
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(marker, "");
}

/// The daemon's loop: a look every second, or sooner when asked.
pub fn run_loop(shared: Arc<Shared>) {
    loop {
        {
            let mut state = shared.state.lock().unwrap();
            let before = state.generation;
            state.tick();
            if state.generation != before {
                shared.changed.notify_all();
            }
            let _ = shared.changed.wait_timeout(state, TICK);
        }
    }
}

impl Shared {
    /// The host's link and the cards with an address, for mDNS; neither in
    /// schizo mode or before the mode is known.
    pub fn mdns_links(&self) -> (Option<String>, Vec<String>) {
        let state = self.state.lock().unwrap();
        if state.schizo != Some(false) {
            return (None, Vec::new());
        }
        let up = state.config.link("up").map(|l| l.name.clone());
        let cards = state
            .links
            .iter()
            .filter(|(_, l)| l.kind != Kind::Static && l.ipv4.is_some())
            .map(|(name, _)| name.clone())
            .collect();
        (up, cards)
    }
}

impl Shared {
    /// What the edge thread works from (edge_run.rs).
    pub fn edge_view(&self) -> crate::edge_run::View {
        let state = self.state.lock().unwrap();
        let state_dir = state
            .config
            .state_disk
            .as_ref()
            .map(|(_, path)| path.clone())
            .unwrap_or_else(|| PathBuf::from("/run/losos-netd"));
        crate::edge_run::View {
            known: state.schizo.is_some(),
            host: state.config.host.clone(),
            schizo: state.schizo_on(),
            gateway: state.config.gateway.clone(),
            up: state.config.link("up").map(|l| l.name.clone()),
            cards: state
                .links
                .iter()
                .filter(|(_, l)| l.kind != Kind::Static)
                .filter_map(|(name, l)| Some((name.clone(), l.ipv4.as_deref()?.parse().ok()?)))
                .collect(),
            nets: crate::edge_run::own_networks(),
            state: state_dir,
        }
    }

    /// Whether the forwarder sends names through the edge's tunnel.
    pub fn set_edge_dns(&self, on: bool) {
        let mut state = self.state.lock().unwrap();
        if state.edge_dns != on {
            state.edge_dns = on;
            state.follow_host();
        }
    }
}

pub fn shared(config: Config) -> Arc<Shared> {
    Arc::new(Shared {
        state: Mutex::new(State::new(config)),
        changed: Condvar::new(),
        edge: Arc::default(),
    })
}

/// Stops everything the daemon started; the power button's last act.
pub fn shutdown(shared: &Shared) {
    if let Ok(mut state) = shared.state.lock() {
        state.stop_all();
    }
}

/// Answers `losos-netd nmcli` on the control socket.
pub fn serve(shared: Arc<Shared>) -> io::Result<()> {
    let _ = std::fs::remove_file(CONTROL);
    let listener = UnixListener::bind(CONTROL)?;
    for stream in listener.incoming().flatten() {
        let shared = shared.clone();
        std::thread::spawn(move || {
            if let Err(e) = answer(&shared, stream) {
                debug!(error = %e, "a control connection");
            }
        });
    }
    Ok(())
}

/// A request is the arguments, each ended by a NUL, an empty one, then the
/// passwd-file's text. The answer is the exit status on a line, then what
/// to print: stdout for 0, the error for anything else.
fn answer(shared: &Shared, mut stream: UnixStream) -> io::Result<()> {
    let mut input = Vec::new();
    stream.read_to_end(&mut input)?;
    let (args_part, secret) = match input.windows(2).position(|w| w == b"\0\0") {
        Some(at) => (&input[..at + 1], &input[at + 2..]),
        None => (&input[..], &[][..]),
    };
    let args: Vec<String> = args_part
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    if args.first().map(String::as_str) == Some(crate::APPLY) {
        let (code, text) = apply(shared);
        return reply(&mut stream, code, &text);
    }
    if args.first().map(String::as_str) == Some(crate::EDGE) {
        return match args.get(1).map(String::as_str) {
            Some("status") => reply(&mut stream, 0, &format!("{}\n", shared.edge.status_json())),
            // Nothing while there is no file for the gateway: the host
            // then takes its copy away.
            Some("chain") => match shared.edge.chain_json() {
                Some(json) => reply(&mut stream, 0, &format!("{json}\n")),
                None => reply(&mut stream, 3, "no edge to leave Tor through"),
            },
            _ => reply(&mut stream, 2, "usage: losos-netd edge status|chain"),
        };
    }
    let command = match nmcli::parse(&args) {
        Ok(c) => c,
        Err(e) => return reply(&mut stream, 2, &e),
    };
    if command.request == Request::Monitor {
        return monitor(shared, stream);
    }
    let secret = String::from_utf8_lossy(secret).into_owned();
    match handle(shared, &command, &secret) {
        Ok(text) => reply(&mut stream, 0, &text),
        Err((code, text)) => reply(&mut stream, code, &text),
    }
}

/// `losos-netd apply`: a look now, and whether schizo mode is then what the
/// host asks for. The host's schizo service fails, and says so, until it is.
fn apply(shared: &Shared) -> (i32, String) {
    let mut state = shared.state.lock().unwrap();
    state.tick();
    let wanted = state
        .config
        .host
        .as_ref()
        .is_some_and(|h| h.join("schizo").exists());
    if state.schizo == Some(wanted) {
        (0, String::new())
    } else {
        (
            1,
            format!(
                "schizo mode is not {} yet: the gateway's link is not here",
                if wanted { "on" } else { "off" }
            ),
        )
    }
}

fn reply(stream: &mut UnixStream, code: i32, text: &str) -> io::Result<()> {
    stream.write_all(format!("{code}\n{text}").as_bytes())
}

fn monitor(shared: &Shared, mut stream: UnixStream) -> io::Result<()> {
    stream.write_all(b"0\n")?;
    let mut seen = shared.state.lock().unwrap().generation;
    loop {
        let lines: Vec<String> = {
            let state = shared.state.lock().unwrap();
            let (state, _) = shared
                .changed
                .wait_timeout_while(state, Duration::from_secs(30), |s| s.generation == seen)
                .unwrap();
            let new = (state.generation - seen) as usize;
            seen = state.generation;
            let start = state.changes.len().saturating_sub(new);
            state.changes[start..].to_vec()
        };
        for line in lines {
            stream.write_all(format!("{line}\n").as_bytes())?;
        }
        // A caller that went away is found by the next write.
        stream.flush()?;
    }
}

type Answer = Result<String, (i32, String)>;

fn fail(code: i32, text: impl Into<String>) -> (i32, String) {
    (code, text.into())
}

fn handle(shared: &Shared, command: &nmcli::Command, secret: &str) -> Answer {
    let out = &command.output;
    let mut state = shared.state.lock().unwrap();
    let text =
        |rows: &[Row], default: &[&str]| nmcli::format(rows, out, default).map_err(|e| fail(2, e));
    match &command.request {
        Request::General => {
            let row: Row = vec![
                ("RUNNING", "running".into()),
                ("STATE", state.general_state().into()),
                (
                    "CONNECTIVITY",
                    if state.general_state() == "connected" {
                        "full"
                    } else {
                        "none"
                    }
                    .into(),
                ),
                ("NETWORKING", "enabled".into()),
                ("WIFI-HW", "enabled".into()),
                (
                    "WIFI",
                    if state.radio { "enabled" } else { "disabled" }.into(),
                ),
            ];
            text(&[row], &["STATE", "CONNECTIVITY", "WIFI"])
        }
        Request::Devices => {
            let rows: Vec<Row> = state
                .links
                .iter()
                .filter(|(_, l)| l.kind != Kind::Static)
                .map(|(name, link)| {
                    let connection = match &link.wifi {
                        Some((uuid, _, _)) => state
                            .all_saved()
                            .find(|s| &s.uuid == uuid)
                            .map(|s| s.name.clone())
                            .unwrap_or_default(),
                        None if link.ipv4.is_some() => format!("Wired connection {name}"),
                        None => String::new(),
                    };
                    vec![
                        ("DEVICE", name.clone()),
                        (
                            "TYPE",
                            if link.kind == Kind::Wifi {
                                "wifi"
                            } else {
                                "ethernet"
                            }
                            .into(),
                        ),
                        ("STATE", state.device_state(link).into()),
                        ("CONNECTION", connection),
                    ]
                })
                .collect();
            text(&rows, &["DEVICE", "TYPE", "STATE", "CONNECTION"])
        }
        Request::WifiRescan { ifname } => {
            let name = state
                .first_wifi(ifname.as_deref())
                .map_err(|e| fail(10, e))?;
            if let Some(link) = state.links.get_mut(&name) {
                link.last_scan = None;
            }
            drop(state);
            shared.changed.notify_all();
            if let Ok(control) = Control::open(&name) {
                let _ = control.request("SCAN");
            }
            Ok(String::new())
        }
        Request::WifiList { ifname, rescan } => {
            let name = state
                .first_wifi(ifname.as_deref())
                .map_err(|e| fail(10, e))?;
            if *rescan {
                if let Ok(control) = Control::open(&name) {
                    let _ = control.request("SCAN");
                }
            }
            let link = &state.links[&name];
            let current = link
                .wifi
                .as_ref()
                .filter(|(_, _, j)| *j)
                .and_then(|(uuid, _, _)| {
                    state
                        .all_saved()
                        .find(|s| &s.uuid == uuid)
                        .map(|s| s.ssid.clone())
                });
            let mut aps = link.scan.clone();
            aps.sort_by_key(|a| std::cmp::Reverse(a.signal));
            let rows: Vec<Row> = aps
                .iter()
                .map(|ap| {
                    let in_use = current.as_ref() == Some(&ap.ssid);
                    vec![
                        ("IN-USE", if in_use { "*" } else { " " }.into()),
                        ("BSSID", ap.bssid.to_uppercase()),
                        ("SSID", String::from_utf8_lossy(&ap.ssid).into_owned()),
                        ("SSID-HEX", nmcli::hex(&ap.ssid)),
                        ("MODE", "Infra".into()),
                        ("CHAN", wpa::channel(ap.frequency).to_string()),
                        ("FREQ", format!("{} MHz", ap.frequency)),
                        ("RATE", String::new()),
                        ("SIGNAL", ap.signal.to_string()),
                        ("BARS", nmcli::bars(ap.signal).into()),
                        ("SECURITY", ap.security.clone()),
                        ("DEVICE", name.clone()),
                    ]
                })
                .collect();
            text(
                &rows,
                &[
                    "IN-USE", "BSSID", "SSID", "MODE", "CHAN", "RATE", "SIGNAL", "BARS", "SECURITY",
                ],
            )
        }
        Request::Connections => {
            let rows: Vec<Row> = state
                .all_saved()
                .map(|s| {
                    let device = state.active_on(&s.uuid).unwrap_or("").to_owned();
                    vec![
                        ("NAME", s.name.clone()),
                        ("UUID", s.uuid.clone()),
                        ("TYPE", "802-11-wireless".into()),
                        ("DEVICE", device.clone()),
                        (
                            "ACTIVE",
                            if device.is_empty() { "no" } else { "yes" }.into(),
                        ),
                    ]
                })
                .collect();
            text(&rows, &["NAME", "UUID", "TYPE", "DEVICE"])
        }
        Request::Connection { key } => {
            let s = state
                .find(key)
                .ok_or_else(|| fail(10, "no such connection profile."))?;
            let row: Row = vec![
                ("connection.id", s.name.clone()),
                ("connection.uuid", s.uuid.clone()),
                ("connection.type", "802-11-wireless".into()),
                (
                    "802-11-wireless.ssid",
                    String::from_utf8_lossy(&s.ssid).into_owned(),
                ),
                ("802-11-wireless-security.key-mgmt", s.key_mgmt.clone()),
            ];
            let mut out = out.clone();
            // `-g` of one property prints its value alone, unescaped, as
            // nmcli does for a setting.
            if out.values_only && out.fields.as_ref().is_some_and(|f| f.len() == 1) {
                let field = &out.fields.as_ref().unwrap()[0];
                return row
                    .iter()
                    .find(|(k, _)| k == field)
                    .map(|(_, v)| format!("{v}\n"))
                    .ok_or_else(|| fail(2, format!("invalid field '{field}'")));
            }
            out.terse = out.terse || out.values_only;
            nmcli::format(
                &[row],
                &out,
                &[
                    "connection.id",
                    "connection.uuid",
                    "connection.type",
                    "802-11-wireless.ssid",
                ],
            )
            .map_err(|e| fail(2, e))
        }
        Request::Add(add) => {
            let Add {
                name,
                ssid,
                save,
                key_mgmt,
                pmf,
                psk,
                ..
            } = add;
            let network = Saved {
                uuid: new_uuid(),
                name: name.clone().unwrap_or_else(|| ssid.clone()),
                ssid: ssid.as_bytes().to_vec(),
                key_mgmt: key_mgmt.clone().unwrap_or_else(|| "none".into()),
                pmf: pmf.clone().unwrap_or_default(),
                psk: psk.clone().unwrap_or_default(),
            };
            if *save {
                saved::store(&state_dir(&state.config), &network)
                    .map_err(|e| fail(4, format!("cannot save the connection: {e}")))?;
                state.saved.push(network.clone());
            } else {
                state.unsaved.push(network.clone());
            }
            state.changed(format!("{}: added", network.name));
            Ok(format!(
                "Connection '{}' ({}) successfully added.\n",
                network.name, network.uuid
            ))
        }
        Request::Delete { key } => {
            let s = state
                .find(key)
                .ok_or_else(|| fail(10, "no such connection profile."))?;
            if let Some(device) = state.active_on(&s.uuid).map(str::to_owned) {
                disconnect(&mut state, &device);
            }
            saved::remove(&state_dir(&state.config), &s.uuid)
                .map_err(|e| fail(4, format!("cannot delete the connection: {e}")))?;
            state.saved.retain(|x| x.uuid != s.uuid);
            state.unsaved.retain(|x| x.uuid != s.uuid);
            state.changed(format!("{}: deleted", s.name));
            Ok(format!(
                "Connection '{}' ({}) successfully deleted.\n",
                s.name, s.uuid
            ))
        }
        Request::Down { key } => {
            let s = state
                .find(key)
                .ok_or_else(|| fail(10, "no such connection profile."))?;
            let device = state
                .active_on(&s.uuid)
                .map(str::to_owned)
                .ok_or_else(|| fail(10, format!("'{}' is not an active connection.", s.name)))?;
            disconnect(&mut state, &device);
            if let Some(link) = state.links.get_mut(&device) {
                link.held = true;
            }
            Ok(format!(
                "Connection '{}' successfully deactivated.\n",
                s.name
            ))
        }
        Request::DeviceConnect { device, connect } => {
            if !state.links.contains_key(device) {
                return Err(fail(10, format!("Device '{device}' not found.")));
            }
            if !connect {
                disconnect(&mut state, device);
            }
            if let Some(link) = state.links.get_mut(device) {
                link.held = !connect;
            }
            let what = format!(
                "{device}: {}",
                if *connect { "connect" } else { "disconnect" }
            );
            state.changed(what);
            Ok(format!(
                "Device '{device}' successfully {}.\n",
                if *connect {
                    "activated"
                } else {
                    "disconnected"
                }
            ))
        }
        Request::Radio { on } => match on {
            None => Ok(format!(
                "{}\n",
                if state.radio { "enabled" } else { "disabled" }
            )),
            Some(on) => {
                state.radio = *on;
                state.changed(format!("Wi-Fi radio {}", if *on { "on" } else { "off" }));
                Ok(String::new())
            }
        },
        Request::Up { key, passwd_file } => {
            let mut s = state
                .find(key)
                .ok_or_else(|| fail(10, "no such connection profile."))?;
            if passwd_file.is_some() {
                if let Some(psk) = nmcli::passwd_file_psk(secret) {
                    s.psk = psk;
                    if state.saved.iter().any(|x| x.uuid == s.uuid) {
                        saved::store(&state_dir(&state.config), &s)
                            .map_err(|e| fail(4, format!("cannot save the connection: {e}")))?;
                    }
                    let state = &mut *state;
                    for list in [&mut state.saved, &mut state.unsaved] {
                        for x in list.iter_mut().filter(|x| x.uuid == s.uuid) {
                            x.psk = s.psk.clone();
                        }
                    }
                }
            }
            let device = state.first_wifi(None).map_err(|e| fail(10, e))?;
            let schizo = state.schizo_on();
            state.failed.remove(&s.uuid);
            let control = Control::open(&device).map_err(|_| {
                fail(
                    4,
                    "Connection activation failed: the Wi-Fi card is not ready",
                )
            })?;
            join(&control, &s, schizo)
                .map_err(|e| fail(4, format!("Connection activation failed: {e}")))?;
            if let Some(link) = state.links.get_mut(&device) {
                link.wifi = Some((s.uuid.clone(), Instant::now(), false));
                link.held = false;
            }
            let what = format!("{device}: joining {}", s.name);
            state.changed(what);
            drop(state);
            let deadline = Instant::now() + Duration::from_secs(command.wait.unwrap_or(90) as u64);
            loop {
                {
                    let state = shared.state.lock().unwrap();
                    match state.links.get(&device).and_then(|l| l.wifi.clone()) {
                        Some((uuid, _, true)) if uuid == s.uuid => {
                            return Ok(
                                "Connection successfully activated.\n".to_owned()
                            )
                        }
                        Some((uuid, _, _)) if uuid == s.uuid => {}
                        _ => {
                            return Err(fail(
                                4,
                                "Connection activation failed: (7) Secrets were required, but not provided.",
                            ))
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return Err(fail(3, "Timeout expired"));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        Request::Monitor => unreachable!("answered before"),
    }
}

fn disconnect(state: &mut State, device: &str) {
    if let Some(link) = state.links.get_mut(device) {
        if link.kind == Kind::Wifi {
            if let Ok(control) = Control::open(device) {
                let _ = control.ok("DISCONNECT");
                let _ = control.request("REMOVE_NETWORK all");
            }
            link.wifi = None;
        }
        stop(&mut link.dhcp);
        ip(&["-4", "addr", "flush", "dev", device]);
        let _ = std::fs::remove_file(Path::new(DHCP_DIR).join(device));
        link.carrier = link.kind == Kind::Ethernet && link.carrier;
    }
}

/// udhcpc's hook: applies a lease and writes it down for the daemon.
pub fn dhcp_hook(event: &str) -> io::Result<()> {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let iface = env("interface");
    if iface.is_empty() || iface.contains('/') {
        return Err(io::Error::other("no interface"));
    }
    let file = Path::new(DHCP_DIR).join(&iface);
    match event {
        "deconfig" => {
            ip(&["-4", "addr", "flush", "dev", &iface]);
            ip(&["link", "set", "dev", &iface, "up"]);
            let _ = std::fs::remove_file(&file);
        }
        "bound" | "renew" => {
            let address = format!("{}/{}", env("ip"), env("mask"));
            ip(&["-4", "addr", "flush", "dev", &iface]);
            ip(&["-4", "addr", "add", &address, "dev", &iface]);
            let metric = if Path::new(&format!("/sys/class/net/{iface}/wireless")).exists() {
                "600"
            } else {
                "100"
            };
            for router in env("router").split_whitespace().take(1) {
                ip(&[
                    "-4", "route", "replace", "default", "via", router, "dev", &iface, "metric",
                    metric,
                ]);
            }
            let partial = file.with_extension("new");
            std::fs::write(
                &partial,
                format!(
                    "ip={}\nrouter={}\ndns={}\n",
                    env("ip"),
                    env("router"),
                    env("dns")
                ),
            )?;
            std::fs::rename(&partial, &file)?;
        }
        _ => {}
    }
    Ok(())
}

/// Reads the power button and powers the machine off when it is pressed,
/// which is how libvirt asks a machine to shut down.
pub fn power_button(shared: Arc<Shared>) {
    loop {
        let device = std::fs::read_dir("/sys/class/input")
            .ok()
            .and_then(|entries| {
                entries.flatten().find_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let label = read(e.path().join("device/name"))?;
                    (name.starts_with("event") && label.contains("Power Button"))
                        .then(|| PathBuf::from("/dev/input").join(name))
                })
            });
        let Some(device) = device else {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        let Ok(mut file) = std::fs::File::open(&device) else {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        info!(device = %device.display(), "watching the power button");
        // struct input_event: a timeval, then type, code and value.
        let size = std::mem::size_of::<libc::timeval>() + 8;
        let mut event = vec![0u8; size];
        while file.read_exact(&mut event).is_ok() {
            let at = std::mem::size_of::<libc::timeval>();
            let kind = u16::from_ne_bytes([event[at], event[at + 1]]);
            let code = u16::from_ne_bytes([event[at + 2], event[at + 3]]);
            let value =
                i32::from_ne_bytes([event[at + 4], event[at + 5], event[at + 6], event[at + 7]]);
            // EV_KEY, KEY_POWER, pressed.
            if kind == 1 && code == 116 && value == 1 {
                info!("the power button: powering off");
                shutdown(&shared);
                crate::power_off();
            }
        }
    }
}

/// Lines of `nmcli monitor` from the daemon, for the client.
pub fn read_lines(stream: UnixStream, mut each: impl FnMut(&str) -> bool) {
    for line in io::BufReader::new(stream).lines() {
        match line {
            Ok(line) if each(&line) => {}
            _ => break,
        }
    }
}
