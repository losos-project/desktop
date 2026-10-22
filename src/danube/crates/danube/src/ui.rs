//! The window: the page, and nothing of a browser's chrome. No toolbar,
//! address bar or tab strip: Danube is the web view, and derisk's command
//! palette and top-bar menus are its address bar and tabs, built from
//! what derisk.rs registers. Ctrl+L asks derisk for the palette, and the
//! window's title names the active tab. What the window still draws is
//! drawn with mcsapi's components in the desktop's theme: a progress
//! line while a page loads, a permission request's bar, a crashed page's
//! notice, the link under the pointer, and toasts.
//!
//! The page is WebKit's last frame for the active tab, painted as a
//! texture; input over it goes back to WebKit (input.rs).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use eframe::egui::{self, Key, Modifiers};
use mcsapi_components::{
    toast, Alert, Button, ButtonSize, ButtonVariant, Checkbox, Progress, Toaster, Tokens,
};

use crate::config;
use crate::derisk::Update;
use crate::input;
use crate::shared::{Command, Commands, Event, ExtCommand, ExtensionInfo, PermissionAsk, Tab, TabId};

/// The window's short side below which it is laid out for a phone.
const PHONE: f32 = 600.0;

pub struct Window {
    commands: Commands,
    events: Receiver<Event>,
    /// The title and tabs, for derisk's menus (derisk.rs), whenever they
    /// change.
    updates: Sender<Update>,
    /// What WebKit has said so far.
    state: State,
    /// The last update did not fit the channel; send it again.
    derisk_behind: bool,
    /// Times Ctrl+L asked for derisk's palette, carried in each update.
    palette: u64,
    /// Captive portal sign-in: one page, no tabs.
    captive: bool,
    /// A button went down over the page and is still held.
    dragging: bool,
    touch_active: bool,
    textures: HashMap<TabId, Option<Texture>>,
    /// The open extension popup's last frame as a texture.
    popup_texture: Option<Texture>,
    /// A button went down over the popup and is still held.
    dragging_popup: bool,
    last_size: Option<(i32, i32, f64)>,
    phone: Option<bool>,
    focused: bool,
    remember: bool,
    theme: ThemeWatch,
}

/// A frame uploaded to the GPU, and the frame it holds: an unchanged frame
/// is not uploaded again.
type Texture = (egui::TextureHandle, Arc<egui::ColorImage>);

/// Paints `image` over `rect` through the texture in `slot`, made on the
/// first frame and replaced only when WebKit drew a new one.
fn paint_frame(
    painter: &egui::Painter,
    slot: &mut Option<Texture>,
    name: &str,
    image: Arc<egui::ColorImage>,
    rect: egui::Rect,
) {
    let texture = match slot {
        Some((handle, held)) => {
            if !Arc::ptr_eq(held, &image) {
                handle.set(egui::ImageData::Color(image.clone()), egui::TextureOptions::NEAREST);
                *held = image;
            }
            handle.id()
        }
        None => {
            let handle = painter
                .ctx()
                .load_texture(name, egui::ImageData::Color(image.clone()), egui::TextureOptions::NEAREST);
            let id = handle.id();
            *slot = Some((handle, image));
            id
        }
    };
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    painter.image(texture, rect, uv, egui::Color32::WHITE);
}

