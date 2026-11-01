//! The company's Mesh CI jobs for this computer (`losos-login-bridge
//! manage`, a service from nixos/modules/company.nix).
//!
//! On the box, a workflow job with `runs-on: [losos-desktop, <computer>]`
//! waits for the joined computer of that name (the box's
//! backend/src/ci/workflow.rs, `desktop_job`). This asks for it with the
//! token joining got, through lososd on the office network, runs its steps
//! and reports the log and the result back.
//!
//! A step is one of [`ACTIONS`] and nothing else: no shell, no other
//! action. The box refuses anything else when it plans the job, and this
//! checks again before it does anything, so a box that sent something else
//! gets an error back rather than a command run as root here.

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{
    homed,
    join::{Joined, Way},
    Error, DIRECTORY_TOKEN_FILE, JOINED_FILE,
};

/// The steps a job may take, and the `with:` each one needs: the same list
/// as the box's `DESKTOP_ACTIONS`.
pub const ACTIONS: &[(&str, &[&str])] = &[
    ("losos/desktop-status", &[]),
    ("losos/desktop-update", &[]),
    ("losos/desktop-reboot", &[]),
    ("losos/desktop-channel", &["channel"]),
    ("losos/desktop-lock", &["user"]),
    ("losos/desktop-unlock", &["user"]),
];

/// The channel picked on this computer, as losos-update-channel keeps it.
const CHANNEL_FILE: &str = "/var/lib/losos/update-channel";

/// How often a running job tells the box it is alive; the box gives up on
/// a computer after two minutes of silence.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// One step, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Status,
    Update,
    Reboot,
    Channel(String),
    Lock(String),
    Unlock(String),
}

/// A `with:` value: a channel or an account name.
fn plain(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A step as the box sent it: `{"uses", "with", "name"}`.
pub fn parse_step(step: &Value) -> Result<(String, Action), String> {
    let uses = step.get("uses").and_then(Value::as_str).unwrap_or_default();
    let Some((action, takes)) = ACTIONS.iter().find(|(a, _)| *a == uses) else {
        return Err(format!("{uses:?} is not a step this computer takes"));
    };
    let with = match step.get("with") {
        None => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(format!("{action}: with is not a mapping")),
    };
    let mut value = None;
    for (k, v) in &with {
        let v = v.as_str().unwrap_or_default();
        if !takes.contains(&k.as_str()) || !plain(v) {
            return Err(format!("{action}: with.{k} is not one this step takes"));
        }
        value = Some(v.to_owned());
    }
    let needs = |what: &str| value.clone().ok_or(format!("{action} needs with.{what}"));
    let parsed = match *action {
        "losos/desktop-status" => Action::Status,
        "losos/desktop-update" => Action::Update,
        "losos/desktop-reboot" => Action::Reboot,
        "losos/desktop-channel" => Action::Channel(needs("channel")?),
        "losos/desktop-lock" => Action::Lock(needs("user")?),
        _ => Action::Unlock(needs("user")?),
    };
    let name = step
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(action);
    Ok((name.to_owned(), parsed))
}

/// A job's steps, every one checked before any runs.
pub fn parse_job(payload: &Value) -> Result<Vec<(String, Action)>, String> {
    let steps = payload
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("the job has no steps")?;
    if steps.is_empty() {
        return Err("the job has no steps".into());
    }
    steps.iter().map(parse_step).collect()
}

/// One job, as the box hands it out.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Work {
    pub id: String,
    pub name: String,
    pub payload: Value,
    #[serde(default)]
    pub timeout_secs: u64,
}

/// The way to the box: its own network only, where lososd answers.
fn local(joined: &Joined) -> Option<Way> {
    joined
        .ways()
        .into_iter()
        .find(|w| matches!(w, Way::Local { .. }))
}

/// The box, as this computer reaches it with its token.
struct BoxLink {
    way: Way,
    agent: ureq::Agent,
    token: String,
}

impl BoxLink {
    fn post(&self, path: &str, body: &Value) -> Result<Option<Value>, Error> {
        let answer = self
            .agent
            .post(&format!("{}{path}", self.way.base()))
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(body.clone())?;
        if answer.status() == 204 {
            return Ok(None);
        }
        Ok(Some(answer.into_json()?))
    }

