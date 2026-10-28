//! Danube's page, tabs and navigation as derisk global menus, the way
//! every derisk app puts its menus in the top bar and the command palette:
//! one line of JSON per request on derisk's agent socket
//! (`$XDG_RUNTIME_DIR/derisk/agent.sock`), `register_menu` for the window,
//! and `{"event":"menu","item":...}` lines back when one is picked. The
//! window has no toolbar of its own: what a browser's would show (the
//! address, whether the page is secure, the ad blocking in force) is the
//! Page menu, and the address bar is the palette itself, which Danube
//! asks derisk to show (`dispatch` of the `palette` action) for Ctrl+L.
//! The palette's own Tabs and Extensions headings come from the second
//! thing Danube registers, `register_palette`: its tabs (and, once there
//! are any, its extensions) as data for derisk's bundled browser palette
//! plugin, which turns them into palette rows and sends what is picked
//! back as `{"event":"palette","source":"danube","command":{...}}`: a tab
//! to activate or close, a new tab, or an address typed in the palette to
//! open. Registering again replaces the data, and derisk drops it when
//! the connection closes.
//!
//! The history and the bookmarks are two more menus (library.rs), which
//! the palette searches as it does every menu's items.
//!
//! derisk names windows by its own ids, so Danube finds its window in
//! derisk's `state` by app id and title. The window sends an [`Update`]
//! whenever its title or tabs change, and this thread runs until the
//! window drops its end of that channel. Outside a derisk session there is
//! no socket, and the thread just keeps trying once a second.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError};
use serde_json::{json, Value};
use tracing::warn;

use crate::library::Page;
use crate::shared::{Command, Commands, ExtCommand, ExtensionInfo, Tab, TabId};

pub const APP_ID: &str = "org.losos.Danube";
const PREFIX: &str = "danube.";

/// What the menus are built from: the window's title, which pairs it with
/// derisk's window, its tabs, and how many content blocker rule sets are
/// in force. `palette` counts the times the window asked for derisk's
/// palette (Ctrl+L); it only ever grows, so a count seen once is not
/// acted on again, and updates that coalesce in the channel lose nothing.
#[derive(Default)]
pub struct Update {
    pub title: String,
    pub tabs: Vec<Tab>,
    pub active: Option<TabId>,
    pub filters: usize,
    pub palette: u64,
    pub extensions: Vec<ExtensionInfo>,
    /// The newest pages in the history, and the bookmarks.
    pub history: Vec<Page>,
    pub bookmarks: Vec<Page>,
}

