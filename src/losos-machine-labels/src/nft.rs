//! The nftables table that keeps machines apart by label, and the changes a
//! machine starting or stopping makes to it.
//!
//! It filters in the bridge family, on libvirt's `default` network's
//! bridge, between the machines' own ports: libvirt's NAT sees only what
//! the bridge hands up to the host, so machines on the same bridge reach
//! each other past it, and a rule there could not tell which machine a
//! packet came from anyway. Here each port is one machine's card.
//!
//! Every rule fails closed. A card no hook has named yet, from a machine
//! started before the table was loaded or one plugged into a running
//! machine, is red until its machine next starts; the forward chain drops
//! what no label lets through.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::domain::{LABELS, Machine};

/// The table, in the bridge family.
pub const TABLE: &str = "losos-machine-labels";

/// libvirt's `default` network's bridge, the one pm puts every machine on
/// (libvirt.nix). Other bridges, libvirt's or not, are left alone.
pub const BRIDGE: &str = "virbr0";

/// The sets beside the labels'.
const OTHER_SETS: [&str; 3] = ["unlabelled", "boxes", "trusted"];

/// Where a red machine may not send: everything that is not the internet.
/// The host's addresses, the LAN, the other machines on any network and
/// the service machines' links, multicast and broadcast.
const PRIVATE4: &str = "0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, \
     172.16.0.0/12, 192.0.0.0/24, 192.168.0.0/16, 198.18.0.0/15, 224.0.0.0/3";
const PRIVATE6: &str = "::/127, ::ffff:0:0/96, fc00::/7, fe80::/10, ff00::/8";

/// The whole table, replacing the one loaded before, so its sets start
/// empty.
pub fn table() -> String {
    let mut sets = String::new();
    for set in LABELS.iter().chain(OTHER_SETS.iter()) {
        sets.push_str(&format!("  set {set} {{ type ifname; }}\n"));
    }
    // A labelled machine reaches its own label's, and so does a machine
    // without one reach the others without one.
    let mut same = String::new();
    for set in LABELS
        .iter()
        .filter(|l| **l != "red")
        .chain(["unlabelled"].iter())
    {
        same.push_str(&format!("    iifname @{set} oifname @{set} accept\n"));
    }
    format!(
        "table bridge {TABLE}
delete table bridge {TABLE}
table bridge {TABLE} {{
{sets}  set gateway4 {{ type ipv4_addr; }}
  set gateway6 {{ type ipv6_addr; }}

  # What a machine sends into the bridge, to the host, past it or to
  # another machine. A trusted card's is left to libvirt and the host;
  # the rest is red's or unknown, and gets the host's DHCP and DNS and
  # the internet, and nothing else.
  chain prerouting {{
    type filter hook prerouting priority filter; policy accept;
    meta ibrname != \"{BRIDGE}\" accept
    iifname @trusted accept
    ether type arp accept
    udp dport 67 accept
    ip daddr @gateway4 udp dport 53 accept
    ip daddr @gateway4 tcp dport 53 accept
    icmpv6 type {{ nd-router-solicit, nd-neighbor-solicit, nd-neighbor-advert }} accept
    ip6 daddr @gateway6 udp dport 53 accept
    ip6 daddr @gateway6 tcp dport 53 accept
    ip daddr {{ {PRIVATE4} }} drop
    ip6 daddr {{ {PRIVATE6} }} drop
    ether type {{ ip, ip6 }} accept
    drop
  }}

  # From one machine's port to another's, which the host never routes.
  # The LosOS boxes machine takes and sends to anyone trusted; otherwise
  # only machines with the same label.
  chain forward {{
    type filter hook forward priority filter; policy drop;
    meta ibrname != \"{BRIDGE}\" accept
    iifname @boxes oifname @trusted accept
    iifname @trusted oifname @boxes accept
{same}  }}
}}
"
    )
}

/// Takes `ports` out of every set they are in, which a port of a machine
/// gone since may still be: libvirt names the next machine's card the same.
fn forget(script: &mut String, ports: &[String]) {
    for port in ports {
        for set in LABELS.iter().chain(OTHER_SETS.iter()) {
            script.push_str(&format!(
                "destroy element bridge {TABLE} {set} {{ \"{port}\" }}\n"
            ));
        }
    }
}

/// The changes for `machine` having started.
pub fn started(machine: &Machine) -> String {
    let mut script = String::new();
    forget(&mut script, &machine.ports);
    for port in &machine.ports {
        script.push_str(&format!(
            "add element bridge {TABLE} {} {{ \"{port}\" }}\n",
            machine.group.set()
        ));
        if machine.group.trusted() {
            script.push_str(&format!(
                "add element bridge {TABLE} trusted {{ \"{port}\" }}\n"
            ));
        }
    }
    script
}

/// The changes for `machine` having stopped.
pub fn stopped(machine: &Machine) -> String {
    let mut script = String::new();
    forget(&mut script, &machine.ports);
    script
}

