//! Each guest window as a window of this desktop.
//!
//! A guest window becomes an xdg toplevel whose app id is `<prefix>.<app>`,
//! the name of the desktop entry `entries` wrote for its app, so the desktop
//! files it under that app's icon and name. A guest's menus, tooltips and
//! panels become xdg popups placed at their offset from the window they
//! belong to. Each picture is copied into shared memory and shown at the
//! guest window's size in this desktop's logical pixels through a viewport,
//! whatever its scale on the guest.
//!
//! The windows carry no title bar of their own: the guest draws its own,
//! and dragging it moves the window here (`Moving`). A size the desktop
//! gives a window (tiling, maximizing) is asked of the guest. Under derisk,
//! a new window floats while the guest resizes it to its tile, and is tiled
//! once it has that size; one the guest will not resize stays floating.
//! Until the
//! guest's window has that size, which a dialog or a fixed-size window
//! never does, the picture keeps its shape in the middle of it.
//!
//! Input goes back in the guest window's own points. Keys go by their Linux
//! code with the text this desktop's layout types with them; held keys
//! repeat at this desktop's rate. The clipboard's text goes both ways. Over
//! the guest's windows the pointer looks as the guest's does: a named look
//! from this desktop's cursor theme, or the guest's own picture.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_windows_protocol::{
    modifiers, AppInfo, Button, Clipboard, Cursor, Key, Message, Pointer, Resize, Scroll, Watch,
    WindowInfo, WindowKind, WindowRef,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
        DataDeviceManagerState, WritePipe,
    },
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            channel::{self, Sender},
            timer::{TimeoutAction, Timer},
            EventLoop, LoopHandle,
        },
        calloop_wayland_source::WaylandSource,
        client::{
            globals::registry_queue_init,
            protocol::{
                wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
                wl_data_source::WlDataSource, wl_keyboard::WlKeyboard, wl_output,
                wl_pointer::WlPointer, wl_seat::WlSeat, wl_shm, wl_surface::WlSurface,
            },
            Connection, Dispatch, QueueHandle,
        },
        protocols::{
            wp::viewporter::client::{
                wp_viewport::{self, WpViewport},
                wp_viewporter::WpViewporter,
            },
            xdg::shell::client::xdg_positioner::{Anchor, ConstraintAdjustment, Gravity},
        },
    },
    registry::{ProvidesRegistryState, RegistryState, SimpleGlobal},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            popup::{Popup, PopupConfigure, PopupHandler},
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgPositioner, XdgShell, XdgSurface,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};

use crate::decode::Picture;
use crate::derisk::{self, Bridge, Place, Register};
use crate::entries;
use crate::link::Link;
use crate::stream::{Event, Frames};

/// Linux's mouse button codes, which Wayland passes on.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// MIME types that carry plain text, best first.
const TEXT: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

pub struct Options {
    /// Goes before each app's id to make its windows' app id.
    pub prefix: String,
    /// Whether the guest going away ends this program. A recording played
    /// back stays on its last pictures instead, to be looked at.
    pub exit_when_lost: bool,
    /// Called with the guest's apps whenever it lists them.
    pub on_apps: AppsListed,
}

pub type AppsListed = Box<dyn FnMut(&[AppInfo])>;

/// How often the guest is asked for its apps again, for ones installed
/// since: often enough that a new app shows up soon, rarely enough that
/// reading every app's icon on the guest costs nothing to speak of.
const APPS_EVERY: Duration = Duration::from_secs(300);

enum Role {
    Toplevel(Window),
    Popup(Popup),
}

struct Shown {
    info: WindowInfo,
    role: Role,
    viewport: Option<WpViewport>,
    configured: bool,
    /// Its size here, in logical pixels: the guest's own unless the desktop
    /// gave it another.
    size: (u32, u32),
    /// Whether the desktop chose the size (tiled, maximized, full screen),
    /// so the guest's own changes do not undo it.
    sized_here: bool,
    /// The last size asked of the guest, so it is asked once.
    asked: Option<(u32, u32)>,
    buffers: Vec<Buffer>,
    /// The picture on screen, drawn again when the desktop resizes the
    /// window and the guest sends no new one, as a window that will not
    /// take the size never does.
    last: Option<Picture>,
    /// The size changed since the last picture was drawn.
    resized: bool,
    /// Its first tile, while it floats and the guest is resized to it.
    placing: Option<(u32, u32)>,
    /// Its first size here came, so it is placed only once.
    placed: bool,
    /// A picture is on screen and the compositor has not yet said when to
    /// draw the next.
    drawing: bool,
}

