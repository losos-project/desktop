//! The guest app's menus in derisk's top bar and command palette.
//!
//! derisk shows a window's menus when something registers them over its
//! agent socket (derisk's crates/derisk/src/ipc.rs). The registering
//! connection owns them and hears each pick as a line of its own, which
//! goes back to the guest as the menu item it names.
//!
//! derisk knows windows by its own ids, so the window is found in its
//! `state` by the app id this program gave it: the focused one, since menus
//! come right after a window takes the focus. On another desktop, or with
//! derisk not running, there is no socket and menus stay in the guest.
//!
//! The same socket floats a new guest window and tiles it again once the
//! guest has resized it to its tile (`Place`, wayland.rs).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use app_windows_protocol::{ActivateMenu, Menu, Message};
use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use serde_json::{json, Value};

use crate::link::Link;

/// A window's menus to put in the bar.
pub struct Register {
    /// The guest's id for the window.
    pub window: u32,
    /// The app id its host window has.
    pub app_id: String,
    pub title: String,
    pub menus: Vec<Menu>,
}

/// A window to float or tile, by derisk's own `float` and `tile` actions.
pub struct Place {
    /// The guest's id for the window.
    pub window: u32,
    pub app_id: String,
    pub title: String,
    pub tile: bool,
}

enum Job {
    Menus(Register),
    Place(Place),
}

#[derive(Clone)]
pub struct Bridge {
    tx: Sender<Job>,
}

impl Bridge {
    pub fn register(&self, register: Register) {
        let _ = self.tx.send(Job::Menus(register));
    }

    pub fn place(&self, place: Place) {
        let _ = self.tx.send(Job::Place(place));
    }
}

fn socket() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DERISK_AGENT_SOCKET").filter(|p| !p.is_empty()) {
        return Some(path.into());
    }
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("derisk/agent.sock"))
}

/// Connects to derisk, if it runs, and returns where to send menus.
pub fn connect(link: Link) -> Option<Bridge> {
    connect_to(&socket()?, link)
}

fn connect_to(path: &std::path::Path, link: Link) -> Option<Bridge> {
    let stream = UnixStream::connect(path).ok()?;
    let reader = BufReader::new(stream.try_clone().ok()?);
    let (tx, rx) = unbounded();
    let (answers_tx, answers) = unbounded();
    // derisk's window ids to the guest's, for picks.
    let windows: Arc<Mutex<HashMap<u64, u32>>> = Arc::default();
    let picked = windows.clone();
    thread::Builder::new()
        .name("derisk events".into())
        .spawn(move || listen(reader, answers_tx, &picked, &link))
        .ok()?;
    thread::Builder::new()
        .name("derisk menus".into())
        .spawn(move || send(stream, rx, answers, &windows))
        .ok()?;
    Some(Bridge { tx })
}

/// Sorts what derisk says into answers to requests and menu picks.
fn listen(
    reader: BufReader<UnixStream>,
    answers: Sender<Value>,
    windows: &Mutex<HashMap<u64, u32>>,
    link: &Link,
) {
    for line in reader.lines() {
        let Ok(line) = line else { return };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("event").and_then(Value::as_str) == Some("menu") {
            let derisk_window = value
                .get("window")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let item = value
                .get("item")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if let Some(&window) = windows.lock().unwrap().get(&derisk_window) {
                link.send(Message::ActivateMenu(ActivateMenu { window, item }));
            }
        } else if value.get("event").is_none() && answers.send(value).is_err() {
            return;
        }
    }
}

fn request(
    stream: &mut UnixStream,
    answers: &Receiver<Value>,
    request: Value,
) -> Result<Value, String> {
    // An answer that came after its request gave up would otherwise be
    // taken for this one's.
    while answers.try_recv().is_ok() {}
    writeln!(stream, "{request}").map_err(|e| e.to_string())?;
    // derisk answers from its own loop, which can be busy for seconds while
    // a new window maps on a slow machine.
    match answers.recv_timeout(Duration::from_secs(10)) {
        Ok(answer) if answer["ok"] == true => Ok(answer["result"].clone()),
        Ok(answer) => Err(answer["error"].as_str().unwrap_or("refused").to_owned()),
        Err(RecvTimeoutError::Timeout) => Err("derisk did not answer".into()),
        Err(RecvTimeoutError::Disconnected) => Err("derisk went away".into()),
    }
}

/// The derisk window showing this app id: the focused one if any, then one
/// with the same title.
fn find(state: &Value, app_id: &str, title: &str) -> Option<u64> {
    let windows = state["windows"].as_array()?;
    let ours = || windows.iter().filter(|w| w["app_id"] == app_id);
    ours()
        .find(|w| w["focused"] == true)
        .or_else(|| ours().find(|w| w["title"] == title))
        .and_then(|w| w["id"].as_u64())
}

