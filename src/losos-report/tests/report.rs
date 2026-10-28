//! losos-report's reports and its protocol, against fixtures and a stand-in
//! for GitHub on 127.0.0.1: nothing here reads this machine.
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use losos_report::problem::{self, Evidence, Problem, Thread, Unit};
use losos_report::serve::{self, Config};
use losos_report::{System, Target};
use serde_json::{Value, json};

fn system() -> System {
    System {
        release: "20261028.120000, x86_64".into(),
        firmware: "UEFI".into(),
        ..System::default()
    }
}

#[test]
fn a_process_is_found_in_its_unit() {
    let app = problem::unit_of(
        "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-derisk-org.gnome.Nautilus@2.service\n",
    )
    .unwrap();
    assert_eq!(app.name, "app-derisk-org.gnome.Nautilus@2.service");
    assert!(app.user);
    let system = problem::unit_of("0::/system.slice/NetworkManager.service\n").unwrap();
    assert_eq!(system.name, "NetworkManager.service");
    assert!(!system.user);
    // The user's manager itself is a system unit.
    let manager =
        problem::unit_of("0::/user.slice/user-1000.slice/user@1000.service/init.scope\n").unwrap();
    assert_eq!(manager.name, "init.scope");
    assert!(manager.user);
    assert_eq!(problem::unit_of("0::/\n"), None);
}

#[test]
fn requests_name_their_problem() {
    assert_eq!(
        Problem::parse(
            &json!({"request": "report", "kind": "not_responding", "app": "org.foo", "pid": 42, "seconds": 9})
        ),
        Ok(Problem::NotResponding {
            app: "org.foo".into(),
            pid: Some(42),
            seconds: 9
        })
    );
    assert_eq!(
        Problem::parse(&json!({"kind": "failed", "unit": "foo.service", "user": false})),
        Ok(Problem::Failed {
            unit: "foo.service".into(),
            user: false
        })
    );
    assert_eq!(
        Problem::parse(
            &json!({"kind": "stuck", "app": "org.derisk.settings", "action": "Pairing headphones", "seconds": 30})
        ),
        Ok(Problem::Stuck {
            app: "org.derisk.settings".into(),
            action: "Pairing headphones".into(),
            seconds: 30,
            pid: None
        })
    );
    assert!(Problem::parse(&json!({"kind": "stuck", "app": "org.foo"})).is_err());
    assert!(Problem::parse(&json!({"kind": "melted"})).is_err());
    assert!(Problem::parse(&json!({})).is_err());
}

#[test]
fn a_hung_window_lists_its_threads_and_its_journal() {
    let problem = Problem::NotResponding {
        app: "org.gnome.Nautilus".into(),
        pid: Some(4242),
        seconds: 12,
    };
    let evidence = Evidence {
        unit: Some(Unit {
            name: "app-derisk-org.gnome.Nautilus@2.service".into(),
            user: true,
            state: vec!["ActiveState=active".into(), "SubState=running".into()],
        }),
        threads: vec![
            Thread {
                id: 4242,
                name: "nautilus".into(),
                state: 'S',
                waiting_in: "futex_wait_queue".into(),
            },
            Thread {
                id: 4250,
                name: "pool-spawner".into(),
                state: 'D',
                waiting_in: "nfs_wait_bit_killable".into(),
            },
        ],
        journal: vec!["[ 12.0] nautilus[4242]: mounting 10.0.0.5:/export".into()],
    };
    let report = problem::build(&problem, &system(), &evidence);
    assert!(
        report
            .title
            .starts_with("org.gnome.Nautilus stopped responding ["),
        "{}",
        report.title
    );
    assert!(report.title.ends_with(&format!("[{}]", report.fingerprint)));
    assert!(report.body.contains("got no reply for 12 s"));
    assert!(report.body.contains("| App | org.gnome.Nautilus |"));
    assert!(
        report
            .body
            .contains("| Unit | app-derisk-org.gnome.Nautilus@2.service |")
    );
    assert!(report.body.contains("Threads (2)"));
    assert!(
        report
            .body
            .contains("4250 D pool-spawner     nfs_wait_bit_killable")
    );
    assert!(report.body.contains("SubState=running"));
    // Addresses come out of the journal too.
    assert!(report.body.contains("mounting [IP]:/export"));
    assert!(!report.body.contains("10.0.0.5"));
}

#[test]
fn every_instance_of_a_failed_unit_is_one_problem() {
    let failed = |unit: &str| {
        let problem = Problem::Failed {
            unit: unit.into(),
            user: true,
        };
        let evidence = Evidence {
            unit: Some(Unit {
                name: unit.into(),
                user: true,
                state: vec!["Result=exit-code".into(), "ExecMainStatus=1".into()],
            }),
            ..Evidence::default()
        };
        problem::build(&problem, &system(), &evidence)
    };
    let one = failed("app-derisk-org.foo@1.service");
    let two = failed("app-derisk-org.foo@7.service");
    assert_eq!(one.fingerprint, two.fingerprint);
    assert!(
        one.title
            .starts_with("app-derisk-org.foo@.service failed (exit-code) ["),
        "{}",
        one.title
    );
    assert!(
        one.body
            .contains("The user service **app-derisk-org.foo@1.service** failed: `exit-code`.")
    );
    assert_ne!(one.fingerprint, failed("other.service").fingerprint);
}