impl Shown {
    fn surface(&self) -> &WlSurface {
        match &self.role {
            Role::Toplevel(w) => w.wl_surface(),
            Role::Popup(p) => p.wl_surface(),
        }
    }

    /// Guest points for a position on this surface, kept on the guest's
    /// window so a press on the bars beside it lands on its edge and not on
    /// whatever is behind it on the guest.
    fn to_guest(&self, x: f64, y: f64) -> (f64, f64) {
        let (scale, ox, oy) = self.fit();
        (
            ((x - ox) / scale).clamp(0.0, self.info.width),
            ((y - oy) / scale).clamp(0.0, self.info.height),
        )
    }

    /// How many logical pixels here one guest point takes, across and down.
    fn stretch(&self) -> (f64, f64) {
        let (scale, _, _) = self.fit();
        (scale, scale)
    }

    /// Where the guest's picture goes on this surface: how many logical
    /// pixels a guest point takes, and its offset from the top left. A size
    /// the desktop chose that the guest's window does not take, as a
    /// dialog or Calculator never does, keeps the picture's shape and
    /// centres it, with bars around it, instead of stretching it. It is
    /// made smaller to fit a smaller size but never larger than the guest
    /// draws it: a dialog blown up to half the screen is blurred, and its
    /// text no longer matches the desktop's.
    fn fit(&self) -> (f64, f64, f64) {
        let (w, h) = (f64::from(self.size.0), f64::from(self.size.1));
        let (gw, gh) = (self.info.width.max(1.0), self.info.height.max(1.0));
        let scale = (w / gw).min(h / gh).clamp(f64::MIN_POSITIVE, 1.0);
        (scale, (w - gw * scale) / 2.0, (h - gh * scale) / 2.0)
    }
}

fn rounded(width: f64, height: f64) -> (u32, u32) {
    (
        width.round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    )
}

/// A left press on a window, kept so that the guest moving that window
/// while it is held, which is its title bar being dragged, moves the window
/// here instead.
#[derive(Clone, Copy)]
struct Press {
    window: u32,
    serial: u32,
    x: f64,
    y: f64,
}

struct App {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    xdg: XdgShell,
    shm: Shm,
    viewporter: SimpleGlobal<WpViewporter, 1>,
    data: Option<DataDeviceManagerState>,
    pool: SlotPool,
    qh: QueueHandle<App>,
    handle: LoopHandle<'static, App>,

    link: Link,
    frames: Arc<Frames>,
    options: Options,
    bridge: Option<Bridge>,
    windows: HashMap<u32, Shown>,
    /// Popups the desktop dismissed, gone here while the guest closes them.
    dismissed: HashSet<u32>,

    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<ThemedPointer>,
    /// The serial of the pointer's last entry into one of the windows,
    /// which a pointer picture is set with.
    entered: Option<u32>,
    /// How the guest's pointer looks, and its picture made ready when it is
    /// one of the guest's own.
    cursor: Cursor,
    cursor_picture: Option<CursorPicture>,
    device: Option<DataDevice>,
    /// Which window has the keyboard, and the serial it came with.
    focus: Option<(u32, u32)>,
    under_pointer: Option<u32>,
    held_keys: HashSet<u32>,
    modifiers: u32,
    press: Option<Press>,
    /// The guest was already told of a release when its window started
    /// moving here; the real one, if it comes, is not passed on.
    swallow_release: bool,

    /// The clipboard's text as last sent or received, shared with the
    /// threads that read offers, so nothing goes back where it came from.
    clipboard: Arc<Mutex<Option<String>>>,
    /// The guest's clipboard waiting for this desktop to take it, which it
    /// does only from a window with the keyboard.
    offer_pending: bool,
    source: Option<CopyPasteSource>,

    lost: Option<String>,
    conn: Connection,
}

/// A guest's own pointer picture in shared memory.
struct CursorPicture {
    buffer: Buffer,
    width: i32,
    height: i32,
    scale: i32,
    hotspot: (i32, i32),
}

