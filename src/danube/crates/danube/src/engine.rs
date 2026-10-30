//! WebKit's half of Danube, on the process's main thread under GLib's main
//! loop: one [`WebView`] per tab on WPE's headless display, each frame
//! copied out for the window to draw, the window's input turned into WPE
//! events, the content blockers losos-adblock compiles loaded into every
//! page, and the extension runtime (extensions/runtime.rs). Each tab has a
//! content manager of its own, so that a script message from an extension
//! says which tab it came from.
//!
//! The engine lives in a thread-local, since every WebKit object belongs to
//! this thread. Signal handlers are closures that know their tab's id and
//! mostly only send the window an [`Event`] (through [`LINK`], another
//! thread-local); the few that need the engine itself borrow it with
//! [`try_with`] and give up cleanly (deny, block the popup) when a command
//! being handled already has it, which only happens when WebKit emits a
//! signal from inside a call the engine made.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use tracing::{debug, info, trace, warn};

use crate::config::{self, Settings};
use crate::downloads::{DownloadId, Job, Request, Update};
use crate::extensions::runtime::{self, Runtime};
use crate::ffi::{
    WEBKIT_LOAD_STARTED, WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION,
    WEBKIT_POLICY_DECISION_TYPE_RESPONSE, WPE_EVENT_KEYBOARD_KEY_DOWN, WPE_EVENT_KEYBOARD_KEY_UP,
    WPE_EVENT_POINTER_DOWN, WPE_EVENT_POINTER_LEAVE, WPE_EVENT_POINTER_MOVE, WPE_EVENT_POINTER_UP,
    WPE_EVENT_TOUCH_CANCEL, WPE_EVENT_TOUCH_DOWN, WPE_EVENT_TOUCH_MOVE, WPE_EVENT_TOUCH_UP,
    WPE_INPUT_SOURCE_MOUSE, WPE_INPUT_SOURCE_TOUCHPAD, WPE_MODIFIER_KEYBOARD_CONTROL,
    WPE_MODIFIER_POINTER_BUTTON1, WPE_MODIFIER_POINTER_BUTTON2, WPE_MODIFIER_POINTER_BUTTON3,
};
use crate::input::Input;
use crate::notify::{self, Card, Notifier};
use crate::permissions::{self, Kind};
use crate::shared::{
    Command, DownloadCommand, Event, Events, ExtCommand, KeyAsk, PermissionAsk, Tab, TabId,
};
use crate::webauthn::{self, Refusal};
use crate::webkit::{
    self, Buffer, Display, Download, FilterStore, Frames, MainLoop, NetworkSession,
    PermissionRequest, PolicyDecision, Reply, UserContentManager, UserScript, View, WebView,
};

/// How the engine starts.
pub struct Options {
    /// Where the profile lives (cookies, storage), or `None` for a
    /// throwaway one that ends with the process (captive portal).
    pub profile: Option<(PathBuf, PathBuf)>,
    /// losos-adblock's compiled rule sets, or `None` to load none.
    pub adblock: Option<PathBuf>,
    /// Where WebKit keeps the rule sets it compiled from them.
    pub filter_cache: PathBuf,
    /// Where lasting permission answers are kept.
    pub permissions: Option<PathBuf>,
    /// Where downloads go.
    pub downloads: PathBuf,
    /// aria2's thread (downloads.rs), which fetches what WebKit would
    /// have; `None` leaves every download to WebKit.
    pub aria2: Option<Sender<Request>>,
    pub settings: Settings,
    /// What to open first; the home page when empty.
    pub open: Vec<String>,
    /// The extension runtime's, or `None` for a browser without
    /// extensions (captive portal).
    pub extensions: Option<runtime::Options>,
    /// No page makes a sound (the desktop background).
    pub muted: bool,
}

/// One tab: its page and the WPE view drawing it.
pub(crate) struct Entry {
    pub(crate) id: TabId,
    pub(crate) page: WebView,
    pub(crate) view: View,
    /// Its own: the content blockers and extensions' scripts go into each.
    pub(crate) content: UserContentManager,
}

/// What happened to a tab, for the extension runtime.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TabChange {
    Opened(TabId),
    Closed(TabId),
    Activated(TabId),
}

pub(crate) struct Engine {
    commands: Receiver<Command>,
    main_loop: MainLoop,
    display: Display,
    session: NetworkSession,
    settings: webkit::Settings,
    /// The content blockers, which every tab's content manager gets.
    filters: Option<Rc<RefCell<Filters>>>,
    pub(crate) tabs: Vec<Entry>,
    pub(crate) active: Option<TabId>,
    next_tab: TabId,
    pub(crate) size: (i32, i32, f64),
    pub(crate) focused: bool,
    /// Whether the active tab is on screen at all (`Command::Shown`).
    shown: bool,
    muted: bool,
    modifiers: u32,
    buttons: u32,
    pointer: (f64, f64),
    pending: HashMap<u64, (PermissionRequest, String, Kind)>,
    next_permission: u64,
    permissions: permissions::Store,
    /// Sites asking for a security key, by the bar's id (webauthn.rs).
    keys: HashMap<u64, Asking>,
    next_key: u64,
    home: String,
    desktop_agent: String,
    started: Instant,
    /// What the window last pasted into WebKit's clipboard, so that
    /// putting it there is not mistaken for the page copying it; shared
    /// with the clipboard's handler.
    pasted: Rc<RefCell<Option<String>>>,
    /// The extension runtime, taken out while it runs with the engine
    /// (`runtime::with`).
    pub(crate) extensions: Option<Runtime>,
    /// Tab changes the runtime missed while it was busy (`runtime::notify`).
    pub(crate) tab_changes: Vec<TabChange>,
    downloads: Downloads,
    /// Each download's notification, shared with WebKit's download signals.
    notifier: Rc<RefCell<Notifier>>,
}

/// A security key request under way: the thread asking the keys has the
/// request, and the page's promise waits here, on WebKit's thread.
struct Asking {
    tab: TabId,
    /// webauthn.js's own number for the call, which a cancel names.
    call: u64,
    reply: Reply,
    pins: Sender<String>,
    cancel: Sender<()>,
}

