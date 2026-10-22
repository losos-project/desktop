//! Connections: who may talk to the server, and what each one asked for.
//!
//! Each connection has a thread that reads it and one that writes it, the
//! writer fed through a bounded channel. Control messages wait for room in
//! it; video does not, so a client that falls behind loses pictures, not
//! its windows' comings and goings, and gets a keyframe once it has caught
//! up (`Hub::video`).

use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use app_windows_protocol::{
    self as protocol, ErrorMessage, Message, Video, Welcome, WindowInfo, WindowRef,
};
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};

use crate::{apps, ax, capture, files, input};

/// Room for messages waiting to be written to one connection: a few
/// seconds of pictures for a handful of windows.
const QUEUE: usize = 256;

/// How long a control message waits for a stuck connection before the
/// connection is given up on.
const STALL: Duration = Duration::from_secs(5);

/// A window the server knows about, as the capture thread last saw it.
#[derive(Clone)]
pub struct Tracked {
    pub info: WindowInfo,
    pub pid: i32,
    /// Set to have the window's next picture be a keyframe.
    pub keyframe: Arc<AtomicBool>,
}

pub struct Session {
    id: u64,
    tx: Sender<Vec<u8>>,
    watching: AtomicBool,
    /// Whether a picture for this connection was dropped since its last
    /// keyframe, so the next one it gets must be a keyframe.
    behind: AtomicBool,
}

/// What every thread shares: the connections and the windows.
pub struct Hub {
    token: String,
    sessions: Mutex<Vec<Arc<Session>>>,
    pub windows: Mutex<BTreeMap<u32, Tracked>>,
    /// The window a client last focused, which popups of its app belong to
    /// and which input raises first.
    pub focused: Mutex<Option<u32>>,
    /// The clipboard's text as last sent or received, so a change is sent
    /// once and a host's own text is not echoed back to it.
    clipboard: Mutex<Option<String>>,
    next_session: AtomicU64,
}

impl Hub {
    fn new(token: String) -> Self {
        Hub {
            token,
            sessions: Mutex::new(Vec::new()),
            windows: Mutex::new(BTreeMap::new()),
            focused: Mutex::new(None),
            clipboard: Mutex::new(None),
            next_session: AtomicU64::new(1),
        }
    }

