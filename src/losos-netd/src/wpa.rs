//! wpa_supplicant, which does Wi-Fi's scanning and handshakes here as it
//! did under NetworkManager on the host, spoken to over its control
//! socket, and what it says read into what nmcli would print.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// Where wpa_supplicant puts a control socket per interface (`-C`).
pub const CONTROL_DIR: &str = "/run/wpa_supplicant";

/// A connection to one interface's wpa_supplicant.
pub struct Control {
    socket: UnixDatagram,
    local: PathBuf,
}

impl Control {
    /// Connects to `interface`'s control socket from a socket of our own,
    /// which wpa_supplicant answers to.
    pub fn open(interface: &str) -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let local = PathBuf::from(format!(
            "/run/losos-netd/wpa-{interface}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&local);
        let socket = UnixDatagram::bind(&local)?;
        socket.connect(Path::new(CONTROL_DIR).join(interface))?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { socket, local })
    }

    /// Sends `command` and returns the reply.
    pub fn request(&self, command: &str) -> io::Result<String> {
        self.socket.send(command.as_bytes())?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = self.socket.recv(&mut buf)?;
            let reply = String::from_utf8_lossy(&buf[..n]).into_owned();
            // An event that slipped in before the reply; this socket is not
            // attached, so it is rare, and never the answer.
            if reply.starts_with('<') {
                continue;
            }
            return Ok(reply);
        }
    }

    /// Sends `command`, which answers OK or FAIL, and says which.
    pub fn ok(&self, command: &str) -> io::Result<()> {
        let reply = self.request(command)?;
        if reply.trim() == "OK" {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "wpa_supplicant: {} for {}",
                reply.trim(),
                command.split_whitespace().next().unwrap_or(command)
            )))
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.local);
    }
}

/// An access point from `SCAN_RESULTS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPoint {
    pub bssid: String,
    pub frequency: u32,
    /// From 0 to 100, as NetworkManager reports it.
    pub signal: u8,
    /// nmcli's SECURITY: `WPA2`, `WPA2 WPA3`, `WPA2 802.1X`, `OWE`, empty
    /// for an open network.
    pub security: String,
    pub ssid: Vec<u8>,
}

/// NetworkManager's quality for a signal level in dBm: -40 or better is
/// 100, -100 or worse is 0.
pub fn quality(dbm: i32) -> u8 {
    if dbm >= 0 {
        return dbm.min(100) as u8;
    }
    let below = (dbm.clamp(-100, -40) + 40).abs();
    (100 - (100 * below) / 60) as u8
}

/// nmcli's SECURITY words for wpa_supplicant's flags, such as
/// `[WPA2-PSK+SAE-CCMP][ESS]`.
pub fn security(flags: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    let mut add = |w: &'static str| {
        if !words.contains(&w) {
            words.push(w);
        }
    };
    for flag in flags.split(['[', ']']).filter(|f| !f.is_empty()) {
        if flag == "WEP" {
            add("WEP");
            continue;
        }
        let (wpa1, rest) = if let Some(rest) = flag.strip_prefix("WPA-") {
            (true, rest)
        } else if let Some(rest) = flag
            .strip_prefix("WPA2-")
            .or_else(|| flag.strip_prefix("RSN-"))
        {
            (false, rest)
        } else {
            continue;
        };
        let mgmt = rest.split('-').next().unwrap_or("");
        for m in mgmt.split('+') {
            match m {
                "PSK" | "PSK-SHA256" => add(if wpa1 { "WPA1" } else { "WPA2" }),
                "SAE" | "FT/SAE" => add("WPA3"),
                "EAP" | "EAP-SHA256" | "FT/EAP" => {
                    add(if wpa1 { "WPA1" } else { "WPA2" });
                    add("802.1X");
                }
                "OWE" => add("OWE"),
                _ => {}
            }
        }
    }
    let order = ["WEP", "WPA1", "WPA2", "WPA3", "OWE", "802.1X"];
    words.sort_by_key(|w| order.iter().position(|o| o == w));
    words.join(" ")
}