/// This frame's input for a view (the page, or an extension's popup) drawn
/// at `rect`, as WPE takes it, and the text of any paste. The wheel counts
/// only while `wheel` (the pointer is over the view), keys only while no
/// component of the window's own (the permission bar's checkbox, say) has
/// the keyboard. egui turns Ctrl+C and Ctrl+X into Copy and Cut events, so
/// those go back to the view as the keys.
fn view_input(
    ctx: &egui::Context,
    rect: egui::Rect,
    wheel: bool,
    dragging: bool,
    touch_active: &mut bool,
) -> (Vec<input::Input>, Vec<String>) {
    let keys = !ctx.egui_wants_keyboard_input();
    let events: Vec<egui::Event> = ctx
        .input(|i| i.events.clone())
        .into_iter()
        .filter(|e| !matches!(e, egui::Event::MouseWheel { .. }) || wheel)
        .collect();
    let mut inputs = input::translate(&events, rect, keys, dragging, touch_active);
    let mut pasted = Vec::new();
    if keys {
        for event in &events {
            match event {
                egui::Event::Copy => inputs.extend(ctrl_key('c')),
                egui::Event::Cut => inputs.extend(ctrl_key('x')),
                egui::Event::Paste(text) => pasted.push(text.clone()),
                _ => {}
            }
        }
    }
    (inputs, pasted)
}

/// The window's picture of WebKit, kept from its events.
#[derive(Default)]
struct State {
    /// In opening order, which derisk's menus list them in.
    tabs: Vec<Tab>,
    active: Option<TabId>,
    /// The last frame each tab rendered.
    frames: HashMap<TabId, Arc<egui::ColorImage>>,
    hovered_link: Option<String>,
    permissions: VecDeque<PermissionAsk>,
    /// How many content blocker rule sets are in force, for the Page menu.
    filters: usize,
    /// The window's title, as last set and sent to derisk.
    title: String,
    /// The installed extensions, for the menus and the shortcuts.
    extensions: Vec<ExtensionInfo>,
    /// An extension's popup is open: its extension and size in logical
    /// pixels, drawn over the page's top right corner.
    popup: Option<(String, i32, i32)>,
    popup_frame: Option<Arc<egui::ColorImage>>,
}

impl State {
    fn tab(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// Applies one event; `true` for one that changes the menus.
    fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::Opened(tab) => self.tabs.push(tab),
            Event::Changed(tab) => {
                if let Some(t) = self.tab(tab.id) {
                    let crashed = t.crashed;
                    *t = Tab { crashed, ..tab };
                }
            }
            Event::LoadStarted(id) => {
                if let Some(t) = self.tab(id) {
                    t.crashed = false;
                }
            }
            Event::Crashed(id) => {
                if let Some(t) = self.tab(id) {
                    t.crashed = true;
                    t.loading = false;
                }
            }
            Event::Activated(id) => self.active = Some(id),
            Event::Closed(id) => {
                self.tabs.retain(|t| t.id != id);
                self.frames.remove(&id);
                self.permissions.retain(|p| p.tab != id);
                if self.active == Some(id) {
                    self.active = None;
                }
            }
            Event::Frame(id, image) => {
                self.frames.insert(id, image);
                return false;
            }
            Event::HoveredLink(link) => {
                self.hovered_link = link;
                return false;
            }
            Event::Permission(ask) => {
                self.permissions.push_back(ask);
                return false;
            }
            Event::PermissionDone(id) => {
                self.permissions.retain(|p| p.id != id);
                return false;
            }
            Event::Filters(n) => self.filters = n,
            Event::Extensions(list) => self.extensions = list,
            Event::PopupOpened { ext, width, height } => {
                self.popup = Some((ext, width, height));
                self.popup_frame = None;
                return false;
            }
            Event::PopupFrame(image) => {
                self.popup_frame = Some(image);
                return false;
            }
            Event::PopupClosed => {
                self.popup = None;
                self.popup_frame = None;
                return false;
            }
            // Handled by the window itself before they get here.
            Event::Copied(_) | Event::Notice(_) | Event::Quit => return false,
            // The tab shows WebKit's error page, which says it better.
            Event::LoadFailed { .. } => return false,
        }
        true
    }
}

/// The desktop theme, read again whenever derisk republishes it.
struct ThemeWatch {
    checked: Option<Instant>,
    id: Option<String>,
    tokens: Tokens,
    /// The theme's font families, which egui draws only once installed.
    fonts: mcsapi_theme::Fonts,
}

