//! The Wi-Fi networks a person saved, one file each under the machine's
//! state disk, and the ones NetworkManager had saved on the host before
//! the host had this machine, read from its keyfiles once.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A saved Wi-Fi network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub uuid: String,
    /// What nmcli calls its NAME, the SSID unless set otherwise.
    pub name: String,
    pub ssid: Vec<u8>,
    /// none, wpa-psk, sae or owe.
    pub key_mgmt: String,
    /// required, optional or disable; empty for the default.
    pub pmf: String,
    /// The passphrase, or the raw key as 64 hex digits; empty for none.
    pub psk: String,
}

impl Saved {
    /// The file's text: `key=value` lines, the SSID in hex so any byte
    /// survives.
    pub fn to_text(&self) -> String {
        format!(
            "uuid={}\nname={}\nssid={}\nkey-mgmt={}\npmf={}\npsk={}\n",
            self.uuid,
            self.name.replace('\n', " "),
            crate::nmcli::hex(&self.ssid),
            self.key_mgmt,
            self.pmf,
            self.psk.replace('\n', ""),
        )
    }

    /// Reads a file written by [`Saved::to_text`].
    pub fn from_text(text: &str) -> Option<Self> {
        let get = |key: &str| {
            text.lines().find_map(|line| {
                let (k, v) = line.split_once('=')?;
                (k == key).then(|| v.to_owned())
            })
        };
        Some(Self {
            uuid: get("uuid").filter(|u| !u.is_empty())?,
            name: get("name").unwrap_or_default(),
            ssid: unhex(&get("ssid")?)?,
            key_mgmt: get("key-mgmt").unwrap_or_else(|| "none".into()),
            pmf: get("pmf").unwrap_or_default(),
            psk: get("psk").unwrap_or_default(),
        })
    }

    /// Reads a NetworkManager keyfile for a Wi-Fi connection: its
    /// `[connection]` id and uuid, `[wifi]` ssid and `[wifi-security]`
    /// key-mgmt, pmf and psk. Anything else (802.1X, a VPN, a wired
    /// profile) is not one this machine joins, and gives nothing.
    pub fn from_keyfile(text: &str) -> Option<Self> {
        let mut section = String::new();
        let mut get = std::collections::HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.to_owned();
            } else if let Some((k, v)) = line.split_once('=') {
                get.insert(format!("{section}.{}", k.trim()), v.to_owned());
            }
        }
        let kind = get.get("connection.type")?;
        if kind != "wifi" && kind != "802-11-wireless" {
            return None;
        }
        let key_mgmt = get
            .get("wifi-security.key-mgmt")
            .cloned()
            .unwrap_or_else(|| "none".into());
        if !matches!(key_mgmt.as_str(), "none" | "wpa-psk" | "sae" | "owe") {
            return None;
        }
        let pmf = match get.get("wifi-security.pmf").map(String::as_str) {
            Some("1") => "disable",
            Some("2") => "optional",
            Some("3") => "required",
            _ => "",
        };
        Some(Self {
            uuid: get.get("connection.uuid")?.clone(),
            name: get.get("connection.id").cloned().unwrap_or_default(),
            ssid: keyfile_ssid(get.get("wifi.ssid")?)?,
            key_mgmt,
            pmf: pmf.into(),
            psk: get.get("wifi-security.psk").cloned().unwrap_or_default(),
        })
    }

    /// The `network={...}` block wpa_supplicant would read for it, as the
    /// `SET_NETWORK` pairs that build the same: each a field and its value
    /// in wpa_supplicant's syntax.
    pub fn wpa_settings(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("ssid", crate::nmcli::hex(&self.ssid).to_lowercase())];
        match self.key_mgmt.as_str() {
            "wpa-psk" => {
                out.push(("key_mgmt", "WPA-PSK WPA-PSK-SHA256".into()));
                out.push(("psk", psk_value(&self.psk)));
            }
            "sae" => {
                out.push(("key_mgmt", "SAE".into()));
                out.push(("sae_password", quote(&self.psk)));
                out.push(("ieee80211w", "2".into()));
            }
            "owe" => {
                out.push(("key_mgmt", "OWE".into()));
                out.push(("ieee80211w", "2".into()));
            }
            _ => out.push(("key_mgmt", "NONE".into())),
        }
        match self.pmf.as_str() {
            "required" => out.push(("ieee80211w", "2".into())),
            "optional" => out.push(("ieee80211w", "1".into())),
            "disable" => out.push(("ieee80211w", "0".into())),
            _ => {}
        }
        out
    }
}

