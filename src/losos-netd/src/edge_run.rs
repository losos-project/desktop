//! The daemon's edge thread (docs/network-machine.md, "Through an
//! edge"; the pure half is losos_netd::edge): while the person has set an
//! edge, it looks for one the way a LosOS box does, enrols this machine at
//! the exit, and sends the host's traffic there.
//!
//! Outside schizo mode the exit is a WireGuard tunnel. The host's traffic
//! takes a routing table of its own whose only way out is the tunnel, so
//! while it is down nothing the host sends leaves at all, the way a VPN's
//! kill switch works; the person's own networks stay in reach. In schizo
//! mode the tunnel is down and the rules gone, and the enrolment's onion
//! address and SOCKS account go to the host, which hands them to the Tor
//! gateway: Tor carries the connection to the edge, and the edge makes it.
//! Everything this thread asks the edges then goes through Tor too.

use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use losos_edge_path::{
    assemble_with, bracketed_host, choose_path, host_of, parse_described, parse_if_inet6,
    parse_proc_route, Described, Net, PathRule, Source,
};
use losos_netd::dnssd;
use losos_netd::edge::{
    exit_addresses_ok, exit_url, Account, Enrolled, Status, DNS, LINK, PREF_EDGE, PREF_LOCAL,
    TABLE, TOR_SOCKS_PORT,
};
use serde_json::Value;
use tracing::{info, warn};

use crate::daemon::{run_quiet, Shared};

/// How long a look waits on any one edge.
const ASK: Duration = Duration::from_secs(10);
/// How long the browse listens for answers.
const BROWSE: Duration = Duration::from_secs(2);
/// A new look while the exit is up, and while it is not.
const LOOK_UP: Duration = Duration::from_secs(300);
const LOOK_DOWN: Duration = Duration::from_secs(20);
/// A WireGuard handshake older than this means the tunnel is gone.
const STALE: u64 = 180;

/// What the host asks for over `losos-netd edge`: the status, and the
/// Tor gateway's file while schizo mode leaves through the edge.
pub struct Edge {
    status: Mutex<Status>,
    chain: Mutex<Option<Value>>,
}

impl Default for Edge {
    /// Nothing set up, which is also how the machine starts.
    fn default() -> Self {
        Edge {
            status: Mutex::new(Status {
                state: "none",
                ..Status::default()
            }),
            chain: Mutex::default(),
        }
    }
}

impl Edge {
    pub fn status_json(&self) -> String {
        serde_json::to_string(&*self.status.lock().unwrap()).unwrap_or_default()
    }

    pub fn chain_json(&self) -> Option<String> {
        self.chain
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
    }

    fn set(&self, status: Status) {
        let mut now = self.status.lock().unwrap();
        if *now != status {
            info!(
                state = status.state,
                exit = ?status.exit,
                reason = ?status.reason,
                "edge"
            );
            *now = status;
        }
    }
}

/// What this thread knows between looks.
#[derive(Default)]
struct Current {
    /// The rule's last path, which it keeps while that edge answers.
    path: Option<String>,
    exit: Option<String>,
    enrolled: Option<Enrolled>,
    /// The tunnel is configured for `enrolled`.
    up: bool,
    schizo: Option<bool>,
    next: Option<Instant>,
}

