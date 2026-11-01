//! Schizo mode, as the box hands it to this computer (docs/company.md,
//! dasmatus/losos `backend/src/schizo.rs`).
//!
//! The owner turns schizo mode on once, on their box. Every computer the
//! box's LosOS Login manages then follows: each checks in on the sync
//! pass's timer with its directory token, is told whether schizo mode is
//! on, and turns its own Tor gateway ([`crate::sync`] calls [`step`]) on or
//! off to match. So the owner flips every company computer to Tor, and back,
//! from one switch.
//!
//! The check-in is `POST /api/login-devices/self`, authenticated by the
//! directory token ([`crate::DIRECTORY_TOKEN_FILE`]) as a Bearer, which
//! nginx lets through without LosOS Login and lososd throttles like the
//! spare key. It is a public route on the box, so a computer away from the
//! office still hears of a change. The box answers with the policy: whether
//! schizo mode is on, and, while it is, the Tor addresses of the box and of
//! the owner's other computers, so the computers can reach each other over
//! Tor.
//!
//! This module follows the setting and nothing more. The duress code and
//! the wipe it orders are not acted on here.

use std::{path::Path, process::Command};

use serde::Deserialize;
use serde_json::json;

use crate::{
    join::{Joined, Way},
    tls, Error,
};

/// The command that turns this computer's Tor gateway on and off
/// (nixos/modules/schizo.nix). `step` runs `<command> on` or `<command> off`.
pub const SWITCH: &str = "losos-schizo";

/// What the box tells a computer that checked in. Only the parts this
/// computer follows; the duress hash and the wipe order are read past.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Policy {
    /// Whether schizo mode is on, which this computer matches.
    #[serde(default)]
    pub on: bool,
    /// While schizo mode is on, where the box and the owner's other
    /// computers can be reached on Tor.
    #[serde(default)]
    pub peers: Vec<Peer>,
}

/// One device's Tor address, as the box lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Peer {
    /// `box`, or the other computer's directory id.
    pub id: String,
    pub name: String,
    pub onion: String,
}

/// What the local Tor gateway is doing, read from the switch's flag so a
/// check-in that agrees with it runs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Local {
    On,
    Off,
}

/// Whether schizo mode is on locally, from the flag the switch keeps
/// (nixos/modules/schizo.nix writes `/var/lib/losos/schizo`). A missing flag
/// is off, the state a fresh computer boots in.
pub fn local(flag: &Path) -> Local {
    match std::fs::read_to_string(flag) {
        Ok(s) if s.trim() == "on" => Local::On,
        _ => Local::Off,
    }
}

/// Checks in with the box and returns its policy. `onion` is this computer's
/// own Tor address, when it has one, which the box passes to the others.
pub fn check_in(joined: &Joined, token: &str, onion: Option<&str>) -> Result<Policy, Error> {
    let body = match onion {
        Some(o) => json!({ "onion": o }),
        None => json!({}),
    };
    // The company network first, then the box's public name: whichever
    // answers. A computer in schizo mode routes both through its own Tor,
    // so the check-in keeps working once the gateway is up.
    let mut last = Error::new("the box has no address this computer knows");
    for way in joined.ways() {
        let agent = match &way {
            Way::Local { certificate, .. } => match tls::pinned(certificate) {
                Ok(a) => a,
                Err(e) => {
                    last = e;
                    continue;
                }
            },
            Way::Public { .. } => tls::agent(),
        };
        match agent
            .post(&format!("{}/api/login-devices/self", way.base()))
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(body.clone())
        {
            Ok(response) => return Ok(response.into_json()?),
            Err(e) => last = e.into(),
        }
    }
    Err(last)
}

/// Brings the local Tor gateway in line with `policy`. Returns whether it
/// changed anything, so the sync pass can log only a real switch.
pub fn apply(policy: &Policy, flag: &Path, switch: &str) -> Result<bool, Error> {
    let want = if policy.on { Local::On } else { Local::Off };
    if local(flag) == want {
        return Ok(false);
    }
    let arg = match want {
        Local::On => "on",
        Local::Off => "off",
    };
    let status = Command::new(switch).arg(arg).status()?;
    if !status.success() {
        return Err(Error::new(format!("{switch} {arg} exited with {status}")));
    }
    Ok(true)
}

/// One sync pass's schizo step: check in and follow the setting. Called from
/// [`crate::sync`] after it has the joined box and the directory token, so
/// it adds no second timer. A box that is out of reach is not an error the
/// pass should fail on, so the caller logs it and carries on.
pub fn step(joined: &Joined, token: &str, flag: &Path, onion: Option<&str>) -> Result<bool, Error> {
    let policy = check_in(joined, token, onion)?;
    apply(&policy, flag, SWITCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fresh empty directory under the system temp dir, removed on drop.
    /// The crate carries no tempfile dependency, so this mirrors join.rs's
    /// own test files.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "losos-schizo-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_the_local_flag() {
        let dir = Scratch::new("flag");
        let flag = dir.join("schizo");
        assert_eq!(local(&flag), Local::Off);
        std::fs::write(&flag, "on\n").unwrap();
        assert_eq!(local(&flag), Local::On);
        std::fs::write(&flag, "off").unwrap();
        assert_eq!(local(&flag), Local::Off);
    }

    #[test]
    fn parses_the_policy_and_reads_past_the_duress_parts() {
        // The box sends more than this computer follows; the extra keys,
        // the duress hash and the wipe order among them, are ignored.
        let policy: Policy = serde_json::from_value(json!({
            "on": true,
            "full": true,
            "since": 10,
            "duress": "$argon2id$v=19$...",
            "wipe": "wipe_abcd",
            "peers": [
                { "id": "box", "name": "box", "onion": "aaaa.onion" },
                { "id": "c1", "name": "laptop", "onion": "bbbb.onion" }
            ]
        }))
        .unwrap();
        assert!(policy.on);
        assert_eq!(policy.peers.len(), 2);
        assert_eq!(policy.peers[0].id, "box");
    }

    #[test]
    fn apply_runs_the_switch_only_on_a_change() {
        let dir = Scratch::new("apply");
        let flag = dir.join("schizo");
        // A switch that records each call and sets the flag, so apply's
        // "only on a change" is checked without the real gateway.
        let log = dir.join("calls");
        let script = dir.join("switch");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$1\" >> '{log}'\nif [ \"$1\" = on ]; then echo on > '{flag}'; else echo off > '{flag}'; fi\n",
                log = log.display(),
                flag = flag.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let switch = script.to_str().unwrap();
        let on = Policy {
            on: true,
            peers: Vec::new(),
        };
        let off = Policy::default();

        // Off → on runs the switch; a second on, now matching, does not.
        assert!(apply(&on, &flag, switch).unwrap());
        assert!(!apply(&on, &flag, switch).unwrap());
        // On → off runs it again.
        assert!(apply(&off, &flag, switch).unwrap());
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls, "on\noff\n");
    }

    #[test]
    fn apply_reports_a_failing_switch() {
        let dir = Scratch::new("fail");
        let flag = dir.join("schizo");
        let err = apply(
            &Policy {
                on: true,
                peers: Vec::new(),
            },
            &flag,
            "/does/not/exist",
        )
        .unwrap_err();
        assert!(!err.message.is_empty());
    }
}
