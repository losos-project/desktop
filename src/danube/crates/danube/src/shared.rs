//! What the window (ui.rs) and WebKit (engine.rs) hand each other. They
//! run on two threads: WebKit on the process's main thread under GLib's
//! main loop, the window on its own under winit's. Two bounded channels
//! join them and nothing is locked: the window sends [`Command`]s, WebKit
//! sends [`Event`]s, and the window keeps its own picture of the tabs
//! from them.

use std::sync::Arc;

use crossbeam_channel::{Receiver, SendError, Sender, TrySendError};
use eframe::egui;

use crate::input::Input;

/// A tab, as long as it is open. Never reused.
pub type TabId = u64;

/// What the window shows for one tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub uri: String,
    /// 0 to 1 while loading.
    pub progress: f64,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    /// An https page whose certificate checked out.
    pub secure: bool,
    /// Its web process died; the page shows nothing until reloaded.
    pub crashed: bool,
}

/// A site asking for something only the person may grant.
#[derive(Clone, Debug)]
pub struct PermissionAsk {
    pub id: u64,
    pub tab: TabId,
    /// The host asking, as shown and as remembered.
    pub host: String,
    pub kind: crate::permissions::Kind,
}

/// A site asking for a security key (webauthn.rs), as its bar shows it.
#[derive(Clone, Debug)]
pub struct KeyAsk {
    pub id: u64,
    pub tab: TabId,
    /// The host asking, which the bar names.
    pub host: String,
    pub op: crate::webauthn::Op,
    pub status: crate::webauthn::Status,
}

/// A security key's PIN on its way to the key, which a `Debug` of the
/// command carrying it does not show.
pub struct Pin(pub String);

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pin(..)")
    }
}

/// What the window asks of WebKit.
#[derive(Debug)]
pub enum Command {
    /// The window's context, sent first, for WebKit to ask it to repaint
    /// with each event.
    Attach(egui::Context),
    NewTab {
        uri: Option<String>,
        activate: bool,
    },
    Activate(TabId),
    Close(TabId),
    Back(TabId),
    Forward(TabId),
    Reload(TabId),
    Stop(TabId),
    /// Put the tab's address on the desktop's clipboard (through
    /// [`Event::Copied`], as a page's own copy goes).
    CopyAddress(TabId),
    /// The page area, in logical pixels, and the window's scale.
    Resize {
        width: i32,
        height: i32,
        scale: f64,
    },
    /// The window gained or lost the keyboard.
    Focus(bool),
    /// The page is on screen or not: a hidden one stops painting, slows
    /// its timers, and pauses its animations and media.
    Shown(bool),
    Input(TabId, Vec<Input>),
    /// Paste `text` into the page: put it on WebKit's clipboard, then
    /// press Ctrl+V there.
    Paste(TabId, String),
    Permission {
        id: u64,
        allow: bool,
        remember: bool,
    },
    /// The window was laid out for a phone (short side under 600 px) or
    /// not; sets the User-Agent string for the next load.
    Phone(bool),
    /// The PIN the person typed in a security key's bar.
    KeyPin(u64, Pin),
    /// The person pressed Cancel in a security key's bar.
    KeyCancel(u64),
    /// From the thread talking to the keys (webauthn.rs): how it is going,
    /// and the answer for the page once it is over.
    KeyStatus(u64, crate::webauthn::Status),
    KeyDone(u64, String),
    /// For the extension runtime (extensions/runtime.rs).
    Extensions(ExtCommand),
    /// About a download handed to aria2 (downloads.rs, engine.rs).
    Download(DownloadCommand),
    Quit,
}

/// What reaches WebKit's thread about the downloads aria2 fetches.
#[derive(Debug)]
pub enum DownloadCommand {
    /// The cookies for a download arrived: aria2 can start it.
    Start(crate::downloads::Job),
    /// How a download is doing, from aria2's thread.
    Update(crate::downloads::Update),
    /// The person pressed Cancel on its notification.
    Cancel(crate::downloads::DownloadId),
}

/// What the window, derisk's menus or a later `danube install-extension`
/// run ask of the extension runtime.
#[derive(Debug)]
pub enum ExtCommand {
    /// The extension's toolbar button: its popup, or its onClicked.
    Action(String),
    ClosePopup,
    /// Input over the open popup, in its own coordinates.
    PopupInput(Vec<Input>),
    /// Open the extension's options page in a tab.
    Options(String),
    Enable(String, bool),
    /// Load, unload or reload one extension, or all of them (`None`) after
    /// an install.
    Reload(Option<String>),
    /// A manifest `commands` shortcut the window saw.
    Shortcut { ext: String, command: String },
    /// Carried over from a signal handler that could not borrow the
    /// engine: a load event in an extension view, a view closing itself,
    /// a load event in a tab.
    ViewLoad(u64, u32),
    ViewClosed(u64),
    TabLoad(TabId, u32),
}