pub fn run(shared: Arc<Shared>, edge: Arc<Edge>) {
    let mut current = Current::default();
    loop {
        pass(&shared, &edge, &mut current);
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn pass(shared: &Shared, edge: &Edge, current: &mut Current) {
    let view = shared.edge_view();
    // Not before the mode is known: in schizo mode even this machine's own
    // enrolment must not leave around Tor.
    if !view.known {
        return;
    }
    let account = view
        .host
        .as_ref()
        .and_then(|h| std::fs::read_to_string(h.join("edge")).ok())
        .map(|t| Account::parse(&t));
    let account = match account {
        None => {
            if edge.status.lock().unwrap().state == "none" {
                return;
            }
            info!("no edge set");
            teardown(&view.up);
            shared.set_edge_dns(false);
            *edge.chain.lock().unwrap() = None;
            *current = Current::default();
            edge.set(Status {
                state: "none",
                ..Status::default()
            });
            return;
        }
        Some(Err(e)) => {
            kill_switch(&view, view.schizo);
            shared.set_edge_dns(false);
            edge.set(blocked(format!("the edge setting: {e}")));
            return;
        }
        Some(Ok(a)) => a,
    };

    // The mode changed: what was set up belongs to the other one.
    if current.schizo != Some(view.schizo) {
        teardown(&view.up);
        *edge.chain.lock().unwrap() = None;
        *current = Current {
            schizo: Some(view.schizo),
            ..Current::default()
        };
    }
    if view.schizo {
        // The gateway's link carries everything; the tunnel would be a way
        // around Tor.
        shared.set_edge_dns(false);
    } else {
        kill_switch(&view, false);
        if current.up && !handshake_fresh() {
            warn!("the tunnel to the edge has had no handshake for minutes");
            current.up = false;
            current.next = None;
            route_nowhere();
        }
    }
    if current.next.is_some_and(|t| Instant::now() < t) {
        return;
    }
    let look = look(&view, &account, current);
    let up = look.is_ok();
    // Names go through the tunnel once it is up. Until then the forwarder
    // keeps the network's servers, which is how this machine finds the
    // edge at all; the host's traffic goes nowhere meanwhile.
    shared.set_edge_dns(up && !view.schizo);
    current.next = Some(Instant::now() + if up { LOOK_UP } else { LOOK_DOWN });
    match look {
        Ok(status) => edge.set(status),
        Err(status) => {
            if !view.schizo {
                route_nowhere();
                current.up = false;
            }
            edge.set(*status)
        }
    }
    // The gateway keeps the enrolment it has while a look fails: its chain
    // refuses what the edge does not take, and never falls back to Tor's
    // exits.
    if view.schizo {
        *edge.chain.lock().unwrap() = current.enrolled.as_ref().and_then(Enrolled::chain_file);
    }
}

fn blocked(reason: String) -> Status {
    Status {
        state: "blocked",
        reason: Some(reason),
        ..Status::default()
    }
}

/// One look: the rule's edge, its exit, the enrolment and the tunnel.
fn look(view: &View, account: &Account, current: &mut Current) -> Result<Status, Box<Status>> {
    let agent = agent(view)?;
    let get = |url: &str| -> Option<String> {
        agent
            .get(url)
            .call()
            .ok()
            .and_then(|mut r| r.body_mut().read_to_string().ok())
    };
    let answered = |url: &str| get(&format!("{url}/health")).is_some();
    let describe = |url: &str| -> Option<Described> {
        parse_described(&get(&format!("{url}/cluster/secondary"))?)
    };

    // No LAN in schizo mode: nothing of the PC's reaches it.
    let adverts = (!view.schizo).then(|| browse(&view.cards));
    // The rule takes only an edge it could tunnel to; the configured one's
    // endpoint is not this machine's business, so any will do.
    let configured_tunnel = account
        .url
        .as_deref()
        .map(|u| format!("{}:443", bracketed_host(u)));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let found = assemble_with(
        adverts.as_deref(),
        account.url.as_deref(),
        configured_tunnel.as_deref(),
        answered,
        describe,
        |_| None,
        now,
    );
    let chosen = choose_path(
        &found.edges,
        &PathRule {
            current: current.path.as_deref(),
            seed: &account.id,
            ..PathRule::default()
        },
    );
    let Some(path) = chosen.path else {
        return Err(Box::new(Status {
            state: if current.up { "blocked" } else { "searching" },
            reason: Some(match &account.url {
                Some(u) => format!("no edge answered, not even {u}"),
                None => "no edge answered on this network, and none is set".to_owned(),
            }),
            ..Status::default()
        }));
    };
    current.path = Some(path.url.clone());
    let mut status = Status {
        state: "searching",
        edge: Some(path.url.clone()),
        source: Some(match path.source {
            Source::Lan => "lan",
            Source::Configured => "configured",
        }),
        tier: Some(path.tier.as_str()),
        ..Status::default()
    };
    let fail = |status: &Status, reason: String| {
        Box::new(Status {
            state: "blocked",
            reason: Some(reason),
            ..status.clone()
        })
    };

    let officials: Vec<String> = describe(&path.url)
        .map(|d| d.officials.into_iter().map(|o| o.url).collect())
        .unwrap_or_default();
    let officials: Vec<String> = officials.into_iter().filter(|u| answered(u)).collect();
    let Some(exit) = exit_url(&path, &officials, &account.id) else {
        return Err(fail(
            &status,
            format!(
                "{} is a gateway that lists no edge on the internet",
                path.url
            ),
        ));
    };
    status.exit = Some(exit.clone());
    // Through Tor every name resolves to Tor's own stand-in, so what the
    // exit is is the rule's word: an edge on the internet, never a gateway.
    if !view.schizo {
        exit_addresses_ok("the exit", &resolve(&host_of(&exit)), &view.nets)
            .map_err(|e| fail(&status, e))?;
    }

    if current.exit.as_deref() != Some(exit.as_str()) || current.enrolled.is_none() {
        current.up = false;
        let key = public_key(&view.state).map_err(|e| fail(&status, e))?;
        let answer = agent
            .post(&format!("{exit}/exit"))
            .send_json(serde_json::json!({
                "appliance_id": account.id,
                "token": account.token,
                "public_key": key,
            }))
            .map_err(|e| fail(&status, refusal(&exit, e)))?
            .body_mut()
            .read_json::<Enrolled>()
            .map_err(|e| fail(&status, format!("{exit} answered something else: {e}")))?;
        info!(%exit, address = answer.address, "enrolled at the edge");
        current.exit = Some(exit.clone());
        current.enrolled = Some(answer);
    }
    let enrolled = current.enrolled.clone().expect("enrolled above");

    if view.schizo {
        if enrolled.chain_file().is_none() {
            return Err(fail(
                &status,
                format!("{exit} has no onion address to reach it through Tor"),
            ));
        }
        status.state = "up";
        status.via = Some("onion");
        return Ok(status);
    }

    let Some((host, port)) = enrolled.endpoint_parts() else {
        return Err(fail(&status, format!("{exit} gave no endpoint")));
    };
    let addresses = resolve(&host);
    exit_addresses_ok("the exit's endpoint", &addresses, &view.nets)
        .map_err(|e| fail(&status, e))?;
    if !current.up {
        // IPv4: this machine has no IPv6 of its own (docs/not-done.md).
        let Some(address) = addresses.iter().find(|a| a.is_ipv4()) else {
            return Err(fail(&status, format!("{host} has no IPv4 address")));
        };
        tunnel_up(&view.state, &enrolled, *address, port).map_err(|e| fail(&status, e))?;
        current.up = true;
    }
    status.state = "up";
    status.via = Some("wireguard");
    Ok(status)
}

/// What the edge said when it refused.
fn refusal(exit: &str, e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(403) => {
            format!("{exit} does not let this account out (ask its operator to mark it `exit`)")
        }
        ureq::Error::StatusCode(401) => format!("{exit} does not know this id and token"),
        ureq::Error::StatusCode(503) => format!("{exit} has no room on its exit, or none"),
        e => format!("cannot enrol at {exit}: {e}"),
    }
}

/// The client for the edges: straight out, or in schizo mode through Tor's
/// SOCKS on the gateway, which resolves the names too.
fn agent(view: &View) -> Result<ureq::Agent, Box<Status>> {
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(ASK))
        .user_agent("losos-netd");
    if view.schizo {
        let Some(gateway) = &view.gateway else {
            return Err(Box::new(blocked("schizo mode has no gateway".to_owned())));
        };
        let proxy = ureq::Proxy::new(&format!("socks5h://{gateway}:{TOR_SOCKS_PORT}"))
            .map_err(|e| Box::new(blocked(format!("Tor's SOCKS: {e}"))))?;
        config = config.proxy(Some(proxy));
    }
    Ok(config.build().into())
}