/// derisk's id for the guest's `window`: the one it was found as before,
/// or the one `state` shows for its app id and title. A window derisk has
/// not mapped yet is not in its state; it will be once its first picture
/// is drawn, which takes seconds on a slow machine.
fn derisk_window(
    stream: &mut UnixStream,
    answers: &Receiver<Value>,
    windows: &Mutex<HashMap<u64, u32>>,
    (window, app_id, title): (u32, &str, &str),
) -> Result<Option<u64>, String> {
    let known = windows
        .lock()
        .unwrap()
        .iter()
        .find(|(_, w)| **w == window)
        .map(|(d, _)| *d);
    if known.is_some() {
        return Ok(known);
    }
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        let state = request(stream, answers, json!({"method": "state"}))?;
        if let Some(found) = find(&state, app_id, title) {
            windows.lock().unwrap().insert(found, window);
            return Ok(Some(found));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(None)
}

fn send(
    mut stream: UnixStream,
    rx: Receiver<Job>,
    answers: Receiver<Value>,
    windows: &Mutex<HashMap<u64, u32>>,
) {
    while let Ok(first) = rx.recv() {
        // Only the newest menus matter when several came meanwhile; every
        // placement goes, in order.
        let mut jobs = vec![first];
        jobs.extend(rx.try_iter());
        let newest_menus = jobs.iter().rposition(|j| matches!(j, Job::Menus(_)));
        for (i, job) in jobs.into_iter().enumerate() {
            let (window, app_id, title) = match &job {
                Job::Menus(_) if Some(i) != newest_menus => continue,
                Job::Menus(r) => (r.window, r.app_id.clone(), r.title.clone()),
                Job::Place(p) => (p.window, p.app_id.clone(), p.title.clone()),
            };
            let found =
                match derisk_window(&mut stream, &answers, windows, (window, &app_id, &title)) {
                    Ok(found) => found,
                    // One slow answer drops only this job, not the jobs
                    // that come after it.
                    Err(e) => {
                        eprintln!("app-windows: derisk: {e}");
                        continue;
                    }
                };
            let Some(derisk_window) = found else { continue };
            let (asked, what) = match job {
                Job::Menus(register) => (
                    json!({"method": "register_menu", "window": derisk_window, "menus": register.menus}),
                    "menus",
                ),
                Job::Place(place) => (
                    json!({"method": "dispatch", "actions": [{
                        "action": if place.tile { "tile" } else { "float" },
                        "window": derisk_window,
                    }]}),
                    if place.tile { "tiling" } else { "floating" },
                ),
            };
            if let Err(e) = request(&mut stream, &answers, asked) {
                eprintln!("app-windows: derisk refused {app_id}'s {what}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_focused_window_of_the_app() {
        let state = json!({"windows": [
            {"id": 1, "app_id": "macos.com.apple.TextEdit", "title": "A", "focused": false},
            {"id": 2, "app_id": "foot", "title": "~", "focused": false},
            {"id": 3, "app_id": "macos.com.apple.TextEdit", "title": "B", "focused": true},
        ]});
        assert_eq!(find(&state, "macos.com.apple.TextEdit", "A"), Some(3));
        assert_eq!(find(&state, "macos.com.apple.Safari", "A"), None);
        let unfocused = json!({"windows": [
            {"id": 1, "app_id": "macos.com.apple.TextEdit", "title": "A", "focused": false},
            {"id": 4, "app_id": "macos.com.apple.TextEdit", "title": "B", "focused": false},
        ]});
        assert_eq!(find(&unfocused, "macos.com.apple.TextEdit", "B"), Some(4));
    }

    #[test]
    fn menus_go_to_derisk_and_picks_come_back() {
        let dir = std::env::temp_dir().join(format!("app-windows-derisk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // A stand-in for derisk: answers state, takes the menus, then
        // reports a pick of the first item.
        let fake = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut out = stream.try_clone().unwrap();
            let mut lines = BufReader::new(stream).lines();
            let state: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            assert_eq!(state["method"], "state");
            writeln!(out, "{}", json!({"ok": true, "result": {"windows": [
                {"id": 9, "app_id": "macos.com.apple.TextEdit", "title": "Untitled", "focused": true}
            ]}}))
            .unwrap();
            let menus: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            assert_eq!(menus["method"], "register_menu");
            assert_eq!(menus["window"], 9);
            assert_eq!(menus["menus"][0]["title"], "File");
            writeln!(
                out,
                "{}",
                json!({"event": "menu", "window": 9, "item": "0/0"})
            )
            .unwrap();
            writeln!(out, "{}", json!({"ok": true, "result": null})).unwrap();
        });
        let (tx, rx) = unbounded();
        let link = Link::for_test(tx);
        let bridge = connect_to(&path, link).expect("connects to the stand-in");
        bridge.register(Register {
            window: 5,
            app_id: "macos.com.apple.TextEdit".into(),
            title: "Untitled".into(),
            menus: vec![Menu {
                title: "File".into(),
                entries: vec![],
            }],
        });
        let picked = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            picked,
            Message::ActivateMenu(ActivateMenu {
                window: 5,
                item: "0/0".into()
            })
        );
        drop(bridge);
        fake.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_window_floats_then_tiles() {
        let dir = std::env::temp_dir().join(format!("app-windows-place-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // derisk is asked for its state once, then to float the window and
        // to tile it again, by the id it found.
        let fake = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut out = stream.try_clone().unwrap();
            let mut lines = BufReader::new(stream).lines();
            let mut next =
                || -> Value { serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap() };
            assert_eq!(next()["method"], "state");
            writeln!(out, "{}", json!({"ok": true, "result": {"windows": [
                {"id": 4, "app_id": "macos.com.apple.calculator", "title": "Calculator", "focused": true}
            ]}}))
            .unwrap();
            let mut actions = Vec::new();
            for _ in 0..2 {
                let dispatch = next();
                assert_eq!(dispatch["method"], "dispatch");
                assert_eq!(dispatch["actions"][0]["window"], 4);
                actions.push(
                    dispatch["actions"][0]["action"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
                writeln!(out, "{}", json!({"ok": true, "result": null})).unwrap();
            }
            actions
        });
        let (tx, _rx) = unbounded();
        let bridge = connect_to(&path, Link::for_test(tx)).expect("connects to the stand-in");
        for tile in [false, true] {
            bridge.place(Place {
                window: 7,
                app_id: "macos.com.apple.calculator".into(),
                title: "Calculator".into(),
                tile,
            });
        }
        assert_eq!(fake.join().unwrap(), ["float", "tile"]);
        drop(bridge);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
