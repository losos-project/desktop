//! The box a computer is joined to, as joining wrote it.
//!
//! Joining asks LosOS Login, as one of the box's administrators, which edge
//! the box reaches the world through (`GET /api/edge`), and keeps that
//! gateway beside the box: it is the company's network the computer joined,
//! and its secondary cluster, when it has one, names the company's realm.

use std::{
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{tls, Error};

/// The LosOS Login group whose people administer the box, and so this
/// computer, unless joining named others.
pub const BOX_ADMINS: &str = "losos-admins";

/// The edge gateway the box used when it was joined, from its `GET
/// /api/edge` `path`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gateway {
    pub name: String,
    pub url: String,
    /// The secondary cluster the gateway is a member of, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Joined {
    /// The box's name on the company network, `mattbox.local`.
    pub host: String,
    /// The box's own certificate, PEM, pinned when it was joined.
    pub certificate: String,
    /// The name the box's edge routes to it from anywhere, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_name: Option<String>,
    pub gateway: Gateway,
    /// The homed realm of every company account: `--realm=` at creation,
    /// and how a login tells a company account from a local one.
    pub realm: String,
    /// The LosOS Login groups whose people administer this computer
    /// (`wheel`).
    pub admin_groups: Vec<String>,
    /// Who joined it, and when (seconds since the epoch).
    pub joined_by: String,
    pub joined_at: u64,
}

/// One way to LosOS Login, in the order a login tries them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Way {
    /// The company network: `https://<host>`, the pinned certificate (DER).
    Local { host: String, certificate: Vec<u8> },
    /// Anywhere: `https://<public name>`, through the box's edge.
    Public { name: String },
}

impl Way {
    /// The base every LosOS Login call goes under.
    pub fn base(&self) -> String {
        match self {
            Way::Local { host, .. } => format!("https://{host}"),
            Way::Public { name } => format!("https://{name}"),
        }
    }

    pub fn agent(&self) -> Result<ureq::Agent, Error> {
        match self {
            Way::Local { certificate, .. } => tls::pinned(certificate),
            Way::Public { .. } => Ok(tls::agent()),
        }
    }
}

impl Joined {
    /// The joined box, or `None` on a computer nobody joined.
    pub fn load(path: &Path) -> Result<Option<Self>, Error> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| Error::new(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
        }
    }

    /// Writes it beside `path` and renames it in, so a login never reads
    /// half of it.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let partial = path.with_extension("json.partial");
        let mut file = std::fs::File::create(&partial)?;
        file.write_all(&serde_json::to_vec_pretty(self).expect("a Joined serializes"))?;
        file.sync_all()?;
        std::fs::rename(&partial, path)?;
        Ok(())
    }

    /// The company network first, then the public name.
    pub fn ways(&self) -> Vec<Way> {
        let mut ways = Vec::new();
        if let Some(certificate) = tls::pem_to_der(&self.certificate) {
            ways.push(Way::Local {
                host: self.host.clone(),
                certificate,
            });
        }
        if let Some(name) = &self.public_name {
            ways.push(Way::Public { name: name.clone() });
        }
        ways
    }

    /// Whether someone in `groups` administers this computer.
    pub fn is_admin(&self, groups: &[String]) -> bool {
        groups.iter().any(|g| self.admin_groups.contains(g))
    }
}

/// The realm company accounts get: the gateway's cluster, which names the
/// company's network, or else the box's own name. homed takes a DNS name.
pub fn realm(gateway: &Gateway, host: &str) -> String {
    gateway
        .cluster
        .as_deref()
        .filter(|c| dns_name(c))
        .unwrap_or(host)
        .to_ascii_lowercase()
}

/// A DNS name as homed checks a realm: dot-separated labels of letters,
/// digits and `-`, none empty, at most 63 bytes each.
pub fn dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// The edge path in a box's `GET /api/edge`, if it has one.
pub fn gateway_from_edge(edge: &serde_json::Value) -> Option<Gateway> {
    let path = edge.get("path")?;
    let field = |k: &str| path.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    Some(Gateway {
        name: field("name")?,
        url: field("url")?,
        cluster: field("cluster"),
    })
}

/// Whether the edge path is a gateway on the company's own network
/// (secondary), rather than an official edge on the internet (tertiary).
pub fn is_local_gateway(edge: &serde_json::Value) -> bool {
    edge.pointer("/path/tier").and_then(|t| t.as_str()) == Some("secondary")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn joined() -> Joined {
        Joined {
            host: "mattbox.local".into(),
            certificate: "-----BEGIN CERTIFICATE-----\nAAEC\nAw==\n-----END CERTIFICATE-----\n"
                .into(),
            public_name: Some("mattbox.acme.losos.cfd".into()),
            gateway: Gateway {
                name: "office-1".into(),
                url: "http://gw.local:8443".into(),
                cluster: Some("office".into()),
            },
            realm: "office".into(),
            admin_groups: vec![BOX_ADMINS.into()],
            joined_by: "matus".into(),
            joined_at: 1,
        }
    }

    #[test]
    fn tries_the_company_network_first() {
        let ways = joined().ways();
        assert_eq!(
            ways,
            [
                Way::Local {
                    host: "mattbox.local".into(),
                    certificate: vec![0, 1, 2, 3]
                },
                Way::Public {
                    name: "mattbox.acme.losos.cfd".into()
                }
            ]
        );
        assert_eq!(ways[0].base(), "https://mattbox.local");
    }

    #[test]
    fn keeps_what_it_wrote() {
        let path = std::env::temp_dir().join(format!("joined-{}.json", std::process::id()));
        assert_eq!(Joined::load(&path), Ok(None));
        joined().save(&path).unwrap();
        assert_eq!(Joined::load(&path), Ok(Some(joined())));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn reads_the_boxs_gateway() {
        let edge = json!({"path": {"name": "office-1", "url": "http://gw.local:8443",
            "tunnel": "gw.local:7000", "source": "lan", "tier": "secondary",
            "cluster": "office", "member": "office-1"}});
        assert_eq!(gateway_from_edge(&edge), Some(joined().gateway));
        assert!(is_local_gateway(&edge));
        let official = json!({"path": {"name": "eu-1", "url": "https://register.losos.cfd",
            "tier": "tertiary"}});
        assert!(!is_local_gateway(&official));
        assert_eq!(gateway_from_edge(&json!({"path": null})), None);
    }

    #[test]
    fn names_the_realm_after_the_cluster() {
        let mut gateway = joined().gateway;
        assert_eq!(realm(&gateway, "mattbox.local"), "office");
        gateway.cluster = None;
        assert_eq!(realm(&gateway, "mattbox.local"), "mattbox.local");
        gateway.cluster = Some("not a name".into());
        assert_eq!(realm(&gateway, "MattBox.local"), "mattbox.local");
        assert!(!dns_name("a..b"));
        assert!(!dns_name("-a"));
    }

    #[test]
    fn makes_admins_of_the_named_groups() {
        assert!(joined().is_admin(&["losos-admins".into()]));
        assert!(!joined().is_admin(&["staff".into()]));
    }
}