    /// Whether any connection wants windows, without which nothing is
    /// captured at all.
    pub fn watched(&self) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.watching.load(Ordering::Relaxed))
    }

    /// Sends a control message to every connection that watches windows.
    pub fn broadcast(&self, message: &Message) {
        let bytes = message.encode();
        let sessions: Vec<_> = self.sessions.lock().unwrap().clone();
        for session in sessions
            .iter()
            .filter(|s| s.watching.load(Ordering::Relaxed))
        {
            if session.tx.send_timeout(bytes.clone(), STALL).is_err() {
                eprintln!("connection {} stopped reading; letting it go", session.id);
                self.remove(session.id);
            }
        }
    }

    /// Hands a picture to every watching connection with room for it. A
    /// connection without room misses it, and since every picture after it
    /// depends on it, the window's next picture is made a keyframe.
    pub fn video(&self, video: Video, keyframe_flag: &AtomicBool) {
        let keyframe = video.keyframe;
        let bytes = Message::Video(video).encode();
        for session in self.sessions.lock().unwrap().iter() {
            if !session.watching.load(Ordering::Relaxed) {
                continue;
            }
            if keyframe {
                session.behind.store(false, Ordering::Relaxed);
            } else if session.behind.load(Ordering::Relaxed) {
                continue;
            }
            match session.tx.try_send(bytes.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    session.behind.store(true, Ordering::Relaxed);
                    keyframe_flag.store(true, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }

    pub fn window(&self, id: u32) -> Option<Tracked> {
        self.windows.lock().unwrap().get(&id).cloned()
    }

    fn remove(&self, id: u64) {
        self.sessions.lock().unwrap().retain(|s| s.id != id);
    }
}

struct Options {
    listen: SocketAddr,
    token_file: PathBuf,
}

fn options(args: &[String]) -> Result<Options, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_owned())?;
    let mut options = Options {
        listen: SocketAddr::from(([0, 0, 0, 0], protocol::DEFAULT_PORT)),
        token_file: Path::new(&home).join("Library/Application Support/LosOS App Server/token"),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--listen" => {
                options.listen = value()?.parse().map_err(|e| format!("--listen: {e}"))?;
            }
            "--token-file" => options.token_file = value()?.into(),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("permissions") => {
            let missing = input::request_permissions();
            if missing.is_empty() {
                println!("Screen Recording and Accessibility are allowed");
                return ExitCode::SUCCESS;
            }
            println!(
                "still to allow in System Settings › Privacy & Security: {}",
                missing.join(", ")
            );
            return ExitCode::FAILURE;
        }
        Some("apps") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&apps::installed()).unwrap()
            );
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let options = match options(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("macos-app-server: {e}");
            return ExitCode::from(2);
        }
    };
    let token = match std::fs::read_to_string(&options.token_file) {
        Ok(t) if !t.trim().is_empty() => t.trim().to_owned(),
        Ok(_) | Err(_) => {
            eprintln!(
                "macos-app-server: no token in {}; the installer writes one",
                options.token_file.display()
            );
            return ExitCode::from(2);
        }
    };
    let listener = match TcpListener::bind(options.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("macos-app-server: listening on {}: {e}", options.listen);
            return ExitCode::FAILURE;
        }
    };
    // Asked now, while nobody is connected, so that macOS's own prompts
    // show the first time the server runs rather than mid-session.
    let missing = input::request_permissions();
    if !missing.is_empty() {
        eprintln!(
            "waiting for {} to be allowed in System Settings",
            missing.join(" and ")
        );
    }
    eprintln!("listening on {}", options.listen);

    let hub = Arc::new(Hub::new(token));
    let tracker = hub.clone();
    thread::Builder::new()
        .name("windows".into())
        .spawn(move || capture::track(&tracker))
        .expect("spawning the window thread");
    let watcher = hub.clone();
    thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || watch_clipboard(&watcher))
        .expect("spawning the clipboard thread");
    let accepter = hub.clone();
    thread::Builder::new()
        .name("accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let hub = accepter.clone();
                        thread::spawn(move || connection(&hub, stream));
                    }
                    Err(e) => eprintln!("accepting: {e}"),
                }
            }
        })
        .expect("spawning the accept thread");

    // AppKit and ScreenCaptureKit hand some work to the main queue, which
    // only runs while the main thread runs its run loop.
    apps::run_main_loop();
    ExitCode::SUCCESS
}

fn connection(hub: &Arc<Hub>, stream: TcpStream) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let _ = stream.set_nodelay(true);
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let (tx, rx) = bounded(QUEUE);
    let session = Arc::new(Session {
        id: hub.next_session.fetch_add(1, Ordering::Relaxed),
        tx,
        watching: AtomicBool::new(false),
        behind: AtomicBool::new(false),
    });
    let writer = thread::spawn(move || write_loop(write_half, rx));

    // Nobody gets as far as a session without the token.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(&stream);
    let welcomed = match protocol::read(&mut reader) {
        Ok(Message::Hello(hello)) if hello.version != protocol::VERSION => Err(format!(
            "this Mac's app server speaks version {}, not {}",
            protocol::VERSION,
            hello.version
        )),
        Ok(Message::Hello(hello)) if protocol::token_matches(&hello.token, &hub.token) => Ok(()),
        Ok(Message::Hello(_)) => Err("wrong token".to_owned()),
        Ok(_) => Err("expected hello".to_owned()),
        Err(e) => Err(format!("reading hello: {e}")),
    };
    if let Err(message) = welcomed {
        eprintln!("{peer}: {message}");
        let _ = session
            .tx
            .send(Message::Error(ErrorMessage { message }).encode());
        drop(session);
        let _ = writer.join();
        return;
    }
    let _ = stream.set_read_timeout(None);
    let welcome = Welcome {
        version: protocol::VERSION,
        name: apps::computer_name(),
        system: apps::system_version(),
        missing: input::missing_permissions(),
        encoder: capture::encoder_kind().to_owned(),
    };
    let _ = session.tx.send(Message::Welcome(welcome).encode());
    hub.sessions.lock().unwrap().push(session.clone());
    eprintln!("{peer}: connected as {}", session.id);

    let mut state = Connection {
        pointer: input::PointerState::default(),
        keyboard: input::Keyboard::default(),
        incoming: files::Incoming::new(files::downloads()),
    };
    loop {
        let message = match protocol::read(&mut reader) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{peer}: {e}");
                break;
            }
        };
        handle(hub, &session, &mut state, message);
    }
    // Keys a host that went away was holding would stay down on the Mac.
    state.keyboard.release_all();
    hub.remove(session.id);
    drop(session);
    let _ = writer.join();
}