impl ThemeWatch {
    fn new() -> Self {
        Self {
            checked: None,
            id: None,
            tokens: Tokens::from_spec(&mcsapi_theme::Theme::dark()),
            fonts: mcsapi_theme::Fonts::default(),
        }
    }

    /// The theme derisk last published (`$XDG_RUNTIME_DIR/derisk/theme.json`,
    /// whose `id` names a built-in theme or a file in derisk's theme
    /// library), checked once a second, as derisk's own portal does.
    fn tokens(&mut self) -> Tokens {
        if self
            .checked
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return self.tokens;
        }
        self.checked = Some(Instant::now());
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return self.tokens;
        };
        let path = std::path::Path::new(&dir).join("derisk/theme.json");
        let id = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|json| json["id"].as_str().map(str::to_owned));
        if id.is_some() && id != self.id {
            let id_str = id.as_deref().unwrap();
            let theme = mcsapi_theme::Theme::builtin(id_str).or_else(|| {
                mcsapi_theme::Library::xdg("derisk")
                    .load(id_str)
                    .ok()
                    .map(|p| p.theme)
            });
            if let Some(theme) = theme {
                self.tokens = Tokens::from_spec(&theme);
                self.fonts = theme.fonts;
            }
            self.id = id;
        }
        self.tokens
    }
}

impl Window {
    pub fn new(
        commands: Commands,
        events: Receiver<Event>,
        updates: Sender<Update>,
        captive: bool,
    ) -> Self {
        Self {
            commands,
            events,
            updates,
            state: State::default(),
            derisk_behind: false,
            palette: 0,
            captive,
            dragging: false,
            touch_active: false,
            textures: HashMap::new(),
            popup_texture: None,
            dragging_popup: false,
            last_size: None,
            phone: None,
            focused: true,
            remember: false,
            theme: ThemeWatch::new(),
        }
    }

    fn send(&self, command: Command) {
        self.commands.send(command);
    }

    /// Takes everything WebKit sent since the last frame; `false` when
    /// WebKit is gone and the window should close.
    fn drain(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        loop {
            match self.events.try_recv() {
                Ok(Event::Quit) | Err(TryRecvError::Disconnected) => return false,
                Ok(Event::Copied(text)) => ctx.copy_text(text),
                Ok(Event::Notice(text)) => toast(ctx, text, None),
                Ok(event) => changed |= self.state.apply(event),
                Err(TryRecvError::Empty) => break,
            }
        }
        if changed || self.derisk_behind {
            self.tell_derisk();
        }
        true
    }

    /// Hands derisk the title and tabs its menus are built from. The
    /// channel is small and the window never waits on it: when derisk has
    /// fallen behind, the update goes with the next frame instead.
    fn tell_derisk(&mut self) {
        let update = Update {
            title: self.state.title.clone(),
            tabs: self.state.tabs.clone(),
            active: self.state.active,
            filters: self.state.filters,
            palette: self.palette,
            extensions: self.state.extensions.clone(),
        };
        self.derisk_behind = self.updates.try_send(update).is_err();
    }

    /// Asks derisk for its command palette, the address bar. The captive
    /// portal window has no derisk thread, and its page is the one it was
    /// opened for.
    fn open_palette(&mut self) {
        if self.captive {
            return;
        }
        self.palette += 1;
        self.tell_derisk();
    }

