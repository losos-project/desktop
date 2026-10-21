//! A session with no screen: a script of what a person would do, run
//! against a guest, with every window's pictures saved and checked. CI runs
//! it on real Macs to test the whole path, from an app drawing on macOS to
//! a decoded picture here, and keeps the recording it makes, which
//! `replay` shows as windows on a desktop.
//!
//! One step per line; `#` starts a comment.
//!
//!     launch com.apple.TextEdit        start an app
//!     window com.apple.TextEdit [s]    wait for its window and its first picture
//!     keys Hello, Mac!                 type by key position, as a US keyboard would
//!     text héllo                       type text as it is
//!     key cmd+a                        a key with modifiers
//!     hold right 1500                  hold a key, repeating, for so many ms
//!     click 120 80 [right]            press and release at a point in the window
//!     drag 10 10 200 10                press, move and release
//!     scroll 0 200                     scroll by so many points
//!     shot name                        save the window's picture as name.png
//!     shot-popup name [s]              wait for a popup of its app and save it
//!     expect-change [s]                wait for a picture unlike the last shot
//!     resize 700 450                   ask for a size
//!     expect-size 700 450 [s]
//!     expect-title Untitled [s]
//!     expect-menus Format [s]          focus the window, wait for its menus
//!     menu Format > Font > Bold        pick a menu item by its labels
//!     clipboard some text              set the guest's clipboard
//!     expect-clipboard some text [s]
//!     latency 20                       time a key's round trip so many times
//!     expect-rate 20                   pictures per second during the last hold
//!     close                            close the window as its close button would
//!     expect-closed [s]
//!     wait 500
//!
//! Everything goes in the output folder: the pictures, `report.json` with
//! each step's outcome and what the video cost, and `session.rec`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use app_windows_protocol::{
    modifiers, ActivateMenu, Button, Clipboard, Key, Launch, Menu, MenuEntry, Message, Pointer,
    Resize, Scroll, Text, Welcome, WindowInfo, WindowKind, WindowRef,
};
use crossbeam_channel::{unbounded, Receiver};
use serde::Serialize;

use crate::decode::Picture;
use crate::link::{self, Link, Target};
use crate::stream::{self, Event, Frames, Stats};

/// How long a step waits when its line does not say.
const DEFAULT_WAIT: f64 = 10.0;

struct Seen {
    count: u64,
    latest: Picture,
}

/// Everything the guest has said so far.
#[derive(Default)]
struct World {
    windows: BTreeMap<u32, WindowInfo>,
    pictures: HashMap<u32, Seen>,
    menus: HashMap<u32, Vec<Menu>>,
    clipboard: Option<String>,
    errors: Vec<String>,
    lost: Option<String>,
}

struct Run {
    link: Link,
    events: Receiver<Event>,
    frames: Arc<Frames>,
    stats: Stats,
    world: World,
    out: PathBuf,
    /// The window steps act on.
    current: Option<u32>,
    /// A hash of the picture last saved, for `expect-change`.
    last_shot: Option<u64>,
    /// Pictures and time of the last `hold`, for `expect-rate`.
    last_hold: Option<(u64, Duration)>,
    latencies: Vec<f64>,
}

#[derive(Serialize)]
struct StepReport {
    line: usize,
    step: String,
    ok: bool,
    millis: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Report<'a> {
    welcome: &'a Welcome,
    passed: bool,
    steps: Vec<StepReport>,
    windows: BTreeMap<u32, stream::WindowStats>,
    /// Key press to the picture showing it, in milliseconds.
    latency_ms: Option<Latency>,
    guest_errors: &'a [String],
}

#[derive(Serialize)]
struct Latency {
    samples: usize,
    median: f64,
    p90: f64,
    max: f64,
}

