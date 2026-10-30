//! sys-vpn (docs/vpn.md): while a VPN is set, the network machine hands
//! every connection from the host to sys-vpn's inside address, and sys-vpn
//! sends it through the provider's WireGuard tunnel; its own uplink comes
//! back here and leaves like the host's would have, through the edge when
//! one is set. In schizo mode it is Tor's uplink that goes to sys-vpn, so
//! the network sees only a VPN, and the VPN only Tor.
//!
//! The rules are ahead of the edge's (150, 151) and schizo mode's (152), so
//! they take what they name first. While sys-vpn's link is not here, the
//! table leads nowhere: the VPN fails closed, as the edge does.

/// The routing table: sys-vpn, or nowhere.
pub const TABLE: u32 = 154;
pub const PREF_LOCAL: u32 = 148;
pub const PREF_VPN: u32 = 149;

/// The `ip -4 rule add` arguments for a VPN carrying what comes in on
/// `from`: its own networks through the main table without its default
/// route, everything else and `dns`, this machine's forwarder's servers,
/// into the table.
#[must_use]
pub fn rules(from: &str, dns: &[String]) -> Vec<Vec<String>> {
    let table = TABLE.to_string();
    let mut all = vec![
        vec![
            "pref".into(),
            PREF_LOCAL.to_string(),
            "iif".into(),
            from.to_owned(),
            "lookup".into(),
            "main".into(),
            "suppress_prefixlength".into(),
            "0".into(),
        ],
        vec![
            "pref".into(),
            PREF_VPN.to_string(),
            "iif".into(),
            from.to_owned(),
            "lookup".into(),
            table.clone(),
        ],
    ];
    for server in dns {
        all.push(vec![
            "pref".into(),
            PREF_VPN.to_string(),
            "to".into(),
            server.clone(),
            "lookup".into(),
            table.clone(),
        ]);
    }
    all
}

/// The forwarder's servers while a VPN is set: the provider's own, inside
/// its tunnel, else the edge's choice through it.
#[must_use]
pub fn dns(provider: &[String]) -> Vec<String> {
    if provider.is_empty() {
        crate::edge::DNS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        provider.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_link_and_the_servers() {
        let r = rules("up0", &["10.64.0.1".to_owned()]);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0][..4], ["pref", "148", "iif", "up0"]);
        assert_eq!(r[1].last().unwrap(), "154");
        assert_eq!(r[2][2..4], ["to", "10.64.0.1"]);
        // Schizo mode: Tor's uplink, and no servers, which are the gateway.
        assert_eq!(rules("torext0", &[]).len(), 2);
        assert_eq!(dns(&[]), ["9.9.9.9", "149.112.112.112"]);
    }
}