/// A passphrase in quotes, or a raw key as it is.
fn psk_value(psk: &str) -> String {
    if psk.len() == 64 && psk.bytes().all(|b| b.is_ascii_hexdigit()) {
        psk.to_owned()
    } else {
        quote(psk)
    }
}

/// A string in wpa_supplicant's quotes. A passphrase is printable ASCII
/// (derisk checks it), and wpa_supplicant ends a quoted value at its last
/// quote, so one inside needs no escaping.
fn quote(text: &str) -> String {
    format!("\"{text}\"")
}

/// A keyfile's SSID: the text itself, or `a;b;c;` decimal bytes.
fn keyfile_ssid(value: &str) -> Option<Vec<u8>> {
    if value.ends_with(';')
        && value
            .split(';')
            .filter(|p| !p.is_empty())
            .all(|p| p.parse::<u8>().is_ok())
    {
        return Some(
            value
                .split(';')
                .filter(|p| !p.is_empty())
                .map(|p| p.parse().unwrap())
                .collect(),
        );
    }
    Some(value.as_bytes().to_vec())
}

/// Hex digits to bytes.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The saved networks in `dir`.
pub fn load(dir: &Path) -> Vec<Saved> {
    let mut all: Vec<Saved> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "conf"))
        .filter_map(|e| Saved::from_text(&std::fs::read_to_string(e.path()).ok()?))
        .collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

fn path(dir: &Path, uuid: &str) -> PathBuf {
    dir.join(format!("{uuid}.conf"))
}

/// Writes `saved` to `dir`, durably: a network that was just joined is
/// there after the machine is switched off.
pub fn store(dir: &Path, saved: &Saved) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = path(dir, &saved.uuid);
    let partial = target.with_extension("partial");
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&partial)?;
        file.write_all(saved.to_text().as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&partial, &target)?;
    std::fs::File::open(dir)?.sync_all()
}

/// Removes the saved network `uuid` from `dir`.
pub fn remove(dir: &Path, uuid: &str) -> io::Result<()> {
    match std::fs::remove_file(path(dir, uuid)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => std::fs::File::open(dir)?.sync_all(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Saved {
        Saved {
            uuid: "4b1f".into(),
            name: "Home".into(),
            ssid: b"Home \"5G\"".to_vec(),
            key_mgmt: "wpa-psk".into(),
            pmf: String::new(),
            psk: "correct horse".into(),
        }
    }

    #[test]
    fn round_trips() {
        assert_eq!(Saved::from_text(&home().to_text()), Some(home()));
    }

    #[test]
    fn stores_and_removes() {
        let dir = std::env::temp_dir().join(format!("losos-netd-saved-{}", std::process::id()));
        store(&dir, &home()).unwrap();
        assert_eq!(load(&dir), [home()]);
        remove(&dir, "4b1f").unwrap();
        assert!(load(&dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_networkmanagers_keyfiles() {
        let keyfile = "[connection]\nid=Cafe\nuuid=9c2d\ntype=wifi\ninterface-name=wlp2s0\n\n\
                       [wifi]\nmode=infrastructure\nssid=Cafe\n\n\
                       [wifi-security]\nkey-mgmt=sae\npmf=3\npsk=latte art\n";
        assert_eq!(
            Saved::from_keyfile(keyfile),
            Some(Saved {
                uuid: "9c2d".into(),
                name: "Cafe".into(),
                ssid: b"Cafe".to_vec(),
                key_mgmt: "sae".into(),
                pmf: "required".into(),
                psk: "latte art".into(),
            })
        );
        let bytes = "[connection]\nid=x\nuuid=1\ntype=wifi\n[wifi]\nssid=104;105;\n";
        assert_eq!(Saved::from_keyfile(bytes).unwrap().ssid, b"hi");
        assert_eq!(
            Saved::from_keyfile("[connection]\nuuid=1\ntype=ethernet\n"),
            None
        );
        let eap = "[connection]\nid=x\nuuid=1\ntype=wifi\n[wifi]\nssid=Work\n[wifi-security]\nkey-mgmt=wpa-eap\n";
        assert_eq!(Saved::from_keyfile(eap), None);
    }

    #[test]
    fn tells_wpa_supplicant() {
        let settings = home().wpa_settings();
        assert_eq!(settings[0], ("ssid", "486f6d652022354722".into()));
        assert_eq!(settings[1], ("key_mgmt", "WPA-PSK WPA-PSK-SHA256".into()));
        assert_eq!(settings[2], ("psk", "\"correct horse\"".into()));
        let quoted = Saved {
            psk: "a\"b".into(),
            key_mgmt: "sae".into(),
            ..home()
        };
        assert_eq!(
            quoted.wpa_settings()[2],
            ("sae_password", "\"a\"b\"".into())
        );
    }
}