pub fn main(target: &Target, script: &Path, out: &Path, hardware: bool) -> Result<bool, String> {
    let script =
        std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let (welcome, link, reader) = link::connect(target)?;
    eprintln!(
        "connected to {} ({}), video encoded in {}{}",
        welcome.name,
        welcome.system,
        welcome.encoder,
        if welcome.missing.is_empty() {
            String::new()
        } else {
            format!(", still to allow: {}", welcome.missing.join(", "))
        }
    );
    let recording = std::fs::File::create(out.join("session.rec")).map_err(|e| e.to_string())?;
    let (tx, events) = unbounded();
    let frames = Arc::new(Frames::default());
    let stats: Stats = Arc::default();
    {
        let (link, frames, stats) = (link.clone(), frames.clone(), stats.clone());
        let options = stream::Options {
            hardware,
            recording: Some(recording),
        };
        thread::Builder::new()
            .name("read".into())
            .spawn(move || {
                stream::run(
                    reader,
                    link,
                    options,
                    frames,
                    stats,
                    Arc::new(move |e| drop(tx.send(e))),
                )
            })
            .map_err(|e| e.to_string())?;
    }
    link.send(Message::Watch);
    let mut run = Run {
        link,
        events,
        frames,
        stats,
        world: World::default(),
        out: out.to_owned(),
        current: None,
        last_shot: None,
        last_hold: None,
        latencies: Vec::new(),
    };

    let mut steps = Vec::new();
    let mut passed = welcome.missing.is_empty();
    if !passed {
        eprintln!("not running the script: the guest cannot see or drive its windows yet");
    }
    for (number, line) in script.lines().enumerate() {
        let line = line.split(" #").next().unwrap_or_default().trim();
        if line.is_empty() || line.starts_with('#') || !passed {
            continue;
        }
        let started = Instant::now();
        let result = run.step(line);
        let millis = started.elapsed().as_millis();
        eprintln!(
            "{:>5} ms  {}  {line}",
            millis,
            if result.is_ok() { "ok  " } else { "FAIL" }
        );
        if let Err(e) = &result {
            eprintln!("          {e}");
            passed = false;
        }
        steps.push(StepReport {
            line: number + 1,
            step: line.to_owned(),
            ok: result.is_ok(),
            millis,
            error: result.err(),
        });
    }
    run.pump(Duration::from_millis(200), |_| false);

    let mut latency = run.latencies.clone();
    latency.sort_by(f64::total_cmp);
    let at = |q: f64| latency[((latency.len() - 1) as f64 * q).round() as usize];
    let report = Report {
        welcome: &welcome,
        passed,
        steps,
        windows: run
            .stats
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect(),
        latency_ms: (!latency.is_empty()).then(|| Latency {
            samples: latency.len(),
            median: at(0.5),
            p90: at(0.9),
            max: *latency.last().unwrap(),
        }),
        guest_errors: &run.world.errors,
    };
    let json = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(out.join("report.json"), &json).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(passed)
}

fn seconds(arg: Option<&str>) -> Result<Duration, String> {
    let s = arg
        .map(str::parse::<f64>)
        .transpose()
        .map_err(|e| format!("not a number of seconds: {e}"))?;
    Ok(Duration::from_secs_f64(s.unwrap_or(DEFAULT_WAIT)))
}

fn number(arg: Option<&str>) -> Result<f64, String> {
    arg.ok_or("a number is missing")?
        .parse()
        .map_err(|e| format!("not a number: {e}"))
}

fn hash(picture: &Picture) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (picture.width, picture.height).hash(&mut h);
    picture.pixels.hash(&mut h);
    h.finish()
}

