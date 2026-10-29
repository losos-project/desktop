//! The installer's backend: what `derisk installer` on the ISO talks to.
//!
//! derisk draws the installer and joins networks itself; this answers its
//! requests in derisk's JSON-lines protocol (derisk's `src/install.rs`), one
//! object per line, requests on stdin and events on stdout:
//!
//! - it says `hello` first, with the system's name, the channel it installs
//!   from and, when a release disk is mounted, `release`, its directory, which
//!   is installed from instead, so no network is needed; `hello` is said
//!   again before a `disks` answer whenever that changed;
//! - `disks` lists the disks [`crate::disks`] offers, the ISO's own and the
//!   release disk left out;
//! - `install` runs [`crate::install`] on the disk named, reporting `steps`,
//!   then a `step` and `line`s as it goes, and ends with `installed` or
//!   `failed`;
//! - `reboot` and `power_off` ask systemd;
//! - after a failed install, `report` writes up the failure
//!   ([`crate::report`]) and answers it as a `report` event for derisk to
//!   show; `report_send` signs in to GitHub and files it, saying
//!   `report_sign_in` with the code to enter, then `reported` or
//!   `report_failed`; `report_cancel` stops waiting for the code. `hello`
//!   names the repository as `report` only when the ISO was given a GitHub
//!   App to sign in with, and derisk offers none of it otherwise.
//!
//! Nothing here decides what lands on the disk, any more than the text
//! interface this replaced did: repart and sysupdate do, from definitions the
//! OS generated.
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use serde_json::{Value, json};

use crate::disks::{self, Disk};
use crate::install::{self, Plan, Step};
use crate::report::{self, Failure, Progress, Target};

/// What the backend was started with.
#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub repart_definitions: PathBuf,
    pub sysupdate_templates: PathBuf,
    pub esp: PathBuf,
    /// Where the ISO is mounted, so its disk is never offered.
    pub medium: String,
    pub work: PathBuf,
    pub source: Option<String>,
    /// [`Plan::index`].
    pub index: Option<String>,
    /// Where a release disk is mounted when one is attached
    /// ([`install::has_release`]).
    pub local_source: Option<PathBuf>,
    /// [`Plan::bios_boot`].
    pub bios_boot: Option<PathBuf>,
    /// The repository whose GitHub release of `release_version` an online
    /// install reads ([`Plan::release`]).
    pub release_repo: Option<String>,
    /// The version the ISO was built as.
    pub release_version: Option<String>,
    /// The cadence the ISO was built for; `nightly` when not given.
    pub release_channel: Option<String>,
    /// The repository failed installs are reported to, `owner/name`; the
    /// release's when not given.
    pub report_repo: Option<String>,
    /// The GitHub App reports are signed in with ([`Target::client_id`]).
    /// Without one, nothing is offered.
    pub report_client_id: Option<String>,
    /// [`Target::login`], `https://github.com` when not given.
    pub report_login: Option<String>,
    /// [`Target::api`], `https://api.github.com` when not given.
    pub report_api: Option<String>,
}

/// Where failed installs are reported, when the ISO says how.
pub fn report_target(config: &Config) -> Option<Target> {
    Some(Target {
        repo: config
            .report_repo
            .clone()
            .or_else(|| config.release_repo.clone())?,
        client_id: config.report_client_id.clone()?,
        login: config
            .report_login
            .clone()
            .unwrap_or_else(|| "https://github.com".to_owned()),
        api: config
            .report_api
            .clone()
            .unwrap_or_else(|| "https://api.github.com".to_owned()),
    })
}

/// The GitHub release an install reads, when the ISO was told both halves.
pub fn github(config: &Config) -> Option<install::GitHub> {
    Some(install::GitHub {
        repo: config.release_repo.clone()?,
        channel: config
            .release_channel
            .clone()
            .unwrap_or_else(|| "nightly".to_owned()),
        version: config.release_version.clone()?,
        // The names release files carry, which are the kernel's: x86_64
        // and aarch64.
        arch: std::env::consts::ARCH.to_owned(),
    })
}