    /// The next job, after the box waited a while for one.
    fn claim(&self) -> Result<Option<Work>, Error> {
        match self.post("/api/directory/jobs/claim", &json!({}))? {
            None => Ok(None),
            Some(v) => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| Error::new(format!("the box's job has the wrong shape: {e}"))),
        }
    }

    /// Sends `log` (and the end, with `state`); answers whether to stop.
    fn report(&self, id: &str, log: &str, state: Option<&str>) -> bool {
        let mut body = json!({ "id": id, "log": log });
        if let Some(state) = state {
            body["state"] = json!(state);
        }
        match self.post("/api/directory/jobs/report", &body) {
            Ok(Some(answer)) => answer.get("stop").and_then(Value::as_bool) == Some(true),
            Ok(None) => false,
            Err(e) => {
                tracing::warn!("reporting on job {id}: {e}");
                false
            }
        }
    }
}

/// How a step ended.
enum Ended {
    Done,
    Failed(String),
    Stopped,
}

/// Runs `cmd`, telling the box it is alive every [`HEARTBEAT`], and stops
/// it when the box says the run was cancelled or the job's time is up.
fn run(link: &BoxLink, id: &str, deadline: Instant, cmd: &mut Command, log: &mut String) -> Ended {
    let mut child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return Ended::Failed(format!("cannot start {:?}: {e}", cmd.get_program())),
    };
    let mut beat = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => return Ended::Failed(e.to_string()),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ended::Failed("the job's time ran out".into());
        }
        if beat.elapsed() >= HEARTBEAT {
            beat = Instant::now();
            if link.report(id, &std::mem::take(log), None) {
                let _ = child.kill();
                let _ = child.wait();
                return Ended::Stopped;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    match child.wait_with_output() {
        Ok(out) => {
            log.push_str(&String::from_utf8_lossy(&out.stdout));
            log.push_str(&String::from_utf8_lossy(&out.stderr));
            if out.status.success() {
                Ended::Done
            } else {
                Ended::Failed(format!("{:?} ended with {}", cmd.get_program(), out.status))
            }
        }
        Err(e) => Ended::Failed(e.to_string()),
    }
}

/// What this computer is: its version, channel and company accounts.
fn status(joined: &Joined, log: &mut String) -> Ended {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    for line in release.lines() {
        if let Some(v) = line
            .strip_prefix("PRETTY_NAME=")
            .or_else(|| line.strip_prefix("IMAGE_VERSION="))
        {
            log.push_str(&format!("{}\n", v.trim_matches('"')));
        }
    }
    // What losos-update-channel wrote when someone picked one; otherwise
    // the image's own (nixos/modules/update-source.nix).
    let channel = std::fs::read_to_string(CHANNEL_FILE).unwrap_or_default();
    let channel = channel.trim();
    log.push_str(&format!(
        "Update channel: {}\n",
        if channel.is_empty() {
            "the image's own"
        } else {
            channel
        }
    ));
    for (name, record) in crate::sync::homes() {
        if record.is_company(joined) {
            log.push_str(&format!(
                "Company account {name}{}\n",
                if record.locked { " (locked)" } else { "" }
            ));
        }
    }
    Ended::Done
}

/// A company account by `user`, or why not.
fn company_account(joined: &Joined, user: &str) -> Result<(), String> {
    match homed::lookup(user) {
        homed::Known::Home(record) if record.is_company(joined) => Ok(()),
        _ => Err(format!("{user} is not a company account on this computer")),
    }
}

/// Runs one job and reports how it ended. Whether to restart afterwards.
fn job(link: &BoxLink, joined: &Joined, work: &Work) -> bool {
    tracing::info!(id = %work.id, name = %work.name, "running a job");
    let steps = match parse_job(&work.payload) {
        Ok(steps) => steps,
        Err(why) => {
            link.report(
                &work.id,
                &format!("This computer does not run this job: {why}\n"),
                Some("error"),
            );
            return false;
        }
    };
    let timeout = Duration::from_secs(work.timeout_secs.clamp(60, 6 * 3600));
    let deadline = Instant::now() + timeout;
    let mut reboot = false;
    let mut log = String::new();
    let mut state = "success";
    for (name, action) in &steps {
        log.push_str(&format!("▶ {name}\n"));
        if link.report(&work.id, &std::mem::take(&mut log), None) {
            state = "cancelled";
            break;
        }
        let ended = match action {
            Action::Status => status(joined, &mut log),
            Action::Update => {
                let ended = run(
                    link,
                    &work.id,
                    deadline,
                    Command::new("systemctl").args(["start", "systemd-sysupdate.service"]),
                    &mut log,
                );
                // systemctl says only that it failed; the why is in the
                // unit's journal.
                if let Ended::Failed(_) = ended {
                    if let Ok(out) = Command::new("journalctl")
                        .args([
                            "-u",
                            "systemd-sysupdate.service",
                            "-n",
                            "20",
                            "--no-pager",
                            "-o",
                            "cat",
                        ])
                        .output()
                    {
                        log.push_str(&String::from_utf8_lossy(&out.stdout));
                    }
                }
                ended
            }
            Action::Reboot => {
                reboot = true;
                log.push_str("Restarting a minute after this job ends.\n");
                Ended::Done
            }
            Action::Channel(channel) => run(
                link,
                &work.id,
                deadline,
                Command::new("systemctl")
                    .arg("start")
                    .arg(format!("losos-update-channel@{channel}.service")),
                &mut log,
            ),
            Action::Lock(user) | Action::Unlock(user) => {
                let lock = matches!(action, Action::Lock(_));
                match company_account(joined, user)
                    .and_then(|()| homed::set_locked(user, lock).map_err(|e| e.message))
                {
                    Ok(()) => {
                        log.push_str(&format!(
                            "{user} is {}.\n",
                            if lock { "locked" } else { "unlocked" }
                        ));
                        Ended::Done
                    }
                    Err(e) => Ended::Failed(e),
                }
            }
        };
        match ended {
            Ended::Done => {}
            Ended::Failed(why) => {
                log.push_str(&format!("{why}\n"));
                state = "failure";
                break;
            }
            Ended::Stopped => {
                state = "cancelled";
                break;
            }
        }
    }
    link.report(&work.id, &log, Some(state));
    reboot && state == "success"
}

/// Asks the box for jobs and runs them, while this computer is joined and
/// has a token. Ends when it no longer is; the service's path unit starts
/// it again once a join writes a token.
pub fn serve() -> Result<(), Error> {
    loop {
        let Some(joined) = Joined::load(std::path::Path::new(JOINED_FILE))? else {
            return Ok(());
        };
        let token = match std::fs::read_to_string(DIRECTORY_TOKEN_FILE) {
            Ok(token) => token.trim().to_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let Some(way) = local(&joined) else {
            return Ok(());
        };
        let link = BoxLink {
            agent: way.agent()?,
            way,
            token,
        };
        match link.claim() {
            Ok(Some(work)) => {
                if job(&link, &joined, &work) {
                    tracing::info!("restarting, as the job asked");
                    let _ = Command::new("systemctl")
                        .args(["reboot", "--when=+1min"])
                        .status();
                }
            }
            // The box waited for work and had none.
            Ok(None) => {}
            Err(e) => {
                tracing::info!("asking the box for work: {}", e.message);
                std::thread::sleep(Duration::from_secs(30));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_only_the_fixed_steps() {
        let job = json!({ "steps": [
            { "uses": "losos/desktop-update", "with": {}, "name": "Update" },
            { "uses": "losos/desktop-lock", "with": { "user": "bob" } },
            { "uses": "losos/desktop-channel", "with": { "channel": "weekly" } },
        ]});
        assert_eq!(
            parse_job(&job).unwrap(),
            vec![
                ("Update".into(), Action::Update),
                ("losos/desktop-lock".into(), Action::Lock("bob".into())),
                (
                    "losos/desktop-channel".into(),
                    Action::Channel("weekly".into())
                ),
            ]
        );
        for bad in [
            json!({ "steps": [] }),
            json!({ "steps": [{ "run": "rm -rf /" }] }),
            json!({ "steps": [{ "uses": "actions/checkout@v4" }] }),
            json!({ "steps": [{ "uses": "losos/desktop-lock" }] }),
            json!({ "steps": [{ "uses": "losos/desktop-lock", "with": { "user": "a b" } }] }),
            json!({ "steps": [{ "uses": "losos/desktop-update", "with": { "x": "y" } }] }),
            json!({ "steps": [{ "uses": "losos/desktop-channel", "with": { "channel": "../x" } }] }),
        ] {
            assert!(parse_job(&bad).is_err(), "{bad}");
        }
        // One bad step and none run.
        let mixed = json!({ "steps": [
            { "uses": "losos/desktop-status" },
            { "uses": "losos/desktop-shell" },
        ]});
        assert!(parse_job(&mixed).is_err());
    }
}