/// The menus for a window showing `tabs`, `active` the one in front, with
/// `filters` content blocker rule sets loaded and `extensions` installed.
pub fn menus(tabs: &[Tab], active: Option<TabId>, filters: usize, extensions: &[ExtensionInfo]) -> Value {
    let current = active.and_then(|id| tabs.iter().find(|t| t.id == id));
    // The Page menu says what a toolbar would: disabled items, which the
    // top bar shows and the palette leaves out, then what can be done.
    let address = match current {
        Some(t) if !t.uri.is_empty() && t.uri != "about:blank" => t.uri.clone(),
        _ => "New tab".to_owned(),
    };
    let security = current.map(|t| {
        if t.secure {
            "Secure"
        } else if t.uri.starts_with("http://") {
            "Not secure"
        } else {
            ""
        }
    });
    let blocking = match filters {
        0 => "No filter lists compiled yet".to_owned(),
        1 => "Ad blocking: 1 filter set".to_owned(),
        n => format!("Ad blocking: {n} filter sets"),
    };
    let mut page = vec![
        json!({"kind": "item", "id": format!("{PREFIX}info.address"), "label": address, "enabled": false}),
    ];
    if let Some(security) = security.filter(|s| !s.is_empty()) {
        page.push(json!({"kind": "item", "id": format!("{PREFIX}info.security"), "label": security, "enabled": false}));
    }
    page.push(json!({"kind": "item", "id": format!("{PREFIX}info.blocking"), "label": blocking, "enabled": false}));
    page.push(json!({"kind": "separator"}));
    page.push(json!({"kind": "item", "id": ADDRESS, "label": "Open Address\u{2026}", "shortcut": "Ctrl+L"}));
    page.push(json!({"kind": "item", "id": format!("{PREFIX}copy"), "label": "Copy Address", "enabled": current.is_some_and(|t| !t.uri.is_empty())}));
    page.push(json!({"kind": "item", "id": format!("{PREFIX}find"), "label": "Find in Page\u{2026}", "shortcut": "Ctrl+F", "enabled": current.is_some()}));
    let mut switch = vec![
        json!({"kind": "item", "id": format!("{PREFIX}new"), "label": "New Tab", "shortcut": "Ctrl+T"}),
        json!({"kind": "separator"}),
    ];
    let mut close = Vec::new();
    for tab in tabs {
        let title = if tab.title.is_empty() {
            tab.uri.clone()
        } else {
            tab.title.clone()
        };
        switch
            .push(json!({"kind": "item", "id": format!("{PREFIX}tab.{}", tab.id), "label": title}));
        close.push(json!({"kind": "item", "id": format!("{PREFIX}close.{}", tab.id), "label": format!("Close {title}")}));
    }
    // Next and previous, so the palette cycles tabs as Ctrl+Tab does.
    switch.push(json!({"kind": "separator"}));
    switch.push(json!({"kind": "item", "id": format!("{PREFIX}next"), "label": "Next Tab", "shortcut": "Ctrl+Tab", "enabled": tabs.len() > 1}));
    switch.push(json!({"kind": "item", "id": format!("{PREFIX}previous"), "label": "Previous Tab", "shortcut": "Ctrl+Shift+Tab", "enabled": tabs.len() > 1}));
    let can = |f: fn(&Tab) -> bool| current.is_some_and(f);
    let navigate = vec![
        json!({"kind": "item", "id": format!("{PREFIX}back"), "label": "Back", "shortcut": "Alt+Left", "enabled": can(|t| t.can_go_back)}),
        json!({"kind": "item", "id": format!("{PREFIX}forward"), "label": "Forward", "shortcut": "Alt+Right", "enabled": can(|t| t.can_go_forward)}),
        json!({"kind": "item", "id": format!("{PREFIX}reload"), "label": "Reload", "shortcut": "Ctrl+R"}),
        json!({"kind": "item", "id": format!("{PREFIX}stop"), "label": "Stop", "enabled": can(|t| t.loading)}),
    ];
    // Extensions: each one's toolbar button (its popup or its action), its
    // options page, and whether it runs; what a browser's toolbar and
    // extensions page would hold.
    let mut ext = Vec::new();
    for (n, x) in extensions.iter().enumerate() {
        if n > 0 {
            ext.push(json!({"kind": "separator"}));
        }
        let mut button = json!({"kind": "item", "id": format!("{PREFIX}ext.{}", x.id), "label": x.title, "enabled": x.enabled});
        if let Some(key) = x.commands.iter().find(|c| c.name == "_execute_action" || c.name == "_execute_browser_action").and_then(|c| c.key.clone()) {
            button["shortcut"] = json!(key);
        }
        ext.push(button);
        if x.options {
            ext.push(json!({"kind": "item", "id": format!("{PREFIX}extoptions.{}", x.id), "label": format!("{} Options", x.name), "enabled": x.enabled}));
        }
        let toggle = if x.enabled { "Disable" } else { "Enable" };
        ext.push(json!({"kind": "item", "id": format!("{PREFIX}exttoggle.{}", x.id), "label": format!("{toggle} {}", x.name)}));
    }
    if extensions.is_empty() {
        ext.push(json!({"kind": "item", "id": format!("{PREFIX}info.extensions"), "label": "No extensions installed", "enabled": false}));
        ext.push(json!({"kind": "item", "id": format!("{PREFIX}info.install"), "label": "Install one with: danube install-extension FILE", "enabled": false}));
    }
    json!([
        {"title": "Page", "entries": page},
        {"title": "Tabs", "entries": switch},
        {"title": "Navigate", "entries": navigate},
        {"title": "Close Tab", "entries": close},
        {"title": "Extensions", "entries": ext},
    ])
}