/// The downloads under way, by Danube's id for them.
struct Downloads {
    dir: PathBuf,
    aria2: Option<Sender<Request>>,
    held: HashMap<DownloadId, Held>,
    /// Addresses aria2 could not fetch, for WebKit to fetch itself under
    /// the same id and name when it starts the download again.
    webkit_only: HashMap<String, (DownloadId, String)>,
}

enum Held {
    /// aria2 has it, or will once its cookies are read. `origin` is the
    /// address the page or extension asked for, which the extensions know
    /// it by; `tab` the tab whose page asked, which WebKit asks again
    /// when aria2 cannot fetch it.
    Aria2 {
        origin: String,
        path: PathBuf,
        tab: Option<TabId>,
    },
    /// WebKit fetches it itself; `cancelled` tells its `failed` handler
    /// the person stopped it.
    WebKit {
        download: Download,
        cancelled: Rc<Cell<bool>>,
    },
}

thread_local! {
    /// Ids for downloads, which WebKit's download signals take even when
    /// the engine is busy.
    static NEXT_DOWNLOAD: Cell<DownloadId> = const { Cell::new(1) };
}

fn next_download() -> DownloadId {
    NEXT_DOWNLOAD.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    })
}

/// Pause and resume a hidden page's animations and media, keeping the
/// ones paused here under a symbol of their own, so that only those
/// resume. In the page's own world: an isolated one keeps no globals from
/// one evaluation to the next.
const PAUSE: &str = "document[Symbol.for('danube.held')] = [...document.getAnimations(), ...document.querySelectorAll('video, audio')]
    .filter(m => m instanceof Animation ? m.playState === 'running' : !m.paused);
for (const m of document[Symbol.for('danube.held')]) m.pause();";
const RESUME: &str = "for (const m of document[Symbol.for('danube.held')] ?? []) m.play();
delete document[Symbol.for('danube.held')];";

/// The rule sets in force, kept apart from the engine because the store's
/// callbacks run on their own schedule.
struct Filters {
    /// Every open tab's content manager.
    managers: HashMap<TabId, UserContentManager>,
    /// The rule sets loaded so far, for a tab that opens later.
    loaded_sets: Vec<webkit::Filter>,
    store: FilterStore,
    dir: PathBuf,
    /// The index's modification time when it was last read.
    stamp: Option<SystemTime>,
    /// Identifiers of the sets now loaded, `<id>-<mtime>`, so a changed set
    /// compiles again under a new name and the old one can be removed.
    current: Vec<String>,
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
    /// The channel to the window, for signal handlers, which must reach
    /// it even while a command has the engine borrowed.
    static LINK: RefCell<Option<Events>> = const { RefCell::new(None) };
}

