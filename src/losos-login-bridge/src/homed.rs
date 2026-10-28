//! The homed side of the bridge: company accounts as systemd-homed records,
//! through `homectl`, as derisk's Setup and Users pages make every other
//! account here (docs/users.md).
//!
//! A company account is a homed record whose realm is the joined company's
//! ([`crate::join::Joined::realm`]). The bridge owns three things of it:
//! the real name and e-mail, which follow LosOS Login; `wheel`, which
//! follows the administrator groups; and whether it is locked. Every other
//! group, and everything the person sets up themselves, stays theirs.

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
};

use serde_json::Value;

use crate::{join::Joined, login::Person, Error};

/// The group an administrator is in, which polkit, and through it
/// `pm system`, asks about.
pub const ADMIN_GROUP: &str = "wheel";
/// The groups derisk's Setup puts the first account in when the system has
/// them (derisk-install's `DEVICE_GROUPS`): a company account gets the same
/// at creation, so its Wi-Fi works as anyone else's does.
pub const DEVICE_GROUPS: &[&str] = &["networkmanager"];
/// Where systemd's programs are, when the PAM stack says (`bin=`): a login
/// screen's PATH is whatever its display manager was started with.
static BIN: OnceLock<PathBuf> = OnceLock::new();

/// Takes systemd's programs from `dir` from now on.
pub fn use_bin(dir: &str) {
    let _ = BIN.set(PathBuf::from(dir));
}

fn tool(name: &str) -> Command {
    Command::new(
        BIN.get()
            .map_or_else(|| PathBuf::from(name), |dir| dir.join(name)),
    )
}

/// The userdb service that hands out homed's records.
const HOMED_SERVICE: &str = "io.systemd.Home";

/// What userdb knows about a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Known {
    /// Nobody by that name.
    Nobody,
    /// A homed record.
    Home(Record),
    /// Someone else: a system account, or one from /etc/passwd.
    Other,
}

/// The parts of a homed record the bridge reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub realm: Option<String>,
    pub real_name: Option<String>,
    pub email: Option<String>,
    pub member_of: Vec<String>,
    pub locked: bool,
}

impl Known {
    /// A record from `userdbctl user NAME --json=short`.
    pub fn parse(json: &str) -> Known {
        let Ok(v) = serde_json::from_str::<Value>(json.trim_start_matches('\u{1e}')) else {
            return Known::Nobody;
        };
        if v.get("service").and_then(Value::as_str) != Some(HOMED_SERVICE) {
            return Known::Other;
        }
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Known::Home(Record {
            realm: text("realm"),
            real_name: text("realName"),
            email: text("emailAddress"),
            member_of: v
                .get("memberOf")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|g| g.as_str().map(str::to_owned))
                .collect(),
            locked: v.get("locked").and_then(Value::as_bool) == Some(true),
        })
    }
}