/// What one connection keeps between its messages.
struct Connection {
    pointer: input::PointerState,
    keyboard: input::Keyboard,
    incoming: files::Incoming,
}

fn handle(hub: &Arc<Hub>, session: &Arc<Session>, state: &mut Connection, message: Message) {
    let Connection {
        pointer,
        keyboard,
        incoming,
    } = state;
    let reply = |m: Message| {
        let _ = session.tx.send_timeout(m.encode(), STALL);
    };
    match message {
        Message::ListApps => reply(Message::Apps(apps::installed())),
        Message::Launch(launch) => {
            if let Err(e) = apps::launch(&launch.id) {
                reply(Message::Error(ErrorMessage { message: e }));
            }
        }
        Message::Watch => {
            // Everything already open, as if it had just opened, each with
            // a keyframe coming so this connection can start decoding.
            let windows: Vec<Tracked> = hub.windows.lock().unwrap().values().cloned().collect();
            session.behind.store(true, Ordering::Relaxed);
            session.watching.store(true, Ordering::Relaxed);
            for w in windows {
                reply(Message::WindowOpened(w.info.clone()));
                w.keyframe.store(true, Ordering::Relaxed);
            }
        }
        Message::Keyframe(WindowRef { window }) => {
            if let Some(w) = hub.window(window) {
                w.keyframe.store(true, Ordering::Relaxed);
            }
        }
        Message::Pointer(p) => {
            if let Some(w) = hub.window(p.window) {
                pointer.moved(&w, p.x, p.y);
            }
        }
        Message::Button(b) => {
            if let Some(w) = hub.window(b.window) {
                if b.pressed {
                    raise_if_needed(hub, &w);
                }
                pointer.button(&w, b.button, b.pressed, b.x, b.y);
            }
        }
        Message::Scroll(s) => {
            if let Some(w) = hub.window(s.window) {
                pointer.scroll(&w, s.x, s.y, s.dx, s.dy);
            }
        }
        Message::Key(k) => {
            if let Some(w) = hub.window(k.window) {
                raise_if_needed(hub, &w);
            }
            keyboard.key(k.code, k.pressed, k.modifiers, k.text.as_deref());
        }
        Message::Text(t) => {
            if let Some(w) = hub.window(t.window) {
                raise_if_needed(hub, &w);
            }
            input::text(&t.text);
        }
        Message::Resize(r) => {
            if let Some(w) = hub.window(r.window) {
                ax::resize(w.pid, r.window, r.width, r.height);
            }
        }
        Message::Close(WindowRef { window }) => {
            if let Some(w) = hub.window(window) {
                match w.info.kind {
                    // A menu closes the way a person closes one, with Esc.
                    protocol::WindowKind::Popup => {
                        input::key(1, true, 0, None, false);
                        input::key(1, false, 0, None, false);
                    }
                    protocol::WindowKind::Normal => ax::close(w.pid, window),
                }
            }
        }
        Message::Focus(WindowRef { window }) => {
            if let Some(w) = hub.window(window) {
                if w.info.kind == protocol::WindowKind::Normal {
                    *hub.focused.lock().unwrap() = Some(window);
                    ax::raise(w.pid, window);
                    // Reading every menu takes a moment; the connection
                    // carries on meanwhile.
                    let tx = session.tx.clone();
                    thread::spawn(move || {
                        let menus = ax::menus(w.pid);
                        let _ = tx.send_timeout(
                            Message::Menus(protocol::Menus { window, menus }).encode(),
                            STALL,
                        );
                    });
                }
            }
        }
        Message::ActivateMenu(pick) => {
            if let Some(w) = hub.window(pick.window) {
                raise_if_needed(hub, &w);
                if let Err(e) = ax::activate_menu(w.pid, &pick.item) {
                    reply(Message::Error(ErrorMessage { message: e }));
                }
            }
        }
        Message::Clipboard(c) => {
            // Remembered so the clipboard thread does not send it back.
            *hub.clipboard.lock().unwrap() = Some(c.text.clone());
            apps::set_clipboard(&c.text);
        }
        Message::FileStart(start) => {
            let name = start.name.clone();
            match incoming.start(start) {
                Ok(()) => eprintln!("connection {}: receiving {name}", session.id),
                Err(message) => reply(Message::Error(ErrorMessage { message })),
            }
        }
        Message::FileData(data) => {
            if let Err(message) = incoming.data(data) {
                reply(Message::Error(ErrorMessage { message }));
            }
        }
        Message::FileEnd(protocol::FileRef { file }) => match incoming.end(file) {
            Ok((saved, failed)) => {
                eprintln!(
                    "connection {}: saved {}{}",
                    session.id,
                    saved.path,
                    if saved.opened { " and opened it" } else { "" }
                );
                reply(Message::FileSaved(saved));
                if let Some(message) = failed {
                    reply(Message::Error(ErrorMessage { message }));
                }
            }
            Err(message) => reply(Message::Error(ErrorMessage { message })),
        },
        other => eprintln!("connection {}: ignoring {other:?}", session.id),
    }
}