/// An installed extension as the menus and the palette list it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// The toolbar button's title, with its badge when it has one.
    pub title: String,
    /// It has an options page.
    pub options: bool,
    pub commands: Vec<Shortcut>,
}

/// A keyboard shortcut from an extension's manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct Shortcut {
    pub name: String,
    /// As the manifest writes it (`Ctrl+Shift+L`), when it has one.
    pub key: Option<String>,
}

/// What WebKit tells the window.
#[derive(Debug)]
pub enum Event {
    /// A tab joined the end of the strip.
    Opened(Tab),
    /// A tab's title, address, progress or history changed; `crashed` is
    /// not carried, the window keeps it from [`Event::Crashed`].
    Changed(Tab),
    /// A load began, so a page that had crashed is running again.
    LoadStarted(TabId),
    Crashed(TabId),
    /// A load failed for good (not cancelled by the next one): the
    /// address and why. WebKit shows its error page in the tab.
    LoadFailed {
        tab: TabId,
        uri: String,
        error: String,
    },
    Activated(TabId),
    Closed(TabId),
    /// A tab's latest frame, premultiplied RGBA at device pixels.
    Frame(TabId, Arc<egui::ColorImage>),
    /// Where the pointer's link goes, for the status line.
    HoveredLink(Option<String>),
    /// Text a page put on the clipboard, for the window to hand to the
    /// desktop's.
    Copied(String),
    /// A one-line message for the person: a finished download when
    /// nothing shows notifications, an extension's notification.
    Notice(String),
    Permission(PermissionAsk),
    /// The request was answered (or its tab closed); the bar goes away.
    PermissionDone(u64),
    /// A site asks for a security key, and how that goes until it is over.
    Key(KeyAsk),
    KeyStatus(u64, crate::webauthn::Status),
    KeyDone(u64),
    /// How many content blocker rule sets are in force, for the Page menu.
    Filters(usize),
    /// The installed extensions, whenever they or their badges change.
    Extensions(Vec<ExtensionInfo>),
    /// An extension's popup opened, `width` by `height` logical pixels,
    /// to be drawn over the page's top right corner.
    PopupOpened { ext: String, width: i32, height: i32 },
    PopupFrame(Arc<egui::ColorImage>),
    PopupClosed,
    /// WebKit is gone; the window closes.
    Quit,
}

/// How many commands may wait for WebKit, and how many events for the
/// window. Each side drains the other's channel whole every time it
/// wakes, so neither fills unless the other has hung; a sender then
/// blocks rather than piling up work, except for frames, which are
/// dropped (the next one is as good).
const COMMANDS: usize = 64;
const EVENTS: usize = 256;

/// The window's end of the command channel, cloned into every thread that
/// asks WebKit for something: the window, derisk's menus, later `danube`
/// runs and the captive portal's recheck.
#[derive(Clone)]
pub struct Commands {
    tx: Sender<Command>,
}

impl Commands {
    pub fn channel() -> (Self, Receiver<Command>) {
        let (tx, rx) = crossbeam_channel::bounded(COMMANDS);
        (Self { tx }, rx)
    }

    /// Sends `command` to WebKit and wakes its main loop to drain the
    /// channel. Once WebKit has stopped the command goes nowhere, which
    /// every sender is content with: it is stopping too.
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
        crate::engine::wake();
    }
}

/// WebKit's end of the event channel, with the window's context once the
/// window has attached it, so that each event also repaints.
pub struct Events {
    tx: Sender<Event>,
    ctx: Option<egui::Context>,
    /// The command channel's sending end, for a signal handler that has
    /// to put a command of its own behind the one being handled.
    commands: Commands,
}

impl Events {
    pub fn channel(commands: Commands) -> (Self, Receiver<Event>) {
        let (tx, rx) = crossbeam_channel::bounded(EVENTS);
        (
            Self {
                tx,
                ctx: None,
                commands,
            },
            rx,
        )
    }

    pub fn attach(&mut self, ctx: egui::Context) {
        self.ctx = Some(ctx);
    }

    /// The command channel, for a thread of WebKit's that reports back.
    pub fn commands(&self) -> Commands {
        self.commands.clone()
    }

    /// Queues `command` for WebKit's own thread to handle next.
    pub fn defer(&self, command: Command) {
        self.commands.send(command);
    }

    pub fn send(&self, event: Event) -> Result<(), SendError<Event>> {
        let sent = match event {
            // A frame the window has not drawn yet is replaced by the
            // next, never queued behind it.
            Event::Frame(..) | Event::PopupFrame(..) => match self.tx.try_send(event) {
                Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
                Err(TrySendError::Disconnected(event)) => Err(SendError(event)),
            },
            event => self.tx.send(event),
        };
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
        sent
    }
}
