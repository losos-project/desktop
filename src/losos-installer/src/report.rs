//! Reporting a failed install as a GitHub issue.
//!
//! When an install fails, derisk offers to report it. The report is written
//! here, from what the backend saw: the step it stopped at and why, the
//! tools' output, the boot's warnings from the journal, and what the machine
//! is. derisk shows the whole text before anything is sent, and nothing in
//! it is collected that the person cannot read there.
//!
//! Signing in and filing it, the fingerprint that makes the same failure one
//! issue, and taking addresses out are losos-report's, which the desktop
//! reports its own problems with (`src/losos-report`).
use std::collections::VecDeque;

pub use losos_report::{
    BODY_LIMIT, Progress, Report, System, Target, fenced, fingerprint, normalise,
    os_release_version, redact, send, with_note,
};

/// How many lines of the tools' output a report carries.
pub const LOG_LINES: usize = 300;

/// What the person adds is quoted under this, before the report.
pub const NOTE_HEADING: &str = "From the person installing:";

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
    body.push_str(&system.table(&[("Disk", &failure.disk)]));

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
    losos_report::journal(&["--boot", "--priority=warning", "--lines=200"])
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let noted = with_note(&report, NOTE_HEADING, "It froze\nat 3%");
        assert!(noted.starts_with("**From the person installing:**\n\n> It froze\n> at 3%\n\n"));
        assert_eq!(with_note(&report, NOTE_HEADING, "  "), report.body);
    }
}
