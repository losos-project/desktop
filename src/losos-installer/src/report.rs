//! Reporting a failed install as a GitHub issue.
//!
//! When an install fails, derisk offers to report it. The report is written
//! here, from what the backend saw: the step it stopped at and why, the
//! tools' output, the boot's warnings from the journal, and what the machine
//! is. derisk shows the whole text before anything is sent, and nothing in
//! it is collected that the person cannot read there.
//!
//! GitHub opens an issue only for a signed-in account, and a token on the
//! ISO would be anyone's who downloads it. So the person signs in with
//! GitHub's device flow: the installer asks GitHub for a short code, derisk
//! shows it with a QR code for github.com/login/device, and the person
//! approves it on a phone or another computer. The token that comes back
//! lives in this process's memory for the one report and is never written
//! down. It belongs to a GitHub App whose only permission is issues, so it
//! can do nothing else with the account.
//!
//! The same failure on many machines should be one issue. Each report's
//! title ends with a fingerprint of its failure message, its numbers and
//! device names taken out, and a report whose fingerprint an open issue
//! already carries becomes a comment on that issue instead.
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How many lines of the tools' output a report carries.
pub const LOG_LINES: usize = 300;

/// GitHub refuses an issue body over 65536 characters; this leaves room for
/// the note the person adds.
const BODY_LIMIT: usize = 60_000;

/// Where the reports go and how to sign in to send them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `owner/name`.
    pub repo: String,
    /// The GitHub App's client ID, which the device flow names it by. It is
    /// not a secret: anyone can read it from the sign-in page.
    pub client_id: String,
    /// Where GitHub signs people in, `https://github.com`.
    pub login: String,
    /// GitHub's REST API, `https://api.github.com`.
    pub api: String,
}

/// What an install that failed left to report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Failure {
    /// The step's title, as the page showed it.
    pub step: String,
    /// Which step it was, from 1, and how many there are.
    pub position: (usize, usize),
    pub message: String,
    /// The tools' output, oldest first, at most [`LOG_LINES`].
    pub log: VecDeque<String>,
    /// The disk it was installing onto, as the disk list described it.
    pub disk: String,
}

/// What the machine and the ISO are, for the report's System table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    /// The ISO's version and the architecture: `20261027.081500, x86_64`.
    pub release: String,
    pub firmware: String,
    pub machine: String,
    pub cpu: String,
    pub memory: String,
    pub kernel: String,
}