/// The release disk's directory, when one holding a release is mounted.
pub fn release(config: &Config) -> Option<&Path> {
    config
        .local_source
        .as_deref()
        .filter(|dir| install::has_release(dir))
}

pub fn hello(config: &Config) -> Value {
    let mut hello = json!({"event": "hello", "name": config.name, "source": config.source});
    if let Some(dir) = release(config) {
        hello["release"] = json!(dir.to_string_lossy());
    }
    if let Some(target) = report_target(config) {
        hello["report"] = json!(target.repo);
    }
    hello
}

/// How the disk list names `disk`, for a report.
fn describe(disk: &Disk) -> String {
    let size = disk.size as f64 / 1e9;
    if disk.model.is_empty() {
        format!("{}, {size:.1} GB", disk.name)
    } else {
        format!("{}, {size:.1} GB", disk.model)
    }
}

/// The `report` event: what would be sent, to be shown before it is.
pub fn report_event(target: &Target, report: &report::Report) -> Value {
    json!({
        "event": "report",
        "repository": target.repo,
        "title": report.title,
        "body": report.body,
    })
}

pub fn disks_event(disks: &[Disk]) -> Value {
    let disks: Vec<Value> = disks
        .iter()
        .map(|d| {
            json!({
                "path": d.path.to_string_lossy(),
                "name": d.name,
                "model": d.model,
                "size": d.size,
                "removable": d.removable,
            })
        })
        .collect();
    json!({"event": "disks", "disks": disks})
}

pub fn steps_event() -> Value {
    let labels: Vec<&str> = Step::ALL.iter().map(|s| s.title()).collect();
    json!({"event": "steps", "labels": labels})
}

/// A `step` event, with how far a download is when `percent` has a
/// reading from sysupdate's output.
pub fn step_event(step: Step, fraction: Option<f32>) -> Value {
    let index = Step::ALL.iter().position(|s| *s == step).unwrap_or(0);
    json!({
        "event": "step",
        "index": index,
        "count": Step::ALL.len(),
        "label": step.title(),
        "fraction": fraction,
    })
}

/// The last percentage in a line of systemd-pull's or sysupdate's output,
/// as a fraction: "Got 45% of https://…" is 0.45. Each file sysupdate
/// fetches counts from 0 to 100 again; the page shows the current one.
pub fn percent(line: &str) -> Option<f32> {
    let end = line.rfind('%')?;
    let digits: String = line[..end]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let value: f32 = digits.parse().ok()?;
    (value <= 100.0).then_some(value / 100.0)
}

/// One request, parsed.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Disks,
    Install(String),
    Reboot,
    PowerOff,
    Report,
    /// With the person's own words to add, which may be empty.
    ReportSend(String),
    ReportCancel,
}

pub fn parse_request(line: &str) -> Result<Request, String> {
    let value: Value = serde_json::from_str(line.trim()).map_err(|e| format!("not JSON: {e}"))?;
    match value["method"].as_str() {
        Some("disks") => Ok(Request::Disks),
        Some("install") => value["disk"]
            .as_str()
            .filter(|d| d.starts_with("/dev/"))
            .map(|d| Request::Install(d.to_owned()))
            .ok_or_else(|| "install needs a disk under /dev".to_owned()),
        Some("reboot") => Ok(Request::Reboot),
        Some("power_off") => Ok(Request::PowerOff),
        Some("report") => Ok(Request::Report),
        Some("report_send") => Ok(Request::ReportSend(
            value["note"].as_str().unwrap_or_default().to_owned(),
        )),
        Some("report_cancel") => Ok(Request::ReportCancel),
        other => Err(format!("unknown method {other:?}")),
    }
}

fn failed(message: impl Into<String>) -> Value {
    json!({"event": "failed", "message": message.into()})
}

/// Not `failed`: a report that could not be sent is no failed install.
fn report_failed(message: impl Into<String>) -> Value {
    json!({"event": "report_failed", "message": message.into()})
}

