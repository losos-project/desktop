//! Reporting a problem as a GitHub issue: what the installer does after a
//! failed install, and what `losos-report` does for the desktop when a window
//! stops responding, a service fails or an action never finishes.
//!
//! GitHub opens an issue only for a signed-in account, and a token in the
//! image would be anyone's who downloads it. So the person signs in with
//! GitHub's device flow: [`send`] asks GitHub for a short code, derisk shows
//! it with a QR code for github.com/login/device, and the person approves it
//! on a phone or another computer. The token that comes back lives in this
//! process's memory for the one report and is never written down. It belongs
//! to a GitHub App whose only permission is issues, so it can do nothing else
//! with the account.
//!
//! The same problem on many machines should be one issue. Each report's
//! title ends with a [`fingerprint`] of what went wrong, its numbers and
//! device names taken out, and a report whose fingerprint an open issue
//! already carries becomes a comment on that issue instead.
//!
//! Nothing goes in a report that the person cannot read before sending it:
//! derisk shows the whole text, and [`redact`] has already taken out what
//! says where the machine is.
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub mod problem;
pub mod serve;

/// GitHub refuses an issue body over 65536 characters; this leaves room for
/// the note the person adds.
pub const BODY_LIMIT: usize = 60_000;

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

/// What the machine and the image are, for the report's System table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    /// The image's version and the architecture: `20261027.081500, x86_64`.
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

impl System {
    /// The System table, in Markdown, with a row for each of `extra` after
    /// the machine's own. An empty value leaves its row out.
    pub fn table(&self, extra: &[(&str, &str)]) -> String {
        let mut table = "### System\n\n| | |\n| --- | --- |\n".to_owned();
        let rows = [
            ("LosOS", self.release.as_str()),
            ("Firmware", &self.firmware),
            ("Machine", &self.machine),
            ("CPU", &self.cpu),
            ("Memory", &self.memory),
            ("Kernel", &self.kernel),
        ];
        for (name, value) in rows.iter().chain(extra) {
            if !value.is_empty() {
                table.push_str(&format!("| {name} | {} |\n", value.replace('|', "\\|")));
            }
        }
        table
    }
}

/// This image's version and architecture, for [`System::release`]:
/// os-release's `IMAGE_VERSION` when it has one.
pub fn this_release() -> String {
    let os_release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let arch = std::env::consts::ARCH;
    match os_release_version(&os_release) {
        Some(version) => format!("{version}, {arch}"),
        None => arch.to_owned(),
    }
}

/// `IMAGE_VERSION` from an os-release file's text, which the image's
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
pub fn fenced(lines: &[String], budget: usize) -> String {
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

/// The lines `journalctl` prints for `args`, oldest first, uncoloured and
/// unpaged; none when it cannot run.
pub fn journal(args: &[&str]) -> Vec<String> {
    Command::new("journalctl")
        .args(["--no-pager", "--output=short-monotonic"])
        .args(args)
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
/// the rest under `heading` (**From the person installing:**).
pub fn with_note(report: &Report, heading: &str, note: &str) -> String {
    let note = note.trim();
    if note.is_empty() {
        report.body.clone()
    } else {
        let quoted: Vec<String> = note.lines().map(|l| format!("> {l}")).collect();
        format!("**{heading}**\n\n{}\n\n{}", quoted.join("\n"), report.body)
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
/// curl rather than a Rust HTTP client, as everywhere in the installer and
/// the image. The
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

/// Signs in with the device flow, then files `report` under `target` with
/// `body` (its own, or [`with_note`]'s), telling `progress` as it goes. Stops,
/// with an error, once `cancel` is set.
pub fn send(
    target: &Target,
    report: &Report,
    body: &str,
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
    fn a_note_goes_first_under_its_heading() {
        let report = Report {
            title: "t".into(),
            body: "body".into(),
            fingerprint: "00000000".into(),
        };
        assert_eq!(
            with_note(&report, "From the person using it:", "It froze\nagain"),
            "**From the person using it:**\n\n> It froze\n> again\n\nbody"
        );
        assert_eq!(with_note(&report, "x", "  "), "body");
    }

    #[test]
    fn the_system_table_leaves_out_what_it_does_not_know() {
        let system = System {
            release: "20261028.1, x86_64".into(),
            cpu: "a | b".into(),
            ..System::default()
        };
        let table = system.table(&[("App", "org.foo"), ("Unit", "")]);
        assert!(table.contains("| LosOS | 20261028.1, x86_64 |"));
        assert!(table.contains("| CPU | a \\| b |"));
        assert!(table.contains("| App | org.foo |"));
        assert!(!table.contains("| Unit |"));
        assert!(!table.contains("| Machine |"));
    }
}
