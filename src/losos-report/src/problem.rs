//! What the desktop reports, and what goes with each.
//!
//! derisk asks for a report in three places: a window that stopped answering
//! the compositor's pings, a service in the failed state, and an action in an
//! app (Settings pairing a device, adding a user) that is still running long
//! after it should have finished. Each names the app or unit it is about, and
//! the report carries what says why: the unit's state and its journal, and
//! for a process that hangs, what each of its threads is waiting in.
//!
//! The window's title is not sent, only its app's ID: a title is often a
//! document's name or a page's, which is the person's, not the bug's.
use std::fs;
use std::process::Command;

use serde_json::Value;

use crate::{BODY_LIMIT, Report, System, fenced, fingerprint, redact};

/// How many journal lines a report carries.
pub const JOURNAL_LINES: usize = 200;

/// How many of a process's threads a report lists; a browser has hundreds,
/// and the first ones are the ones that matter (the main thread is first).
const THREADS: usize = 64;

/// Something that went wrong on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A window got no answer to the compositor's ping for `seconds`.
    NotResponding {
        /// The window's app ID: `org.gnome.Nautilus`.
        app: String,
        /// The client's process, when the compositor knew it.
        pid: Option<u32>,
        seconds: u64,
    },
    /// A unit entered the failed state.
    Failed {
        /// `foo.service`.
        unit: String,
        /// A user unit (`systemctl --user`) rather than a system one.
        user: bool,
    },
    /// An action in an app was still running after `seconds`.
    Stuck {
        /// The app's ID: `org.derisk.settings`.
        app: String,
        /// What the app said it was doing: `Pairing headphones`.
        action: String,
        seconds: u64,
        /// The process doing it, whose journal goes with the report.
        pid: Option<u32>,
    },
}

impl Problem {
    /// The problem a `report` request names in its `kind` and fields.
    pub fn parse(request: &Value) -> Result<Self, String> {
        let text = |key: &str| {
            request[key]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("a {} report needs `{key}`", request["kind"]))
        };
        let pid = request["pid"].as_u64().and_then(|p| u32::try_from(p).ok());
        let seconds = request["seconds"].as_u64().unwrap_or(0);
        match request["kind"].as_str() {
            Some("not_responding") => Ok(Self::NotResponding {
                app: text("app")?,
                pid,
                seconds,
            }),
            Some("failed") => Ok(Self::Failed {
                unit: text("unit")?,
                user: request["user"].as_bool().unwrap_or(true),
            }),
            Some("stuck") => Ok(Self::Stuck {
                app: text("app")?,
                action: text("action")?,
                seconds,
                pid,
            }),
            Some(other) => Err(format!("no such kind of report: {other}")),
            None => Err("a report needs a `kind`".to_owned()),
        }
    }
}

/// A unit and what systemd says of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub user: bool,
    /// `systemctl show`'s properties that say how it ended, as `Key=value`.
    pub state: Vec<String>,
}

/// One thread of a process that stopped answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: u32,
    pub name: String,
    /// The kernel's one-letter state: `R` running, `S` sleeping, `D` waiting
    /// on a disk or a driver.
    pub state: char,
    /// The kernel function it sleeps in, when it sleeps: `futex_wait_queue`
    /// is a lock, `do_epoll_wait` an idle loop.
    pub waiting_in: String,
}

/// What was read about a problem before its report was written, apart, so
/// a report can be written from fixtures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evidence {
    pub unit: Option<Unit>,
    pub threads: Vec<Thread>,
    /// The unit's journal from this boot, or the process's when it has no
    /// unit, oldest first.
    pub journal: Vec<String>,
}

/// The unit a process runs in, from its `/proc/<pid>/cgroup` text: the last
/// `.service` or `.scope` on its path, and whether that is under a user's
/// manager (`user@1000.service`).
pub fn unit_of(cgroup: &str) -> Option<Unit> {
    let path = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    let name = path
        .rsplit('/')
        .find(|c| c.ends_with(".service") || c.ends_with(".scope"))?;
    // The manager itself is a unit too, but a process under it is in one of
    // its units, which is further along the path.
    Some(Unit {
        name: name.to_owned(),
        user: path.contains("/user@") && !name.starts_with("user@"),
        state: Vec::new(),
    })
}