/// Shows the guest's windows until it goes away.
///
/// `start` is handed the mailbox and the sink once there is a desktop to
/// show on, and starts whatever reads the guest into them.
pub fn show(
    link: Link,
    options: Options,
    start: impl FnOnce(Arc<Frames>, crate::stream::Sink),
) -> Result<(), String> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("no Wayland desktop to show windows on: {e}"))?;
    let (globals, queue) = registry_queue_init(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .map_err(|e| e.to_string())?;

    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e}"))?;
    let xdg = XdgShell::bind(&globals, &qh).map_err(|e| format!("xdg_wm_base: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e}"))?;
    let pool = SlotPool::new(1 << 22, &shm).map_err(|e| e.to_string())?;

    let (tx, rx) = channel::channel::<Event>();
    handle
        .insert_source(rx, |event, _, app: &mut App| {
            if let channel::Event::Msg(event) = event {
                app.event(event);
            }
        })
        .map_err(|e| e.to_string())?;
    let frames = Arc::new(Frames::default());
    let tx: Mutex<Sender<Event>> = Mutex::new(tx);
    start(
        frames.clone(),
        Arc::new(move |e| drop(tx.lock().unwrap().send(e))),
    );

    let mut app = App {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        viewporter: SimpleGlobal::bind(&globals, &qh).map_err(|e| e.to_string())?,
        data: DataDeviceManagerState::bind(&globals, &qh).ok(),
        compositor,
        xdg,
        shm,
        pool,
        qh,
        handle,
        bridge: derisk::connect(link.clone()),
        link,
        frames,
        options,
        windows: HashMap::new(),
        dismissed: HashSet::new(),
        seat: None,
        keyboard: None,
        pointer: None,
        entered: None,
        cursor: Cursor::default(),
        cursor_picture: None,
        device: None,
        focus: None,
        under_pointer: None,
        held_keys: HashSet::new(),
        modifiers: 0,
        press: None,
        swallow_release: false,
        clipboard: Arc::default(),
        offer_pending: false,
        source: None,
        lost: None,
        conn: conn.clone(),
    };
    app.link.send(Message::Watch(Watch { cursor: true }));
    app.handle
        .insert_source(Timer::immediate(), |_, _, app: &mut App| {
            app.link.send(Message::ListApps);
            TimeoutAction::ToDuration(APPS_EVERY)
        })
        .map_err(|e| e.to_string())?;
    loop {
        event_loop
            .dispatch(None, &mut app)
            .map_err(|e| e.to_string())?;
        if let Some(why) = app.lost.take() {
            if app.options.exit_when_lost {
                return Err(why);
            }
            eprintln!("app-windows: {why}; the windows stay as they were");
        }
    }
}

impl App {
    fn event(&mut self, event: Event) {
        match event {
            Event::Message(Message::WindowOpened(info)) => self.open(info),
            Event::Message(Message::WindowChanged(info)) => self.changed(info),
            Event::Message(Message::WindowClosed(WindowRef { window })) => self.close(window),
            Event::Message(Message::Menus(menus)) => {
                if let (Some(bridge), Some(shown)) = (&self.bridge, self.windows.get(&menus.window))
                {
                    bridge.register(Register {
                        window: menus.window,
                        app_id: entries::app_id(&self.options.prefix, &shown.info.app),
                        title: shown.info.title.clone(),
                        menus: menus.menus,
                    });
                }
            }
            Event::Message(Message::Clipboard(Clipboard { text })) => {
                *self.clipboard.lock().unwrap() = Some(text);
                self.offer_pending = true;
                self.offer_clipboard();
            }
            Event::Message(Message::Cursor(cursor)) => self.cursor_changed(cursor),
            Event::Message(Message::Apps(apps)) => (self.options.on_apps)(&apps),
            Event::Message(Message::Error(e)) => {
                eprintln!("app-windows: the guest says: {}", e.message)
            }
            Event::Message(_) => {}
            Event::Picture(window) => self.draw(window),
            Event::Lost(why) => self.lost = Some(why),
        }
    }