    /// Browser shortcuts, taken before the page sees the keys.
    fn shortcuts(&mut self, ctx: &egui::Context, active: Option<&Tab>, tabs: &[Tab]) {
        let ctrl = |key| egui::KeyboardShortcut::new(Modifiers::COMMAND, key);
        let alt = |key| egui::KeyboardShortcut::new(Modifiers::ALT, key);
        let pressed = |shortcut| ctx.input_mut(|i| i.consume_shortcut(&shortcut));
        // The extensions' own shortcuts, from their manifests, go first:
        // egui takes Ctrl+L as pressed when Ctrl+Shift+L is, so a
        // shortcut with more modifiers has to be asked for before ours.
        if !self.captive {
            // Between extensions likewise, the most modifiers first.
            let mut shortcuts: Vec<_> = (self.state.extensions.iter())
                .filter(|ext| ext.enabled)
                .flat_map(|ext| ext.commands.iter().map(move |command| (ext, command)))
                .filter_map(|(ext, command)| Some((command.key.as_deref().and_then(parse_shortcut)?, ext, command)))
                .collect();
            let held = |m: Modifiers| [m.command, m.shift, m.alt].into_iter().filter(|&on| on).count();
            shortcuts.sort_by_key(|(shortcut, ..)| std::cmp::Reverse(held(shortcut.modifiers)));
            let mut fired = Vec::new();
            for (shortcut, ext, command) in shortcuts {
                if pressed(shortcut) {
                    fired.push((ext.id.clone(), command.name.clone()));
                }
            }
            for (ext, command) in fired {
                self.send(Command::Extensions(ExtCommand::Shortcut { ext, command }));
            }
            if self.state.popup.is_some() && pressed(egui::KeyboardShortcut::new(Modifiers::NONE, Key::Escape)) {
                self.send(Command::Extensions(ExtCommand::ClosePopup));
            }
        }
        if pressed(ctrl(Key::L)) || pressed(alt(Key::D)) {
            self.open_palette();
        }
        if !self.captive && pressed(ctrl(Key::T)) {
            // The new tab is empty, and the palette is where its address
            // is typed.
            self.send(Command::NewTab {
                uri: None,
                activate: true,
            });
            self.open_palette();
        }
        let Some(tab) = active else { return };
        if !self.captive && pressed(ctrl(Key::W)) {
            self.send(Command::Close(tab.id));
        }
        if pressed(ctrl(Key::R)) || pressed(egui::KeyboardShortcut::new(Modifiers::NONE, Key::F5)) {
            self.send(Command::Reload(tab.id));
        }
        if pressed(alt(Key::ArrowLeft)) {
            self.send(Command::Back(tab.id));
        }
        if pressed(alt(Key::ArrowRight)) {
            self.send(Command::Forward(tab.id));
        }
        if self.captive || tabs.is_empty() {
            return;
        }
        let ctrl_shift =
            |key| egui::KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        if pressed(ctrl_shift(Key::Tab)) {
            if let Some(next) = crate::derisk::neighbour(tabs, tab.id, false) {
                self.send(Command::Activate(next));
            }
        } else if pressed(ctrl(Key::Tab)) {
            if let Some(next) = crate::derisk::neighbour(tabs, tab.id, true) {
                self.send(Command::Activate(next));
            }
        }
    }