/// The disks that can be installed onto, the medium's own left out, and the
/// release disk's: it is being read from, so it is no more a place to install
/// onto than the medium is.
///
/// sysfs and mountinfo are read before this returns; only the release disk's
/// filtering is left to the caller's walk.
fn current_disks(config: &Config) -> impl Iterator<Item = Disk> + use<> {
    let sys = Path::new("/sys");
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let medium = disks::backing_disk(&info, sys, &config.medium);
    let release = config
        .local_source
        .as_deref()
        .and_then(|dir| disks::backing_disk(&info, sys, &dir.to_string_lossy()));
    disks::list(sys, medium.as_deref())
        .into_iter()
        .filter(move |disk| Some(&disk.name) != release.as_ref())
}

/// Serves requests from `input` until it closes, writing events to `output`.
pub fn run(config: &Config, input: impl BufRead, output: impl Write + Send + 'static) {
    let (events, outbox) = mpsc::channel::<Value>();
    // One writer, so an install's events and the answers to other requests
    // never interleave inside a line.
    let writer = thread::spawn(move || {
        let mut output = output;
        for event in outbox {
            if writeln!(output, "{event}")
                .and_then(|()| output.flush())
                .is_err()
            {
                return;
            }
        }
    });
    let mut said = hello(config);
    let _ = events.send(said.clone());

    let mut installing: Option<thread::JoinHandle<()>> = None;
    // What the last install that failed left, for a report.
    let failure: Arc<Mutex<Option<Failure>>> = Arc::default();
    let mut written: Option<report::Report> = None;
    let mut sending: Option<(thread::JoinHandle<()>, Arc<AtomicBool>)> = None;
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request = match parse_request(&line) {
            Ok(request) => request,
            Err(e) => {
                let _ = events.send(failed(e));
                continue;
            }
        };
        let busy = installing.as_ref().is_some_and(|t| !t.is_finished());
        match request {
            Request::Disks => {
                // udev mounts a release disk whenever it is plugged in, so
                // derisk hears of one the next time it asks for the disks.
                let now = hello(config);
                if now != said {
                    let _ = events.send(now.clone());
                    said = now;
                }
                let _ = events.send(disks_event(&current_disks(config).collect::<Vec<_>>()));
            }
            Request::Install(_) if busy => {
                let _ = events.send(failed("An install is already running."));
            }
            Request::Install(disk) => {
                // Only a disk this backend offered, which the person picked
                // from the list: never the medium, never a partition.
                if !current_disks(config).any(|d| d.path == Path::new(&disk)) {
                    let _ = events.send(failed(format!(
                        "{disk} is not a disk that can be installed onto."
                    )));
                    continue;
                }
                let described = current_disks(config)
                    .find(|d| d.path == Path::new(&disk))
                    .map(|d| describe(&d))
                    .unwrap_or_default();
                *failure.lock().expect("never poisoned") = None;
                let plan = Plan {
                    disk: PathBuf::from(disk),
                    repart_definitions: config.repart_definitions.clone(),
                    sysupdate_templates: config.sysupdate_templates.clone(),
                    esp: config.esp.clone(),
                    work: config.work.clone(),
                    source: config.source.clone(),
                    index: config.index.clone(),
                    local: config.local_source.clone(),
                    release: github(config),
                    bios_boot: config.bios_boot.clone(),
                };
                let events = events.clone();
                let failure = Arc::clone(&failure);
                installing = Some(thread::spawn(move || {
                    install_reporting(&plan, &described, &events, &failure)
                }));
            }
            Request::Reboot | Request::PowerOff if busy => {
                let _ = events.send(failed("An install is still running."));
            }
            Request::Reboot => power("reboot", &events),
            Request::PowerOff => power("poweroff", &events),
            Request::Report => {
                let last = failure.lock().expect("never poisoned").clone();
                match (report_target(config), last) {
                    (None, _) => {
                        let _ =
                            events.send(report_failed("This installer does not report problems."));
                    }
                    (_, None) => {
                        let _ = events.send(report_failed("There is no failed install to report."));
                    }
                    (Some(target), Some(last)) => {
                        // The ISO's version is its os-release's: the backend
                        // is told one only when it installs that exact release.
                        let os_release =
                            std::fs::read_to_string("/etc/os-release").unwrap_or_default();
                        let version = report::os_release_version(&os_release)
                            .or_else(|| config.release_version.clone())
                            .unwrap_or_else(|| "unknown version".to_owned());
                        let release = format!("{version}, {}", std::env::consts::ARCH);
                        let system = report::System::read(release);
                        let built = report::build(&last, &system, &report::journal());
                        let _ = events.send(report_event(&target, &built));
                        written = Some(built);
                    }
                }
            }
            Request::ReportSend(_) if sending.as_ref().is_some_and(|(t, _)| !t.is_finished()) => {
                let _ = events.send(report_failed("The report is already being sent."));
            }
            Request::ReportSend(note) => {
                let (Some(target), Some(built)) = (report_target(config), written.clone()) else {
                    let _ = events.send(report_failed("Write the report before sending it."));
                    continue;
                };
                let cancel = Arc::new(AtomicBool::new(false));
                let events = events.clone();
                let scratch = config.work.join("report");
                let stop = Arc::clone(&cancel);
                let thread = thread::spawn(move || {
                    let tell = |progress| {
                        let _ = events.send(match progress {
                            Progress::SignIn { uri, code } => {
                                json!({"event": "report_sign_in", "uri": uri, "code": code})
                            }
                            Progress::Sent { url, existing } => {
                                json!({"event": "reported", "url": url, "existing": existing})
                            }
                        });
                    };
                    let body = report::with_note(&built, report::NOTE_HEADING, &note);
                    if let Err(e) = report::send(&target, &built, &body, &scratch, &stop, tell) {
                        let _ = events.send(report_failed(e));
                    }
                });
                sending = Some((thread, cancel));
            }
            Request::ReportCancel => {
                if let Some((_, cancel)) = &sending {
                    cancel.store(true, Ordering::Release);
                }
            }
        }
    }
    if let Some((t, cancel)) = sending {
        cancel.store(true, Ordering::Release);
        let _ = t.join();
    }
    if let Some(t) = installing {
        let _ = t.join();
    }
    drop(events);
    let _ = writer.join();
}