    fn open(&mut self, info: WindowInfo) {
        if self.windows.contains_key(&info.window) {
            return self.changed(info);
        }
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let viewport = self
            .viewporter
            .get()
            .ok()
            .map(|v| v.get_viewport(&surface, &qh, ()));
        let parent = info
            .parent
            .and_then(|p| self.windows.get(&p))
            .filter(|p| p.configured);
        let role = match (info.kind, parent) {
            (WindowKind::Popup, Some(parent)) => {
                let positioner = match self.positioner(&info, parent) {
                    Ok(p) => p,
                    Err(e) => return eprintln!("app-windows: placing a popup: {e}"),
                };
                let parent_surface = match &parent.role {
                    Role::Toplevel(w) => w.xdg_surface().clone(),
                    Role::Popup(p) => p.xdg_surface().clone(),
                };
                match Popup::from_surface(
                    Some(&parent_surface),
                    &positioner,
                    &qh,
                    surface,
                    &self.xdg,
                ) {
                    Ok(popup) => {
                        popup.wl_surface().commit();
                        Role::Popup(popup)
                    }
                    Err(e) => return eprintln!("app-windows: making a popup: {e}"),
                }
            }
            // A panel with nothing to belong to stands on its own.
            _ => {
                // The guest draws its own title bar.
                let window = self
                    .xdg
                    .create_window(surface, WindowDecorations::RequestClient, &qh);
                window.set_title(info.title.clone());
                window.set_app_id(entries::app_id(&self.options.prefix, &info.app));
                if let Some(Shown {
                    role: Role::Toplevel(parent),
                    ..
                }) = parent
                {
                    window.set_parent(Some(parent));
                }
                window.commit();
                Role::Toplevel(window)
            }
        };
        let size = rounded(info.width, info.height);
        let (size, stretched) = match &role {
            Role::Popup(_) => {
                let (sx, sy) = parent.map(Shown::stretch).unwrap_or((1.0, 1.0));
                (rounded(info.width * sx, info.height * sy), true)
            }
            Role::Toplevel(_) => (size, false),
        };
        self.windows.insert(
            info.window,
            Shown {
                info,
                role,
                viewport,
                configured: false,
                size,
                sized_here: stretched,
                asked: None,
                buffers: Vec::new(),
                last: None,
                resized: false,
                placing: None,
                placed: false,
                drawing: false,
            },
        );
    }