pub(crate) fn try_with<R>(f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    ENGINE.with(|e| e.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Tells the window `event`, and repaints it. The window being gone is
/// not an error here: WebKit is about to quit on its command.
pub(crate) fn emit(event: Event) {
    LINK.with(|l| {
        if let Some(link) = l.borrow().as_ref() {
            let _ = link.send(event);
        }
    });
}

/// Sends the engine a command of its own, through the window's channel,
/// for a handler that cannot borrow the engine right now.
pub(crate) fn later(command: Command) {
    LINK.with(|l| {
        if let Some(link) = l.borrow().as_ref() {
            link.defer(command);
        }
    });
}

/// Wakes the main loop to handle the window's commands. Safe from any
/// thread.
pub fn wake() {
    webkit::idle_add(|| {
        let busy = try_with(|e| {
            while let Ok(command) = e.commands.try_recv() {
                e.handle(command);
            }
        })
        .is_none();
        // Busy only if a command's own WebKit call ran the loop; try again.
        busy
    });
}

/// Runs WebKit until the window closes or asks it to quit.
pub fn run(commands: Receiver<Command>, events: Events, options: Options) {
    LINK.with(|l| *l.borrow_mut() = Some(events));

    let display = match Display::headless() {
        Ok(display) => display,
        Err(error) => {
            warn!(error, "cannot connect WPE's headless display");
            return;
        }
    };

    let session = match &options.profile {
        Some((data, cache)) => NetworkSession::persistent(data, cache),
        None => NetworkSession::ephemeral(),
    };
    // Tracking prevention: third-party cookies and storage are cut off
    // for sites that only ever appear as trackers.
    session.set_itp_enabled(true);
    // No keyring: the sandbox has no Secret Service to talk to, and a
    // password manager extension keeps passwords.
    session.set_persistent_credential_storage_enabled(false);
    let notifier = Notifier::new();
    let (dir, notes) = (options.downloads.clone(), notifier.clone());
    session.on_download_started(move |_, download| watch_download(download, &dir, &notes));

    // No popups a page opens by itself. Without developer tools, a page's
    // console (and an extension's frame in it) is only seen on stdout,
    // when danube::engine traces.
    let settings = webkit::Settings::new(false, tracing::enabled!(tracing::Level::TRACE));
    settings.set_application("Danube", env!("CARGO_PKG_VERSION"));
    let desktop_agent = settings.user_agent();

    let pasted = Rc::new(RefCell::new(None));
    let ours = pasted.clone();
    display.clipboard().on_change(move |clipboard| {
        let Some(text) = clipboard.text() else { return };
        if ours.borrow().as_deref() != Some(text.as_str()) {
            emit(Event::Copied(text));
        }
    });

    let filters = options.adblock.as_ref().map(|dir| {
        let _ = std::fs::create_dir_all(&options.filter_cache);
        let filters = Rc::new(RefCell::new(Filters {
            managers: HashMap::new(),
            loaded_sets: Vec::new(),
            store: FilterStore::new(&options.filter_cache),
            dir: dir.clone(),
            stamp: None,
            current: Vec::new(),
        }));
        reload_filters(&filters);
        // losos-adblock compiles daily; a minute is soon enough to pick
        // up a new compile, and costs one stat.
        let timer = filters.clone();
        webkit::timeout_add(60_000, move || {
            reload_filters(&timer);
            true
        });
        filters
    });

    // The runtime before the engine: its views come from the same display
    // and session.
    let extensions = options
        .extensions
        .map(|o| Runtime::new(o, &display, &session));

    let engine = Engine {
        commands,
        main_loop: MainLoop::new(),
        display,
        session,
        settings,
        filters,
        tabs: Vec::new(),
        active: None,
        next_tab: 1,
        size: (800, 600, 1.0),
        focused: true,
        shown: true,
        muted: options.muted,
        modifiers: 0,
        buttons: 0,
        pointer: (0.0, 0.0),
        pending: HashMap::new(),
        next_permission: 1,
        permissions: permissions::Store::open(options.permissions.clone()),
        keys: HashMap::new(),
        next_key: 1,
        home: options.settings.home.clone(),
        desktop_agent,
        started: Instant::now(),
        pasted,
        extensions,
        tab_changes: Vec::new(),
        downloads: Downloads {
            dir: options.downloads.clone(),
            aria2: options.aria2,
            held: HashMap::new(),
            webkit_only: HashMap::new(),
        },
        notifier,
    };
    let main_loop = engine.main_loop.clone();
    ENGINE.with(|e| *e.borrow_mut() = Some(engine));

    let open = if options.open.is_empty() {
        vec![options.settings.home.clone()]
    } else {
        options.open.clone()
    };
    try_with(|e| {
        for (n, uri) in open.iter().enumerate() {
            let id = e.open(Some(uri), None);
            if n == 0 {
                e.activate(id);
            }
        }
    });
    // Commands the window sent before the engine existed.
    wake();

    info!("WebKit is up");
    main_loop.run();

    emit(Event::Quit);
    // Dropping the channels: senders of commands see WebKit is gone.
    ENGINE.with(|e| e.borrow_mut().take());
    LINK.with(|l| l.borrow_mut().take());
}

impl Engine {
    fn handle(&mut self, command: Command) {
        match command {
            Command::Attach(ctx) => LINK.with(|l| {
                if let Some(link) = l.borrow_mut().as_mut() {
                    link.attach(ctx);
                }
            }),
            Command::NewTab { uri, activate } => {
                let id = self.open(uri.as_deref(), None);
                if activate {
                    self.activate(id);
                }
            }
            Command::Activate(id) => self.activate(id),
            Command::Close(id) => self.close(id),
            Command::Back(id) => self.page(id).map_or((), WebView::go_back),
            Command::Forward(id) => self.page(id).map_or((), WebView::go_forward),
            Command::Reload(id) => self.page(id).map_or((), WebView::reload),
            Command::Stop(id) => self.page(id).map_or((), WebView::stop_loading),
            Command::CopyAddress(id) => {
                if let Some(uri) = self.page(id).map(WebView::uri).filter(|u| !u.is_empty()) {
                    emit(Event::Copied(uri));
                }
            }
            Command::Resize {
                width,
                height,
                scale,
            } => {
                let width = width.max(1);
                let height = height.max(1);
                if self.size != (width, height, scale) {
                    self.size = (width, height, scale);
                    for entry in &self.tabs {
                        self.fit(&entry.view);
                    }
                }
            }
            Command::Focus(focused) => {
                self.focused = focused;
                if let Some(view) = self.active.and_then(|id| self.view(id)) {
                    self.set_focus(&view, focused);
                }
            }
            Command::Shown(shown) => {
                self.shown = shown;
                let Some(entry) = self.active.and_then(|id| self.entry(id)) else {
                    return;
                };
                entry.view.set_visible(shown);
                // Hidden, WebKit stops painting the page and slows its
                // timers, but its compositor keeps running CSS animations
                // and videos keep playing; those are paused by hand, and
                // only those resume.
                let script = if shown { RESUME } else { PAUSE };
                entry.page.evaluate(script, None, None);
            }
            Command::Input(id, inputs) => {
                if let Some(view) = self.view(id) {
                    for input in inputs {
                        self.input(&view, input);
                    }
                }
            }
            Command::Paste(id, text) => {
                *self.pasted.borrow_mut() = Some(text.clone());
                self.display.clipboard().set_text(&text);
                if let Some(view) = self.view(id) {
                    let held = self.modifiers;
                    self.modifiers = WPE_MODIFIER_KEYBOARD_CONTROL;
                    self.input(
                        &view,
                        Input::Key {
                            keyval: 'v' as u32,
                            pressed: true,
                        },
                    );
                    self.input(
                        &view,
                        Input::Key {
                            keyval: 'v' as u32,
                            pressed: false,
                        },
                    );
                    self.modifiers = held;
                }
            }
            Command::Permission {
                id,
                allow,
                remember,
            } => {
                if let Some((request, host, kind)) = self.pending.remove(&id) {
                    if remember {
                        self.permissions.set(&host, kind, allow);
                    }
                    if allow {
                        request.allow();
                    } else {
                        request.deny();
                    }
                }
                emit(Event::PermissionDone(id));
            }
            Command::KeyPin(id, pin) => {
                if let Some(asking) = self.keys.get(&id) {
                    let _ = asking.pins.try_send(pin.0);
                }
            }
            Command::KeyCancel(id) => {
                if let Some(asking) = self.keys.get(&id) {
                    let _ = asking.cancel.try_send(());
                }
            }
            Command::KeyStatus(id, status) => emit(Event::KeyStatus(id, status)),
            Command::KeyDone(id, answer) => {
                if let Some(asking) = self.keys.remove(&id) {
                    asking.reply.value(&answer);
                }
                emit(Event::KeyDone(id));
            }
            Command::Phone(phone) => {
                let agent = if phone {
                    // What phone sites look for: "Mobile", and "Android"
                    // for the many that only check for it. The engine is
                    // still WebKit's, as the rest of the string says.
                    format!(
                        "Mozilla/5.0 (Linux; Android 14; Mobile) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile Safari/605.1.15 Danube/{}",
                        env!("CARGO_PKG_VERSION")
                    )
                } else {
                    self.desktop_agent.clone()
                };
                self.settings.set_user_agent(&agent);
            }
            Command::Extensions(command) => {
                runtime::with(self, |rt, e| rt.command(e, command));
            }
            Command::Download(command) => self.download(command),
            Command::Quit => {
                if let Some(rt) = &mut self.extensions {
                    rt.flush_storage();
                }
                self.main_loop.quit()
            }
        }
    }

    fn entry(&self, id: TabId) -> Option<&Entry> {
        self.tabs.iter().find(|e| e.id == id)
    }

    fn page(&self, id: TabId) -> Option<&WebView> {
        self.entry(id).map(|e| &e.page)
    }

    pub(crate) fn view(&self, id: TabId) -> Option<View> {
        self.entry(id).map(|e| e.view.clone())
    }

    /// Opens a tab, loading `uri` (the home page when `None`), or, with
    /// `related`, the view WebKit asked for to open a popup in; that one
    /// loads by itself.
    pub(crate) fn open(&mut self, uri: Option<&str>, related: Option<&WebView>) -> TabId {
        let id = self.next_tab;
        self.next_tab += 1;
        let content = UserContentManager::new();
        if let Some(filters) = &self.filters {
            filters.borrow_mut().attach(id, &content);
        }
        // Security keys, in the top frame only (webauthn.rs).
        content.add_script(&UserScript::new(webauthn::PAGE_SCRIPT, Frames::Top, true, None, &[], &[]));
        content.add_script(&UserScript::new(
            webauthn::BRIDGE_SCRIPT,
            Frames::Top,
            true,
            Some(webauthn::WORLD),
            &[],
            &[],
        ));
        content.register_message_handler(webauthn::HANDLER, Some(webauthn::WORLD), move |text, reply| {
            security_key(id, &text, reply)
        });
        let page = match related {
            // A popup shares its opener's process, display and session.
            Some(related) => WebView::related(related, &self.settings, &content),
            None => WebView::new(&self.display, &self.session, &self.settings, &content),
        };
        for property in ["title", "uri", "estimated-load-progress", "is-loading"] {
            page.on_notify(property, move |page| {
                emit(Event::Changed(refresh(page, id)))
            });
        }
        page.on_load_changed(move |page, event| {
            debug!(tab = id, event, "load changed");
            if event == WEBKIT_LOAD_STARTED {
                emit(Event::LoadStarted(id));
                // The page that asked for a key is going.
                try_with(|e| e.stop_keys(id, None));
            }
            emit(Event::Changed(refresh(page, id)));
            // The extensions hear of it now, or once the engine is free.
            if try_with(|e| runtime::with(e, |rt, e| rt.tab_load(e, id, event))).is_none() {
                later(Command::Extensions(ExtCommand::TabLoad(id, event)));
            }
        });
        page.on_create(move |_| {
            try_with(|e| {
                let related = e.page(id)?.clone();
                let new = e.open(None, Some(&related));
                e.page(new).cloned()
            })
            .flatten()
        });
        page.on_ready_to_show(move |_| {
            if try_with(|e| e.activate(id)).is_none() {
                later(Command::Activate(id));
            }
        });
        page.on_close(move |_| later(Command::Close(id)));
        page.on_load_failed(move |_, uri, error| {
            warn!(tab = id, uri, error, "a load failed");
            emit(Event::LoadFailed {
                tab: id,
                uri,
                error,
            });
        });
        page.on_web_process_terminated(move |_| {
            warn!(tab = id, "a web process ended");
            emit(Event::Crashed(id));
        });
        page.on_decide_policy(|_, decision, kind| decide(decision, kind));
        page.on_permission_request(move |page, request| ask(page, request, id));
        page.on_mouse_target_changed(|_, result| emit(Event::HoveredLink(result.link_uri())));
        page.set_muted(self.muted);
        let view = page.view();
        view.on_buffer_rendered(move |_, buffer| frame(buffer, id));
        view.set_visible(false);
        self.fit(&view);
        self.tabs.push(Entry {
            id,
            page,
            view,
            content,
        });
        emit(Event::Opened(Tab {
            id,
            ..Tab::default()
        }));
        // The extensions' scripts go in before the first load.
        runtime::notify(self, TabChange::Opened(id));
        if related.is_none() {
            let uri = uri.unwrap_or(&self.home);
            self.tabs.last().expect("just pushed").page.load_uri(uri);
        }
        id
    }

    pub(crate) fn activate(&mut self, id: TabId) {
        if self.entry(id).is_none() {
            return;
        }
        let previous = self.active.replace(id);
        for entry in &self.tabs {
            entry.view.set_visible(entry.id == id && self.shown);
        }
        if let Some(old) = previous
            .filter(|old| *old != id)
            .and_then(|old| self.view(old))
        {
            self.set_focus(&old, false);
        }
        let view = self.view(id).expect("checked above");
        self.set_focus(&view, self.focused);
        emit(Event::Activated(id));
        runtime::notify(self, TabChange::Activated(id));
    }

    pub(crate) fn close(&mut self, id: TabId) {
        let Some(index) = self.tabs.iter().position(|e| e.id == id) else {
            return;
        };
        self.stop_keys(id, None);
        // Dropping the entry drops the view's last reference.
        drop(self.tabs.remove(index));
        if let Some(filters) = &self.filters {
            filters.borrow_mut().managers.remove(&id);
        }
        if self.active == Some(id) {
            self.active = None;
            // The neighbour to the right takes its place, as in most
            // browsers, or the one to the left at the end of the strip.
            let next = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|e| e.id);
            if let Some(next) = next {
                self.activate(next);
            }
        }
        emit(Event::Closed(id));
        runtime::notify(self, TabChange::Closed(id));
        if self.tabs.is_empty() {
            // The last tab closed: the window goes too.
            self.main_loop.quit();
        }
    }

    /// Gives a view the page area's size and scale.
    fn fit(&self, view: &View) {
        let (width, height, scale) = self.size;
        if let Some(toplevel) = view.toplevel() {
            toplevel.set_scale(scale);
            toplevel.resize(width, height);
        }
    }

    pub(crate) fn set_focus(&self, view: &View, focused: bool) {
        if let Some(toplevel) = view.toplevel() {
            toplevel.set_active(focused);
        }
        view.set_focused(focused);
    }

    fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    pub(crate) fn input(&mut self, view: &View, input: Input) {
        let time = self.time();
        let event = match input {
            Input::Modifiers(m) => {
                self.modifiers = m;
                return;
            }
            Input::Move { x, y } => {
                let (dx, dy) = (x - self.pointer.0, y - self.pointer.1);
                self.pointer = (x, y);
                webkit::Event::pointer_move(
                    WPE_EVENT_POINTER_MOVE,
                    view,
                    WPE_INPUT_SOURCE_MOUSE,
                    time,
                    self.modifiers | self.buttons,
                    x,
                    y,
                    dx,
                    dy,
                )
            }
            Input::Leave => webkit::Event::pointer_move(
                WPE_EVENT_POINTER_LEAVE,
                view,
                WPE_INPUT_SOURCE_MOUSE,
                time,
                self.modifiers,
                self.pointer.0,
                self.pointer.1,
                0.0,
                0.0,
            ),
            Input::Button {
                x,
                y,
                button,
                pressed,
            } => {
                self.pointer = (x, y);
                let mask = match button {
                    1 => WPE_MODIFIER_POINTER_BUTTON1,
                    2 => WPE_MODIFIER_POINTER_BUTTON2,
                    _ => WPE_MODIFIER_POINTER_BUTTON3,
                };
                let (kind, count) = if pressed {
                    self.buttons |= mask;
                    (WPE_EVENT_POINTER_DOWN, view.press_count(x, y, button, time))
                } else {
                    self.buttons &= !mask;
                    (WPE_EVENT_POINTER_UP, 0)
                };
                webkit::Event::pointer_button(
                    kind,
                    view,
                    WPE_INPUT_SOURCE_MOUSE,
                    time,
                    self.modifiers | self.buttons,
                    button,
                    x,
                    y,
                    count,
                )
            }
            Input::Scroll { dx, dy, precise } => {
                let (x, y) = self.pointer;
                let source = if precise {
                    WPE_INPUT_SOURCE_TOUCHPAD
                } else {
                    WPE_INPUT_SOURCE_MOUSE
                };
                webkit::Event::scroll(view, source, time, self.modifiers, dx, dy, precise, x, y)
            }
            Input::Key { keyval, pressed } => {
                let kind = if pressed {
                    WPE_EVENT_KEYBOARD_KEY_DOWN
                } else {
                    WPE_EVENT_KEYBOARD_KEY_UP
                };
                let keycode = self.display.keycode(keyval);
                webkit::Event::keyboard(kind, view, time, self.modifiers, keycode, keyval)
            }
            Input::Touch { id, x, y, phase } => {
                let kind = match phase {
                    egui::TouchPhase::Start => WPE_EVENT_TOUCH_DOWN,
                    egui::TouchPhase::Move => WPE_EVENT_TOUCH_MOVE,
                    egui::TouchPhase::End => WPE_EVENT_TOUCH_UP,
                    egui::TouchPhase::Cancel => WPE_EVENT_TOUCH_CANCEL,
                };
                webkit::Event::touch(kind, view, time, self.modifiers, id, x, y)
            }
        };
        view.send(&event);
    }
}

/// The tab's state, read from its page.
fn refresh(page: &WebView, id: TabId) -> Tab {
    Tab {
        id,
        title: page.title(),
        uri: page.uri(),
        progress: page.estimated_load_progress(),
        loading: page.is_loading(),
        can_go_back: page.can_go_back(),
        can_go_forward: page.can_go_forward(),
        secure: page.is_secure(),
        crashed: false,
    }
}

/// A rendered frame, copied out for the window.
fn frame(buffer: &Buffer, id: TabId) {
    let (width, height) = (buffer.width(), buffer.height());
    trace!(tab = id, width, height, "frame");
    let pixels = match buffer.pixels() {
        Ok(pixels) => pixels,
        Err(error) => {
            warn!(error, "cannot read a frame");
            return;
        }
    };
    if width == 0 || height == 0 {
        return;
    }
    if let Some(image) = to_image(pixels, width, height) {
        emit(Event::Frame(id, Arc::new(image)));
    }
}

/// A frame in WPE's ARGB8888 (B, G, R, A bytes, premultiplied) as egui's
/// premultiplied RGBA; `None` when the buffer is too short for its size.
pub fn to_image(pixels: &[u8], width: usize, height: usize) -> Option<egui::ColorImage> {
    let stride = pixels.len() / height;
    if stride < width * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(width * height);
    for row in pixels.chunks_exact(stride).take(height) {
        out.extend(
            row[..width * 4]
                .chunks_exact(4)
                .map(|p| egui::Color32::from_rgba_premultiplied(p[2], p[1], p[0], p[3])),
        );
    }
    Some(egui::ColorImage::new([width, height], out))
}

/// `true` once the decision is made here rather than left to WebKit.
fn decide(decision: &PolicyDecision, kind: u32) -> bool {
    match kind {
        // What the engine cannot show is saved instead.
        WEBKIT_POLICY_DECISION_TYPE_RESPONSE => {
            if decision.is_mime_type_supported() {
                return false;
            }
            decision.download();
            true
        }
        // A middle click on a link opens it in a new tab behind this one.
        WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION => {
            if decision.mouse_button() != 2 {
                return false;
            }
            let Some(uri) = decision.uri() else {
                return false;
            };
            decision.ignore();
            later(Command::NewTab {
                uri: Some(uri),
                activate: false,
            });
            true
        }
        _ => false,
    }
}

impl Engine {
    /// Starts asking the keys for tab `tab`'s page, on a thread of its own
    /// (webauthn.rs blocks until the key answers), and puts up the bar.
    fn ask_keys(
        &mut self,
        tab: TabId,
        call: u64,
        (message, uri): (String, String),
        request: webauthn::Request,
        reply: Reply,
    ) {
        // One at a time per tab, as other browsers do: the person can
        // only be touching a key for one of them.
        if self.keys.values().any(|a| a.tab == tab) {
            reply.value(&Refusal::not_allowed("A request is already pending.").to_json());
            return;
        }
        let Some(commands) = LINK.with(|l| l.borrow().as_ref().map(Events::commands)) else {
            reply.value(&Refusal::cancelled().to_json());
            return;
        };
        let id = self.next_key;
        self.next_key += 1;
        let (pins, typed) = crossbeam_channel::bounded(1);
        let (cancel, cancelled) = crossbeam_channel::bounded(1);
        emit(Event::Key(KeyAsk {
            id,
            tab,
            host: config::host(&request.origin),
            op: request.op,
            status: webauthn::Status::Touch,
        }));
        self.keys.insert(
            id,
            Asking {
                tab,
                call,
                reply,
                pins,
                cancel,
            },
        );
        let spawned = std::thread::Builder::new()
            .name("security-key".into())
            .spawn(move || {
                let answer = webauthn::answer(
                    &message,
                    &uri,
                    request,
                    |status| commands.send(Command::KeyStatus(id, status)),
                    &typed,
                    &cancelled,
                );
                commands.send(Command::KeyDone(id, answer));
            });
        if let Err(error) = spawned {
            warn!(%error, "cannot start asking the security keys");
            if let Some(asking) = self.keys.remove(&id) {
                asking.reply.value(&Refusal::cancelled().to_json());
            }
            emit(Event::KeyDone(id));
        }
    }

    /// Stops tab `tab`'s request, or only the call `call` of it: the
    /// thread hears the cancel, and its answer then ends the request.
    fn stop_keys(&self, tab: TabId, call: Option<u64>) {
        for asking in self.keys.values() {
            if asking.tab == tab && call.is_none_or(|c| c == asking.call) {
                let _ = asking.cancel.try_send(());
            }
        }
    }
}

/// A message from webauthn-bridge.js in tab `tab`: a call to start, or one
/// the page aborted. The page is checked against the address WebKit has
/// for the tab, never against what it says of itself.
fn security_key(tab: TabId, text: &str, reply: Reply) {
    let call = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|m| Some((m.get("id")?.as_u64()?, m.get("op")?.as_str()? == "cancel")));
    let Some((call, cancel)) = call else {
        reply.value(&Refusal::cancelled().to_json());
        return;
    };
    if cancel {
        try_with(|e| e.stop_keys(tab, Some(call)));
        reply.value("{}");
        return;
    }
    let mut unanswered = Some(reply);
    try_with(|e| {
        let reply = unanswered.take().expect("taken once");
        let uri = e.page(tab).map(WebView::uri).unwrap_or_default();
        match webauthn::parse(text, &uri) {
            Ok(request) => e.ask_keys(tab, call, (text.to_owned(), uri), request, reply),
            Err(refusal) => {
                debug!(tab, ?refusal, "a security key request was refused");
                reply.value(&refusal.to_json());
            }
        }
    });
    // The engine is busy only inside a call of its own that ran the loop,
    // which a page's message never comes from; the site can ask again.
    if let Some(reply) = unanswered {
        warn!(tab, "a security key request arrived while the engine was busy");
        reply.value(&Refusal::not_allowed("Danube was busy. Try again.").to_json());
    }
}