#[test]
fn a_stuck_action_says_what_it_was_doing_and_fits_on_github() {
    let problem = Problem::Stuck {
        app: "org.derisk.settings".into(),
        action: "Pairing headphones".into(),
        seconds: 30,
        pid: Some(1),
    };
    let evidence = Evidence {
        journal: (0..5000)
            .map(|i| format!("line {i} {}", "x".repeat(80)))
            .collect(),
        ..Evidence::default()
    };
    let report = problem::build(&problem, &system(), &evidence);
    assert!(
        report
            .title
            .starts_with("org.derisk.settings: \"Pairing headphones\" never finished [")
    );
    assert!(report.body.contains("still running after 30 s"));
    assert!(report.body.contains("Journal of the process"));
    assert!(report.body.contains("line 4999 "));
    assert!(report.body.contains("earlier lines left out"));
    assert!(report.body.len() < 65_536);
}

/// Captures what `run` writes, for the test to read as it comes.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Sink {
    fn events(&self) -> Vec<Value> {
        String::from_utf8_lossy(&self.0.lock().unwrap())
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Waits for an event named `name`, at most ten seconds.
    fn wait_for(&self, name: &str) -> Value {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(event) = self.events().into_iter().find(|e| e["event"] == name) {
                return event;
            }
            assert!(Instant::now() < until, "no {name} in {:?}", self.events());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn written(problem: &Problem) -> losos_report::Report {
    problem::build(problem, &System::default(), &Evidence::default())
}

#[test]
fn without_an_app_to_sign_in_with_nothing_is_offered() {
    let config = Config {
        target: None,
        scratch: std::env::temp_dir(),
    };
    let sink = Sink::default();
    let input = br#"{"request":"report","kind":"failed","unit":"foo.service"}"#;
    serve::run(&config, &input[..], sink.clone(), written);
    let events = sink.events();
    assert_eq!(events[0], json!({"event": "hello", "report": null}));
    assert_eq!(events[1]["event"], "report_failed");
}

#[test]
fn a_report_is_shown_then_signed_in_for_and_filed() {
    let polls = std::sync::atomic::AtomicUsize::new(0);
    let (url, asked) = fake_github(move |method, path| {
        match (method, path) {
        ("POST", "/login/device/code") => (
            200,
            r#"{"device_code":"dev","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":0}"#.into(),
        ),
        ("POST", "/login/oauth/access_token") => {
            if polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                (200, r#"{"error":"authorization_pending"}"#.into())
            } else {
                (200, r#"{"access_token":"ghu_secret"}"#.into())
            }
        }
        ("GET", p) if p.starts_with("/search/issues?q=") => (200, r#"{"items":[]}"#.into()),
        ("POST", "/repos/losos-project/desktop/issues") => (
            201,
            r#"{"number":7,"html_url":"https://github.com/losos-project/desktop/issues/7"}"#.into(),
        ),
        _ => (404, r#"{"message":"Not Found"}"#.into()),
    }
    });
    let scratch = tempdir();
    let config = Config {
        target: Some(Target {
            repo: "losos-project/desktop".into(),
            client_id: "Iv23liExample".into(),
            login: url.clone(),
            api: url,
        }),
        scratch: scratch.clone(),
    };
    let sink = Sink::default();
    let (reader, mut writer) = std::io::pipe().unwrap();
    let served = {
        let sink = sink.clone();
        std::thread::spawn(move || serve::run(&config, BufReader::new(reader), sink, written))
    };
    assert_eq!(
        sink.wait_for("hello"),
        json!({"event": "hello", "report": "losos-project/desktop"})
    );
    writeln!(
        writer,
        r#"{{"request":"report","kind":"stuck","app":"org.derisk.settings","action":"Adding a user","seconds":30}}"#
    )
    .unwrap();
    let shown = sink.wait_for("report");
    assert_eq!(shown["repository"], "losos-project/desktop");
    let title = shown["title"].as_str().unwrap().to_owned();
    assert!(
        title.contains("\"Adding a user\" never finished"),
        "{title}"
    );
    writeln!(
        writer,
        r#"{{"request":"report_send","note":"Clicked Add twice"}}"#
    )
    .unwrap();
    assert_eq!(
        sink.wait_for("report_sign_in"),
        json!({"event": "report_sign_in", "uri": "https://github.com/login/device", "code": "WDJB-MJHT"})
    );
    assert_eq!(
        sink.wait_for("reported"),
        json!({"event": "reported", "url": "https://github.com/losos-project/desktop/issues/7", "existing": false})
    );
    drop(writer);
    served.join().unwrap();
    let asked: Vec<_> = asked.try_iter().collect();
    let issue: Value = serde_json::from_str(&asked.last().unwrap().2).unwrap();
    assert_eq!(issue["title"], title);
    assert!(
        issue["body"]
            .as_str()
            .unwrap()
            .starts_with("**From the person using it:**\n\n> Clicked Add twice\n\n")
    );
    assert_eq!(
        asked.last().unwrap().1.as_deref(),
        Some("Bearer ghu_secret")
    );
    let _ = std::fs::remove_dir_all(scratch);
}

fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "losos-report-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// GitHub's login and API on 127.0.0.1, answering each request from
/// `answer` and keeping what was asked: the request line, whether a token
/// came with it, and the body.
fn fake_github(
    answer: impl Fn(&str, &str) -> (u16, String) + Send + 'static,
) -> (
    String,
    std::sync::mpsc::Receiver<(String, Option<String>, String)>,
) {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let (mut length, mut token) = (0, None);
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(n) = lower.strip_prefix("content-length:") {
                    length = n.trim().parse().unwrap();
                }
                if lower.starts_with("authorization:") {
                    token = Some(line.trim()["authorization:".len()..].trim().to_owned());
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let mut words = request.split_whitespace();
            let (method, path) = (words.next().unwrap(), words.next().unwrap());
            let (status, json) = answer(method, path);
            let _ = tx.send((format!("{method} {path}"), token, body));
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            )
            .unwrap();
        }
    });
    (url, rx)
}
