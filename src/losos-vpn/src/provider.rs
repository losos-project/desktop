//! The VPN providers sys-vpn takes a WireGuard configuration from
//! (docs/vpn.md, "Which providers"). Each is popular, offers WireGuard
//! configurations a person can download, and is not owned by an
//! advertising or ad-tech company. Kape Technologies' services (ExpressVPN,
//! Private Internet Access, CyberGhost, ZenMate) are left out on purpose:
//! Kape was Crossrider, an ad-injection platform security firms flagged as
//! adware, and it still owns VPN review sites that rank its own services.
//!
//! What each entry says about its owner comes from the sources it lists, and
//! a source on the provider's own site is the provider's own word.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provider {
    Mullvad,
    ProtonVpn,
    Ivpn,
    Windscribe,
    AirVpn,
}

impl Provider {
    pub const ALL: [Provider; 5] = [
        Provider::Mullvad,
        Provider::ProtonVpn,
        Provider::Ivpn,
        Provider::Windscribe,
        Provider::AirVpn,
    ];

    /// What `losos-vpn set` takes and the host keeps: lower case, no spaces.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Provider::Mullvad => "mullvad",
            Provider::ProtonVpn => "proton",
            Provider::Ivpn => "ivpn",
            Provider::Windscribe => "windscribe",
            Provider::AirVpn => "airvpn",
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Provider::Mullvad => "Mullvad VPN",
            Provider::ProtonVpn => "Proton VPN",
            Provider::Ivpn => "IVPN",
            Provider::Windscribe => "Windscribe",
            Provider::AirVpn => "AirVPN",
        }
    }

    /// The company that runs it, and where.
    #[must_use]
    pub fn operator(self) -> &'static str {
        match self {
            Provider::Mullvad => "Mullvad VPN AB, Sweden",
            Provider::ProtonVpn => "Proton AG, Switzerland",
            Provider::Ivpn => "IVPN Limited (formerly Privatus Limited), Gibraltar",
            Provider::Windscribe => "Windscribe Limited, Canada",
            Provider::AirVpn => "Air di Paolo Brini, Italy",
        }
    }

    /// Who owns the operator.
    #[must_use]
    pub fn owners(self) -> &'static str {
        match self {
            Provider::Mullvad => {
                "Amagicom AB, owned outright by its founders Fredrik Strömberg and Daniel Berntsson"
            }
            Provider::ProtonVpn => {
                "the non-profit Proton Foundation as largest shareholder, then employees and early users"
            }
            Provider::Ivpn => "its founder Nicholas Pestell, as reported; the company says the owners did not change in 2023",
            Provider::Windscribe => {
                "its founders Yegor Sak, Alex Paguis and Mark Ulicki and its employees, no outside investors"
            }
            Provider::AirVpn => "Paolo Brini, for the activists who started it in 2010",
        }
    }

    /// Where a person makes the WireGuard configuration to hand to
    /// `losos-vpn set`. Each needs a paid account.
    #[must_use]
    pub fn config_page(self) -> &'static str {
        match self {
            Provider::Mullvad => "https://mullvad.net/en/account/wireguard-config",
            Provider::ProtonVpn => "https://account.protonvpn.com/downloads",
            Provider::Ivpn => "https://www.ivpn.net/account/wireguard-config",
            Provider::Windscribe => "https://windscribe.com/getconfig/wireguard",
            Provider::AirVpn => "https://airvpn.org/generator/",
        }
    }

    /// Where the ownership above comes from.
    #[must_use]
    pub fn sources(self) -> &'static [&'static str] {
        match self {
            Provider::Mullvad => &[
                "https://mullvad.net/en/about",
                "https://mullvad.net/en/blog/2020/2/3/doing-corporate-shuffle",
            ],
            Provider::ProtonVpn => &["https://proton.me/blog/proton-non-profit-foundation"],
            Provider::Ivpn => &[
                "https://www.ivpn.net/blog/change-company-name-privatus-ivpn-limited/",
                "https://discuss.privacyguides.net/t/ivpn-dual-jurisdiction/36230",
            ],
            Provider::Windscribe => {
                &["https://windscribe.com/knowledge-base/articles/who-owns-windscribe"]
            }
            Provider::AirVpn => &[
                "https://www.top10vpn.com/reviews/airvpn/",
                "https://torrentfreak.com/?p=246572",
            ],
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Provider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let wanted = s.trim().to_ascii_lowercase();
        Provider::ALL
            .into_iter()
            .find(|p| p.id() == wanted)
            .ok_or_else(|| {
                let ids: Vec<&str> = Provider::ALL.iter().map(|p| p.id()).collect();
                format!("no provider '{s}': one of {}", ids.join(", "))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_by_its_id() {
        for p in Provider::ALL {
            assert_eq!(p.id().parse::<Provider>(), Ok(p));
            assert!(p.config_page().starts_with("https://"));
            assert!(!p.sources().is_empty());
        }
        assert_eq!(" Mullvad ".parse(), Ok(Provider::Mullvad));
        assert!("expressvpn".parse::<Provider>().is_err());
    }
}
