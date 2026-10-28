//! losos-netd, the network machine's init and its one daemon
//! (docs/network-machine.md): it runs the PC's network cards, handed to
//! the machine, and routes the host's and its machines' connections out
//! through them, with nothing else in the machine but BusyBox and the few
//! programs it starts.
//!
//! The pieces that are pure text, and tested as such, are here: nmcli's
//! language ([`nmcli`]), the saved networks ([`saved`]), wpa_supplicant's
//! answers ([`wpa`]), the machine's configuration ([`Config`]), and the
//! edge its traffic leaves through ([`edge`], [`dnssd`]).

pub mod dnssd;
pub mod edge;
pub mod nmcli;
pub mod saved;
pub mod wpa;

use std::path::PathBuf;

/// Where the control socket is, which `losos-netd nmcli` talks to.
pub const CONTROL: &str = "/run/losos-netd/control.sock";

/// A link to the host or the gateway: its MAC, the name it gets, its
/// address and prefix, and its router, if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticLink {
    pub role: String,
    pub mac: String,
    pub name: String,
    pub address: String,
    pub router: Option<String>,
}

/// A read-only share from the host, by its 9p tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub tag: String,
    pub path: PathBuf,
}

/// The machine's configuration, which network-machine.nix writes: `key
/// value...` lines, `#` comments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// The links by role: up, wan, tor, torext.
    pub links: Vec<StaticLink>,
    /// The Tor gateway's address, where schizo mode sends everything.
    pub gateway: Option<String>,
    /// The routing table schizo mode uses.
    pub schizo_table: u32,
    /// nft rulesets: the whole firewall at boot, and schizo mode on and off.
    pub nft_base: Option<PathBuf>,
    pub nft_on: Option<PathBuf>,
    pub nft_off: Option<PathBuf>,
    /// The host's share: `schizo` while the mode is on, `upstreams` while
    /// the host knows its network's servers, `import/` its saved networks.
    pub host: Option<PathBuf>,
    /// The state disk and where it goes.
    pub state_disk: Option<(PathBuf, PathBuf)>,
    pub shares: Vec<Share>,
    /// Programs kept running, each a command line.
    pub run: Vec<Vec<String>>,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut config = Config {
            schizo_table: 152,
            ..Default::default()
        };
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let bad = || format!("line {}: {line}", number + 1);
            match words.as_slice() {
                ["link", role, mac, name, address, rest @ ..] => config.links.push(StaticLink {
                    role: (*role).into(),
                    mac: mac.to_lowercase(),
                    name: (*name).into(),
                    address: (*address).into(),
                    router: rest.first().map(|r| (*r).into()),
                }),
                ["gateway", address] => config.gateway = Some((*address).into()),
                ["schizo-table", table] => {
                    config.schizo_table = table.parse().map_err(|_| bad())?
                }
                ["nft-base", path] => config.nft_base = Some(path.into()),
                ["nft-on", path] => config.nft_on = Some(path.into()),
                ["nft-off", path] => config.nft_off = Some(path.into()),
                ["host", path] => config.host = Some(path.into()),
                ["state-disk", device, path] => {
                    config.state_disk = Some((device.into(), path.into()))
                }
                ["share", tag, path] => config.shares.push(Share {
                    tag: (*tag).into(),
                    path: path.into(),
                }),
                ["run", command @ ..] if !command.is_empty() => config
                    .run
                    .push(command.iter().map(|w| (*w).to_owned()).collect()),
                _ => return Err(bad()),
            }
        }
        Ok(config)
    }

    /// The link with `role`.
    pub fn link(&self, role: &str) -> Option<&StaticLink> {
        self.links.iter().find(|l| l.role == role)
    }
}

/// The DNS servers in resolv.conf-style `nameserver` lines.
pub fn nameservers(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .map(|rest| rest.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_configuration() {
        let config = Config::parse(
            "# the network machine\n\
             link up 52:54:00:4C:1E:01 up0 10.139.0.1/24\n\
             link wan 52:54:00:4c:1e:02 wan0 10.139.1.2/24 10.139.1.1\n\
             gateway 10.152.152.10\n\
             host /var/lib/losos-host\n\
             state-disk /dev/vda /var/lib/losos-network\n\
             share adblock /var/lib/losos-adblock\n\
             run losos-vm-call serve 5000 /bin/losos-netd nmcli\n",
        )
        .unwrap();
        assert_eq!(config.links.len(), 2);
        assert_eq!(config.link("up").unwrap().mac, "52:54:00:4c:1e:01");
        assert_eq!(
            config.link("wan").unwrap().router.as_deref(),
            Some("10.139.1.1")
        );
        assert_eq!(config.schizo_table, 152);
        assert_eq!(config.run[0][..3], ["losos-vm-call", "serve", "5000"]);
        assert!(Config::parse("link up").is_err());
    }

    #[test]
    fn reads_nameservers() {
        assert_eq!(
            nameservers("# x\nnameserver 192.168.1.1\nnameserver fe80::1%wlan0\n"),
            ["192.168.1.1", "fe80::1%wlan0"]
        );
    }
}