/// Answers a permission request from what the person said before, or
/// asks them; always `true`, the request is this engine's to answer.
fn ask(page: &WebView, request: PermissionRequest, tab: TabId) -> bool {
    let kind = match Kind::from_type_name(&request.type_name()) {
        Some(Kind::CameraAndMicrophone) => {
            let (audio, video, display) = request.user_media();
            Some(if display {
                Kind::Screen
            } else if audio && video {
                Kind::CameraAndMicrophone
            } else if video {
                Kind::Camera
            } else {
                Kind::Microphone
            })
        }
        other => other,
    };
    let Some(kind) = kind else {
        request.deny();
        return true;
    };
    let host = config::host(&page.uri());
    let asked = try_with(|e| match e.permissions.get(&host, kind) {
        Some(true) => request.allow(),
        Some(false) => request.deny(),
        None => {
            let id = e.next_permission;
            e.next_permission += 1;
            e.pending.insert(id, (request.clone(), host.clone(), kind));
            emit(Event::Permission(PermissionAsk {
                id,
                tab,
                host: host.clone(),
                kind,
            }));
        }
    });
    if asked.is_none() {
        request.deny();
    }
    true
}

/// Decides where a download goes, once WebKit has its response, and who
/// fetches it: aria2 (downloads.rs) for a plain GET over HTTP(S), WebKit
/// for anything else (a POST's answer, a `blob:` or `data:` URL, a page of
/// an extension's own scheme) and for what aria2 could not fetch. Either
/// way the download shows a notification while it runs (notify.rs).
fn watch_download(download: &Download, dir: &Path, notifier: &Rc<RefCell<Notifier>>) {
    // Set once WebKit's own transfer is the one that runs, with how it is
    // stopped; aria2's leaves WebKit's cancelled and its signals unheard.
    let mine: Rc<RefCell<Option<WebKitDownload>>> = Rc::default();
    let dir = dir.to_path_buf();
    let decided = mine.clone();
    download.on_decide_destination(move |download, suggested| {
        let _ = std::fs::create_dir_all(&dir);
        match try_with(|e| e.decide_download(download, suggested)) {
            Some(Some(webkit)) => {
                download.set_destination(&webkit.path);
                *decided.borrow_mut() = Some(webkit);
            }
            // Handed to aria2: WebKit's transfer stops here, before any
            // of the file is written.
            Some(None) => download.cancel(),
            // The engine is busy inside a call that made WebKit ask, which
            // never happens for a page's own download: saved by WebKit, and
            // shown, but not in the engine's list to be cancelled from.
            None => {
                let path = destination(&dir, suggested);
                download.set_destination(&path);
                *decided.borrow_mut() = Some(WebKitDownload {
                    id: next_download(),
                    path,
                    cancelled: Rc::default(),
                });
            }
        }
        true
    });
    let (progress, notes) = (mine.clone(), notifier.clone());
    download.on_received(move |download| {
        let Some(webkit) = &*progress.borrow() else { return };
        let total = download.content_length();
        notes.borrow_mut().show(
            webkit.id,
            Card {
                summary: file_name(&webkit.path.to_string_lossy()),
                body: notify::progress_body(download.received(), total, None, None),
                progress: notify::percent(download.received(), total),
                running: true,
            },
        );
    });
    let failure: Rc<RefCell<Option<String>>> = Rc::default();
    let failed = failure.clone();
    download.on_failed(move |_, message| *failed.borrow_mut() = Some(message));
    let notes = notifier.clone();
    download.on_finished(move |download| {
        let Some(WebKitDownload { id, cancelled, .. }) = mine.borrow_mut().take() else { return };
        try_with(|e| e.downloads.held.remove(&id));
        let path = download.destination().unwrap_or_default();
        let name = file_name(&path);
        let body = match failure.borrow_mut().take() {
            _ if cancelled.get() => "Cancelled".to_owned(),
            Some(message) => {
                warn!(message, "a download failed");
                format!("Failed: {message}")
            }
            None => {
                let uri = download.uri();
                try_with(|e| runtime::with(e, |rt, _| rt.download_finished(&uri, &path)));
                "Downloaded to Downloads".to_owned()
            }
        };
        notes.borrow_mut().show(id, ended(name, body));
    });
}