impl Run {
    /// Takes in what the guest says until `done` or the time is up, and
    /// says which.
    fn pump(&mut self, within: Duration, mut done: impl FnMut(&World) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if done(&self.world) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let Ok(event) = self.events.recv_timeout(left) else {
                return done(&self.world);
            };
            self.take(event);
            while let Ok(event) = self.events.try_recv() {
                self.take(event);
            }
        }
    }

    fn take(&mut self, event: Event) {
        let world = &mut self.world;
        match event {
            Event::Message(Message::WindowOpened(info) | Message::WindowChanged(info)) => {
                world.windows.insert(info.window, info);
            }
            Event::Message(Message::WindowClosed(WindowRef { window })) => {
                world.windows.remove(&window);
                world.pictures.remove(&window);
            }
            Event::Message(Message::Menus(m)) => {
                world.menus.insert(m.window, m.menus);
            }
            Event::Message(Message::Clipboard(c)) => world.clipboard = Some(c.text),
            Event::Message(Message::Error(e)) => {
                eprintln!("          the guest says: {}", e.message);
                world.errors.push(e.message);
            }
            Event::Message(_) => {}
            Event::Picture(window) => {
                if let Some(latest) = self.frames.take(window) {
                    let count = world.pictures.get(&window).map_or(0, |s| s.count) + 1;
                    world.pictures.insert(window, Seen { count, latest });
                }
            }
            Event::Lost(why) => world.lost = Some(why),
        }
    }

    fn current(&self) -> Result<WindowInfo, String> {
        let id = self
            .current
            .ok_or("no window yet: a `window` step picks one")?;
        self.world
            .windows
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("window {id} has closed"))
    }

    fn pictures(&self, window: u32) -> u64 {
        self.world.pictures.get(&window).map_or(0, |s| s.count)
    }

    fn save(&mut self, window: u32, name: &str) -> Result<(), String> {
        let seen = self
            .world
            .pictures
            .get(&window)
            .ok_or("no picture of it yet")?;
        let path = self.out.join(format!("{name}.png"));
        save_png(&seen.latest, &path)?;
        self.last_shot = Some(hash(&seen.latest));
        Ok(())
    }

    fn key(&self, window: u32, code: u32, pressed: bool, mods: u32, text: Option<String>) {
        self.link.send(Message::Key(Key {
            window,
            code,
            pressed,
            modifiers: mods,
            text,
        }));
    }

    /// A key and its modifiers pressed and released in the order hands do.
    fn chord(&self, window: u32, mods: u32, code: u32, text: Option<String>) {
        let held: Vec<u32> = MODIFIER_KEYS
            .iter()
            .filter(|(bit, _)| mods & bit != 0)
            .map(|(_, k)| *k)
            .collect();
        let mut so_far = 0;
        for (bit, code) in MODIFIER_KEYS.iter().filter(|(bit, _)| mods & bit != 0) {
            so_far |= bit;
            self.key(window, *code, true, so_far, None);
        }
        self.key(window, code, true, mods, text);
        self.key(window, code, false, mods, None);
        for code in held.iter().rev() {
            self.key(window, *code, false, 0, None);
        }
        // CGEvent posts are not queued behind each other's handling; a
        // moment between keys keeps them in order in the app.
        thread::sleep(Duration::from_millis(15));
    }

    fn step(&mut self, line: &str) -> Result<(), String> {
        if let Some(why) = &self.world.lost {
            return Err(format!("the connection is gone: {why}"));
        }
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let args: Vec<&str> = rest.split_whitespace().collect();
        match verb {
            "launch" => {
                self.link.send(Message::Launch(Launch {
                    id: rest.trim().to_owned(),
                }));
                Ok(())
            }
            "window" => {
                let app = args.first().ok_or("which app?")?.to_string();
                let found = |w: &World| {
                    w.windows
                        .values()
                        .filter(|i| i.app == app && i.kind == WindowKind::Normal)
                        .filter(|i| w.pictures.contains_key(&i.window))
                        // The app's main window, not a palette beside it.
                        .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
                        .map(|i| i.window)
                };
                if !self.pump(seconds(args.get(1).copied())?, |w| found(w).is_some()) {
                    let open: Vec<String> = self
                        .world
                        .windows
                        .values()
                        .map(|i| format!("{} \"{}\"", i.app, i.title))
                        .collect();
                    return Err(format!("no window of {app} with a picture; open: {open:?}"));
                }
                self.current = found(&self.world);
                let info = self.current()?;
                eprintln!(
                    "          window {} \"{}\" {}x{} at {}x",
                    info.window, info.title, info.width, info.height, info.scale
                );
                Ok(())
            }
            "keys" => {
                let window = self.current()?.window;
                for c in rest.chars() {
                    let (code, shift) =
                        us_key(c).ok_or_else(|| format!("no key on a US keyboard types {c:?}"))?;
                    self.chord(
                        window,
                        if shift { modifiers::SHIFT } else { 0 },
                        code,
                        Some(c.to_string()),
                    );
                }
                Ok(())
            }
            "text" => {
                let window = self.current()?.window;
                self.link.send(Message::Text(Text {
                    window,
                    text: rest.to_owned(),
                }));
                Ok(())
            }
            "key" => {
                let window = self.current()?.window;
                let (mods, code) = combo(rest.trim())?;
                self.chord(window, mods, code, None);
                Ok(())
            }
            "hold" => {
                let window = self.current()?.window;
                let (mods, code) = combo(args.first().ok_or("which key?")?)?;
                let length = Duration::from_millis(number(args.get(1).copied())? as u64);
                let before = self.pictures(window);
                let started = Instant::now();
                // What a host's own key repeat does: the press again and
                // again, 30 times a second, until the release.
                while started.elapsed() < length {
                    self.key(window, code, true, mods, None);
                    self.pump(Duration::from_millis(33), |_| false);
                }
                self.key(window, code, false, mods, None);
                self.pump(Duration::from_millis(300), |_| false);
                self.last_hold = Some((self.pictures(window) - before, started.elapsed()));
                Ok(())
            }
            "expect-rate" => {
                let wanted = number(args.first().copied())?;
                let (pictures, took) = self.last_hold.ok_or("no `hold` to measure")?;
                let rate = pictures as f64 / took.as_secs_f64();
                eprintln!(
                    "          {pictures} pictures in {:.2} s: {rate:.1} a second",
                    took.as_secs_f64()
                );
                if rate < wanted {
                    return Err(format!("{rate:.1} pictures a second, wanted {wanted}"));
                }
                Ok(())
            }
            "click" | "drag" | "scroll" => {
                let window = self.current()?.window;
                let n = |i: usize| number(args.get(i).copied());
                match verb {
                    "click" => {
                        let (x, y) = (n(0)?, n(1)?);
                        let button = match args.get(2).copied() {
                            Some("right") => 1,
                            Some("middle") => 2,
                            _ => 0,
                        };
                        self.link.send(Message::Pointer(Pointer { window, x, y }));
                        for pressed in [true, false] {
                            self.link.send(Message::Button(Button {
                                window,
                                button,
                                pressed,
                                x,
                                y,
                            }));
                        }
                    }
                    "drag" => {
                        let (x0, y0, x1, y1) = (n(0)?, n(1)?, n(2)?, n(3)?);
                        self.link.send(Message::Pointer(Pointer {
                            window,
                            x: x0,
                            y: y0,
                        }));
                        self.link.send(Message::Button(Button {
                            window,
                            button: 0,
                            pressed: true,
                            x: x0,
                            y: y0,
                        }));
                        for i in 1..=20 {
                            let t = f64::from(i) / 20.0;
                            let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
                            self.link.send(Message::Pointer(Pointer { window, x, y }));
                            thread::sleep(Duration::from_millis(16));
                        }
                        self.link.send(Message::Button(Button {
                            window,
                            button: 0,
                            pressed: false,
                            x: x1,
                            y: y1,
                        }));
                    }
                    _ => {
                        let info = self.current()?;
                        let (dx, dy) = (n(0)?, n(1)?);
                        let (x, y) = (info.width / 2.0, info.height / 2.0);
                        self.link.send(Message::Scroll(Scroll {
                            window,
                            x,
                            y,
                            dx,
                            dy,
                        }));
                    }
                }
                Ok(())
            }
            "shot" => {
                let window = self.current()?.window;
                // The picture after whatever was just sent, not before it.
                self.pump(Duration::from_millis(400), |_| false);
                self.save(window, args.first().ok_or("which name?")?)
            }
            "shot-popup" => {
                let parent = self.current()?;
                let popup = |w: &World| {
                    w.windows
                        .values()
                        .filter(|i| i.app == parent.app && i.kind == WindowKind::Popup)
                        .filter(|i| w.pictures.contains_key(&i.window))
                        .map(|i| i.window)
                        .max()
                };
                if !self.pump(seconds(args.get(1).copied())?, |w| popup(w).is_some()) {
                    return Err(format!("no popup of {} came", parent.app));
                }
                let window = popup(&self.world).unwrap();
                self.pump(Duration::from_millis(300), |_| false);
                let info = &self.world.windows[&window];
                eprintln!(
                    "          popup {window} {}x{} at ({}, {}) from its window",
                    info.width,
                    info.height,
                    info.x - parent.x,
                    info.y - parent.y
                );
                self.save(window, args.first().ok_or("which name?")?)
            }
            "expect-change" => {
                let window = self.current()?.window;
                let before = self.last_shot;
                let changed = self.pump(seconds(args.first().copied())?, |w| {
                    w.pictures
                        .get(&window)
                        .is_some_and(|s| Some(hash(&s.latest)) != before)
                });
                if !changed {
                    return Err("the window looks the same as at the last shot".into());
                }
                Ok(())
            }
            "resize" => {
                let window = self.current()?.window;
                self.link.send(Message::Resize(Resize {
                    window,
                    width: number(args.first().copied())?,
                    height: number(args.get(1).copied())?,
                }));
                Ok(())
            }
            "expect-size" => {
                let window = self.current()?.window;
                let (width, height) = (
                    number(args.first().copied())?,
                    number(args.get(1).copied())?,
                );
                // The picture too, not only the window's outline.
                let sized = |w: &World| {
                    let close = |a: f64, b: f64| (a - b).abs() <= 2.0;
                    w.windows
                        .get(&window)
                        .zip(w.pictures.get(&window))
                        .is_some_and(|(i, s)| {
                            close(i.width, width)
                                && close(i.height, height)
                                && close(f64::from(s.latest.width) / i.scale, width)
                                && close(f64::from(s.latest.height) / i.scale, height)
                        })
                };
                if !self.pump(seconds(args.get(2).copied())?, sized) {
                    let info = self.current()?;
                    return Err(format!("still {}x{}", info.width, info.height));
                }
                Ok(())
            }
            "expect-title" => {
                let window = self.current()?.window;
                let (title, wait) = trailing_seconds(rest);
                let titled = |w: &World| {
                    w.windows
                        .get(&window)
                        .is_some_and(|i| i.title.contains(&title))
                };
                if !self.pump(wait, titled) {
                    return Err(format!("its title is \"{}\"", self.current()?.title));
                }
                Ok(())
            }
            "expect-menus" => {
                let window = self.current()?.window;
                let (title, wait) = trailing_seconds(rest);
                self.world.menus.remove(&window);
                self.link.send(Message::Focus(WindowRef { window }));
                let has = |w: &World| {
                    w.menus
                        .get(&window)
                        .is_some_and(|m| m.iter().any(|m| m.title == title))
                };
                if !self.pump(wait, has) {
                    let titles: Vec<String> = self
                        .world
                        .menus
                        .get(&window)
                        .into_iter()
                        .flatten()
                        .map(|m| m.title.clone())
                        .collect();
                    return Err(format!("no {title} menu; menus: {titles:?}"));
                }
                let menus = &self.world.menus[&window];
                let json = serde_json::to_string_pretty(menus).unwrap();
                std::fs::write(self.out.join("menus.json"), json).map_err(|e| e.to_string())?;
                eprintln!(
                    "          menus: {}",
                    menus
                        .iter()
                        .map(|m| m.title.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                Ok(())
            }
            "menu" => {
                let window = self.current()?.window;
                let path: Vec<&str> = rest.split('>').map(str::trim).collect();
                let menus = self
                    .world
                    .menus
                    .get(&window)
                    .ok_or("no menus yet: `expect-menus` first")?;
                let item = find_item(menus, &path).ok_or_else(|| format!("no item {rest}"))?;
                self.link
                    .send(Message::ActivateMenu(ActivateMenu { window, item }));
                Ok(())
            }
            "clipboard" => {
                self.link.send(Message::Clipboard(Clipboard {
                    text: rest.to_owned(),
                }));
                Ok(())
            }
            "expect-clipboard" => {
                let (text, wait) = trailing_seconds(rest);
                if !self.pump(wait, |w| w.clipboard.as_deref() == Some(text.as_str())) {
                    return Err(format!(
                        "the guest's clipboard is {:?}",
                        self.world.clipboard
                    ));
                }
                Ok(())
            }
            "latency" => {
                let window = self.current()?.window;
                let times = number(args.first().copied())? as usize;
                for _ in 0..times {
                    // Settle, so the picture waited for is this key's.
                    self.pump(Duration::from_millis(250), |_| false);
                    let before = self.pictures(window);
                    let sent = Instant::now();
                    self.chord(window, 0, 57, Some(" ".into()));
                    if self.pump(Duration::from_secs(2), |w| {
                        w.pictures.get(&window).is_some_and(|s| s.count > before)
                    }) {
                        self.latencies.push(sent.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                if self.latencies.is_empty() {
                    return Err("no key showed in a picture".into());
                }
                Ok(())
            }
            "close" => {
                let window = self.current()?.window;
                self.link.send(Message::Close(WindowRef { window }));
                Ok(())
            }
            "expect-closed" => {
                let window = self.current.ok_or("no window")?;
                if !self.pump(seconds(args.first().copied())?, |w| {
                    !w.windows.contains_key(&window)
                }) {
                    return Err("it is still open".into());
                }
                Ok(())
            }
            "wait" => {
                self.pump(
                    Duration::from_millis(number(args.first().copied())? as u64),
                    |_| false,
                );
                Ok(())
            }
            other => Err(format!("unknown step {other}")),
        }
    }
}

/// Text with an optional number of seconds after it.
fn trailing_seconds(rest: &str) -> (String, Duration) {
    match rest.rsplit_once(' ') {
        Some((text, s)) if s.parse::<f64>().is_ok() => (
            text.trim().to_owned(),
            Duration::from_secs_f64(s.parse().unwrap()),
        ),
        _ => (
            rest.trim().to_owned(),
            Duration::from_secs_f64(DEFAULT_WAIT),
        ),
    }
}

fn find_item(menus: &[Menu], path: &[&str]) -> Option<String> {
    let (first, rest) = path.split_first()?;
    let menu = menus.iter().find(|m| m.title == *first)?;
    let mut entries = &menu.entries;
    for (i, label) in rest.iter().enumerate() {
        let last = i + 1 == rest.len();
        let found = entries.iter().find(|e| match e {
            MenuEntry::Item { label: l, .. } | MenuEntry::Submenu { label: l, .. } => l == label,
            MenuEntry::Separator => false,
        })?;
        match found {
            MenuEntry::Item { id, .. } if last => return Some(id.clone()),
            MenuEntry::Submenu { entries: inner, .. } if !last => entries = inner,
            _ => return None,
        }
    }
    None
}

/// Modifier bits and the left-hand key for each.
const MODIFIER_KEYS: [(u32, u32); 4] = [
    (modifiers::CONTROL, 29),
    (modifiers::ALT, 56),
    (modifiers::SHIFT, 42),
    (modifiers::SUPER, 125),
];

/// `cmd+shift+s` and the like.
fn combo(text: &str) -> Result<(u32, u32), String> {
    let mut mods = 0;
    let mut code = None;
    for part in text.to_lowercase().split('+') {
        match part {
            "cmd" | "command" | "super" | "meta" => mods |= modifiers::SUPER,
            "ctrl" | "control" => mods |= modifiers::CONTROL,
            "alt" | "option" => mods |= modifiers::ALT,
            "shift" => mods |= modifiers::SHIFT,
            key => code = Some(named_key(key).ok_or_else(|| format!("no key called {key}"))?),
        }
    }
    Ok((mods, code.ok_or("which key?")?))
}

fn named_key(name: &str) -> Option<u32> {
    Some(match name {
        "escape" | "esc" => 1,
        "backspace" => 14,
        "tab" => 15,
        "return" | "enter" => 28,
        "space" => 57,
        "home" => 102,
        "up" => 103,
        "pageup" => 104,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "pagedown" => 109,
        "delete" => 111,
        f if f.starts_with('f') && f[1..].parse::<u32>().is_ok_and(|n| (1..=10).contains(&n)) => {
            58 + f[1..].parse::<u32>().unwrap()
        }
        single if single.chars().count() == 1 => us_key(single.chars().next()?)?.0,
        _ => return None,
    })
}

/// The key a US keyboard types a character with, and whether with Shift.
fn us_key(c: char) -> Option<(u32, bool)> {
    const ROWS: [(&str, &str, u32); 4] = [
        ("`1234567890-=", "~!@#$%^&*()_+", 0),
        ("qwertyuiop[]\\", "QWERTYUIOP{}|", 1),
        ("asdfghjkl;'", "ASDFGHJKL:\"", 2),
        ("zxcvbnm,./", "ZXCVBNM<>?", 3),
    ];
    // Where each row starts among Linux's key codes; the backquote sits
    // apart from the rest of its row.
    const STARTS: [u32; 4] = [1, 16, 30, 44];
    if c == ' ' {
        return Some((57, false));
    }
    if c == '\n' {
        return Some((28, false));
    }
    for (plain, shifted, row) in ROWS {
        for (shift, keys) in [(false, plain), (true, shifted)] {
            if let Some(i) = keys.chars().position(|k| k == c) {
                let code = match (row, i) {
                    (0, 0) => 41,
                    (0, i) => i as u32 + STARTS[0],
                    (1, 12) => 43,
                    (r, i) => i as u32 + STARTS[r as usize],
                };
                return Some((code, shift));
            }
        }
    }
    None
}

pub fn save_png(picture: &Picture, path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder =
        png::Encoder::new(std::io::BufWriter::new(file), picture.width, picture.height);
    encoder.set_color(png::ColorType::Rgb);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity(picture.width as usize * picture.height as usize * 3);
    for row in picture.pixels.chunks(picture.stride) {
        for px in row[..picture.width as usize * 4].as_chunks::<4>().0 {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_keys_are_linux_codes() {
        assert_eq!(us_key('a'), Some((30, false)));
        assert_eq!(us_key('Q'), Some((16, true)));
        assert_eq!(us_key('1'), Some((2, false)));
        assert_eq!(us_key('0'), Some((11, false)));
        assert_eq!(us_key('!'), Some((2, true)));
        assert_eq!(us_key('`'), Some((41, false)));
        assert_eq!(us_key('\\'), Some((43, false)));
        assert_eq!(us_key(','), Some((51, false)));
        assert_eq!(us_key('?'), Some((53, true)));
        assert_eq!(us_key('"'), Some((40, true)));
        assert_eq!(us_key('é'), None);
        assert_eq!(
            combo("cmd+shift+s"),
            Ok((modifiers::SUPER | modifiers::SHIFT, 31))
        );
        assert_eq!(combo("return"), Ok((0, 28)));
        assert_eq!(combo("f5"), Ok((0, 63)));
    }

    #[test]
    fn menu_items_by_label() {
        let menus = vec![Menu {
            title: "Format".into(),
            entries: vec![
                MenuEntry::Submenu {
                    label: "Font".into(),
                    entries: vec![MenuEntry::Item {
                        id: "2/0/1".into(),
                        label: "Bold".into(),
                        shortcut: None,
                        enabled: true,
                    }],
                },
                MenuEntry::Separator,
            ],
        }];
        assert_eq!(
            find_item(&menus, &["Format", "Font", "Bold"]),
            Some("2/0/1".into())
        );
        assert_eq!(find_item(&menus, &["Format", "Font"]), None);
        assert_eq!(find_item(&menus, &["Edit", "Copy"]), None);
    }
}
