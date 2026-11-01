//! The edge the PC's traffic leaves through (docs/network-machine.md,
//! "Through an edge"): the pure half. The daemon's `edge` thread does the
//! browse, the probes, the enrolment and the routing around it.
//!
//! The edge is chosen by LosOS's own rule ([`losos_edge_path`]), the one a
//! box goes by: a gateway on the network first, else the edge the person
//! set. A gateway on the network is never the exit itself, though: only an
//! edge out on the internet may be, so through a gateway the exit is one of
//! the official edges it lists, the one the account's id weighs heaviest
//! on, as a box spreads over them. Whatever the exit, every address its URL
//! and its WireGuard endpoint resolve to must be public and off this
//! machine's own networks ([`exit_addresses_ok`]), so traffic meant for the
//! internet never ends at something on the person's LAN.

use std::net::{IpAddr, Ipv4Addr};

use losos_edge_path::{weight, EdgePath, Net, Tier};
use serde::{Deserialize, Serialize};

/// The WireGuard interface to the exit.
pub const LINK: &str = "losos-edge";
/// The routing table the host's traffic takes while an edge is wanted:
/// the tunnel, or nowhere while it is down.
pub const TABLE: u32 = 153;
/// The rules' priorities, ahead of schizo mode's (152), which turns these
/// off before it turns its own on.
pub const PREF_LOCAL: u32 = 150;
pub const PREF_EDGE: u32 = 151;
/// Where the ad blocker's forwarder sends names while traffic leaves
/// through an edge, through the tunnel: Quad9, a Swiss foundation that
/// keeps no addresses. The networks' own servers would see every name, and
/// the edge drops traffic to private addresses anyway.
pub const DNS: [&str; 2] = ["9.9.9.9", "149.112.112.112"];
/// Tor's SOCKS on the gateway, which schizo mode reaches the edge through.
pub const TOR_SOCKS_PORT: u16 = 9050;

/// What the person set: the file `edge` in the host's share, `key value`
/// lines. The id and token are the account on the edge (a tenant marked
/// `exit`); the URL is the edge to use when no gateway is in reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub token: String,
    pub url: Option<String>,
}

impl Account {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (mut id, mut token, mut url) = (None, None, None);
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.split_once(char::is_whitespace) {
                Some(("id", v)) => id = Some(v.trim().to_owned()),
                Some(("token", v)) => token = Some(v.trim().to_owned()),
                Some(("url", v)) => url = Some(v.trim().trim_end_matches('/').to_owned()),
                _ => {
                    return Err(format!(
                        "not understood: {}",
                        line.split(' ').next().unwrap_or("")
                    ))
                }
            }
        }
        let id = id.filter(|s| !s.is_empty()).ok_or("no id")?;
        let token = token.filter(|s| !s.is_empty()).ok_or("no token")?;
        if let Some(u) = &url {
            if !losos_edge_path::plain_http_url(u) {
                return Err(format!("not an http(s) URL: {u}"));
            }
        }
        Ok(Account { id, token, url })
    }
}

/// The edge's answer to `POST /exit` (LosOS's edge/registrar/src/exit.rs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrolled {
    /// This machine's address on the edge's interface, `x.x.x.x/32`.
    pub address: String,
    pub server_public_key: String,
    /// `host:port` of the edge's WireGuard.
    pub endpoint: String,
    pub allowed_ips: Vec<String>,
    #[serde(default)]
    pub onion: Option<String>,
    #[serde(default)]
    pub onion_port: u16,
    pub socks_username: String,
    pub socks_password: String,
}

impl Enrolled {
    /// What the Tor gateway's losos-edge-chain reads (docs/schizo-mode.md,
    /// "Through an edge"), when the edge has an onion to reach it by.
    #[must_use]
    pub fn chain_file(&self) -> Option<serde_json::Value> {
        let onion = self.onion.as_deref().filter(|o| o.ends_with(".onion"))?;
        Some(serde_json::json!({
            "onion": onion,
            "port": self.onion_port,
            "username": self.socks_username,
            "password": self.socks_password,
        }))
    }

    /// The endpoint's host and port, the host without brackets.
    #[must_use]
    pub fn endpoint_parts(&self) -> Option<(String, u16)> {
        let (host, port) = self.endpoint.rsplit_once(':')?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        Some((host.to_owned(), port.parse().ok()?)).filter(|(h, _)| !h.is_empty())
    }
}

/// The exit for the path the rule chose. An edge out on the internet
/// (tertiary) is its own exit. Through a gateway (secondary) it is the
/// official edge out of `officials`, the gateway's list, that `seed` weighs
/// heaviest on; none when the gateway lists none.
#[must_use]
pub fn exit_url(path: &EdgePath, officials: &[String], seed: &str) -> Option<String> {
    match path.tier {
        Tier::Tertiary | Tier::Primary => Some(path.url.clone()),
        Tier::Secondary => officials
            .iter()
            .filter(|u| losos_edge_path::plain_http_url(u))
            .max_by_key(|u| weight(seed, u))
            .cloned(),
    }
}