/// Every address `host` resolves to, through this machine's resolver.
fn resolve(host: &str) -> Vec<IpAddr> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse() {
        return vec![ip];
    }
    (host, 0)
        .to_socket_addrs()
        .map(|it| it.map(|a| a.ip()).collect())
        .unwrap_or_default()
}

/// One mDNS question for the edges on each card's network, and whatever
/// answers arrive within [`BROWSE`].
fn browse(cards: &[(String, std::net::Ipv4Addr)]) -> Vec<losos_edge_path::Advert> {
    let mut answers = dnssd::Answers::default();
    let query = dnssd::query();
    for (name, address) in cards {
        let Ok(socket) = UdpSocket::bind((*address, 0)) else {
            continue;
        };
        if let Err(e) = socket
            .set_multicast_if_v4(address)
            .and_then(|()| socket.send_to(&query, ("224.0.0.251", 5353)).map(|_| ()))
        {
            warn!(card = name, error = %e, "cannot look for edges");
            continue;
        }
        let deadline = Instant::now() + BROWSE;
        let mut packet = [0u8; 9000];
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if socket
                .set_read_timeout(Some(left.max(Duration::from_millis(1))))
                .is_err()
            {
                break;
            }
            match socket.recv_from(&mut packet) {
                Ok((n, _)) => answers.read(&packet[..n]),
                Err(_) => break,
            }
        }
    }
    answers.adverts()
}

