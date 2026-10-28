//! Keeping company accounts in step with the box between logins
//! (`losos-login-bridge sync`, on a timer from nixos/modules/company.nix).
//!
//! A login that reaches LosOS Login already refuses someone the box
//! removed or disabled. A login that does not reach it, a laptop away from
//! the office, is homed's alone, so without this someone the company let go
//! would keep logging in offline until their laptop next saw the box. So
//! while the computer is on the office network it reads the box's people
//! list (`GET /api/directory/people`, with the token joining got) and:
//!
//! - locks the account of anyone the list no longer has, or has disabled;
//! - takes `wheel` from anyone no longer in an administrator group.
//!
//! It never unlocks or grants anything: that waits for the person's own
//! login, which LosOS Login answers. And it never touches a local account.

use serde::Deserialize;
use serde_json::Value;

use crate::{
    homed::{self, Record, ADMIN_GROUP},
    join::{Joined, Way},
    Error, DIRECTORY_TOKEN_FILE, JOINED_FILE,
};

/// One person as the box lists them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    pub name: String,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub disabled: bool,
}

/// What a sync does to one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The box no longer has them, or has disabled them.
    Lock(String),
    /// They no longer administer the box's computers: these are the groups
    /// their record keeps.
    DropAdmin(String, Vec<String>),
}

/// The changes the box's `people` call for, given every homed account
/// here by name.
pub fn plan(joined: &Joined, people: &[Listed], homes: &[(String, Record)]) -> Vec<Change> {
    let mut changes = Vec::new();
    for (name, record) in homes {
        if !record.is_company(joined) || record.locked {
            continue;
        }
        match people.iter().find(|p| &p.name == name) {
            None => changes.push(Change::Lock(name.clone())),
            Some(p) if p.disabled => changes.push(Change::Lock(name.clone())),
            Some(p) => {
                if record.member_of.iter().any(|g| g == ADMIN_GROUP) && !joined.is_admin(&p.groups)
                {
                    changes.push(Change::DropAdmin(
                        name.clone(),
                        homed::groups(&record.member_of, false),
                    ));
                }
            }
        }
    }
    changes
}

/// The box's answer to `GET /api/directory/people`.
pub fn parse_people(answer: &Value) -> Option<Vec<Listed>> {
    serde_json::from_value(answer.get("people")?.clone()).ok()
}

/// Every homed account here, from `userdbctl --service=io.systemd.Home
/// user --json=short`, which prints one record a line.
pub fn parse_homes(listing: &str) -> Vec<(String, Record)> {
    listing
        .split(['\n', '\u{1e}'])
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let name = serde_json::from_str::<Value>(line)
                .ok()?
                .get("userName")?
                .as_str()?
                .to_owned();
            Some((name, Record::parse(line)?))
        })
        .collect()
}

fn homes() -> Vec<(String, Record)> {
    homed::userdb(true, &[]).map_or_else(Vec::new, |listing| parse_homes(&listing))
}

/// Reads the people list from the box on the office network. `None` when
/// the box is out of reach, which is no reason to change anything.
fn people(joined: &Joined, token: &str) -> Result<Option<Vec<Listed>>, Error> {
    // The admin API answers on the box's own network only, so the public
    // name is never asked.
    let Some(way @ Way::Local { .. }) = joined.ways().into_iter().next() else {
        return Ok(None);
    };
    let answer = way
        .agent()?
        .get(&format!("{}/api/directory/people", way.base()))
        .set("Authorization", &format!("Bearer {token}"))
        .call();
    match answer {
        Ok(response) => {
            let body: Value = response.into_json()?;
            parse_people(&body)
                .map(Some)
                .ok_or_else(|| Error::new("the box's people list has the wrong shape"))
        }
        Err(ureq::Error::Transport(t)) => {
            tracing::info!("the box is out of reach: {t}");
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// One sync: what it changed.
pub fn run() -> Result<Vec<Change>, Error> {
    let Some(joined) = Joined::load(std::path::Path::new(JOINED_FILE))? else {
        return Ok(Vec::new());
    };
    let token = match std::fs::read_to_string(DIRECTORY_TOKEN_FILE) {
        Ok(token) => token.trim().to_owned(),
        // Joined to a box that had no people list for computers yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let Some(people) = people(&joined, &token)? else {
        return Ok(Vec::new());
    };
    let changes = plan(&joined, &people, &homes());
    for change in &changes {
        let done = match change {
            Change::Lock(user) => homed::set_locked(user, true),
            Change::DropAdmin(user, groups) => homed::set_groups(user, groups),
        };
        match done {
            Ok(()) => tracing::info!(?change, "done"),
            Err(e) => tracing::warn!(?change, "failed: {e}"),
        }
    }
    Ok(changes)
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
            joined_by: "ada".into(),
            joined_at: 0,
        }
    }

    fn home(realm: &str, groups: &[&str], locked: bool) -> Record {
        Record {
            realm: Some(realm.into()),
            real_name: None,
            email: None,
            member_of: groups.iter().map(|g| (*g).to_owned()).collect(),
            locked,
        }
    }

    fn listed(name: &str, groups: &[&str], disabled: bool) -> Listed {
        Listed {
            name: name.into(),
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
            disabled,
        }
    }

    #[test]
    fn locks_people_the_box_removed_or_disabled() {
        let homes = vec![
            ("ada".into(), home("office", &["wheel"], false)),
            ("bob".into(), home("office", &[], false)),
            ("carol".into(), home("office", &[], false)),
        ];
        let people = vec![
            listed("ada", &[BOX_ADMINS], false),
            listed("bob", &["staff"], true),
        ];
        assert_eq!(
            plan(&joined(), &people, &homes),
            vec![Change::Lock("bob".into()), Change::Lock("carol".into())]
        );
    }

    #[test]
    fn takes_wheel_from_someone_no_longer_an_administrator() {
        let homes = vec![(
            "ada".into(),
            home("office", &["wheel", "networkmanager"], false),
        )];
        let people = vec![listed("ada", &["staff"], false)];
        assert_eq!(
            plan(&joined(), &people, &homes),
            vec![Change::DropAdmin(
                "ada".into(),
                vec!["networkmanager".into()]
            )]
        );
    }

    #[test]
    fn leaves_local_and_already_locked_accounts_alone() {
        let homes = vec![
            ("me".into(), home("elsewhere", &["wheel"], false)),
            ("dan".into(), home("office", &[], true)),
        ];
        assert!(plan(&joined(), &[], &homes).is_empty());
    }

    #[test]
    fn reads_the_box_and_userdb() {
        let answer = serde_json::json!({ "people": [
            { "name": "ada", "displayName": "Ada", "email": "", "groups": ["losos-admins"], "disabled": false }
        ]});
        assert_eq!(
            parse_people(&answer),
            Some(vec![listed("ada", &[BOX_ADMINS], false)])
        );
        let listing = concat!(
            r#"{"userName":"ada","service":"io.systemd.Home","realm":"office","memberOf":["wheel"]}"#,
            "\n",
            r#"{"userName":"bob","service":"io.systemd.Home","realm":"office"}"#,
            "\n"
        );
        let homes = parse_homes(listing);
        assert_eq!(homes.len(), 2);
        assert_eq!(homes[0].0, "ada");
        assert_eq!(homes[0].1.realm.as_deref(), Some("office"));
    }
}