    /// Where a popup goes: at its offset from its parent, in the parent's
    /// pixels here, and slid back onto the screen if that is off it.
    fn positioner(&self, info: &WindowInfo, parent: &Shown) -> Result<XdgPositioner, String> {
        let positioner = XdgPositioner::new(&self.xdg).map_err(|e| e.to_string())?;
        let (sx, sy) = parent.stretch();
        let (w, h) = rounded(info.width * sx, info.height * sy);
        let (_, ox, oy) = parent.fit();
        let x = ((info.x - parent.info.x) * sx + ox).round() as i32;
        let y = ((info.y - parent.info.y) * sy + oy).round() as i32;
        // The anchor has to lie on the parent; whatever of the offset does
        // not is added after.
        let ax = x.clamp(0, parent.size.0 as i32 - 1);
        let ay = y.clamp(0, parent.size.1 as i32 - 1);
        positioner.set_size(w as i32, h as i32);
        positioner.set_anchor_rect(ax, ay, 1, 1);
        positioner.set_anchor(Anchor::TopLeft);
        positioner.set_gravity(Gravity::BottomRight);
        positioner.set_offset(x - ax, y - ay);
        positioner
            .set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY);
        Ok(positioner)
    }

    fn changed(&mut self, info: WindowInfo) {
        let Some(shown) = self.windows.get_mut(&info.window) else {
            return;
        };
        let old = std::mem::replace(&mut shown.info, info.clone());
        if let Role::Toplevel(window) = &shown.role {
            if old.title != info.title {
                window.set_title(info.title.clone());
            }
            let resized =
                (old.width - info.width).abs() > 0.5 || (old.height - info.height).abs() > 0.5;
            let moved = (old.x - info.x).abs() > 0.5 || (old.y - info.y).abs() > 0.5;
            if resized && !shown.sized_here {
                shown.size = rounded(info.width, info.height);
            }
            // The guest took the tile's size: the window goes back to it.
            if let Some((w, h)) = shown.placing {
                if (info.width - f64::from(w)).abs() <= 1.0
                    && (info.height - f64::from(h)).abs() <= 1.0
                {
                    shown.placing = None;
                    if let Some(bridge) = &self.bridge {
                        bridge.place(Place {
                            window: info.window,
                            app_id: entries::app_id(&self.options.prefix, &info.app),
                            title: info.title.clone(),
                            tile: true,
                        });
                    }
                }
            }
            // The title bar is being dragged on the guest: from here on the
            // desktop moves the window, and the guest is let go of.
            if moved && !resized {
                if let (Some(press), Some(seat)) = (self.press, &self.seat) {
                    if press.window == info.window {
                        self.link.send(Message::Button(Button {
                            window: info.window,
                            button: 0,
                            pressed: false,
                            x: press.x,
                            y: press.y,
                        }));
                        window.move_(seat, press.serial);
                        self.press = None;
                        self.swallow_release = true;
                    }
                }
            }
        } else if matches!(shown.role, Role::Popup(_)) {
            let Some(parent) = info.parent.and_then(|p| self.windows.get(&p)) else {
                return;
            };
            let (sx, sy) = parent.stretch();
            let size = rounded(info.width * sx, info.height * sy);
            let positioner = self.positioner(&info, parent).ok();
            let Some(shown) = self.windows.get_mut(&info.window) else {
                return;
            };
            shown.size = size;
            if let (Role::Popup(popup), Some(positioner)) = (&shown.role, positioner) {
                popup.reposition(&positioner, 0);
            }
        }
    }

    fn close(&mut self, window: u32) {
        self.dismissed.remove(&window);
        // Popups go before what they belong to, as Wayland wants.
        let children: Vec<u32> = self
            .windows
            .values()
            .filter(|s| s.info.parent == Some(window) && s.info.window != window)
            .map(|s| s.info.window)
            .collect();
        for child in children {
            if matches!(
                self.windows.get(&child).map(|s| &s.role),
                Some(Role::Popup(_))
            ) {
                self.close(child);
            }
        }
        if let Some(shown) = self.windows.remove(&window) {
            if let Some(viewport) = &shown.viewport {
                viewport.destroy();
            }
        }
        self.frames.take(window);
        if self.focus.is_some_and(|(w, _)| w == window) {
            self.focus = None;
        }
        if self.under_pointer == Some(window) {
            self.under_pointer = None;
        }
    }

    fn by_surface(&self, surface: &WlSurface) -> Option<u32> {
        self.windows
            .values()
            .find(|s| s.surface() == surface)
            .map(|s| s.info.window)
    }

    fn draw(&mut self, window: u32) {
        let Some(shown) = self.windows.get_mut(&window) else {
            return;
        };
        if !shown.configured || shown.drawing {
            // The picture waits in its mailbox for the frame callback.
            return;
        }
        let picture = match self.frames.take(window) {
            Some(picture) => picture,
            None if shown.resized && shown.last.is_some() => shown.last.take().unwrap(),
            None => return,
        };
        shown.resized = false;
        // With bars beside the picture, the buffer is the surface's shape
        // in the picture's pixels, black but for the picture in its middle.
        let (pw, ph) = (picture.width as usize, picture.height as usize);
        let (scale, ox, oy) = shown.fit();
        let per_point = pw as f64 / (shown.info.width.max(1.0) * scale);
        let (bw, bh, bx, by) = if shown.viewport.is_some() && (ox >= 1.0 || oy >= 1.0) {
            let bw = ((f64::from(shown.size.0) * per_point).round() as usize).max(pw);
            let bh = ((f64::from(shown.size.1) * per_point).round() as usize).max(ph);
            (bw, bh, (bw - pw) / 2, (bh - ph) / 2)
        } else {
            (pw, ph, 0, 0)
        };
        let barred = (bw, bh) != (pw, ph);
        let (width, height) = (bw as i32, bh as i32);
        let stride = if barred {
            (bw * 4) as i32
        } else {
            picture.stride as i32
        };
        let pool = &mut self.pool;
        // A buffer the compositor is done with, of this size, or a new one;
        // buffers of an old size go.
        shown
            .buffers
            .retain(|b| b.height() == height && b.stride() == stride);
        let free = shown.buffers.iter().position(|b| b.canvas(pool).is_some());
        let index = match free {
            Some(i) => i,
            None => match pool.create_buffer(width, height, stride, wl_shm::Format::Xrgb8888) {
                Ok((buffer, _)) => {
                    shown.buffers.push(buffer);
                    shown.buffers.len() - 1
                }
                Err(e) => return eprintln!("app-windows: no memory for a picture: {e}"),
            },
        };
        let buffer = &shown.buffers[index];
        let Some(canvas) = buffer.canvas(pool) else {
            return;
        };
        if barred {
            canvas.fill(0);
            let row = pw * 4;
            for (y, line) in picture.pixels.chunks(picture.stride).take(ph).enumerate() {
                let at = (by + y) * bw * 4 + bx * 4;
                canvas[at..at + row].copy_from_slice(&line[..row]);
            }
        } else {
            let n = canvas.len().min(picture.pixels.len());
            canvas[..n].copy_from_slice(&picture.pixels[..n]);
        }

        let surface = shown.surface().clone();
        if buffer.attach_to(&surface).is_err() {
            return;
        }
        surface.damage_buffer(0, 0, width, height);
        match &shown.viewport {
            Some(viewport) => {
                viewport.set_source(0.0, 0.0, f64::from(width), f64::from(height));
                viewport.set_destination(shown.size.0 as i32, shown.size.1 as i32);
            }
            // Without a viewport, the nearest whole scale.
            None => surface.set_buffer_scale(shown.info.scale.round().max(1.0) as i32),
        }
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        surface.commit();
        shown.drawing = true;
        shown.last = Some(picture);
    }

    /// The guest's pointer changed look.
    fn cursor_changed(&mut self, cursor: Cursor) {
        self.cursor_picture = cursor.image.as_ref().and_then(|image| {
            let pixels = crate::cursor::pixels(&image.png)
                .map_err(|e| eprintln!("app-windows: the guest's pointer: {e}"))
                .ok()?;
            let (width, height) = (pixels.width as i32, pixels.height as i32);
            let (buffer, canvas) = self
                .pool
                .create_buffer(width, height, width * 4, wl_shm::Format::Argb8888)
                .map_err(|e| eprintln!("app-windows: no memory for the guest's pointer: {e}"))
                .ok()?;
            canvas[..pixels.argb.len()].copy_from_slice(&pixels.argb);
            let scale = image.scale.clamp(1, 4) as i32;
            Some(CursorPicture {
                buffer,
                width,
                height,
                scale,
                hotspot: (
                    image.hotspot_x as i32 / scale,
                    image.hotspot_y as i32 / scale,
                ),
            })
        });
        self.cursor = cursor;
        self.show_cursor();
    }

    /// Shows the guest's pointer, while this desktop's is over one of its
    /// windows. A look this desktop's theme has no name for, or a picture
    /// that would not decode, shows as the arrow.
    fn show_cursor(&self) {
        let (Some(pointer), Some(_)) = (&self.pointer, self.under_pointer) else {
            return;
        };
        if let (Some(picture), Some(serial)) = (&self.cursor_picture, self.entered) {
            let surface = pointer.surface();
            // A picture already on the surface, from the last time the
            // pointer was here, is still busy there and shows as it is.
            if picture.buffer.attach_to(surface).is_ok() {
                surface.set_buffer_scale(picture.scale);
                surface.damage_buffer(0, 0, picture.width, picture.height);
                surface.commit();
            }
            pointer.pointer().set_cursor(
                serial,
                Some(surface),
                picture.hotspot.0,
                picture.hotspot.1,
            );
            return;
        }
        let icon = self.cursor.shape.parse().unwrap_or(CursorIcon::Default);
        let _ = pointer.set_cursor(&self.conn, icon);
    }

    fn send_pointer(&self, window: u32, x: f64, y: f64) {
        if let Some(shown) = self.windows.get(&window) {
            let (x, y) = shown.to_guest(x, y);
            self.link.send(Message::Pointer(Pointer { window, x, y }));
        }
    }

    /// Lets go of every key held into a window that is losing the keyboard:
    /// their releases will go wherever the keyboard goes next.
    fn release_keys(&mut self) {
        let Some((window, _)) = self.focus else {
            return;
        };
        for code in self.held_keys.drain() {
            self.link.send(Message::Key(Key {
                window,
                code,
                pressed: false,
                modifiers: 0,
                text: None,
            }));
        }
    }

    fn key(&mut self, event: &KeyEvent, pressed: bool) {
        let Some((window, _)) = self.focus else {
            return;
        };
        if pressed {
            self.held_keys.insert(event.raw_code);
        } else {
            self.held_keys.remove(&event.raw_code);
        }
        self.link.send(Message::Key(Key {
            window,
            code: event.raw_code,
            pressed,
            modifiers: self.modifiers,
            text: if pressed { event.utf8.clone() } else { None },
        }));
    }

    /// Puts the guest's clipboard on this desktop's, once a window here has
    /// the keyboard to take it with.
    fn offer_clipboard(&mut self) {
        let (Some(data), Some(device), Some((_, serial))) = (&self.data, &self.device, self.focus)
        else {
            return;
        };
        if !self.offer_pending {
            return;
        }
        let source = data.create_copy_paste_source(&self.qh, TEXT.to_vec());
        source.set_selection(device, serial);
        self.source = Some(source);
        self.offer_pending = false;
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        let Some(window) = self.by_surface(surface) else {
            return;
        };
        if let Some(shown) = self.windows.get_mut(&window) {
            shown.drawing = false;
        }
        self.draw(window);
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        // The guest decides: an unsaved document asks first, as it would
        // on the guest's own screen.
        if let Some(id) = self.by_surface(window.wl_surface()) {
            self.link.send(Message::Close(WindowRef { window: id }));
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let Some(id) = self.by_surface(window.wl_surface()) else {
            return;
        };
        let Some(shown) = self.windows.get_mut(&id) else {
            return;
        };
        shown.configured = true;
        if let (Some(w), Some(h)) = configure.new_size {
            let size = (w.get(), h.get());
            let first = !std::mem::replace(&mut shown.placed, true);
            if size != shown.size {
                shown.size = size;
                shown.resized = true;
                let tiled =
                    configure.is_tiled() && !configure.is_maximized() && !configure.is_fullscreen();
                shown.sized_here = tiled || configure.is_maximized() || configure.is_fullscreen();
                // A new window that is not the tile's size floats, at its
                // own size as near as derisk lets it, while the guest is
                // asked for the tile's; changed() tiles it again once the
                // guest has it. Meanwhile derisk's floating size is not
                // asked of the guest.
                let fits = (shown.info.width - f64::from(size.0)).abs() <= 1.0
                    && (shown.info.height - f64::from(size.1)).abs() <= 1.0;
                if first && tiled && !fits {
                    if let Some(bridge) = &self.bridge {
                        shown.placing = Some(size);
                        bridge.place(Place {
                            window: id,
                            app_id: entries::app_id(&self.options.prefix, &shown.info.app),
                            title: shown.info.title.clone(),
                            tile: false,
                        });
                    }
                }
                if shown.placing.is_some_and(|target| target != size) {
                    // Floating while the guest resizes: drawn at the guest's
                    // own size rather than derisk's floating frame, which a
                    // floating window may differ from, so it grows to the
                    // tile's size as the guest does and has no bars meanwhile.
                    shown.size = rounded(shown.info.width, shown.info.height);
                } else if shown.asked != Some(size) {
                    shown.asked = Some(size);
                    let (width, height) = (f64::from(size.0), f64::from(size.1));
                    self.link.send(Message::Resize(Resize {
                        window: id,
                        width,
                        height,
                    }));
                }
            }
        } else {
            shown.sized_here = false;
        }
        shown.drawing = false;
        self.draw(id);
    }
}

