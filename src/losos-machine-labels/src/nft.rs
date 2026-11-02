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
//! machine, is blocked until its MAC is registered by the next hook.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::domain::{LABELS, Machine, is_mac};

/// The table, in the bridge family.
pub const TABLE: &str = "losos-machine-labels";

/// libvirt's `default` network's bridge, the one pm puts every machine on
/// (libvirt.nix). Other bridges, libvirt's or not, are left alone.
pub const BRIDGE: &str = "virbr0";

/// The sets beside the labels'.
const OTHER_SETS: [&str; 4] = ["unlabelled", "boxes", "trusted", "mac_bound"];

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
    let mut marks = String::new();
    let mut output_rules = String::new();
    for (index, set) in LABELS.iter().enumerate() {
        let mark = format!("0x4c{:02x}0000", index + 1);
        marks.push_str(&format!(
            "    iifname @{set} meta mark set (meta mark & 0x0000ffff) | {mark}\n"
        ));
        if *set != "red" {
            output_rules.push_str(&format!(
                "    meta mark & 0xffff0000 == {mark} oifname @{set} accept\n"
            ));
            output_rules.push_str(&format!(
                "    meta mark & 0xffff0000 == {mark} oifname @boxes accept\n"
            ));
        }
        output_rules.push_str(&format!("    meta mark & 0xffff0000 == {mark} drop\n"));
    }
    let boxes_mark = "0x4c090000";
    let unlabelled_mark = "0x4c0a0000";
    marks.push_str(&format!(
        "    iifname @boxes meta mark set (meta mark & 0x0000ffff) | {boxes_mark}\n    iifname @unlabelled meta mark set (meta mark & 0x0000ffff) | {unlabelled_mark}\n"
    ));
    output_rules.push_str(&format!(
        "    meta mark & 0xffff0000 == {boxes_mark} oifname @trusted accept\n    meta mark & 0xffff0000 == {boxes_mark} drop\n    meta mark & 0xffff0000 == {unlabelled_mark} oifname @unlabelled accept\n    meta mark & 0xffff0000 == {unlabelled_mark} oifname @boxes accept\n    meta mark & 0xffff0000 == {unlabelled_mark} drop\n"
    ));
    format!(
        "table bridge {TABLE}
delete table bridge {TABLE}
table bridge {TABLE} {{
{sets}  set gateway4 {{ type ipv4_addr; }}
  set gateway6 {{ type ipv6_addr; }}
map macs {{ type ifname : ether_addr; }}
map arp4 {{ type ifname : ipv4_addr; }}

  # What a machine sends into the bridge, to the host, past it or to
  # another machine. A trusted card's is left to libvirt and the host;
  # the rest is red's or unknown, and gets the host's DHCP and DNS and
  # the internet, and nothing else.
  chain prerouting {{
    type filter hook prerouting priority filter; policy accept;
    meta ibrname != \"{BRIDGE}\" accept
    iifname != @mac_bound drop
    iifname map @macs != ether saddr drop
    icmpv6 type nd-neighbor-advert drop
    icmpv6 type nd-neighbor-solicit ip6 saddr :: accept
    icmpv6 type nd-neighbor-solicit drop
{marks}    iifname map @macs == ether saddr iifname map @macs == arp saddr ether iifname map @arp4 == arp saddr ip accept
    iifname map @macs == ether saddr iifname map @macs == arp saddr ether arp saddr ip 0.0.0.0 accept
    ether type arp drop
    iifname @trusted accept
    udp sport 68 udp dport 67 ip daddr @gateway4 accept
    udp sport 68 udp dport 67 ip daddr 255.255.255.255 accept
    ip daddr @gateway4 udp dport 53 accept
    ip daddr @gateway4 tcp dport 53 accept
    icmpv6 type nd-router-solicit accept
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

  # Routed traffic takes the host's output path, not the bridge's forward
  # path. Carry its source label through routing and check the destination.
  chain output {{
    type filter hook output priority filter; policy accept;
    meta obrname != \"{BRIDGE}\" accept
{output_rules}    oifname @red udp sport 67 udp dport 68 accept
    oifname @red ip saddr @gateway4 udp sport 53 accept
    oifname @red ip saddr @gateway4 tcp sport 53 accept
    oifname @red ip6 saddr @gateway6 udp sport 53 accept
    oifname @red ip6 saddr @gateway6 tcp sport 53 accept
  }}
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
        for map in ["macs", "arp4"] {
            script.push_str(&format!(
                "destroy element bridge {TABLE} {map} {{ \"{port}\" }}\n"
            ));
        }
    }
}

/// The changes for `machine` having started.
pub fn started(machine: &Machine) -> String {
    let mut script = unassign(machine);
    script.push_str(&register(machine));
    script
}

/// Adds a machine to an empty table, as during a full reload.
pub fn register(machine: &Machine) -> String {
    let mut script = mac_bindings(machine);
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

/// Adds the MAC bindings before a guest starts, without granting a label.
pub fn mac_bindings(machine: &Machine) -> String {
    let mut script = String::new();
    for (mac, port) in &machine.macs {
        script.push_str(&format!(
            "add element bridge {TABLE} mac_bound {{ \"{port}\" }}\n"
        ));
        script.push_str(&format!(
            "add element bridge {TABLE} macs {{ \"{port}\" : {mac} }}\n"
        ));
    }
    script
}

/// Removes any previous policy and address binding for a machine's ports.
pub fn unassign(machine: &Machine) -> String {
    let mut script = String::new();
    forget(&mut script, &machine.ports);
    script
}

/// The current port-to-MAC map, as nftables prints it.
pub fn port_macs(listing: &str) -> Vec<(String, String)> {
    let mut owners = Vec::new();
    let mut inside = false;
    for line in listing.lines() {
        let line = line.trim();
        if line.starts_with("elements = {") {
            inside = true;
        }
        if inside {
            let elements = line
                .strip_prefix("elements = {")
                .unwrap_or(line)
                .trim_end_matches('}')
                .trim();
            for element in elements.split(',') {
                if let Some((port, mac)) = element.split_once(" : ") {
                    let port = port.trim().trim_matches('"');
                    let mac = mac.trim().to_ascii_lowercase();
                    if crate::domain::is_port(port) && is_mac(&mac) {
                        owners.push((port.to_owned(), mac));
                    }
                }
            }
        }
        if inside && line.ends_with('}') {
            inside = false;
        }
    }
    owners
}

/// DHCP leases for live IPv4 clients, keyed by their MAC address.
pub fn leases(listing: &str) -> Vec<(String, std::net::Ipv4Addr)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let expires = fields.next()?.parse::<u64>().ok()?;
            let mac = fields.next()?.to_ascii_lowercase();
            let address = fields.next()?.parse::<std::net::Ipv4Addr>().ok()?;
            (is_mac(&mac) && (expires == 0 || expires > now)).then_some((mac, address))
        })
        .collect()
}

/// Replaces the port-to-IPv4 bindings from current DHCP leases.
pub fn arp_owners(ports: &[(String, String)], leases: &[(String, std::net::Ipv4Addr)]) -> String {
    let leases: std::collections::HashMap<_, _> = leases.iter().cloned().collect();
    let mut script = format!("flush map bridge {TABLE} arp4\n");
    for (port, mac) in ports {
        if let Some(address) = leases.get(mac) {
            script.push_str(&format!(
                "add element bridge {TABLE} arp4 {{ \"{port}\" : {address} }}\n"
            ));
        }
    }
    script
}

/// Adds a machine's current leases after its port bindings have changed.
pub fn arp_bindings(machine: &Machine, leases: &[(String, std::net::Ipv4Addr)]) -> String {
    let leases: std::collections::HashMap<_, _> = leases.iter().cloned().collect();
    let mut script = String::new();
    for (mac, port) in &machine.macs {
        if let Some(address) = leases.get(mac) {
            script.push_str(&format!(
                "add element bridge {TABLE} arp4 {{ \"{port}\" : {address} }}\n"
            ));
        }
    }
    script
}

/// Reads the current port-to-MAC bindings from nftables.
pub fn current_port_macs() -> std::io::Result<Vec<(String, String)>> {
    let listing = Command::new("nft")
        .args(["list", "map", "bridge", TABLE, "macs"])
        .stderr(Stdio::null())
        .output()?;
    if !listing.status.success() {
        return Err(std::io::Error::other(format!(
            "nft exited with {}",
            listing.status
        )));
    }
    Ok(port_macs(&String::from_utf8_lossy(&listing.stdout)))
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
        assert!(table.contains("chain output"));
        assert!(table.contains("meta mark & 0xffff0000 == 0x4c060000 oifname @boxes accept"));
        assert!(table.contains("meta mark & 0xffff0000 == 0x4c060000 drop"));
        assert!(table.contains("meta mark & 0x0000ffff"));
        assert!(table.contains("policy drop"));
    }

    #[test]
    fn arp_and_dhcp_allowances_are_narrow() {
        let table = table();
        assert!(table.contains("iifname map @macs == arp saddr ether"));
        assert!(table.contains("iifname map @arp4 == arp saddr ip accept"));
        assert!(table.contains("udp sport 68 udp dport 67 ip daddr @gateway4 accept"));
        assert!(table.contains("udp sport 68 udp dport 67 ip daddr 255.255.255.255 accept"));
        assert!(!table.contains("udp dport 67 accept"));
    }

    #[test]
    fn ownership_and_label_checks_precede_protocol_allowances() {
        let table = table();
        let source_mac = table.find("iifname map @macs != ether saddr drop").unwrap();
        let arp = table.find("iifname map @macs == arp saddr ether").unwrap();
        let advertisement = table.find("icmpv6 type nd-neighbor-advert drop").unwrap();
        let dad = table
            .find("icmpv6 type nd-neighbor-solicit ip6 saddr :: accept")
            .unwrap();
        let solicitation = table.find("icmpv6 type nd-neighbor-solicit drop").unwrap();
        let router_solicitation = table.find("icmpv6 type nd-router-solicit accept").unwrap();
        assert!(table.find("iifname != @mac_bound drop").unwrap() < source_mac);
        assert!(source_mac < arp);
        assert!(advertisement < table.find("iifname @trusted accept").unwrap());
        assert!(dad < solicitation);
        assert!(solicitation < table.find("iifname @trusted accept").unwrap());
        assert!(solicitation < router_solicitation);

        let blue_drop = table
            .find("meta mark & 0xffff0000 == 0x4c060000 drop")
            .unwrap();
        let red_dhcp = table
            .find("oifname @red udp sport 67 udp dport 68 accept")
            .unwrap();
        let red_dns = table
            .find("oifname @red ip saddr @gateway4 udp sport 53 accept")
            .unwrap();
        assert!(blue_drop < red_dhcp);
        assert!(blue_drop < red_dns);
    }

    #[test]
    fn a_red_machine_is_not_trusted() {
        let red = Machine {
            group: Group::Untrusted,
            ports: vec!["vnet1".into()],
            macs: vec![("52:54:00:00:00:01".into(), "vnet1".into())],
        };
        let script = started(&red);
        assert!(script.contains("add element bridge losos-machine-labels red { \"vnet1\" }"));
        assert!(!script.contains("add element bridge losos-machine-labels trusted"));
        assert!(script.contains("macs { \"vnet1\" : 52:54:00:00:00:01 }"));
        assert!(script.contains("mac_bound { \"vnet1\" }"));
        // Whatever the port was in before goes first.
        assert!(script.find("destroy element").unwrap() < script.find("add element").unwrap());

        let blue = Machine {
            group: Group::Label("blue"),
            ports: vec!["vnet2".into()],
            macs: vec![],
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

    #[test]
    fn binds_live_dhcp_addresses_to_their_ports() {
        let ports = port_macs(
            "map macs {\n type ifname : ether_addr\n elements = { \"vnet0\" : 52:54:00:00:00:01 }\n}",
        );
        assert_eq!(ports, [("vnet0".into(), "52:54:00:00:00:01".into())]);
        let leases = leases(
            "0 52:54:00:00:00:01 192.168.122.11 red *\n\
             1 52:54:00:00:00:02 192.168.122.12 expired *",
        );
        let script = arp_owners(&ports, &leases);
        assert!(script.contains("flush map bridge losos-machine-labels arp4"));
        assert!(script.contains("\"vnet0\" : 192.168.122.11"));
        assert!(!script.contains("192.168.122.12"));
    }
}
