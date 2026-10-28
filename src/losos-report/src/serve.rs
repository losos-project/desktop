//! `losos-report serve`: the reporter derisk runs as a child for each report,
//! talking to it in JSON lines on stdin and stdout.
//!
//! It speaks the installer's report protocol (`losos-installer serve`), so
//! derisk draws both with the same page:
//!
//! - it starts with `hello`, naming the repository as `report`, or `null`
//!   when this image was built without a GitHub App to sign in with;
//! - `report` names a [`Problem`] in its `kind` and fields, and is answered
//!   with a `report` event: the repository, title and body that would be
//!   sent, for derisk to show before anything is;
//! - `report_send` signs in to GitHub and files it, saying `report_sign_in`
//!   with the code to enter, then `reported` or `report_failed`;
//!   `report_cancel` stops waiting for the code.
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};

use crate::problem::{self, Problem};
use crate::{Progress, Report, System, Target};

/// What the person adds is quoted under this, before the report.
const NOTE_HEADING: &str = "From the person using it:";

/// How it was started.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where reports go, when this image can send them.
    pub target: Option<Target>,
    /// A private directory for the request bodies curl reads.
    pub scratch: PathBuf,
}

/// The `hello` that starts the conversation.
pub fn hello(config: &Config) -> Value {
    json!({
        "event": "hello",
        "report": config.target.as_ref().map(|t| t.repo.clone()),
    })
}

fn report_failed(message: impl Into<String>) -> Value {
    json!({"event": "report_failed", "message": message.into()})
}

/// Serves requests from `input` until it closes, writing events to `output`.
/// `write` turns a problem into its report; [`serve_stdio`] reads this
/// machine for it, and tests pass fixtures.
pub fn run(
    config: &Config,
    input: impl BufRead,
    output: impl Write + Send + 'static,
    write: impl Fn(&Problem) -> Report,
) {
    let (events, outbox) = mpsc::channel::<Value>();
    // One writer, so the sending thread's events and the answers to other
    // requests never interleave inside a line.
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
    let _ = events.send(hello(config));

    let mut written: Option<Report> = None;
    let mut sending: Option<(thread::JoinHandle<()>, Arc<AtomicBool>)> = None;
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                let _ = events.send(report_failed(format!("not a request: {e}")));
                continue;
            }
        };
        let busy = sending.as_ref().is_some_and(|(t, _)| !t.is_finished());
        match request["request"].as_str() {
            Some("report") => {
                let Some(target) = &config.target else {
                    let _ = events.send(report_failed("This system does not report problems."));
                    continue;
                };
                match Problem::parse(&request) {
                    Ok(problem) => {
                        let report = write(&problem);
                        let _ = events.send(json!({
                            "event": "report",
                            "repository": target.repo,
                            "title": report.title,
                            "body": report.body,
                        }));
                        written = Some(report);
                    }
                    Err(e) => {
                        let _ = events.send(report_failed(e));
                    }
                }
            }
            Some("report_send") if busy => {
                let _ = events.send(report_failed("The report is already being sent."));
            }
            Some("report_send") => {
                let (Some(target), Some(report)) = (config.target.clone(), written.clone()) else {
                    let _ = events.send(report_failed("Write the report before sending it."));
                    continue;
                };
                let note = request["note"].as_str().unwrap_or("").to_owned();
                let cancel = Arc::new(AtomicBool::new(false));
                let stop = Arc::clone(&cancel);
                let events = events.clone();
                let scratch = config.scratch.clone();
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
                    let body = crate::with_note(&report, NOTE_HEADING, &note);
                    if let Err(e) = crate::send(&target, &report, &body, &scratch, &stop, tell) {
                        let _ = events.send(report_failed(e));
                    }
                });
                sending = Some((thread, cancel));
            }
            Some("report_cancel") => {
                if let Some((_, cancel)) = &sending {
                    cancel.store(true, Ordering::Release);
                }
            }
            Some(other) => {
                let _ = events.send(report_failed(format!("no such request: {other}")));
            }
            None => {
                let _ = events.send(report_failed("a request needs a `request`"));
            }
        }
    }
    if let Some((thread, cancel)) = sending {
        cancel.store(true, Ordering::Release);
        let _ = thread.join();
    }
    drop(events);
    let _ = writer.join();
}

/// [`run`] on stdin and stdout, reading this machine for each report.
pub fn serve_stdio(config: &Config) {
    let system = System::read(crate::this_release());
    run(
        config,
        std::io::stdin().lock(),
        std::io::stdout(),
        |problem| problem::build(problem, &system, &problem::gather(problem)),
    );
}