impl PopupHandler for App {
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        popup: &Popup,
        _: PopupConfigure,
    ) {
        let Some(id) = self.by_surface(popup.wl_surface()) else {
            return;
        };
        if let Some(shown) = self.windows.get_mut(&id) {
            shown.configured = true;
            shown.drawing = false;
        }
        self.draw(id);
    }

    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        // Dismissed here, by a click outside it: closed on the guest too.
        let Some(id) = self.by_surface(popup.wl_surface()) else {
            return;
        };
        self.windows.remove(&id);
        self.dismissed.insert(id);
        self.link.send(Message::Close(WindowRef { window: id }));
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        if self.seat.is_none() {
            self.device = self.data.as_ref().map(|d| d.get_data_device(qh, &seat));
            self.seat = Some(seat);
        }
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            // This desktop's own repeat rate and delay, with each repeat
            // sent as a press again, which the guest marks as a repeat.
            let repeat =
                Box::new(|app: &mut App, _: &WlKeyboard, event: KeyEvent| app.key(&event, true));
            self.keyboard = self
                .seats
                .get_keyboard_with_repeat(qh, &seat, None, self.handle.clone(), repeat)
                .ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let cursor = self.compositor.create_surface(qh);
            self.pointer = self
                .seats
                .get_pointer_with_theme::<App, ()>(
                    qh,
                    &seat,
                    self.shm.wl_shm(),
                    cursor,
                    ThemeSpec::default(),
                )
                .ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.pointer().release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        serial: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        let Some(window) = self.by_surface(surface) else {
            return;
        };
        self.focus = Some((window, serial));
        self.link.send(Message::Focus(WindowRef { window }));
        self.offer_clipboard();
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
    ) {
        let Some(window) = self.by_surface(surface) else {
            return;
        };
        self.release_keys();
        // A menu left open on the guest would stay over a window here that
        // the person has moved on from.
        let popups: Vec<u32> = self
            .windows
            .values()
            .filter(|s| s.info.parent == Some(window) && matches!(s.role, Role::Popup(_)))
            .map(|s| s.info.window)
            .collect();
        for popup in popups {
            self.link.send(Message::Close(WindowRef { window: popup }));
        }
        self.focus = None;
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, true);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, false);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        m: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = [
            (m.shift, modifiers::SHIFT),
            (m.ctrl, modifiers::CONTROL),
            (m.alt, modifiers::ALT),
            (m.logo, modifiers::SUPER),
            (m.caps_lock, modifiers::CAPS_LOCK),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .fold(0, |bits, (_, bit)| bits | bit);
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(window) = self.by_surface(&event.surface) else {
                continue;
            };
            let (x, y) = event.position;
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.under_pointer = Some(window);
                    self.entered = Some(serial);
                    self.show_cursor();
                    self.send_pointer(window, x, y);
                }
                PointerEventKind::Leave { .. } => self.under_pointer = None,
                PointerEventKind::Motion { .. } => self.send_pointer(window, x, y),
                PointerEventKind::Press { button, serial, .. }
                | PointerEventKind::Release { button, serial, .. } => {
                    let pressed = matches!(event.kind, PointerEventKind::Press { .. });
                    let button = match button {
                        BTN_LEFT => 0,
                        BTN_RIGHT => 1,
                        BTN_MIDDLE => 2,
                        _ => continue,
                    };
                    let Some(shown) = self.windows.get(&window) else {
                        continue;
                    };
                    let (gx, gy) = shown.to_guest(x, y);
                    if button == 0 {
                        if pressed {
                            self.press = Some(Press {
                                window,
                                serial,
                                x: gx,
                                y: gy,
                            });
                            self.swallow_release = false;
                        } else {
                            self.press = None;
                            if std::mem::take(&mut self.swallow_release) {
                                continue;
                            }
                        }
                    }
                    self.link.send(Message::Button(Button {
                        window,
                        button,
                        pressed,
                        x: gx,
                        y: gy,
                    }));
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    let Some(shown) = self.windows.get(&window) else {
                        continue;
                    };
                    let (gx, gy) = shown.to_guest(x, y);
                    // A wheel's notch is 15 pixels where only steps are
                    // given.
                    let amount = |a: &smithay_client_toolkit::seat::pointer::AxisScroll| {
                        if a.absolute != 0.0 {
                            a.absolute
                        } else {
                            f64::from(a.discrete) * 15.0
                        }
                    };
                    let (dx, dy) = (amount(&horizontal), amount(&vertical));
                    if dx != 0.0 || dy != 0.0 {
                        self.link.send(Message::Scroll(Scroll {
                            window,
                            x: gx,
                            y: gy,
                            dx,
                            dy,
                        }));
                    }
                }
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(offer) = self
            .device
            .as_ref()
            .and_then(|d| d.data().selection_offer())
        else {
            return;
        };
        let Some(mime) = offer.with_mime_types(|types| {
            TEXT.iter()
                .find(|t| types.iter().any(|m| m == *t))
                .map(|t| t.to_string())
        }) else {
            return;
        };
        let Ok(mut pipe) = offer.receive(mime) else {
            return;
        };
        let (clipboard, link) = (self.clipboard.clone(), self.link.clone());
        // Read off this thread: the offer may be this program's own, whose
        // text this thread has to write.
        std::thread::spawn(move || {
            let mut text = String::new();
            if pipe.read_to_string(&mut text).is_err() {
                return;
            }
            let mut last = clipboard.lock().unwrap();
            if last.as_deref() != Some(text.as_str()) {
                *last = Some(text.clone());
                link.send(Message::Clipboard(Clipboard { text }));
            }
        });
    }
}

impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        mut pipe: WritePipe,
    ) {
        let text = self.clipboard.lock().unwrap().clone().unwrap_or_default();
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self.source.as_ref().is_some_and(|s| s.inner() == source) {
            self.source = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

smithay_client_toolkit::reexports::client::delegate_noop!(App: WpViewporter);

impl AsMut<SimpleGlobal<WpViewporter, 1>> for App {
    fn as_mut(&mut self) -> &mut SimpleGlobal<WpViewporter, 1> {
        &mut self.viewporter
    }
}

impl Dispatch<WpViewport, ()> for App {
    fn event(
        _: &mut App,
        _: &WpViewport,
        _: wp_viewport::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);