    /// The prompt for the oldest pending permission request.
    fn permission(&mut self, ui: &mut egui::Ui, ask: &crate::shared::PermissionAsk) {
        let host = if ask.host.is_empty() {
            "This page"
        } else {
            ask.host.as_str()
        };
        ui.add(Alert::new(format!("{host} wants to {}", ask.kind.describe())).description(
            "Nothing is shared unless you allow it. A remembered answer is kept in ~/.config/danube/permissions.",
        ));
        ui.horizontal(|ui| {
            if ui.add(Button::new("Allow").size(ButtonSize::Sm)).clicked() {
                self.send(Command::Permission {
                    id: ask.id,
                    allow: true,
                    remember: self.remember,
                });
            }
            if ui
                .add(
                    Button::new("Deny")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm),
                )
                .clicked()
            {
                self.send(Command::Permission {
                    id: ask.id,
                    allow: false,
                    remember: self.remember,
                });
            }
            ui.add(Checkbox::new(&mut self.remember).label("Remember for this site"));
        });
    }

    /// Draws the active tab's page and sends it the input over it.
    fn page(
        &mut self,
        ui: &mut egui::Ui,
        tab: Option<&Tab>,
        frame: Option<Arc<egui::ColorImage>>,
        tokens: &Tokens,
    ) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let ctx = ui.ctx().clone();

        let scale = ctx.pixels_per_point() as f64;
        let size = (
            rect.width().round() as i32,
            rect.height().round() as i32,
            scale,
        );
        if self.last_size != Some(size) {
            self.last_size = Some(size);
            self.send(Command::Resize {
                width: size.0,
                height: size.1,
                scale,
            });
        }

        let Some(tab) = tab else {
            ui.painter().rect_filled(rect, 0.0, tokens.background);
            return;
        };
        ui.painter().rect_filled(rect, 0.0, egui::Color32::WHITE);
        if let Some(image) = frame {
            let slot = self.textures.entry(tab.id).or_default();
            paint_frame(ui.painter(), slot, &format!("tab-{}", tab.id), image, rect);
        }
        if tab.crashed {
            ui.painter().rect_filled(rect, 0.0, tokens.background);
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(32.0)));
            child.add(Alert::new("This page stopped working").description(
                "Its web process ended, so nothing on it runs. Reload to start it again.",
            ));
            if child.add(Button::new("Reload")).clicked() {
                self.send(Command::Reload(tab.id));
            }
            return;
        }

        // An extension's popup over the page's top right corner takes the
        // input while it is open; a click anywhere else closes it.
        if let Some((_, width, height)) = self.state.popup.clone() {
            let size = egui::vec2(width as f32, height as f32).min(rect.size());
            let popup_rect = egui::Rect::from_min_size(egui::pos2(rect.max.x - size.x, rect.min.y), size);
            self.popup(ui, popup_rect, tokens);
            let over = ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| popup_rect.contains(p));
            if ctx.input(|i| i.pointer.any_pressed()) {
                if over {
                    self.dragging_popup = true;
                } else if response.hovered() {
                    self.send(Command::Extensions(ExtCommand::ClosePopup));
                    return;
                }
            }
            if !ctx.input(|i| i.pointer.any_down()) {
                self.dragging_popup = false;
            }
            let (mut inputs, pasted) =
                view_input(&ctx, popup_rect, over, self.dragging_popup, &mut self.touch_active);
            // The popup has no paste command of its own: Ctrl+V has WebKit
            // read the clipboard itself.
            for _ in pasted {
                inputs.extend(ctrl_key('v'));
            }
            if !inputs.is_empty() {
                self.send(Command::Extensions(ExtCommand::PopupInput(inputs)));
            }
            return;
        }

        if ctx.input(|i| i.pointer.any_pressed()) && response.hovered() {
            self.dragging = true;
        }
        if !ctx.input(|i| i.pointer.any_down()) {
            self.dragging = false;
        }
        let hovered = response.hovered() || self.dragging;
        let (inputs, pasted) = view_input(&ctx, rect, hovered, self.dragging, &mut self.touch_active);
        for text in pasted {
            self.send(Command::Paste(tab.id, text));
        }
        if !inputs.is_empty() {
            self.send(Command::Input(tab.id, inputs));
        }
        if response.hovered() {
            if let Some(link) = &self.state.hovered_link {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                let galley_pos = egui::pos2(rect.min.x + 6.0, rect.max.y - 22.0);
                let text = egui::RichText::new(link).small().color(tokens.foreground);
                let mut status = ui.new_child(egui::UiBuilder::new().max_rect(
                    egui::Rect::from_min_size(galley_pos, egui::vec2(rect.width() * 0.6, 20.0)),
                ));
                egui::Frame::new()
                    .fill(tokens.card)
                    .corner_radius(tokens.control_radius())
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(&mut status, |ui| {
                        ui.add(egui::Label::new(text).truncate());
                    });
            }
        }
    }
}

impl Window {
    /// Draws the open popup's last frame in `rect`, with a border, as the
    /// page's own layer above the page.
    fn popup(&mut self, ui: &mut egui::Ui, rect: egui::Rect, tokens: &Tokens) {
        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
        if let Some(image) = self.state.popup_frame.clone() {
            paint_frame(painter, &mut self.popup_texture, "extension-popup", image, rect);
        }
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, tokens.border), egui::StrokeKind::Inside);
    }
}