/// The last card of a download.
fn ended(summary: String, body: String) -> Card {
    Card {
        summary,
        body,
        progress: None,
        running: false,
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

/// A download WebKit keeps: its id, where it goes, and its stop flag.
struct WebKitDownload {
    id: DownloadId,
    path: PathBuf,
    cancelled: Rc<Cell<bool>>,
}

impl Engine {
    /// Names `download`'s file and hands it to aria2, or returns where
    /// WebKit should save it itself.
    fn decide_download(&mut self, download: &Download, suggested: &str) -> Option<WebKitDownload> {
        let origin = download.uri();
        // An extension that started the download may have named the file.
        let asked = runtime::with(self, |rt, _| rt.download_name(&origin)).flatten();
        let retry = self.downloads.webkit_only.remove(&origin);
        let id = retry.as_ref().map_or_else(next_download, |(id, _)| *id);
        let name = retry.as_ref().map(|(_, name)| name.clone()).or(asked);
        let path = destination(&self.downloads.dir, name.as_deref().unwrap_or(suggested));
        let uri = Some(download.response_uri()).filter(|u| !u.is_empty()).unwrap_or_else(|| origin.clone());
        let aria2 = self.downloads.aria2.clone().filter(|_| {
            retry.is_none()
                && download.method().eq_ignore_ascii_case("GET")
                && (uri.starts_with("https://") || uri.starts_with("http://"))
        });
        // The name is taken now, with an empty file, so that a second
        // download of the same name started before aria2 writes this one
        // is given another.
        let reserved = aria2.is_some()
            && std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .is_ok();
        let name = file_name(&path.to_string_lossy());
        self.notifier.borrow_mut().show(
            id,
            Card {
                summary: name,
                body: "Starting".into(),
                progress: None,
                running: true,
            },
        );
        if !reserved {
            let cancelled = Rc::new(Cell::new(false));
            self.downloads.held.insert(
                id,
                Held::WebKit {
                    download: download.clone(),
                    cancelled: cancelled.clone(),
                },
            );
            return Some(WebKitDownload { id, path, cancelled });
        }
        info!(id, uri, "aria2 takes a download");
        let job = Job {
            id,
            uri: uri.clone(),
            path: path.clone(),
            cookie: None,
            // What WebKit sent: a page's referrer policy is already in it,
            // so a download it would have sent none for gets none here.
            referer: download.request_header("Referer"),
            // The page's own, the phone's when the window is laid out
            // for one.
            user_agent: self.settings.user_agent(),
        };
        let tab = download
            .web_view()
            .and_then(|view| self.tabs.iter().find(|t| t.page.is(&view)))
            .map(|t| t.id);
        self.downloads.held.insert(id, Held::Aria2 { origin, path, tab });
        self.session.cookies(&uri, move |cookies| {
            let cookie = (!cookies.is_empty()).then(|| {
                cookies
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            });
            later(Command::Download(DownloadCommand::Start(Job { cookie, ..job })));
        });
        None
    }

    fn download(&mut self, command: DownloadCommand) {
        match command {
            DownloadCommand::Start(job) => match (&self.downloads.held.get(&job.id), &self.downloads.aria2) {
                (Some(Held::Aria2 { .. }), Some(aria2)) => {
                    let _ = aria2.send(Request::Start(job));
                }
                // Gone already, or aria2's thread is: nothing fetches it.
                _ => {
                    let _ = std::fs::remove_file(&job.path);
                }
            },
            DownloadCommand::Cancel(id) => match self.downloads.held.get(&id) {
                Some(Held::Aria2 { .. }) => {
                    if let Some(aria2) = &self.downloads.aria2 {
                        let _ = aria2.send(Request::Cancel(id));
                    }
                }
                Some(Held::WebKit {
                    download,
                    cancelled,
                }) => {
                    cancelled.set(true);
                    download.cancel();
                }
                None => {}
            },
            DownloadCommand::Update(update) => self.aria2_update(update),
        }
    }

    /// What aria2's thread said about one of its downloads.
    fn aria2_update(&mut self, update: Update) {
        let id = match &update {
            Update::Progress { id, .. } | Update::Done(id) | Update::Cancelled(id) => *id,
            Update::Failed { id, .. } => *id,
        };
        let Some(Held::Aria2 { origin, path, tab }) = self.downloads.held.get(&id) else {
            return;
        };
        let (origin, path, tab) = (origin.clone(), path.clone(), *tab);
        let name = file_name(&path.to_string_lossy());
        let card = match update {
            Update::Progress {
                received,
                total,
                speed,
                connections,
                ..
            } => {
                let card = Card {
                    summary: name,
                    body: notify::progress_body(received, total, Some(speed), Some(connections)),
                    progress: notify::percent(received, total),
                    running: true,
                };
                return self.notifier.borrow_mut().show(id, card);
            }
            Update::Done(_) => {
                let path = path.to_string_lossy().into_owned();
                runtime::with(self, |rt, _| rt.download_finished(&origin, &path));
                ended(name, "Downloaded to Downloads".into())
            }
            Update::Cancelled(_) => ended(name, "Cancelled".into()),
            Update::Failed {
                retry: true,
                message,
                started,
                ..
            } => {
                // Nothing of it arrived: WebKit asks for it again, under
                // the same id and name, from the tab that asked first, so
                // that its cookies go with it (a download of the session's
                // own carries none). A download no tab started (an
                // extension's) is asked for by the session.
                info!(id, message, "aria2 could not fetch a download; WebKit tries");
                if !started {
                    // aria2 cannot run at all: WebKit keeps the rest.
                    self.downloads.aria2 = None;
                }
                self.downloads.held.remove(&id);
                self.downloads.webkit_only.insert(origin.clone(), (id, name));
                match tab.and_then(|t| self.page(t)) {
                    Some(page) => drop(page.download_uri(&origin)),
                    None => drop(self.session.download_uri(&origin)),
                }
                return;
            }
            Update::Failed { message, .. } => {
                warn!(id, message, "a download failed");
                ended(name, format!("Failed: {message}"))
            }
        };
        self.downloads.held.remove(&id);
        self.notifier.borrow_mut().show(id, card);
    }
}

/// A file name for `suggested` in `dir` that is not taken: "name.ext",
/// then "name (1).ext", and so on, with anything that could leave the
/// directory taken out.
pub fn destination(dir: &Path, suggested: &str) -> PathBuf {
    let name: String = suggested
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let name = name.trim_start_matches('.').trim();
    let name = if name.is_empty() { "download" } else { name };
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    let mut path = dir.join(name);
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

/// Reloads every rule set if losos-adblock compiled since the last look.
impl Filters {
    /// Gives a new tab's content manager the rule sets loaded so far, and
    /// the ones to come.
    fn attach(&mut self, tab: TabId, content: &UserContentManager) {
        for filter in &self.loaded_sets {
            content.add_filter(filter);
        }
        self.managers.insert(tab, content.clone());
    }
}

fn reload_filters(filters: &Rc<RefCell<Filters>>) {
    let jobs = {
        let mut f = filters.borrow_mut();
        let index = f.dir.join("index");
        let stamp = std::fs::metadata(&index).and_then(|m| m.modified()).ok();
        if stamp == f.stamp {
            return;
        }
        f.stamp = stamp;
        let text = std::fs::read_to_string(&index).unwrap_or_default();
        let mut jobs = Vec::new();
        for id in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if id.contains(['/', '.']) {
                continue;
            }
            let path = f.dir.join(format!("{id}.json"));
            let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
                continue;
            };
            let secs = modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            jobs.push((format!("{id}-{secs}"), path));
        }
        for content in f.managers.values() {
            content.remove_all_filters();
        }
        f.loaded_sets.clear();
        f.current = jobs.iter().map(|(ident, _)| ident.clone()).collect();
        emit(Event::Filters(0));
        jobs
    };
    info!(sets = jobs.len(), "loading content blockers");
    let store = filters.borrow().store.clone();
    for (ident, path) in jobs {
        // A set compiled before is loaded as it is; one that is new or
        // changed is compiled from losos-adblock's JSON, which takes WebKit
        // a few seconds for a big list.
        let (filters, store2) = (filters.clone(), store.clone());
        store.load(&ident.clone(), move |loaded| match loaded {
            Ok(filter) => add_filter(&filters, &filter),
            Err(_) => {
                let (filters, set) = (filters.clone(), path.clone());
                store2.save_from_file(&ident, &set, move |saved| match saved {
                    Ok(filter) => add_filter(&filters, &filter),
                    Err(error) => {
                        warn!(set = %path.display(), error, "WebKit refused a rule set")
                    }
                });
            }
        });
    }
    // Removes the compiled sets no longer named, so the cache does not
    // grow with every daily compile.
    let (filters, store2) = (filters.clone(), store.clone());
    store.fetch_identifiers(move |identifiers| {
        let current = filters.borrow().current.clone();
        for ident in identifiers {
            if !current.contains(&ident) {
                store2.remove(&ident);
            }
        }
    });
}

fn add_filter(filters: &Rc<RefCell<Filters>>, filter: &webkit::Filter) {
    let mut f = filters.borrow_mut();
    for content in f.managers.values() {
        content.add_filter(filter);
    }
    f.loaded_sets.push(filter.clone());
    emit(Event::Filters(f.loaded_sets.len()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_drop_their_row_padding() {
        // 2x2 pixels, rows padded to 12 bytes: BGRA blue, then red.
        let row = [255, 0, 0, 255, 0, 0, 255, 255, 9, 9, 9, 9];
        let pixels: Vec<u8> = row.iter().chain(row.iter()).copied().collect();
        let image = to_image(&pixels, 2, 2).unwrap();
        assert_eq!(image.pixels[0], egui::Color32::from_rgb(0, 0, 255));
        assert_eq!(image.pixels[1], egui::Color32::from_rgb(255, 0, 0));
        assert_eq!(image.pixels.len(), 4);
    }

    #[test]
    fn downloads_never_overwrite_or_escape() {
        let dir = std::env::temp_dir().join(format!("danube-dl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(destination(&dir, "../../etc/passwd"), dir.join("passwd"));
        assert_eq!(destination(&dir, ".bashrc"), dir.join("bashrc"));
        std::fs::write(dir.join("a.pdf"), "").unwrap();
        assert_eq!(destination(&dir, "a.pdf"), dir.join("a (1).pdf"));
        assert_eq!(destination(&dir, ""), dir.join("download"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