/// `menus` with Bookmarks and History after Navigate: bookmarking the
/// active tab's page, then each bookmark; the newest pages, then clearing
/// them. Picking a page loads it in the active tab.
pub fn with_library(mut menus: Value, tabs: &[Tab], active: Option<TabId>, history: &[Page], bookmarks: &[Page]) -> Value {
    let current = active.and_then(|id| tabs.iter().find(|t| t.id == id));
    let marked = current.is_some_and(|t| bookmarks.iter().any(|b| b.uri == t.uri));
    let markable = current.is_some_and(|t| ["https://", "http://", "file://"].iter().any(|s| t.uri.starts_with(s)));
    let mut marks = vec![json!({
        "kind": "item",
        "id": format!("{PREFIX}bookmark"),
        "label": if marked { "Remove Bookmark" } else { "Bookmark This Page" },
        "shortcut": "Ctrl+D",
        "enabled": markable,
    })];
    if !bookmarks.is_empty() {
        marks.push(json!({"kind": "separator"}));
    }
    for (n, page) in bookmarks.iter().enumerate() {
        marks.push(json!({"kind": "item", "id": format!("{PREFIX}bookmarks.{n}"), "label": page.label()}));
    }
    let mut recent: Vec<Value> = history
        .iter()
        .enumerate()
        .map(|(n, page)| json!({"kind": "item", "id": format!("{PREFIX}history.{n}"), "label": page.label()}))
        .collect();
    if recent.is_empty() {
        recent.push(json!({"kind": "item", "id": format!("{PREFIX}info.history"), "label": "No pages visited yet", "enabled": false}));
    }
    recent.push(json!({"kind": "separator"}));
    recent.push(json!({"kind": "item", "id": format!("{PREFIX}clearhistory"), "label": "Clear History", "enabled": !history.is_empty()}));
    if let Some(list) = menus.as_array_mut() {
        let at = list.iter().position(|m| m["title"] == "Navigate").map_or(list.len(), |n| n + 1);
        list.insert(at, json!({"title": "History", "entries": recent}));
        list.insert(at, json!({"title": "Bookmarks", "entries": marks}));
    }
    menus
}

/// The command for a picked Bookmarks or History item; `None` for any
/// other.
pub fn library_command(item: &str, active: Option<TabId>, history: &[Page], bookmarks: &[Page]) -> Option<Command> {
    let rest = item.strip_prefix(PREFIX)?;
    let load = |page: &Page| match active {
        Some(tab) => Command::Load(tab, page.uri.clone()),
        None => Command::NewTab {
            uri: Some(page.uri.clone()),
            activate: true,
        },
    };
    let at = |n: &str| n.parse::<usize>().ok();
    Some(match rest.split_once('.') {
        Some(("bookmarks", n)) => load(bookmarks.get(at(n)?)?),
        Some(("history", n)) => load(history.get(at(n)?)?),
        None if rest == "bookmark" => Command::Bookmark(active?),
        None if rest == "clearhistory" => Command::ClearHistory,
        _ => return None,
    })
}

/// The tab after (or before) `tab` in the strip's order, wrapping round;
/// `None` when it is the only one or unknown.
pub fn neighbour(tabs: &[Tab], tab: TabId, forward: bool) -> Option<TabId> {
    let at = tabs.iter().position(|t| t.id == tab)?;
    if tabs.len() < 2 {
        return None;
    }
    let next = if forward {
        (at + 1) % tabs.len()
    } else {
        (at + tabs.len() - 1) % tabs.len()
    };
    Some(tabs[next].id)
}

/// The item that opens derisk's palette, handled here rather than by
/// WebKit: the palette is the address bar.
const ADDRESS: &str = "danube.address";