/// A manifest shortcut (`Ctrl+Shift+L`, `Alt+Comma`) as egui's; `None`
/// for one it cannot press.
fn parse_shortcut(key: &str) -> Option<egui::KeyboardShortcut> {
    let mut modifiers = Modifiers::NONE;
    let mut last = None;
    for part in key.split('+') {
        match part {
            "Ctrl" | "Command" | "MacCtrl" => modifiers |= Modifiers::COMMAND,
            "Shift" => modifiers |= Modifiers::SHIFT,
            "Alt" => modifiers |= Modifiers::ALT,
            other => last = Some(other),
        }
    }
    let name = match last? {
        "Up" => "ArrowUp",
        "Down" => "ArrowDown",
        "Left" => "ArrowLeft",
        "Right" => "ArrowRight",
        "Period" => ".",
        "Comma" => ",",
        other => other,
    };
    Some(egui::KeyboardShortcut::new(modifiers, Key::from_name(name)?))
}

fn ctrl_key(c: char) -> Vec<input::Input> {
    let keyval = c as u32;
    vec![
        input::Input::Modifiers(crate::ffi::WPE_MODIFIER_KEYBOARD_CONTROL),
        input::Input::Key {
            keyval,
            pressed: true,
        },
        input::Input::Key {
            keyval,
            pressed: false,
        },
        input::Input::Modifiers(0),
    ]
}

fn tab_title(tab: &Tab) -> String {
    if !tab.title.is_empty() {
        tab.title.clone()
    } else if !tab.uri.is_empty() && tab.uri != "about:blank" {
        config::host(&tab.uri)
    } else {
        "New tab".into()
    }
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let tokens = self.theme.tokens();
        tokens.install(&ctx);
        mcsapi_components::install_fonts(&ctx, &self.theme.fonts);

        if !self.drain(&ctx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let tabs = self.state.tabs.clone();
        let active = self.state.active;
        let frame = active.and_then(|id| self.state.frames.get(&id).cloned());
        let permission = self.state.permissions.front().cloned();
        self.textures
            .retain(|id, _| tabs.iter().any(|t| t.id == *id));
        let tab = active
            .and_then(|id| tabs.iter().find(|t| t.id == id))
            .cloned();

        let focused = ctx.input(|i| i.focused);
        if focused != self.focused {
            self.focused = focused;
            self.send(Command::Focus(focused));
        }
        let screen = ctx.content_rect();
        let phone = screen.width().min(screen.height()) < PHONE;
        if self.phone != Some(phone) {
            self.phone = Some(phone);
            self.send(Command::Phone(phone));
        }

        let title = if self.captive {
            "Sign in to network".to_owned()
        } else {
            match &tab {
                Some(t) => format!("{} — Danube", tab_title(t)),
                None => "Danube".to_owned(),
            }
        };
        if title != self.state.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.state.title = title;
            self.tell_derisk();
        }

        self.shortcuts(&ctx, tab.as_ref(), &tabs);

        let bar = egui::Frame::new()
            .fill(tokens.background)
            .inner_margin(egui::Margin::symmetric(8, 6));
        // A loading page's only sign, a line along the top edge: the
        // window has no toolbar to put a spinner in.
        if let Some(t) = tab.as_ref().filter(|t| t.loading) {
            egui::Panel::top("progress")
                .frame(egui::Frame::new().fill(tokens.background))
                .show(ui, |ui| {
                    ui.add(Progress::new(t.progress as f32).width(ui.available_width()));
                });
        }
        if let Some(ask) = &permission {
            egui::Panel::top("permission")
                .frame(bar)
                .show(ui, |ui| self.permission(ui, ask));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(tokens.background))
            .show(ui, |ui| {
                self.page(ui, tab.as_ref(), frame, &tokens);
            });
        Toaster::show(&ctx);
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.send(Command::Quit);
    }
}