/// Raises a window before input meant for it, when another was raised
/// since: on macOS's one screen windows overlap that the host shows side
/// by side, and a click goes to whatever is on top where it lands.
fn raise_if_needed(hub: &Hub, window: &Tracked) {
    if window.info.kind != protocol::WindowKind::Normal {
        return;
    }
    let mut focused = hub.focused.lock().unwrap();
    if *focused != Some(window.info.window) {
        *focused = Some(window.info.window);
        ax::raise(window.pid, window.info.window);
    }
}

/// Sends the clipboard's text to watching connections whenever someone on
/// the Mac copies something. macOS has no notification for it; its change
/// count, read twice a second, is how every clipboard manager finds out.
fn watch_clipboard(hub: &Hub) {
    let mut seen = apps::clipboard().0;
    loop {
        thread::sleep(Duration::from_millis(500));
        let (count, text) = apps::clipboard();
        if count == seen {
            continue;
        }
        seen = count;
        let Some(text) = text else { continue };
        let mut last = hub.clipboard.lock().unwrap();
        if last.as_deref() == Some(text.as_str()) {
            continue;
        }
        *last = Some(text.clone());
        drop(last);
        hub.broadcast(&Message::Clipboard(protocol::Clipboard { text }));
    }
}

fn write_loop(stream: TcpStream, rx: Receiver<Vec<u8>>) {
    let mut out = BufWriter::with_capacity(1 << 20, stream);
    while let Ok(bytes) = rx.recv() {
        if out.write_all(&bytes).is_err() {
            return;
        }
        // Flushing only once the queue is empty lets a burst of pictures
        // go out in few writes.
        if rx.is_empty() && out.flush().is_err() {
            return;
        }
    }
    let _ = out.flush();
}