/// Runs the install and turns its events into the protocol's, keeping what
/// a report of a failure would need in `failure`.
fn install_reporting(
    plan: &Plan,
    disk: &str,
    events: &mpsc::Sender<Value>,
    failure: &Mutex<Option<Failure>>,
) {
    let _ = events.send(steps_event());
    let (tx, rx) = mpsc::channel();
    let plan = plan.clone();
    let worker = thread::spawn(move || install::run(&plan, &tx));
    let mut step = Step::Partition;
    let mut log = std::collections::VecDeque::with_capacity(report::LOG_LINES);
    for event in rx {
        let out = match event {
            install::Event::Step(s) => {
                step = s;
                step_event(s, None)
            }
            install::Event::Line(text) => {
                if step == Step::Download
                    && let Some(fraction) = percent(&text)
                {
                    let _ = events.send(step_event(step, Some(fraction)));
                }
                // A download's progress lines would push out everything
                // that says why it stopped.
                if percent(&text).is_none() {
                    if log.len() == report::LOG_LINES {
                        log.pop_front();
                    }
                    log.push_back(text.clone());
                }
                json!({"event": "line", "text": text})
            }
            install::Event::Finished(Ok(())) => json!({"event": "installed"}),
            install::Event::Finished(Err(e)) => {
                let index = Step::ALL.iter().position(|s| *s == step).unwrap_or(0);
                *failure.lock().expect("never poisoned") = Some(Failure {
                    step: step.title().to_owned(),
                    position: (index + 1, Step::ALL.len()),
                    message: e.clone(),
                    log: std::mem::take(&mut log),
                    disk: disk.to_owned(),
                });
                failed(e)
            }
        };
        let _ = events.send(out);
    }
    let _ = worker.join();
}

fn power(verb: &str, events: &mpsc::Sender<Value>) {
    if let Err(e) = Command::new("systemctl").arg(verb).status() {
        let _ = events.send(failed(format!("systemctl {verb}: {e}")));
    }
}

/// Serves on this process's stdin and stdout.
pub fn serve_stdio(config: &Config) {
    run(config, io::stdin().lock(), io::stdout());
}