/// Every port named in the table, from `nft list table` output.
pub fn named(listing: &str) -> Vec<String> {
    let mut ports = Vec::new();
    let mut inside = false;
    for line in listing.lines() {
        let line = line.trim();
        if line.starts_with("elements = {") {
            inside = true;
        }
        if !inside {
            continue;
        }
        ports.extend(
            line.split('"')
                .skip(1)
                .step_by(2)
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
        );
        if line.ends_with('}') {
            inside = false;
        }
    }
    ports
}

/// The changes that take out every port that is no longer there. libvirt
/// hands a stopping machine's hook its definition with the names it made
/// up, `vnet0` and on, already cleared, so a machine stopping cannot say
/// which ports were its own; the ports gone from the host were.
pub fn forget_gone() -> String {
    let Some(listing) = Command::new("nft")
        .args(["list", "table", "bridge", TABLE])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
    else {
        return String::new();
    };
    let gone: Vec<String> = named(&String::from_utf8_lossy(&listing.stdout))
        .into_iter()
        .filter(|port| !std::path::Path::new("/sys/class/net").join(port).exists())
        .collect();
    let mut script = String::new();
    forget(&mut script, &gone);
    script
}

/// The bridge's own addresses, where its DHCP and DNS answer, from
/// `ip -o addr show` output.
pub fn gateways(ip_output: &str) -> String {
    let mut script =
        format!("flush set bridge {TABLE} gateway4\nflush set bridge {TABLE} gateway6\n");
    for line in ip_output.lines() {
        let mut words = line.split_whitespace();
        while let Some(word) = words.next() {
            let set = match word {
                "inet" => "gateway4",
                "inet6" => "gateway6",
                _ => continue,
            };
            let Some(address) = words.next().and_then(|a| a.split('/').next()) else {
                continue;
            };
            if address.parse::<std::net::IpAddr>().is_ok() {
                script.push_str(&format!(
                    "add element bridge {TABLE} {set} {{ {address} }}\n"
                ));
            }
        }
    }
    script
}

/// The bridge's addresses now, or nothing while it is not there.
pub fn current_gateways() -> String {
    Command::new("ip")
        .args(["-o", "addr", "show", "dev", BRIDGE])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| gateways(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Whether the table is loaded.
pub fn loaded() -> bool {
    Command::new("nft")
        .args(["list", "table", "bridge", TABLE])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs `script` as one transaction.
pub fn apply(script: &str) -> std::io::Result<()> {
    let mut nft = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .spawn()?;
    nft.stdin
        .take()
        .expect("stdin is piped")
        .write_all(script.as_bytes())?;
    let status = nft.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("nft exited with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Group;

    #[test]
    fn a_label_reaches_its_own_and_red_nothing() {
        let table = table();
        assert!(table.contains("iifname @blue oifname @blue accept"));
        assert!(table.contains("iifname @unlabelled oifname @unlabelled accept"));
        assert!(!table.contains("iifname @red oifname"));
        assert!(table.contains("policy drop"));
    }

    #[test]
    fn a_red_machine_is_not_trusted() {
        let red = Machine {
            group: Group::Untrusted,
            ports: vec!["vnet1".into()],
        };
        let script = started(&red);
        assert!(script.contains("add element bridge losos-machine-labels red { \"vnet1\" }"));
        assert!(!script.contains("add element bridge losos-machine-labels trusted"));
        // Whatever the port was in before goes first.
        assert!(script.find("destroy element").unwrap() < script.find("add element").unwrap());

        let blue = Machine {
            group: Group::Label("blue"),
            ports: vec!["vnet2".into()],
        };
        let script = started(&blue);
        assert!(script.contains("blue { \"vnet2\" }"));
        assert!(script.contains("trusted { \"vnet2\" }"));
        assert!(!stopped(&blue).contains("add element"));
    }

    #[test]
    fn reads_the_ports_the_table_names() {
        let listing = "table bridge losos-machine-labels {
\tset blue {
\t\ttype ifname
\t\telements = { \"vnet0\", \"lab-blue\",
\t\t\t     \"lab-blue2\" }
\t}
\tset red {
\t\ttype ifname
\t}
\tset gateway4 {
\t\ttype ipv4_addr
\t\telements = { 192.168.122.1 }
\t}
}
";
        assert_eq!(named(listing), ["vnet0", "lab-blue", "lab-blue2"]);
    }

    #[test]
    fn reads_the_bridges_addresses() {
        let out = "4: virbr0    inet 192.168.122.1/24 brd 192.168.122.255 scope global virbr0\\       valid_lft forever preferred_lft forever\n\
                   4: virbr0    inet6 fe80::5054:ff:fe12:3456/64 scope link \\       valid_lft forever preferred_lft forever\n";
        let script = gateways(out);
        assert!(script.contains("gateway4 { 192.168.122.1 }"));
        assert!(script.contains("gateway6 { fe80::5054:ff:fe12:3456 }"));
        assert!(script.starts_with("flush set"));
    }
}