trait UdpExt {
    fn set_multicast_if_v4(&self, address: &std::net::Ipv4Addr) -> std::io::Result<()>;
}

impl UdpExt for UdpSocket {
    fn set_multicast_if_v4(&self, address: &std::net::Ipv4Addr) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let addr = libc::in_addr {
            s_addr: u32::from(*address).to_be(),
        };
        // SAFETY: a plain setsockopt with a local of the size passed.
        let ret = unsafe {
            libc::setsockopt(
                self.as_raw_fd(),
                libc::IPPROTO_IP,
                libc::IP_MULTICAST_IF,
                (&addr as *const libc::in_addr).cast(),
                std::mem::size_of::<libc::in_addr>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// This machine's WireGuard key, made once on its state disk, and its
/// public half. The private one never leaves the machine.
fn public_key(state: &Path) -> Result<String, String> {
    let file = state.join("edge.key");
    if !file.exists() {
        let key = output("wg", &["genkey"], None)?;
        let _ = std::fs::create_dir_all(state);
        let partial = state.join("edge.key.new");
        std::fs::write(&partial, format!("{key}\n"))
            .and_then(|()| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600))
            })
            .and_then(|()| std::fs::rename(&partial, &file))
            .map_err(|e| format!("cannot keep the WireGuard key: {e}"))?;
    }
    let key = std::fs::read(&file).map_err(|e| format!("cannot read the WireGuard key: {e}"))?;
    output("wg", &["pubkey"], Some(&key))
}

fn output(program: &str, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if let Some(input) = input {
        use std::io::Write;
        let _ = child.stdin.take().map(|mut s| s.write_all(input));
    }
    drop(child.stdin.take());
    let mut text = String::new();
    let _ = child.stdout.take().map(|mut s| s.read_to_string(&mut text));
    match child.wait() {
        Ok(s) if s.success() => Ok(text.trim().to_owned()),
        _ => Err(format!("{program} {} failed", args.join(" "))),
    }
}

fn ip(args: &[&str]) -> bool {
    run_quiet("ip", args)
}

/// The tunnel, made afresh for `enrolled`, and the host's table pointed
/// into it.
fn tunnel_up(state: &Path, enrolled: &Enrolled, address: IpAddr, port: u16) -> Result<(), String> {
    ip(&["link", "del", LINK]);
    if !ip(&["link", "add", LINK, "type", "wireguard"]) {
        return Err("cannot make the WireGuard link (no wireguard module?)".to_owned());
    }
    let endpoint = match address {
        IpAddr::V4(a) => format!("{a}:{port}"),
        IpAddr::V6(a) => format!("[{a}]:{port}"),
    };
    let key = state.join("edge.key");
    let allowed = enrolled.allowed_ips.join(",");
    let ok = run_quiet(
        "wg",
        &[
            "set",
            LINK,
            "private-key",
            &key.to_string_lossy(),
            "peer",
            &enrolled.server_public_key,
            "endpoint",
            &endpoint,
            "allowed-ips",
            &allowed,
            "persistent-keepalive",
            "25",
        ],
    ) && ip(&["-4", "addr", "replace", &enrolled.address, "dev", LINK])
        // Loose: the edge's answers come back on the tunnel from addresses
        // the main table routes out of a card, which strict checking drops.
        && std::fs::write(format!("/proc/sys/net/ipv4/conf/{LINK}/rp_filter"), "2").is_ok()
        && ip(&["link", "set", LINK, "up"])
        && ip(&[
            "-4",
            "route",
            "replace",
            "default",
            "dev",
            LINK,
            "table",
            &TABLE.to_string(),
        ]);
    if ok {
        info!(%endpoint, "the tunnel to the edge is up");
        Ok(())
    } else {
        ip(&["link", "del", LINK]);
        Err("cannot set the tunnel up".to_owned())
    }
}