/// The command for a picked item; `None` for one Danube did not register,
/// and for [`ADDRESS`] and the Page menu's information lines.
pub fn command(item: &str, tabs: &[Tab], active: Option<TabId>, extensions: &[ExtensionInfo]) -> Option<Command> {
    let rest = item.strip_prefix(PREFIX)?;
    let id = |s: &str| s.parse::<TabId>().ok();
    Some(match rest.split_once('.') {
        Some(("tab", n)) => Command::Activate(id(n)?),
        Some(("close", n)) => Command::Close(id(n)?),
        Some(("ext", x)) => Command::Extensions(ExtCommand::Action(x.to_owned())),
        Some(("extoptions", x)) => Command::Extensions(ExtCommand::Options(x.to_owned())),
        Some(("exttoggle", x)) => {
            let enabled = extensions.iter().find(|e| e.id == x)?.enabled;
            Command::Extensions(ExtCommand::Enable(x.to_owned(), !enabled))
        }
        None => match rest {
            "new" => Command::NewTab {
                uri: None,
                activate: true,
            },
            "next" => Command::Activate(neighbour(tabs, active?, true)?),
            "previous" => Command::Activate(neighbour(tabs, active?, false)?),
            "back" => Command::Back(active?),
            "forward" => Command::Forward(active?),
            "reload" => Command::Reload(active?),
            "stop" => Command::Stop(active?),
            "copy" => Command::CopyAddress(active?),
            "find" => Command::ShowFind,
            _ => return None,
        },
        _ => return None,
    })
}

/// What the palette's browser plugin gets: the tabs and the extensions
/// as derisk's `register_palette` schema has them.
pub fn palette_data(tabs: &[Tab], active: Option<TabId>, extensions: &[ExtensionInfo]) -> Value {
    let tabs: Vec<Value> = tabs
        .iter()
        .map(|t| {
            json!({
                "id": t.id.to_string(),
                "title": t.title,
                "url": t.uri,
                "active": Some(t.id) == active,
            })
        })
        .collect();
    let extensions: Vec<Value> = extensions
        .iter()
        .map(|x| json!({"id": x.id, "name": x.name, "enabled": x.enabled, "options": x.options}))
        .collect();
    json!({"tabs": tabs, "extensions": extensions})
}

/// The command for what was picked under the palette's Tabs and
/// Extensions headings; `None` for one Danube does not know.
pub fn palette_command(command: &Value) -> Option<Command> {
    let id = || command["id"].as_str()?.parse::<TabId>().ok();
    let ext = || command["id"].as_str().map(str::to_owned);
    Some(match command["op"].as_str()? {
        "activate_tab" => Command::Activate(id()?),
        "close_tab" => Command::Close(id()?),
        "enable_extension" => Command::Extensions(ExtCommand::Enable(ext()?, true)),
        "disable_extension" => Command::Extensions(ExtCommand::Enable(ext()?, false)),
        "extension_options" => Command::Extensions(ExtCommand::Options(ext()?)),
        "new_tab" => Command::NewTab {
            uri: None,
            activate: true,
        },
        // derisk has already made an http(s) URL of the typed text (https://
        // for a bare host, http:// for localhost); a search is derisk's own.
        "open" => Command::NewTab {
            uri: Some(command["url"].as_str()?.to_owned()),
            activate: true,
        },
        _ => return None,
    })
}

fn socket() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("derisk/agent.sock"))
}

enum Asked {
    State,
    Register,
    /// The palette data registered; a refusal is an older derisk without
    /// the browser plugin, which keeps its menus.
    RegisterPalette,
    /// The palette shown; its answer says nothing Danube acts on.
    Palette,
}

/// The request that shows derisk's command palette.
fn show_palette() -> Value {
    json!({"method": "dispatch", "actions": [{"action": "palette", "visible": true}]})
}