/// The properties of `unit` that say how it is and how it ended.
fn unit_state(unit: &str, user: bool) -> Vec<String> {
    let mut command = Command::new("systemctl");
    if user {
        command.arg("--user");
    }
    command
        .args([
            "show",
            "--no-pager",
            "--property=ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,NRestarts,ActiveEnterTimestamp,InactiveEnterTimestamp",
            "--",
            unit,
        ])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.split_once('=').is_some_and(|(_, v)| !v.is_empty()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `pid`'s threads, at most [`THREADS`], from procfs.
fn threads(pid: u32) -> Vec<Thread> {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut ids: Vec<u32> = tasks
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    ids.sort_unstable();
    ids.into_iter()
        .take(THREADS)
        .filter_map(|id| {
            let dir = format!("/proc/{pid}/task/{id}");
            let stat = fs::read_to_string(format!("{dir}/stat")).ok()?;
            // The name in parentheses can hold spaces and parentheses; the
            // state is the first field after the last `)`.
            let state = stat.rsplit_once(')')?.1.trim_start().chars().next()?;
            let read = |file: &str| {
                fs::read_to_string(format!("{dir}/{file}"))
                    .map(|s| s.trim().to_owned())
                    .unwrap_or_default()
            };
            let waiting_in = read("wchan");
            Some(Thread {
                id,
                name: read("comm"),
                state,
                waiting_in: if waiting_in == "0" {
                    String::new()
                } else {
                    waiting_in
                },
            })
        })
        .collect()
}

/// Reads what `problem` needs from this machine: its unit's state and
/// journal, and its process's threads.
pub fn gather(problem: &Problem) -> Evidence {
    let (unit, pid) = match problem {
        Problem::Failed { unit, user } => (
            Some(Unit {
                name: unit.clone(),
                user: *user,
                state: Vec::new(),
            }),
            None,
        ),
        Problem::NotResponding { pid, .. } | Problem::Stuck { pid, .. } => (
            pid.and_then(|p| fs::read_to_string(format!("/proc/{p}/cgroup")).ok())
                .and_then(|c| unit_of(&c)),
            *pid,
        ),
    };
    let lines = format!("--lines={JOURNAL_LINES}");
    let journal = match (&unit, pid) {
        (Some(unit), _) => {
            let scope = if unit.user { "--user-unit" } else { "--unit" };
            crate::journal(&["--boot", &lines, &format!("{scope}={}", unit.name)])
        }
        (None, Some(pid)) => crate::journal(&["--boot", &lines, &format!("_PID={pid}")]),
        (None, None) => Vec::new(),
    };
    Evidence {
        unit: unit.map(|u| Unit {
            state: unit_state(&u.name, u.user),
            ..u
        }),
        // Only a process that stopped answering is worth its threads: for a
        // slow action, the thread doing it is busy by design.
        threads: match problem {
            Problem::NotResponding { pid: Some(pid), .. } => threads(*pid),
            _ => Vec::new(),
        },
        journal,
    }
}

/// `unit` with its instance taken out, `app-derisk-org.foo@3.service` as
/// `app-derisk-org.foo@.service`, so every instance is the same problem.
fn template(unit: &str) -> String {
    match (unit.split_once('@'), unit.rsplit_once('.')) {
        (Some((name, _)), Some((_, kind))) => format!("{name}@.{kind}"),
        _ => unit.to_owned(),
    }
}

/// `text` cut to `max` characters, with an ellipsis when it was longer.
fn short(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.chars().count() > max {
        format!("{}…", first.chars().take(max).collect::<String>())
    } else {
        first.to_owned()
    }
}

/// The report for `problem` on `system`, from `evidence`.
pub fn build(problem: &Problem, system: &System, evidence: &Evidence) -> Report {
    let unit = evidence.unit.as_ref();
    let result = unit
        .and_then(|u| u.state.iter().find_map(|l| l.strip_prefix("Result=")))
        .unwrap_or("");
    let (title, fingerprint, mut body) = match problem {
        Problem::NotResponding { app, seconds, .. } => {
            let fingerprint = fingerprint("not responding", app);
            (
                format!("{} stopped responding [{fingerprint}]", short(app, 72)),
                fingerprint,
                format!(
                    "A window of **{app}** stopped answering the desktop: the compositor's check \
                     (`xdg_wm_base.ping`) got no reply for {seconds} s.\n\n"
                ),
            )
        }
        Problem::Failed { unit: name, user } => {
            let template = template(name);
            let why = if result.is_empty() { "failed" } else { result };
            let fingerprint = fingerprint("failed", &format!("{template} {why}"));
            let kind = if *user {
                "user service"
            } else {
                "system service"
            };
            (
                format!("{} failed ({why}) [{fingerprint}]", short(&template, 72)),
                fingerprint,
                format!("The {kind} **{name}** failed: `{why}`.\n\n"),
            )
        }
        Problem::Stuck {
            app,
            action,
            seconds,
            ..
        } => {
            let fingerprint = fingerprint(&format!("stuck {app}"), action);
            (
                format!(
                    "{}: \"{}\" never finished [{fingerprint}]",
                    short(app, 40),
                    short(action, 48)
                ),
                fingerprint,
                format!(
                    "In **{app}**, \"{action}\" was still running after {seconds} s with no \
                     result.\n\n"
                ),
            )
        }
    };
    let unit_name = unit.map(|u| u.name.as_str()).unwrap_or("");
    let app = match problem {
        Problem::NotResponding { app, .. } | Problem::Stuck { app, .. } => app.as_str(),
        Problem::Failed { .. } => "",
    };
    body.push_str(&system.table(&[("App", app), ("Unit", unit_name)]));

    let mut details = Vec::new();
    if !evidence.threads.is_empty() {
        let lines: Vec<String> = evidence
            .threads
            .iter()
            .map(|t| format!("{:>7} {} {:<16} {}", t.id, t.state, t.name, t.waiting_in))
            .collect();
        details.push((
            format!("Threads ({}): id, state, name, waiting in", lines.len()),
            lines,
        ));
    }
    if let Some(unit) = unit.filter(|u| !u.state.is_empty()) {
        details.push((format!("State of {}", unit.name), unit.state.clone()));
    }
    // The journal last, with whatever room is left: it is the longest.
    let mut remaining = BODY_LIMIT.saturating_sub(body.len() + 600);
    for (summary, lines) in &details {
        let block = fenced(lines, remaining / 2);
        remaining = remaining.saturating_sub(block.len());
        body.push_str(&format!(
            "\n<details><summary>{summary}</summary>\n\n{block}\n\n</details>\n"
        ));
    }
    if !evidence.journal.is_empty() {
        let from = if unit_name.is_empty() {
            "the process".to_owned()
        } else {
            unit_name.to_owned()
        };
        body.push_str(&format!(
            "\n<details><summary>Journal of {from} from this boot ({} lines)</summary>\n\n{}\n\n</details>\n",
            evidence.journal.len(),
            fenced(&evidence.journal, remaining)
        ));
    }
    body.push_str(&format!(
        "\n<sub>Sent from LosOS Desktop. Fingerprint {fingerprint}: reports of the same problem \
         are added here as comments.</sub>\n"
    ));
    Report {
        title,
        body: redact(&body),
        fingerprint,
    }
}