/// The host's traffic goes nowhere until the tunnel is back.
fn route_nowhere() {
    ip(&["link", "del", LINK]);
    ip(&[
        "-4",
        "route",
        "replace",
        "unreachable",
        "default",
        "table",
        &TABLE.to_string(),
    ]);
}

/// The rules that send the host's traffic to the edge's table: its own
/// networks first, through the main table without its default route, then
/// everything else, and this machine's forwarder's servers, into the table.
/// sys-vpn's uplink takes the same way as the host (docs/vpn.md): with a
/// VPN set, its tunnel rides the edge's. The table starts out nowhere.
fn kill_switch(view: &View, schizo: bool) {
    if schizo {
        return;
    }
    let Some(up) = &view.up else { return };
    let table = TABLE.to_string();
    let rules_present = Command::new("ip")
        .args(["-4", "rule", "show", "pref", &PREF_EDGE.to_string()])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if rules_present {
        return;
    }
    ip(&[
        "-4",
        "route",
        "add",
        "unreachable",
        "default",
        "table",
        &table,
    ]);
    let local = PREF_LOCAL.to_string();
    let edge = PREF_EDGE.to_string();
    let from: Vec<&String> = std::iter::once(up).chain(&view.vpnext).collect();
    for link in &from {
        ip(&[
            "-4",
            "rule",
            "add",
            "pref",
            &local,
            "iif",
            link,
            "lookup",
            "main",
            "suppress_prefixlength",
            "0",
        ]);
    }
    for server in DNS {
        ip(&[
            "-4", "rule", "add", "pref", &edge, "to", server, "lookup", &table,
        ]);
    }
    for link in &from {
        ip(&[
            "-4", "rule", "add", "pref", &edge, "iif", link, "lookup", &table,
        ]);
    }
}

/// Everything of the edge's undone: the rules, the table and the tunnel.
pub fn teardown(_up: &Option<String>) {
    for pref in [PREF_LOCAL, PREF_EDGE] {
        let pref = pref.to_string();
        while ip(&["-4", "rule", "del", "pref", &pref]) {}
    }
    ip(&["-4", "route", "flush", "table", &TABLE.to_string()]);
    ip(&["link", "del", LINK]);
}

/// The last handshake with the edge is recent, or the tunnel is too new to
/// have one yet.
fn handshake_fresh() -> bool {
    let Ok(text) = output("wg", &["show", LINK, "latest-handshakes"], None) else {
        return false;
    };
    let Some(at) = text
        .split_whitespace()
        .nth(1)
        .and_then(|t| t.parse::<u64>().ok())
    else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    at == 0 || now.saturating_sub(at) < STALE
}

/// What the daemon tells this thread on each pass.
pub struct View {
    /// The daemon has applied schizo mode on or off.
    pub known: bool,
    pub host: Option<PathBuf>,
    pub schizo: bool,
    pub gateway: Option<String>,
    pub up: Option<String>,
    /// sys-vpn's uplink, whose tunnel leaves through the edge too.
    pub vpnext: Option<String>,
    /// The cards with an address, by name.
    pub cards: Vec<(String, std::net::Ipv4Addr)>,
    /// The networks this machine is on.
    pub nets: Vec<Net>,
    /// Where the WireGuard key is kept.
    pub state: PathBuf,
}

/// The networks this machine is on, from the kernel.
pub fn own_networks() -> Vec<Net> {
    let mut nets = std::fs::read_to_string("/proc/net/route")
        .map(|t| parse_proc_route(&t))
        .unwrap_or_default();
    nets.extend(
        std::fs::read_to_string("/proc/net/if_inet6")
            .map(|t| parse_if_inet6(&t))
            .unwrap_or_default(),
    );
    nets
}