/// An address on the internet: none of what is private, shared, local,
/// documentation, multicast or reserved.
#[must_use]
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_v4(v4),
            None => {
                let s = v6.segments();
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (s[0] & 0xfe00) == 0xfc00
                    || (s[0] & 0xffc0) == 0xfe80
                    || (s[0] == 0x2001 && s[1] == 0x0db8)
                    || (s[0] == 0x64 && s[1] == 0xff9b))
            }
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        || o[0] >= 240)
}

/// Whether an exit's addresses may carry the PC's traffic: there are some,
/// and every one is public and on none of `nets`, the networks this machine
/// is on. `Err` says why not, for the status.
pub fn exit_addresses_ok(what: &str, addrs: &[IpAddr], nets: &[Net]) -> Result<(), String> {
    if addrs.is_empty() {
        return Err(format!("{what} has no address"));
    }
    if let Some(a) = addrs.iter().find(|a| !public(**a)) {
        return Err(format!("{what} is at {a}, which is not on the internet"));
    }
    if let Some(a) = addrs.iter().find(|a| nets.iter().any(|n| n.contains(**a))) {
        return Err(format!("{what} is at {a}, on this computer's own network"));
    }
    Ok(())
}

/// Where things stand, as `losos-netd edge status` prints it for Settings
/// and `losos-edge status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Status {
    /// `none` (no edge set), `searching`, `up`, or `blocked` (set, and the
    /// host's traffic goes nowhere until it is up).
    pub state: &'static str,
    /// The edge the rule chose and where it was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<&'static str>,
    /// The edge traffic leaves through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<String>,
    /// `wireguard`, or `onion` in schizo mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use losos_edge_path::Source;

    fn path(tier: Tier) -> EdgePath {
        EdgePath {
            name: "gate".into(),
            url: "http://192.168.1.7:8443".into(),
            source: Source::Lan,
            tunnel: "192.168.1.7:7843".into(),
            tier,
            cluster: None,
            member: None,
            official_cluster: None,
        }
    }

    #[test]
    fn reads_the_account() {
        let a = Account::parse("id alice\ntoken s3cret\nurl https://edge.example/\n").unwrap();
        assert_eq!(a.id, "alice");
        assert_eq!(a.url.as_deref(), Some("https://edge.example"));
        assert!(Account::parse("id alice\n").is_err());
        assert!(Account::parse("id a\ntoken t\nurl ftp://x\n").is_err());
        assert!(Account::parse("id a\ntoken t\nmode x\n").is_err());
    }

    #[test]
    fn a_gateway_is_never_the_exit() {
        let officials = vec![
            "https://a.edge.example".to_owned(),
            "https://b.edge.example".to_owned(),
        ];
        let through = exit_url(&path(Tier::Secondary), &officials, "alice").unwrap();
        assert!(officials.contains(&through));
        // The same account takes the same official edge every time.
        assert_eq!(
            exit_url(&path(Tier::Secondary), &officials, "alice").unwrap(),
            through
        );
        assert_eq!(exit_url(&path(Tier::Secondary), &[], "alice"), None);
        let mut out = path(Tier::Tertiary);
        out.url = "https://edge.example".into();
        assert_eq!(
            exit_url(&out, &officials, "alice").as_deref(),
            Some("https://edge.example")
        );
    }

    #[test]
    fn only_the_internet_carries_the_traffic() {
        let lan = Net {
            addr: "203.0.114.0".parse().unwrap(),
            len: 24,
        };
        let ok = |a: &str| exit_addresses_ok("x", &[a.parse().unwrap()], &[lan]);
        assert!(ok("1.1.1.1").is_ok());
        assert!(ok("192.168.1.7").is_err());
        assert!(ok("100.64.0.1").is_err());
        assert!(ok("fd00::1").is_err());
        // Public, but the person's own network: a box on a public subnet.
        assert!(ok("203.0.114.9").is_err());
        assert!(exit_addresses_ok("x", &[], &[]).is_err());
        // One bad address of several is enough.
        assert!(exit_addresses_ok(
            "x",
            &["1.1.1.1".parse().unwrap(), "10.0.0.1".parse().unwrap()],
            &[]
        )
        .is_err());
    }

    #[test]
    fn hands_the_gateway_its_file() {
        let mut e = Enrolled {
            address: "10.153.0.2/32".into(),
            server_public_key: "k".into(),
            endpoint: "[2001:db8::1]:51820".into(),
            allowed_ips: vec!["0.0.0.0/0".into()],
            onion: Some("abc.onion".into()),
            onion_port: 1080,
            socks_username: "alice".into(),
            socks_password: "pw".into(),
        };
        assert_eq!(e.endpoint_parts(), Some(("2001:db8::1".to_owned(), 51820)));
        let file = e.chain_file().unwrap();
        assert_eq!(file["onion"], "abc.onion");
        assert_eq!(file["port"], 1080);
        e.onion = Some("evil.example".into());
        assert!(e.chain_file().is_none());
    }
}