impl System {
    /// This machine's, read from sysfs and procfs.
    pub fn read(release: String) -> Self {
        let read = |path: &str| {
            fs::read_to_string(path)
                .map(|s| s.trim().to_owned())
                .unwrap_or_default()
        };
        let firmware = if Path::new("/sys/firmware/efi").exists() {
            "UEFI"
        } else {
            "BIOS"
        };
        let machine = [
            read("/sys/class/dmi/id/sys_vendor"),
            read("/sys/class/dmi/id/product_name"),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let cpuinfo = read("/proc/cpuinfo");
        let cpu = cpuinfo
            .lines()
            .find_map(|l| {
                let (key, value) = l.split_once(':')?;
                matches!(key.trim(), "model name" | "Model" | "Hardware")
                    .then(|| value.trim().to_owned())
            })
            .unwrap_or_default();
        let threads = cpuinfo
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count();
        let memory = read("/proc/meminfo")
            .lines()
            .find_map(|l| l.strip_prefix("MemTotal:"))
            .and_then(|kb| kb.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
            .map(|kb| format!("{:.1} GiB", kb as f64 / 1024.0 / 1024.0))
            .unwrap_or_default();
        System {
            release,
            firmware: firmware.to_owned(),
            machine,
            cpu: if threads > 0 {
                format!("{cpu} ({threads} threads)")
            } else {
                cpu
            },
            memory,
            kernel: read("/proc/sys/kernel/osrelease"),
        }
    }
}

/// `IMAGE_VERSION` from an os-release file's text, which the ISO's
/// `system.image.version` writes.
pub fn os_release_version(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("IMAGE_VERSION="))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

/// A report, as it is shown and as it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub title: String,
    pub body: String,
    pub fingerprint: String,
}

/// The failure message reduced to what stays the same from one machine to
/// the next: lowercase, each run of digits one `#`, and device names
/// without their number, so `/dev/nvme0n1p2` and `/dev/sda2` match.
pub fn normalise(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let lower = message.to_lowercase();
    let mut rest = lower.as_str();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("/dev/") {
            let end = after
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(after.len());
            out.push_str("/dev/#");
            rest = &after[end..];
            continue;
        }
        let c = rest.chars().next().expect("not empty");
        if c.is_ascii_digit() {
            out.push('#');
            rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Eight hex digits naming a failure: FNV-1a of [`normalise`]d text, which
/// is the same in every build, unlike std's hasher.
pub fn fingerprint(step: &str, message: &str) -> String {
    let text = format!("{step}\n{}", normalise(message));
    let hash = text.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    format!("{hash:08x}")
}

/// `text` with MAC and IPv4 addresses replaced, since a report is public
/// and those can say where a machine is. Hardware names, versions and
/// paths stay: they are what the report is for.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut words = text.split_inclusive(|c: char| c.is_whitespace() || "()[],;'\"<>=".contains(c));
    for word in words.by_ref() {
        let end = word
            .find(|c: char| c.is_whitespace() || "()[],;'\"<>=".contains(c))
            .unwrap_or(word.len());
        let (core, sep) = word.split_at(end);
        // A trailing full stop or colon is the sentence's, not the address's.
        let trimmed = core.trim_end_matches(['.', ':']);
        let tail = &core[trimmed.len()..];
        if is_mac(trimmed) {
            out.push_str("[MAC]");
        } else if let Some(port) = ipv4(trimmed) {
            out.push_str("[IP]");
            out.push_str(port);
        } else {
            out.push_str(trimmed);
        }
        out.push_str(tail);
        out.push_str(sep);
    }
    out
}

fn is_mac(word: &str) -> bool {
    let parts: Vec<&str> = word.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// When `word` is an IPv4 address, with or without a `:port` or `/prefix`,
/// what follows the address. Loopback stays as it is: the installer's own
/// mirror is on 127.0.0.1, and it says nothing about the machine.
fn ipv4(word: &str) -> Option<&str> {
    let end = word.find([':', '/']).unwrap_or(word.len());
    let (address, after) = word.split_at(end);
    let octets: Vec<&str> = address.split('.').collect();
    let valid = octets.len() == 4
        && octets
            .iter()
            .all(|o| !o.is_empty() && o.len() <= 3 && o.parse::<u8>().is_ok());
    (valid && octets[0] != "127").then_some(after)
}

/// A fenced block of `lines`, keeping the newest when they do not fit in
/// `budget` characters.
fn fenced(lines: &[String], budget: usize) -> String {
    let mut kept = Vec::new();
    let mut used = 0;
    for line in lines.iter().rev() {
        used += line.len() + 1;
        if used > budget {
            kept.push("… (earlier lines left out)".to_owned());
            break;
        }
        kept.push(line.clone());
    }
    kept.reverse();
    // A line of backticks in the output would close a fence of three.
    format!(
        "````text\n{}\n````",
        kept.join("\n").replace("````", "` ` ` `")
    )
}

/// The report for `failure` on `system`, with `journal` (the boot's
/// warnings and errors) at the end.
pub fn build(failure: &Failure, system: &System, journal: &[String]) -> Report {
    let fingerprint = fingerprint(&failure.step, &failure.message);
    let first = failure.message.lines().next().unwrap_or("").trim();
    let short: String = first.chars().take(72).collect();
    let ellipsis = if first.chars().count() > 72 {
        "…"
    } else {
        ""
    };
    let title = format!("Installer: {short}{ellipsis} [{fingerprint}]");

    let row = |name: &str, value: &str| {
        if value.is_empty() {
            String::new()
        } else {
            format!("| {name} | {} |\n", value.replace('|', "\\|"))
        }
    };
    let mut body = format!(
        "The install stopped at **{}** (step {} of {}):\n\n{}\n\n",
        failure.step,
        failure.position.0,
        failure.position.1,
        failure
            .message
            .lines()
            .map(|l| format!("> {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    body.push_str("### System\n\n| | |\n| --- | --- |\n");
    body.push_str(&row("LosOS", &system.release));
    body.push_str(&row("Firmware", &system.firmware));
    body.push_str(&row("Machine", &system.machine));
    body.push_str(&row("CPU", &system.cpu));
    body.push_str(&row("Memory", &system.memory));
    body.push_str(&row("Kernel", &system.kernel));
    body.push_str(&row("Disk", &failure.disk));

    // The log first: it is what says why. The journal gets what is left.
    let log: Vec<String> = failure.log.iter().cloned().collect();
    let remaining = BODY_LIMIT.saturating_sub(body.len() + 400);
    let log_block = fenced(&log, remaining * 2 / 3);
    let journal_block = fenced(journal, remaining.saturating_sub(log_block.len()));
    body.push_str(&format!(
        "\n<details><summary>Install output ({} lines)</summary>\n\n{log_block}\n\n</details>\n",
        log.len()
    ));
    if !journal.is_empty() {
        body.push_str(&format!(
            "\n<details><summary>Journal warnings and errors from this boot</summary>\n\n{journal_block}\n\n</details>\n"
        ));
    }
    body.push_str(&format!(
        "\n<sub>Sent by the LosOS installer. Fingerprint {fingerprint}: reports of the same failure are added here as comments.</sub>\n"
    ));
    Report {
        title,
        body: redact(&body),
        fingerprint,
    }
}

/// The boot's warnings and errors, oldest first.
pub fn journal() -> Vec<String> {
    Command::new("journalctl")
        .args([
            "--boot",
            "--priority=warning",
            "--lines=200",
            "--no-pager",
            "--output=short-monotonic",
        ])
        .env("SYSTEMD_COLORS", "0")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `report`'s body with the person's own words, when they wrote any, before
/// the rest.
pub fn with_note(report: &Report, note: &str) -> String {
    let note = note.trim();
    if note.is_empty() {
        report.body.clone()
    } else {
        let quoted: Vec<String> = note.lines().map(|l| format!("> {l}")).collect();
        format!(
            "**From the person installing:**\n\n{}\n\n{}",
            quoted.join("\n"),
            report.body
        )
    }
}

/// What sending a report tells derisk as it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Open `uri` and enter `code` there.
    SignIn { uri: String, code: String },
    /// Filed at `url`; `existing` when it went onto an issue already open.
    Sent { url: String, existing: bool },
}

/// One request to GitHub: `body`'s JSON, or the form fields, sent with the
/// token when there is one. Answers the status and the JSON that came back.
///
/// curl rather than a Rust HTTP client, as everywhere in the installer. The
/// token goes to it on stdin, as a config line, never on its command line,
/// where any process could read it.
fn request(
    method: &str,
    url: &str,
    token: Option<&str>,
    json: Option<&Value>,
    form: &[(&str, &str)],
    scratch: &Path,
) -> Result<(u16, Value), String> {
    let mut args: Vec<String> = [
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "60",
        "--config",
        "-",
        "--write-out",
        "\n%{http_code}",
        "--header",
        "X-GitHub-Api-Version: 2022-11-28",
        "--request",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.push(method.to_owned());
    // The sign-in endpoints answer in form encoding unless asked for JSON;
    // the API wants its own media type, and only the API gets a token.
    args.push("--header".to_owned());
    args.push(if token.is_some() {
        "Accept: application/vnd.github+json".to_owned()
    } else {
        "Accept: application/json".to_owned()
    });
    if method == "GET" {
        // The fields as the query string, not a body.
        args.push("--get".to_owned());
    }
    for (key, value) in form {
        args.push("--data-urlencode".to_owned());
        args.push(format!("{key}={value}"));
    }
    let body_path = scratch.join("report.json");
    if let Some(json) = json {
        // The body can be 60 KB, more than a command line should carry,
        // and it is the person's to read, not every process's.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&body_path)
            .map_err(|e| format!("{}: {e}", body_path.display()))?;
        file.write_all(json.to_string().as_bytes())
            .map_err(|e| format!("{}: {e}", body_path.display()))?;
        args.push("--header".to_owned());
        args.push("Content-Type: application/json".to_owned());
        args.push("--data-binary".to_owned());
        args.push(format!("@{}", body_path.display()));
    }
    args.push(url.to_owned());
    let mut child = Command::new("curl")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    let config = match token {
        Some(token) => format!("header = \"Authorization: Bearer {token}\"\n"),
        None => String::new(),
    };
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(config.as_bytes())
        .map_err(|e| format!("curl: {e}"))?;
    let output = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    let _ = fs::remove_file(&body_path);
    if !output.status.success() {
        return Err(format!(
            "GitHub could not be reached: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (answer, status) = text.rsplit_once('\n').unwrap_or(("", &text));
    let status = status.trim().parse().unwrap_or(0);
    let answer = serde_json::from_str(answer).unwrap_or(Value::Null);
    Ok((status, answer))
}

/// GitHub's own words for a refused request, or its status.
fn refusal(status: u16, answer: &Value) -> String {
    match answer["message"].as_str() {
        Some(message) => format!("GitHub said: {message} ({status})"),
        None => format!("GitHub answered {status}"),
    }
}

/// Signs in with the device flow, then files `report` (with `note`) under
/// `target`, telling `progress` as it goes. Stops, with an error, once
/// `cancel` is set.
pub fn send(
    target: &Target,
    report: &Report,
    note: &str,
    scratch: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(Progress),
) -> Result<(), String> {
    fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let (status, code) = request(
        "POST",
        &format!("{}/login/device/code", target.login),
        None,
        None,
        &[("client_id", &target.client_id)],
        scratch,
    )?;
    let (Some(device), Some(user_code), Some(uri)) = (
        code["device_code"].as_str(),
        code["user_code"].as_str(),
        code["verification_uri"].as_str(),
    ) else {
        return Err(refusal(status, &code));
    };
    progress(Progress::SignIn {
        uri: uri.to_owned(),
        code: user_code.to_owned(),
    });
    let expires = Instant::now() + Duration::from_secs(code["expires_in"].as_u64().unwrap_or(900));
    let mut interval = Duration::from_secs(code["interval"].as_u64().unwrap_or(5));
    let token = loop {
        // A second at a time, so a cancel is seen quickly.
        let until = Instant::now() + interval;
        while Instant::now() < until {
            if cancel.load(Ordering::Acquire) {
                return Err("Cancelled.".to_owned());
            }
            thread::sleep(Duration::from_millis(250));
        }
        if Instant::now() > expires {
            return Err(
                "The code expired before it was entered. Send the report again for a new one."
                    .to_owned(),
            );
        }
        let (status, answer) = request(
            "POST",
            &format!("{}/login/oauth/access_token", target.login),
            None,
            None,
            &[
                ("client_id", &target.client_id),
                ("device_code", device),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
            scratch,
        )?;
        if let Some(token) = answer["access_token"].as_str() {
            break token.to_owned();
        }
        match answer["error"].as_str() {
            Some("authorization_pending") => {}
            // GitHub asks for five seconds more each time.
            Some("slow_down") => interval += Duration::from_secs(5),
            Some("access_denied") => return Err("The sign-in was declined on GitHub.".to_owned()),
            Some("expired_token") => {
                return Err(
                    "The code expired before it was entered. Send the report again for a new one."
                        .to_owned(),
                );
            }
            _ => {
                return Err(match answer["error_description"].as_str() {
                    Some(why) => format!("GitHub said: {why}"),
                    None => refusal(status, &answer),
                });
            }
        }
    };

    let body = with_note(report, note);
    let api = &target.api;
    let repo = &target.repo;
    // An open issue with the same fingerprint is the same failure.
    let query = format!(
        "repo:{repo} is:issue is:open in:title \"[{}]\"",
        report.fingerprint
    );
    // A search that fails only means a new issue, never a lost report.
    let found = request(
        "GET",
        &format!("{api}/search/issues"),
        Some(&token),
        None,
        &[("q", &query)],
        scratch,
    )
    .map(|(_, answer)| answer)
    .unwrap_or(Value::Null);
    let existing = found["items"]
        .as_array()
        .and_then(|items| {
            items.iter().find(|i| {
                i["title"]
                    .as_str()
                    .is_some_and(|t| t.ends_with(&format!("[{}]", report.fingerprint)))
            })
        })
        .and_then(|i| i["number"].as_u64());
    let (status, answer, existing) = match existing {
        Some(number) => {
            let (status, answer) = request(
                "POST",
                &format!("{api}/repos/{repo}/issues/{number}/comments"),
                Some(&token),
                Some(&json!({"body": body})),
                &[],
                scratch,
            )?;
            (status, answer, true)
        }
        None => {
            let (status, answer) = request(
                "POST",
                &format!("{api}/repos/{repo}/issues"),
                Some(&token),
                Some(&json!({"title": report.title, "body": body})),
                &[],
                scratch,
            )?;
            (status, answer, false)
        }
    };
    match answer["html_url"].as_str() {
        Some(url) if (200..300).contains(&status) => {
            progress(Progress::Sent {
                url: url.to_owned(),
                existing,
            });
            Ok(())
        }
        _ => Err(refusal(status, &answer)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_failure_on_two_machines_has_one_fingerprint() {
        assert_eq!(
            normalise("Failed to create   new partition '/dev/nvme0n1p2': busy (16)"),
            "failed to create new partition '/dev/#': busy (#)"
        );
        assert_eq!(
            fingerprint(
                "Partitioning the disk",
                "Failed to mount /dev/sda2 after 10 s"
            ),
            fingerprint(
                "Partitioning the disk",
                "Failed to mount /dev/vda2 after 30 s"
            ),
        );
        assert_ne!(
            fingerprint("Partitioning the disk", "x"),
            fingerprint("Finishing", "x")
        );
        assert_eq!(fingerprint("a", "b").len(), 8);
    }

    #[test]
    fn addresses_are_taken_out_and_the_rest_left() {
        assert_eq!(
            redact("eth0 52:54:00:12:34:56 got 10.0.2.15/24 via 10.0.2.2."),
            "eth0 [MAC] got [IP]/24 via [IP]."
        );
        assert_eq!(
            redact("GET http://127.0.0.1:4321/SHA256SUMS (2.6.1, v10.0.2)"),
            "GET http://127.0.0.1:4321/SHA256SUMS (2.6.1, v10.0.2)"
        );
        assert_eq!(redact("time 12:30:01 ok"), "time 12:30:01 ok");
    }

    #[test]
    fn a_report_says_where_it_stopped_and_fits_on_github() {
        let failure = Failure {
            step: "Downloading and writing the system".into(),
            position: (3, 4),
            message: "oras failed (exit status: 1)".into(),
            log: (0..LOG_LINES)
                .map(|i| format!("line {i} {}", "x".repeat(400)))
                .collect(),
            disk: "QEMU HARDDISK, 21.5 GB".into(),
        };
        let system = System {
            release: "20261027.081500, x86_64".into(),
            firmware: "UEFI".into(),
            ..System::default()
        };
        let journal: Vec<String> = (0..200).map(|i| format!("journal {i}")).collect();
        let report = build(&failure, &system, &journal);
        assert!(
            report
                .title
                .starts_with("Installer: oras failed (exit status: 1) [")
        );
        assert!(report.title.ends_with(&format!("[{}]", report.fingerprint)));
        assert!(
            report
                .body
                .contains("**Downloading and writing the system** (step 3 of 4)")
        );
        assert!(report.body.contains("| Firmware | UEFI |"));
        assert!(!report.body.contains("| Machine |"));
        assert!(report.body.contains(&format!("line {} ", LOG_LINES - 1)));
        assert!(report.body.contains("earlier lines left out"));
        assert!(report.body.len() < 65_536);
        assert_eq!(
            os_release_version("NAME=x\nIMAGE_VERSION=\"20261027.081500\"\n").as_deref(),
            Some("20261027.081500")
        );
        assert_eq!(os_release_version("NAME=x\n"), None);
        let noted = with_note(&report, "It froze\nat 3%");
        assert!(noted.starts_with("**From the person installing:**\n\n> It froze\n> at 3%\n\n"));
        assert_eq!(with_note(&report, "  "), report.body);
    }
}