/// Looks `user` up in userdb.
pub fn lookup(user: &str) -> Known {
    let output = tool("userdbctl")
        .args(["user", user, "--json=short"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(o) if o.status.success() => Known::parse(&String::from_utf8_lossy(&o.stdout)),
        _ => Known::Nobody,
    }
}

impl Record {
    /// Whether this is a company account of `joined`'s.
    pub fn is_company(&self, joined: &Joined) -> bool {
        self.realm.as_deref() == Some(joined.realm.as_str())
    }
}

/// The groups a person's record should have: what it has, `wheel` as the
/// administrator groups say.
pub fn groups(current: &[String], admin: bool) -> Vec<String> {
    let mut groups: Vec<String> = current
        .iter()
        .filter(|g| *g != ADMIN_GROUP)
        .cloned()
        .collect();
    if admin {
        groups.insert(0, ADMIN_GROUP.to_owned());
    }
    groups
}

/// The `homectl create` arguments for a new company account.
pub fn create_args(person: &Person, joined: &Joined, device_groups: &[String]) -> Vec<String> {
    let groups = groups(device_groups, joined.is_admin(&person.groups));
    let mut args = vec![
        "create".to_owned(),
        person.name.clone(),
        format!("--realm={}", joined.realm),
        format!("--real-name={}", person.display_name),
        // The password is LosOS Login's, whose rules the box sets; this
        // computer's libpwquality must not refuse a password the company
        // accepted.
        "--enforce-password-policy=no".to_owned(),
    ];
    if !groups.is_empty() {
        args.push(format!("--member-of={}", groups.join(",")));
    }
    if let Some(email) = &person.email {
        args.push(format!("--email-address={email}"));
    }
    args
}

/// The `homectl update --offline` arguments that bring `record` in line
/// with `person`, or none when it already is.
pub fn update_args(record: &Record, person: &Person, joined: &Joined) -> Vec<String> {
    let mut args = Vec::new();
    if record.real_name.as_deref() != Some(person.display_name.as_str()) {
        args.push(format!("--real-name={}", person.display_name));
    }
    if let Some(email) = &person.email {
        if record.email.as_deref() != Some(email.as_str()) {
            args.push(format!("--email-address={email}"));
        }
    }
    let wanted = groups(&record.member_of, joined.is_admin(&person.groups));
    let mut have = record.member_of.clone();
    let mut want = wanted.clone();
    have.sort();
    want.sort();
    if have != want {
        // --member-of replaces the list, so it carries every group kept.
        args.push(format!("--member-of={}", wanted.join(",")));
    }
    // LosOS Login just accepted them, so a lock the bridge's sync set when
    // they were gone from it no longer holds.
    if record.locked {
        args.push("--locked=no".to_owned());
    }
    if args.is_empty() {
        return args;
    }
    let mut full = vec![
        "update".to_owned(),
        person.name.clone(),
        "--offline".to_owned(),
    ];
    full.extend(args);
    full
}

/// [`DEVICE_GROUPS`] that `/etc/group` lists: homectl refuses an account
/// in a group that does not exist.
pub fn device_groups(group_file: &str) -> Vec<String> {
    DEVICE_GROUPS
        .iter()
        .filter(|name| {
            group_file
                .lines()
                .any(|l| l.split(':').next() == Some(**name))
        })
        .map(|s| (*s).to_owned())
        .collect()
}

/// Runs homectl with `args`, the secrets in its environment as homectl
/// reads them, and never a prompt.
fn homectl(args: &[String], env: &[(&str, &str)]) -> Result<(), Error> {
    let output = tool("homectl")
        .arg("--no-ask-password")
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::new(format!("cannot run homectl: {e}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
    Err(Error::new(format!(
        "homectl {} failed: {}",
        args.first().map_or("", String::as_str),
        last.unwrap_or("no reason given").trim()
    )))
}

/// Makes the company account `person`, with `password` as homed's.
pub fn create(person: &Person, joined: &Joined, password: &str) -> Result<(), Error> {
    let groups = device_groups(&std::fs::read_to_string("/etc/group").unwrap_or_default());
    homectl(
        &create_args(person, joined, &groups),
        &[("NEWPASSWORD", password)],
    )
}

/// Brings `record` in line with `person`, through homed's own copy of the
/// record: the home area is not open yet at login, and homed writes the
/// copy into it when it opens.
pub fn update(record: &Record, person: &Person, joined: &Joined) -> Result<(), Error> {
    let args = update_args(record, person, joined);
    if args.is_empty() {
        return Ok(());
    }
    homectl(&args, &[])
}

/// Whether `password` opens `user`'s home.
pub fn authenticate(user: &str, password: &str) -> bool {
    homectl(
        &["authenticate".to_owned(), user.to_owned()],
        &[("PASSWORD", password)],
    )
    .is_ok()
}

/// Re-keys `user`'s home from `old` to `new`.
pub fn passwd(user: &str, old: &str, new: &str) -> Result<(), Error> {
    homectl(
        &["passwd".to_owned(), user.to_owned()],
        &[("PASSWORD", old), ("NEWPASSWORD", new)],
    )
}

/// Locks or unlocks `user`'s account for logging in, through homed's copy
/// of the record, so it holds whether or not their home is open.
pub fn set_locked(user: &str, locked: bool) -> Result<(), Error> {
    homectl(
        &[
            "update".to_owned(),
            user.to_owned(),
            "--offline".to_owned(),
            format!("--locked={}", if locked { "yes" } else { "no" }),
        ],
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::{Gateway, BOX_ADMINS};

    fn joined() -> Joined {
        Joined {
            host: "mattbox.local".into(),
            certificate: String::new(),
            public_name: None,
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

    fn ada(groups: &[&str]) -> Person {
        Person {
            name: "ada".into(),
            display_name: "Ada Lovelace".into(),
            email: Some("ada@example.com".into()),
            groups: groups.iter().map(|g| (*g).into()).collect(),
        }
    }

    #[test]
    fn reads_userdbs_records() {
        let home = "\u{1e}{\"userName\":\"ada\",\"realm\":\"office\",\"realName\":\"Ada\",\
            \"memberOf\":[\"wheel\",\"networkmanager\"],\"service\":\"io.systemd.Home\"}";
        assert_eq!(
            Known::parse(home),
            Known::Home(Record {
                realm: Some("office".into()),
                real_name: Some("Ada".into()),
                email: None,
                member_of: vec!["wheel".into(), "networkmanager".into()],
                locked: false,
            })
        );
        assert_eq!(
            Known::parse(
                "{\"userName\":\"root\",\"uid\":0,\"service\":\"io.systemd.NameServiceSwitch\"}"
            ),
            Known::Other
        );
        assert_eq!(Known::parse(""), Known::Nobody);
    }

    #[test]
    fn tells_company_accounts_from_local_ones() {
        let Known::Home(mut record) = Known::parse(
            "{\"userName\":\"ada\",\"realm\":\"office\",\"service\":\"io.systemd.Home\"}",
        ) else {
            panic!("a home");
        };
        assert!(record.is_company(&joined()));
        record.realm = None;
        assert!(!record.is_company(&joined()));
    }

    #[test]
    fn creates_an_administrator_with_the_device_groups() {
        assert_eq!(
            create_args(&ada(&[BOX_ADMINS]), &joined(), &["networkmanager".into()]),
            [
                "create",
                "ada",
                "--realm=office",
                "--real-name=Ada Lovelace",
                "--enforce-password-policy=no",
                "--member-of=wheel,networkmanager",
                "--email-address=ada@example.com",
            ]
        );
        let mut plain = ada(&["staff"]);
        plain.email = None;
        assert_eq!(
            create_args(&plain, &joined(), &[]),
            [
                "create",
                "ada",
                "--realm=office",
                "--real-name=Ada Lovelace",
                "--enforce-password-policy=no",
            ]
        );
    }

    #[test]
    fn updates_only_what_changed_and_keeps_other_groups() {
        let record = Record {
            realm: Some("office".into()),
            real_name: Some("Ada Lovelace".into()),
            email: Some("ada@example.com".into()),
            member_of: vec!["networkmanager".into(), "wheel".into()],
            locked: false,
        };
        assert!(update_args(&record, &ada(&[BOX_ADMINS]), &joined()).is_empty());
        assert_eq!(
            update_args(&record, &ada(&["staff"]), &joined()),
            ["update", "ada", "--offline", "--member-of=networkmanager"]
        );
        let renamed = Record {
            real_name: Some("Ada".into()),
            member_of: vec!["libvirtd".into()],
            locked: true,
            ..record
        };
        assert_eq!(
            update_args(&renamed, &ada(&[BOX_ADMINS]), &joined()),
            [
                "update",
                "ada",
                "--offline",
                "--real-name=Ada Lovelace",
                "--member-of=wheel,libvirtd",
                "--locked=no",
            ]
        );
    }

    #[test]
    fn finds_the_device_groups_the_system_has() {
        assert_eq!(
            device_groups("wheel:x:1:\nnetworkmanager:x:57:\n"),
            ["networkmanager"]
        );
        assert!(device_groups("wheel:x:1:\n").is_empty());
    }
}