/// wpa_supplicant's escaped SSID text (`\xNN`, `\\`, `\"`, `\e`, `\n`,
/// `\r`, `\t`) back to its bytes.
pub fn unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'x' if i + 3 < bytes.len() => {
                    if let Ok(b) = u8::from_str_radix(&text[i + 2..i + 4], 16) {
                        out.push(b);
                        i += 4;
                        continue;
                    }
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                    continue;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                    continue;
                }
                b'e' => {
                    out.push(0x1b);
                    i += 2;
                    continue;
                }
                c @ (b'\\' | b'"') => {
                    out.push(c);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// `SCAN_RESULTS`: a header, then a tab-separated line per access point.
pub fn parse_scan(text: &str) -> Vec<AccessPoint> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.splitn(5, '\t');
            let bssid = parts.next()?.to_owned();
            let frequency = parts.next()?.parse().ok()?;
            let level: i32 = parts.next()?.parse().ok()?;
            let flags = parts.next()?;
            let ssid = unescape(parts.next().unwrap_or(""));
            Some(AccessPoint {
                bssid,
                frequency,
                signal: quality(level),
                security: security(flags),
                ssid,
            })
        })
        .collect()
}

/// `STATUS`'s `key=value` lines, the value of `key`.
pub fn status_value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

/// The channel for a frequency in MHz, as nmcli's CHAN shows it.
pub fn channel(frequency: u32) -> u32 {
    match frequency {
        2484 => 14,
        2412..=2472 => (frequency - 2407) / 5,
        5000..=5900 => (frequency - 5000) / 5,
        5955..=7115 => (frequency - 5950) / 5,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_scan() {
        let text = "bssid / frequency / signal level / flags / ssid\n\
                    aa:bb:cc:dd:ee:01\t2437\t-52\t[WPA2-PSK-CCMP][ESS]\tHome\n\
                    aa:bb:cc:dd:ee:02\t5180\t-71\t[WPA2-PSK+SAE-CCMP][ESS]\tCaf\\xc3\\xa9 \\\"A\\\"\n\
                    aa:bb:cc:dd:ee:03\t2412\t-90\t[ESS]\t\n";
        let aps = parse_scan(text);
        assert_eq!(aps.len(), 3);
        assert_eq!(aps[0].ssid, b"Home");
        assert_eq!(aps[0].security, "WPA2");
        assert_eq!(aps[0].signal, 80);
        assert_eq!(aps[1].ssid, "Café \"A\"".as_bytes());
        assert_eq!(aps[1].security, "WPA2 WPA3");
        assert_eq!(aps[2].security, "");
        assert!(aps[2].ssid.is_empty());
    }

    #[test]
    fn names_security_like_nmcli() {
        assert_eq!(security("[WPA-PSK-TKIP][WPA2-PSK-CCMP][ESS]"), "WPA1 WPA2");
        assert_eq!(security("[WPA2-SAE-CCMP][ESS]"), "WPA3");
        assert_eq!(security("[WPA2-EAP-CCMP][ESS]"), "WPA2 802.1X");
        assert_eq!(security("[RSN-OWE-CCMP][ESS]"), "OWE");
        assert_eq!(security("[WEP][ESS]"), "WEP");
    }

    #[test]
    fn signal_and_channel() {
        assert_eq!(quality(-40), 100);
        assert_eq!(quality(-100), 0);
        assert_eq!(quality(-30), 100);
        assert_eq!(quality(-70), 50);
        assert_eq!(channel(2437), 6);
        assert_eq!(channel(5180), 36);
    }

    #[test]
    fn reads_status() {
        let status = "bssid=aa:bb\nfreq=2437\nssid=Home\nid=0\nwpa_state=COMPLETED\n";
        assert_eq!(status_value(status, "wpa_state"), Some("COMPLETED"));
        assert_eq!(status_value(status, "id"), Some("0"));
        assert_eq!(status_value(status, "nope"), None);
    }
}