/// Keeps derisk's menus in step with the tabs until the window is gone.
pub fn run(commands: &Commands, updates: Receiver<Update>) {
    let mut current = Update::default();
    loop {
        if let Some(stream) = socket().and_then(|path| UnixStream::connect(path).ok()) {
            if session(commands, &updates, &mut current, stream).is_err() {
                return;
            }
        }
        // A second's wait for derisk, spent taking the window's updates.
        match updates.recv_timeout(Duration::from_secs(1)) {
            Ok(update) => current = update,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The window is gone.
struct Stopped;

/// Takes what the window sent since the last look; `Err` once it is gone.
fn take(updates: &Receiver<Update>, current: &mut Update) -> Result<(), Stopped> {
    loop {
        match updates.try_recv() {
            Ok(update) => *current = update,
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => return Err(Stopped),
        }
    }
}

/// One connection, until derisk closes it (`Ok`) or the window is gone.
fn session(
    commands: &Commands,
    updates: &Receiver<Update>,
    current: &mut Update,
    mut stream: UnixStream,
) -> Result<(), Stopped> {
    let Ok(reader) = stream.try_clone() else {
        return Ok(());
    };
    let (lines_tx, lines) = crossbeam_channel::bounded(16);
    // The reader blocks on the socket; the loop below closes the socket
    // to end it, so the scope always finishes.
    thread::scope(|s| {
        s.spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Ok(line) = line else { break };
                if lines_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let result = talk(commands, updates, current, &mut stream, &lines);
        let _ = stream.shutdown(Shutdown::Both);
        result
    })
}

fn talk(
    commands: &Commands,
    updates: &Receiver<Update>,
    current: &mut Update,
    stream: &mut UnixStream,
    lines: &Receiver<String>,
) -> Result<(), Stopped> {
    let mut asked: VecDeque<Asked> = VecDeque::new();
    let mut registered: Option<(u64, Value)> = None;
    // The palette data derisk holds; sent again only when it changes.
    let mut registered_palette: Option<Value> = None;
    // Palette requests already passed on; the window only counts up.
    let mut palette = current.palette;
    let send =
        |stream: &mut UnixStream, request: Value, what: Asked, asked: &mut VecDeque<Asked>| {
            let mut line = request.to_string().into_bytes();
            line.push(b'\n');
            asked.push_back(what);
            stream.write_all(&line).is_ok()
        };
    loop {
        take(updates, current)?;
        if current.palette > palette {
            palette = current.palette;
            if !send(stream, show_palette(), Asked::Palette, &mut asked) {
                return Ok(());
            }
        }
        let data = palette_data(&current.tabs, current.active, &current.extensions);
        if registered_palette.as_ref() != Some(&data) {
            let request = json!({"method": "register_palette", "source": "danube", "data": data});
            if !send(stream, request, Asked::RegisterPalette, &mut asked) {
                return Ok(());
            }
            registered_palette = Some(data);
        }
        if asked.is_empty() && !send(stream, json!({"method": "state"}), Asked::State, &mut asked) {
            return Ok(());
        }
        let line = match lines.recv_timeout(Duration::from_secs(1)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message["event"] == "menu" {
            let item = message["item"].as_str().unwrap_or_default();
            if item == ADDRESS {
                if !send(stream, show_palette(), Asked::Palette, &mut asked) {
                    return Ok(());
                }
            } else if let Some(command) = library_command(item, current.active, &current.history, &current.bookmarks)
                .or_else(|| command(item, &current.tabs, current.active, &current.extensions))
            {
                commands.send(command);
            }
            continue;
        }
        if message["event"] == "palette" && message["source"] == "danube" {
            if let Some(command) = palette_command(&message["command"]) {
                commands.send(command);
            } else {
                warn!(command = %message["command"], "a palette command Danube cannot do");
            }
            continue;
        }
        match asked.pop_front() {
            Some(Asked::State) if message["ok"] == true => {
                let menus = with_library(
                    menus(&current.tabs, current.active, current.filters, &current.extensions),
                    &current.tabs,
                    current.active,
                    &current.history,
                    &current.bookmarks,
                );
                let window = message["result"]["windows"].as_array().and_then(|windows| {
                    windows
                        .iter()
                        .find(|w| w["app_id"] == APP_ID && w["title"] == current.title.as_str())
                });
                let Some(window) = window.and_then(|w| w["id"].as_u64()) else {
                    continue;
                };
                if registered.as_ref() == Some(&(window, menus.clone())) {
                    // Nothing changed; ask again in a second, or sooner
                    // when the window has news.
                    match updates.recv_timeout(Duration::from_secs(1)) {
                        Ok(update) => *current = update,
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return Err(Stopped),
                    }
                    continue;
                }
                let request = json!({"method": "register_menu", "window": window, "menus": menus});
                if !send(stream, request, Asked::Register, &mut asked) {
                    return Ok(());
                }
                registered = Some((window, menus));
            }
            Some(Asked::Register) if message["ok"] != true => {
                warn!(error = %message["error"], "derisk refused the menus");
                registered = None;
            }
            Some(Asked::RegisterPalette) if message["ok"] != true => {
                // Not sent again until the tabs change: the menus still
                // list them, so an older derisk loses nothing but rows.
                warn!(error = %message["error"], "derisk refused the palette data");
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_round_trip() {
        let tabs = vec![Tab {
            id: 4,
            title: "Example".into(),
            uri: "http://example.com/".into(),
            can_go_back: true,
            ..Tab::default()
        }];
        let menus = menus(&tabs, Some(4), 3, &[]);
        // Page: the address, security and blocking as disabled lines, then
        // the actions.
        assert_eq!(menus[0]["title"], "Page");
        assert_eq!(menus[0]["entries"][0]["label"], "http://example.com/");
        assert_eq!(menus[0]["entries"][0]["enabled"], false);
        assert_eq!(menus[0]["entries"][1]["label"], "Not secure");
        assert_eq!(
            menus[0]["entries"][2]["label"],
            "Ad blocking: 3 filter sets"
        );
        assert_eq!(menus[0]["entries"][4]["id"], ADDRESS);
        assert_eq!(menus[0]["entries"][5]["id"], "danube.copy");
        assert_eq!(menus[1]["entries"][2]["id"], "danube.tab.4");
        assert_eq!(menus[1]["entries"][4]["id"], "danube.next");
        assert_eq!(menus[1]["entries"][4]["enabled"], false);
        assert_eq!(menus[2]["entries"][0]["enabled"], true);
        assert_eq!(menus[2]["entries"][3]["enabled"], false, "nothing loads");
        assert_eq!(menus[3]["entries"][0]["label"], "Close Example");
        assert!(command(ADDRESS, &tabs, Some(4), &[]).is_none());
        assert!(command("danube.info.address", &tabs, Some(4), &[]).is_none());
        assert!(matches!(
            command("danube.copy", &tabs, Some(4), &[]),
            Some(Command::CopyAddress(4))
        ));
        let empty = super::menus(&[], None, 0, &[]);
        assert_eq!(empty[0]["entries"][0]["label"], "New tab");
        assert_eq!(
            empty[0]["entries"][1]["label"],
            "No filter lists compiled yet"
        );
        assert!(matches!(
            command("danube.close.4", &tabs, None, &[]),
            Some(Command::Close(4))
        ));
        assert!(matches!(
            command("danube.back", &tabs, Some(4), &[]),
            Some(Command::Back(4))
        ));
        assert!(command("danube.back", &tabs, None, &[]).is_none());
        assert!(command("other.tab.4", &tabs, None, &[]).is_none());
        assert!(command("danube.next", &tabs, Some(4), &[]).is_none());
        let two = vec![
            Tab {
                id: 4,
                ..Tab::default()
            },
            Tab {
                id: 7,
                ..Tab::default()
            },
        ];
        assert!(matches!(
            command("danube.next", &two, Some(7), &[]),
            Some(Command::Activate(4))
        ));
        assert!(matches!(
            command("danube.previous", &two, Some(4), &[]),
            Some(Command::Activate(7))
        ));
    }

    #[test]
    fn bookmarks_and_history_have_menus() {
        let tabs = vec![Tab {
            id: 4,
            uri: "https://a.example/".into(),
            ..Tab::default()
        }];
        let page = |uri: &str, title: &str| Page {
            uri: uri.into(),
            title: title.into(),
            time: 0,
        };
        let history = vec![page("https://b.example/", "B"), page("https://c.example/", "")];
        let bookmarks = vec![page("https://a.example/", "A")];
        let all = with_library(menus(&tabs, Some(4), 0, &[]), &tabs, Some(4), &history, &bookmarks);
        let titles: Vec<_> = all.as_array().unwrap().iter().map(|m| m["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Page", "Tabs", "Navigate", "Bookmarks", "History", "Close Tab", "Extensions"]);
        assert_eq!(all[3]["entries"][0]["label"], "Remove Bookmark");
        assert_eq!(all[3]["entries"][2]["label"], "A");
        assert_eq!(all[4]["entries"][1]["label"], "https://c.example/");
        assert!(matches!(
            library_command("danube.history.1", Some(4), &history, &bookmarks),
            Some(Command::Load(4, uri)) if uri == "https://c.example/"
        ));
        assert!(matches!(
            library_command("danube.bookmarks.0", None, &history, &bookmarks),
            Some(Command::NewTab { uri: Some(uri), activate: true }) if uri == "https://a.example/"
        ));
        assert!(matches!(library_command("danube.bookmark", Some(4), &[], &[]), Some(Command::Bookmark(4))));
        assert!(library_command("danube.history.9", Some(4), &history, &bookmarks).is_none());
        assert!(library_command("danube.tab.4", Some(4), &history, &bookmarks).is_none());
        let empty = with_library(menus(&[], None, 0, &[]), &[], None, &[], &[]);
        assert_eq!(empty[3]["entries"][0]["enabled"], false);
        assert_eq!(empty[4]["entries"][0]["label"], "No pages visited yet");
        assert!(matches!(command("danube.find", &tabs, Some(4), &[]), Some(Command::ShowFind)));
    }

    #[test]
    fn palette_data_and_commands() {
        let tabs = vec![
            Tab {
                id: 4,
                title: "Example".into(),
                uri: "https://example.com/".into(),
                ..Tab::default()
            },
            Tab {
                id: 7,
                ..Tab::default()
            },
        ];
        let data = palette_data(&tabs, Some(7), &[]);
        assert_eq!(
            data,
            json!({
                "tabs": [
                    {"id": "4", "title": "Example", "url": "https://example.com/", "active": false},
                    {"id": "7", "title": "", "url": "", "active": true},
                ],
                "extensions": [],
            })
        );
        assert!(matches!(
            palette_command(&json!({"op": "activate_tab", "id": "4"})),
            Some(Command::Activate(4))
        ));
        assert!(matches!(
            palette_command(&json!({"op": "close_tab", "id": "7"})),
            Some(Command::Close(7))
        ));
        assert!(matches!(
            palette_command(&json!({"op": "new_tab"})),
            Some(Command::NewTab {
                uri: None,
                activate: true
            })
        ));
        assert!(matches!(
            palette_command(&json!({"op": "open", "url": "https://example.org/"})),
            Some(Command::NewTab { uri: Some(url), activate: true }) if url == "https://example.org/"
        ));
        assert!(palette_command(&json!({"op": "activate_tab", "id": "x"})).is_none());
        assert!(palette_command(&json!({"op": "open"})).is_none());
        assert!(matches!(
            palette_command(&json!({"op": "enable_extension", "id": "bitwarden"})),
            Some(Command::Extensions(ExtCommand::Enable(id, true))) if id == "bitwarden"
        ));
        assert!(matches!(
            palette_command(&json!({"op": "extension_options", "id": "bitwarden"})),
            Some(Command::Extensions(ExtCommand::Options(id))) if id == "bitwarden"
        ));
    }

    #[test]
    fn extensions_have_a_menu() {
        let ext = vec![ExtensionInfo {
            id: "abc".into(),
            name: "Bitwarden".into(),
            enabled: true,
            title: "Bitwarden (3)".into(),
            options: true,
            commands: vec![crate::shared::Shortcut {
                name: "_execute_action".into(),
                key: Some("Ctrl+Shift+U".into()),
            }],
        }];
        let menus = menus(&[], None, 0, &ext);
        let entries = &menus[4]["entries"];
        assert_eq!(menus[4]["title"], "Extensions");
        assert_eq!(entries[0]["label"], "Bitwarden (3)");
        assert_eq!(entries[0]["shortcut"], "Ctrl+Shift+U");
        assert_eq!(entries[1]["label"], "Bitwarden Options");
        assert_eq!(entries[2]["label"], "Disable Bitwarden");
        assert!(matches!(
            command("danube.ext.abc", &[], None, &ext),
            Some(Command::Extensions(ExtCommand::Action(id))) if id == "abc"
        ));
        assert!(matches!(
            command("danube.exttoggle.abc", &[], None, &ext),
            Some(Command::Extensions(ExtCommand::Enable(id, false))) if id == "abc"
        ));
        let data = palette_data(&[], None, &ext);
        assert_eq!(data["extensions"][0]["name"], "Bitwarden");
        assert_eq!(data["extensions"][0]["options"], true);
    }
}
