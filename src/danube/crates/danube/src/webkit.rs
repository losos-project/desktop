//! WebKit's and WPE's objects as Rust types that own them: each holds one
//! reference, takes another when cloned and gives it back when dropped,
//! and offers the calls Danube makes as methods on `&self`. Signals take
//! closures, which GObject frees with the connection, and hand them the
//! emitting object borrowed. The `unsafe` of ffi.rs ends here: engine.rs
//! never sees a pointer.
//!
//! Every object belongs to the thread that made it, which is the GLib main
//! loop's; none of these types is `Send`, so the compiler keeps it there.

use std::ffi::{c_char, CStr, CString};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::ptr::{self, NonNull};

use crate::ffi::*;

/// A GObject, held by one reference.
macro_rules! object {
    ($(#[$doc:meta])* $name:ident($raw:ty)) => {
        $(#[$doc])*
        pub struct $name {
            ptr: NonNull<$raw>,
            /// Pins the object to its thread: a raw pointer is neither
            /// `Send` nor `Sync`.
            _thread: PhantomData<*mut ()>,
        }

        #[allow(dead_code)]
        impl $name {
            /// Takes over a reference WebKit handed out (transfer full).
            unsafe fn owned(ptr: *mut $raw) -> Option<Self> {
                NonNull::new(ptr).map(|ptr| Self {
                    ptr,
                    _thread: PhantomData,
                })
            }

            /// Adds a reference to an object WebKit keeps (transfer none).
            unsafe fn borrowed(ptr: *mut $raw) -> Option<Self> {
                let object = unsafe { Self::owned(ptr) }?;
                unsafe { g_object_ref(ptr.cast()) };
                Some(object)
            }

            /// An object a signal is passing, for the handler's duration:
            /// no reference taken, none given back.
            unsafe fn passing(ptr: *mut $raw) -> ManuallyDrop<Self> {
                ManuallyDrop::new(unsafe { Self::owned(ptr) }.expect("a signal never passes NULL"))
            }

            fn as_ptr(&self) -> *mut $raw {
                self.ptr.as_ptr()
            }

            /// Connects `f` to `signal` through `trampoline`, a C function
            /// of the signal's signature that calls `f` from its data.
            fn connect<F: 'static>(&self, signal: &str, trampoline: *const (), f: F) {
                unsafe { connect(self.as_ptr().cast(), signal, trampoline, f) }
            }
        }

        impl Clone for $name {
            fn clone(&self) -> Self {
                unsafe { Self::borrowed(self.as_ptr()) }.expect("a held object is not NULL")
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                unsafe { g_object_unref(self.as_ptr().cast()) }
            }
        }
    };
}

/// A reference-counted type of WPE's or WebKit's that is not a GObject.
macro_rules! counted {
    ($(#[$doc:meta])* $name:ident($raw:ty, $ref:ident, $unref:ident)) => {
        $(#[$doc])*
        pub struct $name {
            ptr: NonNull<$raw>,
            _thread: PhantomData<*mut ()>,
        }

        impl $name {
            unsafe fn owned(ptr: *mut $raw) -> Option<Self> {
                NonNull::new(ptr).map(|ptr| Self {
                    ptr,
                    _thread: PhantomData,
                })
            }

            fn as_ptr(&self) -> *mut $raw {
                self.ptr.as_ptr()
            }
        }

        impl Clone for $name {
            fn clone(&self) -> Self {
                unsafe { Self::owned($ref(self.as_ptr())) }.expect("ref returns its argument")
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                unsafe { $unref(self.as_ptr()) }
            }
        }
    };
}

/// Connects `f` to `signal` on `instance`. `f` is boxed as the handler's
/// data and freed by GObject when the signal is disconnected, which is
/// when the object dies.
///
/// # Safety
/// `trampoline` must be an `unsafe extern "C"` function of the signal's
/// signature that reads its data as `*const F`, and `instance` a live
/// GObject.
unsafe fn connect<F: 'static>(instance: gpointer, signal: &str, trampoline: *const (), f: F) {
    unsafe extern "C" fn free<F>(data: gpointer, _: *mut GClosure) {
        drop(unsafe { Box::from_raw(data as *mut F) });
    }
    let signal = CString::new(signal).expect("signal names have no NUL");
    let data = Box::into_raw(Box::new(f)) as gpointer;
    // SAFETY: a plain function pointer as the GCallback GObject stores;
    // GObject calls it back with the signal's own signature.
    let handler: GCallback =
        Some(unsafe { std::mem::transmute::<*const (), unsafe extern "C" fn()>(trampoline) });
    unsafe { g_signal_connect_data(instance, signal.as_ptr(), handler, data, Some(free::<F>), 0) };
}

/// A borrowed C string as an owned Rust one; `None` for NULL.
unsafe fn string(s: *const c_char) -> Option<String> {
    if s.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned())
    }
}

/// A Rust string as a C one, with any NUL cut off rather than failing: a
/// URL or title never legitimately has one.
fn cstring(s: &str) -> CString {
    let s = s.split('\0').next().unwrap_or_default();
    CString::new(s).expect("NUL removed")
}

fn cpath(path: &Path) -> CString {
    cstring(&path.to_string_lossy())
}

/// Takes a GError's message and frees it.
unsafe fn take_error(error: *mut GError) -> String {
    if error.is_null() {
        return "unknown error".into();
    }
    let message = unsafe { string((*error).message) }.unwrap_or_default();
    unsafe { g_error_free(error) };
    message
}

// GLib's TRUE, FALSE and G_PRIORITY_DEFAULT are macros bindgen leaves
// out; their values have been these since GLib 1.
const FALSE: gboolean = 0;
const G_PRIORITY_DEFAULT: std::ffi::c_int = 0;

fn gbool(b: bool) -> gboolean {
    b as gboolean
}

// GLib's main loop and its sources.

counted!(
    /// The thread's main loop; WebKit runs inside it.
    MainLoop(GMainLoop, g_main_loop_ref, g_main_loop_unref)
);

impl MainLoop {
    pub fn new() -> Self {
        unsafe { Self::owned(g_main_loop_new(ptr::null_mut(), FALSE)) }.expect("GLib allocates or aborts")
    }

    pub fn run(&self) {
        unsafe { g_main_loop_run(self.as_ptr()) }
    }

    pub fn quit(&self) {
        unsafe { g_main_loop_quit(self.as_ptr()) }
    }
}

unsafe extern "C" fn source<F: FnMut() -> bool>(data: gpointer) -> gboolean {
    gbool(unsafe { (*(data as *mut F))() })
}

unsafe extern "C" fn free_source<F>(data: gpointer) {
    drop(unsafe { Box::from_raw(data as *mut F) });
}

/// Runs `f` on the main loop's thread as soon as it is idle, and again
/// while it returns `true`. Safe from any thread: this is how GLib takes
/// work from others.
pub fn idle_add<F: FnMut() -> bool + Send + 'static>(f: F) {
    let data = Box::into_raw(Box::new(f)) as gpointer;
    unsafe {
        g_idle_add_full(
            G_PRIORITY_DEFAULT,
            Some(source::<F>),
            data,
            Some(free_source::<F>),
        )
    };
}

/// Runs `f` every `millis` while it returns `true`; the source's id, for
/// [`source_remove`].
pub fn timeout_add<F: FnMut() -> bool + 'static>(millis: u32, f: F) -> u32 {
    let data = Box::into_raw(Box::new(f)) as gpointer;
    unsafe {
        g_timeout_add_full(
            G_PRIORITY_DEFAULT,
            millis,
            Some(source::<F>),
            data,
            Some(free_source::<F>),
        )
    }
}

/// Stops a source [`timeout_add`] made, before it returned `false`.
pub fn source_remove(id: u32) {
    unsafe { g_source_remove(id) };
}

// WPEPlatform.

object!(
    /// WPE's headless display: views render to buffers, not a screen.
    Display(WPEDisplay)
);

impl Display {
    /// A connected headless display, made the primary one.
    pub fn headless() -> Result<Self, String> {
        unsafe {
            let display = Self::owned(wpe_display_headless_new()).expect("WPE allocates or aborts");
            let mut error = ptr::null_mut();
            if wpe_display_connect(display.as_ptr(), &mut error) == FALSE {
                return Err(take_error(error));
            }
            wpe_display_set_primary(display.as_ptr());
            Ok(display)
        }
    }

    pub fn clipboard(&self) -> Clipboard {
        unsafe { Clipboard::borrowed(wpe_display_get_clipboard(self.as_ptr())) }
            .expect("a display has a clipboard")
    }

    /// The hardware keycode the keymap has for `keyval`, which pages see
    /// as `KeyboardEvent.code`; 0 when it has none.
    pub fn keycode(&self, keyval: u32) -> u32 {
        unsafe {
            let keymap = wpe_display_get_keymap(self.as_ptr());
            if keymap.is_null() {
                return 0;
            }
            let mut entries = ptr::null_mut();
            let mut n = 0;
            // A TRUE return fills `entries` with `n` of them.
            if wpe_keymap_get_entries_for_keyval(keymap, keyval, &mut entries, &mut n) == FALSE
                || n == 0
            {
                return 0;
            }
            // `as_ref` is `None` for a keymap that says TRUE and hands back
            // no array, so the read never touches a null pointer.
            let code = entries.as_ref().map_or(0, |entry| entry.keycode);
            g_free(entries.cast());
            code
        }
    }
}

object!(
    /// The display's clipboard, which pages copy to and paste from.
    Clipboard(WPEClipboard)
);

impl Clipboard {
    pub fn set_text(&self, text: &str) {
        unsafe {
            let content = wpe_clipboard_content_new();
            let text = cstring(text);
            wpe_clipboard_content_set_text(content, text.as_ptr());
            wpe_clipboard_set_content(self.as_ptr(), content);
            wpe_clipboard_content_unref(content);
        }
    }

    /// The text on it, when it holds any that is not a page's own.
    pub fn text(&self) -> Option<String> {
        unsafe {
            let content = wpe_clipboard_get_content(self.as_ptr());
            if content.is_null() {
                return None;
            }
            string(wpe_clipboard_content_get_text(content))
        }
    }

    /// `notify::change-count`: something was put on the clipboard.
    pub fn on_change<F: Fn(&Clipboard) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&Clipboard)>(
            clipboard: *mut WPEClipboard,
            _: *mut GParamSpec,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&Clipboard::passing(clipboard)) }
        }
        self.connect("notify::change-count", trampoline::<F> as *const (), f);
    }
}

object!(
    /// A WebKitWebView's rendering surface and input target.
    View(WPEView)
);

impl View {
    pub fn set_visible(&self, visible: bool) {
        unsafe { wpe_view_set_visible(self.as_ptr(), gbool(visible)) }
    }

    pub fn set_focused(&self, focused: bool) {
        unsafe {
            if focused {
                wpe_view_focus_in(self.as_ptr())
            } else {
                wpe_view_focus_out(self.as_ptr())
            }
        }
    }

    /// The toplevel the view is in, once WebKit has given it one.
    pub fn toplevel(&self) -> Option<Toplevel> {
        unsafe { Toplevel::borrowed(wpe_view_get_toplevel(self.as_ptr())) }
    }

    pub fn send(&self, event: &Event) {
        unsafe { wpe_view_event(self.as_ptr(), event.as_ptr()) }
    }

    /// How many clicks a press at `x`, `y` with `button` at `time` makes
    /// in a row: 1, 2 for a double click, and so on.
    pub fn press_count(&self, x: f64, y: f64, button: u32, time: u32) -> u32 {
        unsafe { wpe_view_compute_press_count(self.as_ptr(), x, y, button, time) }
    }

    /// `buffer-rendered`: a new frame.
    pub fn on_buffer_rendered<F: Fn(&View, &Buffer) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&View, &Buffer)>(
            view: *mut WPEView,
            buffer: *mut WPEBuffer,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&View::passing(view), &Buffer::passing(buffer)) }
        }
        self.connect("buffer-rendered", trampoline::<F> as *const (), f);
    }
}

object!(
    /// The headless toplevel a view is in: its size, scale and state.
    Toplevel(WPEToplevel)
);

impl Toplevel {
    pub fn resize(&self, width: i32, height: i32) {
        unsafe { wpe_toplevel_resize(self.as_ptr(), width, height) };
    }

    pub fn set_scale(&self, scale: f64) {
        unsafe { wpe_toplevel_scale_changed(self.as_ptr(), scale) }
    }

    /// Active (the keyboard's) or not.
    pub fn set_active(&self, active: bool) {
        let state = if active { WPE_TOPLEVEL_STATE_ACTIVE } else { 0 };
        unsafe { wpe_toplevel_state_changed(self.as_ptr(), state) }
    }
}

object!(
    /// A rendered frame.
    Buffer(WPEBuffer)
);

impl Buffer {
    pub fn width(&self) -> usize {
        unsafe { wpe_buffer_get_width(self.as_ptr()) }.max(0) as usize
    }

    pub fn height(&self) -> usize {
        unsafe { wpe_buffer_get_height(self.as_ptr()) }.max(0) as usize
    }

    /// The frame's pixels, ARGB8888 rows that may be padded; they belong
    /// to the buffer.
    pub fn pixels(&self) -> Result<&[u8], String> {
        unsafe {
            let mut error = ptr::null_mut();
            let bytes = wpe_buffer_import_to_pixels(self.as_ptr(), &mut error);
            if bytes.is_null() {
                return Err(take_error(error));
            }
            let mut len = 0;
            let data = g_bytes_get_data(bytes, &mut len) as *const u8;
            if data.is_null() {
                return Ok(&[]);
            }
            Ok(std::slice::from_raw_parts(data, len as usize))
        }
    }
}

counted!(
    /// An input event for a view.
    Event(WPEEvent, wpe_event_ref, wpe_event_unref)
);

impl Event {
    fn new(ptr: *mut WPEEvent) -> Self {
        unsafe { Self::owned(ptr) }.expect("WPE allocates or aborts")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn pointer_move(
        kind: WPEEventType,
        view: &View,
        source: WPEInputSource,
        time: u32,
        modifiers: WPEModifiers,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    ) -> Self {
        Self::new(unsafe {
            wpe_event_pointer_move_new(kind, view.as_ptr(), source, time, modifiers, x, y, dx, dy)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn pointer_button(
        kind: WPEEventType,
        view: &View,
        source: WPEInputSource,
        time: u32,
        modifiers: WPEModifiers,
        button: u32,
        x: f64,
        y: f64,
        press_count: u32,
    ) -> Self {
        Self::new(unsafe {
            wpe_event_pointer_button_new(
                kind,
                view.as_ptr(),
                source,
                time,
                modifiers,
                button,
                x,
                y,
                press_count,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn scroll(
        view: &View,
        source: WPEInputSource,
        time: u32,
        modifiers: WPEModifiers,
        dx: f64,
        dy: f64,
        precise: bool,
        x: f64,
        y: f64,
    ) -> Self {
        Self::new(unsafe {
            wpe_event_scroll_new(
                view.as_ptr(),
                source,
                time,
                modifiers,
                dx,
                dy,
                gbool(precise),
                FALSE,
                x,
                y,
            )
        })
    }

    pub fn keyboard(
        kind: WPEEventType,
        view: &View,
        time: u32,
        modifiers: WPEModifiers,
        keycode: u32,
        keyval: u32,
    ) -> Self {
        Self::new(unsafe {
            wpe_event_keyboard_new(
                kind,
                view.as_ptr(),
                WPE_INPUT_SOURCE_KEYBOARD,
                time,
                modifiers,
                keycode,
                keyval,
            )
        })
    }

    pub fn touch(
        kind: WPEEventType,
        view: &View,
        time: u32,
        modifiers: WPEModifiers,
        sequence: u32,
        x: f64,
        y: f64,
    ) -> Self {
        Self::new(unsafe {
            wpe_event_touch_new(
                kind,
                view.as_ptr(),
                WPE_INPUT_SOURCE_TOUCHSCREEN,
                time,
                modifiers,
                sequence,
                x,
                y,
            )
        })
    }
}

// WPE WebKit.

object!(
    /// Cookies, cache and credentials, shared by every view opened on it.
    NetworkSession(WebKitNetworkSession)
);

impl NetworkSession {
    /// A session whose data outlives the process, in `data` and `cache`.
    pub fn persistent(data: &Path, cache: &Path) -> Self {
        let (data, cache) = (cpath(data), cpath(cache));
        unsafe { Self::owned(webkit_network_session_new(data.as_ptr(), cache.as_ptr())) }
            .expect("WebKit allocates or aborts")
    }

    /// A session that forgets everything with the process.
    pub fn ephemeral() -> Self {
        unsafe { Self::owned(webkit_network_session_new_ephemeral()) }
            .expect("WebKit allocates or aborts")
    }

    /// Intelligent Tracking Prevention: third-party cookies and storage
    /// cut off for sites that only ever appear as trackers.
    pub fn set_itp_enabled(&self, enabled: bool) {
        unsafe { webkit_network_session_set_itp_enabled(self.as_ptr(), gbool(enabled)) }
    }

    pub fn set_persistent_credential_storage_enabled(&self, enabled: bool) {
        unsafe {
            webkit_network_session_set_persistent_credential_storage_enabled(
                self.as_ptr(),
                gbool(enabled),
            )
        }
    }

    /// `download-started`.
    pub fn on_download_started<F: Fn(&NetworkSession, &Download) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&NetworkSession, &Download)>(
            session: *mut WebKitNetworkSession,
            download: *mut WebKitDownload,
            data: gpointer,
        ) {
            unsafe {
                (*(data as *const F))(
                    &NetworkSession::passing(session),
                    &Download::passing(download),
                )
            }
        }
        self.connect("download-started", trampoline::<F> as *const (), f);
    }
}

object!(
    /// What pages may do, shared by every view.
    Settings(WebKitSettings)
);

impl Settings {
    /// Settings with WebKit's developer tools and local-file loopholes
    /// shut: no inspector, no file:// page reading other files or any
    /// origin, no top-level navigation to a data: URL. Those are WebKit's
    /// defaults too, set here so a release that changed one does not
    /// change Danube. `popups` lets a page's script open windows without
    /// a click, and `console` writes the console to stdout, which with no
    /// developer tools is the only place it is seen.
    pub fn new(popups: bool, console: bool) -> Self {
        let settings = unsafe {
            g_object_new(
                webkit_settings_get_type(),
                c"enable-developer-extras".as_ptr(),
                FALSE,
                c"enable-write-console-messages-to-stdout".as_ptr(),
                gbool(console),
                c"javascript-can-open-windows-automatically".as_ptr(),
                gbool(popups),
                c"allow-file-access-from-file-urls".as_ptr(),
                FALSE,
                c"allow-universal-access-from-file-urls".as_ptr(),
                FALSE,
                c"allow-top-navigation-to-data-urls".as_ptr(),
                FALSE,
                ptr::null::<c_char>(),
            )
        };
        unsafe { Self::owned(settings.cast()) }.expect("WebKit allocates or aborts")
    }

    /// Names the application in WebKit's own User-Agent string.
    pub fn set_application(&self, name: &str, version: &str) {
        let (name, version) = (cstring(name), cstring(version));
        unsafe {
            webkit_settings_set_user_agent_with_application_details(
                self.as_ptr(),
                name.as_ptr(),
                version.as_ptr(),
            )
        }
    }

    pub fn user_agent(&self) -> String {
        unsafe { string(webkit_settings_get_user_agent(self.as_ptr())) }.unwrap_or_default()
    }

    pub fn set_user_agent(&self, agent: &str) {
        let agent = cstring(agent);
        unsafe { webkit_settings_set_user_agent(self.as_ptr(), agent.as_ptr()) }
    }
}

object!(
    /// The scripts, style sheets and content filters injected into every
    /// view that shares it.
    UserContentManager(WebKitUserContentManager)
);

impl UserContentManager {
    pub fn new() -> Self {
        unsafe { Self::owned(webkit_user_content_manager_new()) }
            .expect("WebKit allocates or aborts")
    }

    pub fn add_filter(&self, filter: &Filter) {
        unsafe { webkit_user_content_manager_add_filter(self.as_ptr(), filter.as_ptr()) }
    }

    pub fn remove_all_filters(&self) {
        unsafe { webkit_user_content_manager_remove_all_filters(self.as_ptr()) }
    }
}

counted!(
    /// A compiled content blocker rule set.
    Filter(
        WebKitUserContentFilter,
        webkit_user_content_filter_ref,
        webkit_user_content_filter_unref
    )
);

object!(
    /// Where WebKit keeps the rule sets it compiled, by identifier.
    FilterStore(WebKitUserContentFilterStore)
);

/// Runs a boxed `FnOnce` as a GIO async callback; `finish` turns the
/// result into what the closure takes.
unsafe extern "C" fn ready<T, F: FnOnce(T)>(
    source: *mut GObject,
    result: *mut GAsyncResult,
    data: gpointer,
) where
    T: Finish,
{
    let f = unsafe { Box::from_raw(data as *mut F) };
    f(unsafe { T::finish(source, result) })
}

/// `f` boxed as an async call's callback and data.
fn boxed<T: Finish, F: FnOnce(T) + 'static>(f: F) -> (GAsyncReadyCallback, gpointer) {
    (Some(ready::<T, F>), Box::into_raw(Box::new(f)) as gpointer)
}

/// What an async call hands back, read from its `_finish` function.
trait Finish {
    unsafe fn finish(source: *mut GObject, result: *mut GAsyncResult) -> Self;
}

/// A rule set loaded or compiled, or why not.
pub type Loaded = Result<Filter, String>;

struct LoadFinish(Loaded);
struct SaveFinish(Loaded);
struct IdentifiersFinish(Vec<String>);

/// The rule set a filter store's `_finish` function hands back, or the
/// error it set; loading and compiling finish alike.
type FilterFinishFn = unsafe extern "C" fn(
    *mut WebKitUserContentFilterStore,
    *mut GAsyncResult,
    *mut *mut GError,
) -> *mut WebKitUserContentFilter;

unsafe fn filter_finish(finish: FilterFinishFn, source: *mut GObject, result: *mut GAsyncResult) -> Loaded {
    let mut error = ptr::null_mut();
    let filter = unsafe { finish(source.cast(), result, &mut error) };
    match unsafe { Filter::owned(filter) } {
        Some(filter) => Ok(filter),
        None => Err(unsafe { take_error(error) }),
    }
}

impl Finish for LoadFinish {
    unsafe fn finish(source: *mut GObject, result: *mut GAsyncResult) -> Self {
        Self(unsafe { filter_finish(webkit_user_content_filter_store_load_finish, source, result) })
    }
}

impl Finish for SaveFinish {
    unsafe fn finish(source: *mut GObject, result: *mut GAsyncResult) -> Self {
        Self(unsafe { filter_finish(webkit_user_content_filter_store_save_from_file_finish, source, result) })
    }
}

impl Finish for IdentifiersFinish {
    unsafe fn finish(source: *mut GObject, result: *mut GAsyncResult) -> Self {
        let list = unsafe {
            webkit_user_content_filter_store_fetch_identifiers_finish(source.cast(), result)
        };
        let mut identifiers = Vec::new();
        if !list.is_null() {
            let mut at = list;
            unsafe {
                while !(*at).is_null() {
                    identifiers.extend(string(*at));
                    at = at.add(1);
                }
                g_strfreev(list);
            }
        }
        Self(identifiers)
    }
}

impl FilterStore {
    pub fn new(path: &Path) -> Self {
        let path = cpath(path);
        unsafe { Self::owned(webkit_user_content_filter_store_new(path.as_ptr())) }
            .expect("WebKit allocates or aborts")
    }

    /// Loads the set compiled under `identifier`; `done` gets an error
    /// when there is none.
    pub fn load<F: FnOnce(Loaded) + 'static>(&self, identifier: &str, done: F) {
        let identifier = cstring(identifier);
        let (callback, data) = boxed(move |LoadFinish(loaded)| done(loaded));
        unsafe {
            webkit_user_content_filter_store_load(
                self.as_ptr(),
                identifier.as_ptr(),
                ptr::null_mut(),
                callback,
                data,
            )
        }
    }

    /// Compiles the JSON rules in `file` and keeps the result under
    /// `identifier`.
    pub fn save_from_file<F: FnOnce(Loaded) + 'static>(
        &self,
        identifier: &str,
        file: &Path,
        done: F,
    ) {
        let identifier = cstring(identifier);
        let path = cpath(file);
        let (callback, data) = boxed(move |SaveFinish(loaded)| done(loaded));
        unsafe {
            let file = g_file_new_for_path(path.as_ptr());
            webkit_user_content_filter_store_save_from_file(
                self.as_ptr(),
                identifier.as_ptr(),
                file,
                ptr::null_mut(),
                callback,
                data,
            );
            g_object_unref(file.cast());
        }
    }

    /// Lists every identifier the store holds.
    pub fn fetch_identifiers<F: FnOnce(Vec<String>) + 'static>(&self, done: F) {
        let (callback, data) = boxed(move |IdentifiersFinish(ids)| done(ids));
        unsafe {
            webkit_user_content_filter_store_fetch_identifiers(
                self.as_ptr(),
                ptr::null_mut(),
                callback,
                data,
            )
        }
    }

    /// Deletes the set under `identifier`, without waiting for it.
    pub fn remove(&self, identifier: &str) {
        let identifier = cstring(identifier);
        unsafe {
            webkit_user_content_filter_store_remove(
                self.as_ptr(),
                identifier.as_ptr(),
                ptr::null_mut(),
                None,
                ptr::null_mut(),
            )
        }
    }
}

object!(
    /// One page, with its own web process.
    WebView(WebKitWebView)
);

impl WebView {
    /// A view on `display`, with `session`'s cookies, `settings` and
    /// `content`'s filters.
    pub fn new(
        display: &Display,
        session: &NetworkSession,
        settings: &Settings,
        content: &UserContentManager,
    ) -> Self {
        let (p_display, p_session, p_settings, p_content) = (
            c"display",
            c"network-session",
            c"settings",
            c"user-content-manager",
        );
        let view = unsafe {
            g_object_new(
                webkit_web_view_get_type(),
                p_display.as_ptr(),
                display.as_ptr(),
                p_session.as_ptr(),
                session.as_ptr(),
                p_settings.as_ptr(),
                settings.as_ptr(),
                p_content.as_ptr(),
                content.as_ptr(),
                ptr::null::<c_char>(),
            )
        };
        unsafe { Self::owned(view.cast()) }.expect("WebKit allocates or aborts")
    }

    /// A popup's view, which shares `related`'s process, display and
    /// session, and loads by itself.
    pub fn related(related: &WebView, settings: &Settings, content: &UserContentManager) -> Self {
        let (p_related, p_settings, p_content) =
            (c"related-view", c"settings", c"user-content-manager");
        let view = unsafe {
            g_object_new(
                webkit_web_view_get_type(),
                p_related.as_ptr(),
                related.as_ptr(),
                p_settings.as_ptr(),
                settings.as_ptr(),
                p_content.as_ptr(),
                content.as_ptr(),
                ptr::null::<c_char>(),
            )
        };
        unsafe { Self::owned(view.cast()) }.expect("WebKit allocates or aborts")
    }

    pub fn view(&self) -> View {
        unsafe { View::borrowed(webkit_web_view_get_wpe_view(self.as_ptr())) }
            .expect("a web view has a WPE view")
    }

    pub fn load_uri(&self, uri: &str) {
        let uri = cstring(uri);
        unsafe { webkit_web_view_load_uri(self.as_ptr(), uri.as_ptr()) }
    }

    pub fn go_back(&self) {
        unsafe { webkit_web_view_go_back(self.as_ptr()) }
    }

    pub fn go_forward(&self) {
        unsafe { webkit_web_view_go_forward(self.as_ptr()) }
    }

    pub fn reload(&self) {
        unsafe { webkit_web_view_reload(self.as_ptr()) }
    }

    pub fn stop_loading(&self) {
        unsafe { webkit_web_view_stop_loading(self.as_ptr()) }
    }

    pub fn uri(&self) -> String {
        unsafe { string(webkit_web_view_get_uri(self.as_ptr())) }.unwrap_or_default()
    }

    pub fn title(&self) -> String {
        unsafe { string(webkit_web_view_get_title(self.as_ptr())) }.unwrap_or_default()
    }

    /// 0 to 1 while loading.
    pub fn estimated_load_progress(&self) -> f64 {
        unsafe { webkit_web_view_get_estimated_load_progress(self.as_ptr()) }
    }

    pub fn is_loading(&self) -> bool {
        unsafe { webkit_web_view_is_loading(self.as_ptr()) != FALSE }
    }

    pub fn can_go_back(&self) -> bool {
        unsafe { webkit_web_view_can_go_back(self.as_ptr()) != FALSE }
    }

    pub fn can_go_forward(&self) -> bool {
        unsafe { webkit_web_view_can_go_forward(self.as_ptr()) != FALSE }
    }

    /// Loaded over https with a certificate that checked out.
    pub fn is_secure(&self) -> bool {
        // WebKit reports the last connection, not the page's address: a
        // page that moved from https to http keeps its info.
        if !self.uri().starts_with("https://") {
            return false;
        }
        let mut certificate = ptr::null_mut();
        let mut errors: GTlsCertificateFlags = 0;
        let https =
            unsafe { webkit_web_view_get_tls_info(self.as_ptr(), &mut certificate, &mut errors) };
        https != FALSE && errors == 0
    }

    /// `notify::<property>`: `property` changed.
    pub fn on_notify<F: Fn(&WebView) + 'static>(&self, property: &str, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView)>(
            view: *mut WebKitWebView,
            _: *mut GParamSpec,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&WebView::passing(view)) }
        }
        self.connect(
            &format!("notify::{property}"),
            trampoline::<F> as *const (),
            f,
        );
    }

    /// `load-changed`, with one of `WEBKIT_LOAD_*`.
    pub fn on_load_changed<F: Fn(&WebView, WebKitLoadEvent) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView, WebKitLoadEvent)>(
            view: *mut WebKitWebView,
            event: WebKitLoadEvent,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&WebView::passing(view), event) }
        }
        self.connect("load-changed", trampoline::<F> as *const (), f);
    }

    /// `load-failed`, with the address and the error's message, for a
    /// load that really failed: one cancelled by the next, or turned into
    /// a download, is not passed on. WebKit's own error page shows either
    /// way.
    pub fn on_load_failed<F: Fn(&WebView, String, String) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView, String, String)>(
            view: *mut WebKitWebView,
            _: WebKitLoadEvent,
            uri: *const c_char,
            error: *mut GError,
            data: gpointer,
        ) -> gboolean {
            // The error belongs to the signal; read it, do not free it.
            let (domain, code, message) = unsafe {
                if error.is_null() {
                    (0, 0, None)
                } else {
                    ((*error).domain, (*error).code, string((*error).message))
                }
            };
            let superseded = unsafe {
                (domain == webkit_network_error_quark()
                    && code == WEBKIT_NETWORK_ERROR_CANCELLED as i32)
                    || (domain == webkit_policy_error_quark()
                        && code
                            == WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE as i32)
            };
            if !superseded {
                let uri = unsafe { string(uri) }.unwrap_or_default();
                unsafe {
                    (*(data as *const F))(&WebView::passing(view), uri, message.unwrap_or_default())
                }
            }
            FALSE
        }
        self.connect("load-failed", trampoline::<F> as *const (), f);
    }

    /// Silences the page's audio and video.
    pub fn set_muted(&self, muted: bool) {
        unsafe { webkit_web_view_set_is_muted(self.as_ptr(), gbool(muted)) }
    }

    /// `create`: a page wants a popup. `f` returns the view to open it in,
    /// made with [`WebView::related`], or `None` to block it. WebKit takes
    /// a reference of its own to the view returned.
    pub fn on_create<F: Fn(&WebView) -> Option<WebView> + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView) -> Option<WebView>>(
            view: *mut WebKitWebView,
            _: *mut WebKitNavigationAction,
            data: gpointer,
        ) -> *mut WebKitWebView {
            match unsafe { (*(data as *const F))(&WebView::passing(view)) } {
                // The signal's return is transfer full: the reference the
                // closure's WebView held goes to WebKit instead of back.
                Some(new) => ManuallyDrop::new(new).as_ptr(),
                None => ptr::null_mut(),
            }
        }
        self.connect("create", trampoline::<F> as *const (), f);
    }

    /// `ready-to-show`: a popup has its first page and may be shown.
    pub fn on_ready_to_show<F: Fn(&WebView) + 'static>(&self, f: F) {
        self.connect("ready-to-show", plain::<F> as *const (), f);
    }

    /// `close`: the page asked to be closed.
    pub fn on_close<F: Fn(&WebView) + 'static>(&self, f: F) {
        self.connect("close", plain::<F> as *const (), f);
    }

    /// `web-process-terminated`: the page's process died.
    pub fn on_web_process_terminated<F: Fn(&WebView) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView)>(
            view: *mut WebKitWebView,
            _reason: WebKitWebProcessTerminationReason,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&WebView::passing(view)) }
        }
        self.connect("web-process-terminated", trampoline::<F> as *const (), f);
    }

    /// `decide-policy`, with one of `WEBKIT_POLICY_DECISION_TYPE_*`; `f`
    /// returns `true` once it has decided, `false` to let WebKit.
    pub fn on_decide_policy<F>(&self, f: F)
    where
        F: Fn(&WebView, &PolicyDecision, WebKitPolicyDecisionType) -> bool + 'static,
    {
        unsafe extern "C" fn trampoline<
            F: Fn(&WebView, &PolicyDecision, WebKitPolicyDecisionType) -> bool,
        >(
            view: *mut WebKitWebView,
            decision: *mut WebKitPolicyDecision,
            kind: WebKitPolicyDecisionType,
            data: gpointer,
        ) -> gboolean {
            gbool(unsafe {
                (*(data as *const F))(
                    &WebView::passing(view),
                    &PolicyDecision::passing(decision),
                    kind,
                )
            })
        }
        self.connect("decide-policy", trampoline::<F> as *const (), f);
    }

    /// `permission-request`. The request is handed over for keeping, to be
    /// answered now or once the person has; `f` returns `true` when it
    /// will be answered, `false` to let WebKit deny it.
    pub fn on_permission_request<F: Fn(&WebView, PermissionRequest) -> bool + 'static>(
        &self,
        f: F,
    ) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView, PermissionRequest) -> bool>(
            view: *mut WebKitWebView,
            request: *mut WebKitPermissionRequest,
            data: gpointer,
        ) -> gboolean {
            let request = unsafe { PermissionRequest::borrowed(request) }
                .expect("a signal never passes NULL");
            gbool(unsafe { (*(data as *const F))(&WebView::passing(view), request) })
        }
        self.connect("permission-request", trampoline::<F> as *const (), f);
    }

    /// `mouse-target-changed`: the pointer is over something else.
    pub fn on_mouse_target_changed<F: Fn(&WebView, &HitTestResult) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView, &HitTestResult)>(
            view: *mut WebKitWebView,
            result: *mut WebKitHitTestResult,
            _modifiers: guint,
            data: gpointer,
        ) {
            unsafe {
                (*(data as *const F))(&WebView::passing(view), &HitTestResult::passing(result))
            }
        }
        self.connect("mouse-target-changed", trampoline::<F> as *const (), f);
    }
}

/// The trampoline for a web view signal with no arguments of its own.
unsafe extern "C" fn plain<F: Fn(&WebView)>(view: *mut WebKitWebView, data: gpointer) {
    unsafe { (*(data as *const F))(&WebView::passing(view)) }
}

object!(
    /// A navigation or response WebKit asks about before going on.
    PolicyDecision(WebKitPolicyDecision)
);

impl PolicyDecision {
    /// For a response decision (the signal's type says which): whether
    /// WebKit can show the content type.
    pub fn is_mime_type_supported(&self) -> bool {
        unsafe {
            webkit_response_policy_decision_is_mime_type_supported(self.as_ptr().cast()) != FALSE
        }
    }

    /// For a navigation decision: what started it.
    fn action(&self) -> *mut WebKitNavigationAction {
        unsafe { webkit_navigation_policy_decision_get_navigation_action(self.as_ptr().cast()) }
    }

    /// For a navigation decision: the mouse button that started it, 0 for
    /// none.
    pub fn mouse_button(&self) -> u32 {
        let action = self.action();
        if action.is_null() {
            0
        } else {
            unsafe { webkit_navigation_action_get_mouse_button(action) }
        }
    }

    /// For a navigation decision: where it goes.
    pub fn uri(&self) -> Option<String> {
        let action = self.action();
        if action.is_null() {
            return None;
        }
        unsafe {
            string(webkit_uri_request_get_uri(
                webkit_navigation_action_get_request(action),
            ))
        }
    }

    pub fn download(&self) {
        unsafe { webkit_policy_decision_download(self.as_ptr()) }
    }

    pub fn ignore(&self) {
        unsafe { webkit_policy_decision_ignore(self.as_ptr()) }
    }
}

object!(
    /// A page asking for something only the person may grant.
    PermissionRequest(WebKitPermissionRequest)
);

impl PermissionRequest {
    /// The request's GObject class, which names what it asks for
    /// (`permissions::Kind::from_type_name`).
    pub fn type_name(&self) -> String {
        unsafe { string(g_type_name_from_instance(self.as_ptr().cast())) }.unwrap_or_default()
    }

    /// For a user media request (`type_name` says so): whether it is for
    /// audio, video, and the screen.
    pub fn user_media(&self) -> (bool, bool, bool) {
        unsafe {
            let p = self.as_ptr().cast();
            (
                webkit_user_media_permission_is_for_audio_device(p) != FALSE,
                webkit_user_media_permission_is_for_video_device(p) != FALSE,
                webkit_user_media_permission_is_for_display_device(p) != FALSE,
            )
        }
    }

    pub fn allow(&self) {
        unsafe { webkit_permission_request_allow(self.as_ptr()) }
    }

    pub fn deny(&self) {
        unsafe { webkit_permission_request_deny(self.as_ptr()) }
    }
}

object!(
    /// What is under the pointer.
    HitTestResult(WebKitHitTestResult)
);

impl HitTestResult {
    /// The link's address when the pointer is over one.
    pub fn link_uri(&self) -> Option<String> {
        unsafe {
            if webkit_hit_test_result_context_is_link(self.as_ptr()) == FALSE {
                return None;
            }
            string(webkit_hit_test_result_get_link_uri(self.as_ptr()))
        }
    }
}

object!(
    /// A file being saved.
    Download(WebKitDownload)
);

impl Download {
    pub fn set_destination(&self, path: &Path) {
        let path = cpath(path);
        unsafe { webkit_download_set_destination(self.as_ptr(), path.as_ptr()) }
    }

    pub fn destination(&self) -> Option<String> {
        unsafe { string(webkit_download_get_destination(self.as_ptr())) }
    }

    /// `decide-destination`, with the name the server suggested; `f`
    /// returns `true` once it has set one.
    pub fn on_decide_destination<F: Fn(&Download, &str) -> bool + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&Download, &str) -> bool>(
            download: *mut WebKitDownload,
            suggested: *const c_char,
            data: gpointer,
        ) -> gboolean {
            let suggested = unsafe { string(suggested) }.unwrap_or_default();
            gbool(unsafe { (*(data as *const F))(&Download::passing(download), &suggested) })
        }
        self.connect("decide-destination", trampoline::<F> as *const (), f);
    }

    /// `finished`.
    pub fn on_finished<F: Fn(&Download) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&Download)>(
            download: *mut WebKitDownload,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&Download::passing(download)) }
        }
        self.connect("finished", trampoline::<F> as *const (), f);
    }

    /// `failed`, with the error's message.
    pub fn on_failed<F: Fn(&Download, String) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&Download, String)>(
            download: *mut WebKitDownload,
            error: *mut GError,
            data: gpointer,
        ) {
            // The error belongs to the signal; read it, do not free it.
            let message = unsafe {
                if error.is_null() {
                    None
                } else {
                    string((*error).message)
                }
            };
            unsafe {
                (*(data as *const F))(&Download::passing(download), message.unwrap_or_default())
            }
        }
        self.connect("failed", trampoline::<F> as *const (), f);
    }
}

// What the extension runtime (extensions/) adds: the default web context
// and its security manager, a URI scheme served by Danube, user scripts and
// style sheets in named worlds, script message handlers that answer, user
// messages to and from the web process extension, and JavaScript run in a
// page from here.

object!(
    /// The web context every view shares: its URI schemes, its security
    /// policy and the web process extension it loads.
    WebContext(WebKitWebContext)
);

impl WebContext {
    pub fn default() -> Self {
        unsafe { Self::borrowed(webkit_web_context_get_default()) }
            .expect("WebKit allocates or aborts")
    }

    pub fn security_manager(&self) -> SecurityManager {
        unsafe { SecurityManager::borrowed(webkit_web_context_get_security_manager(self.as_ptr())) }
            .expect("a context has a security manager")
    }

    /// Serves `scheme` from `f`, which answers each request with
    /// [`URISchemeRequest::finish`] or [`URISchemeRequest::finish_error`].
    pub fn register_uri_scheme<F: Fn(URISchemeRequest) + 'static>(&self, scheme: &str, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(URISchemeRequest)>(
            request: *mut WebKitURISchemeRequest,
            data: gpointer,
        ) {
            let request = unsafe { URISchemeRequest::borrowed(request) }
                .expect("WebKit never passes NULL");
            unsafe { (*(data as *const F))(request) }
        }
        unsafe extern "C" fn free<F>(data: gpointer) {
            drop(unsafe { Box::from_raw(data as *mut F) });
        }
        let scheme = cstring(scheme);
        let data = Box::into_raw(Box::new(f)) as gpointer;
        unsafe {
            webkit_web_context_register_uri_scheme(
                self.as_ptr(),
                scheme.as_ptr(),
                Some(trampoline::<F>),
                data,
                Some(free::<F>),
            )
        }
    }
}

object!(
    /// How the context treats URI schemes it does not know.
    SecurityManager(WebKitSecurityManager)
);

impl SecurityManager {
    /// Pages of `scheme` count as secure contexts, as https does.
    pub fn register_uri_scheme_as_secure(&self, scheme: &str) {
        let scheme = cstring(scheme);
        unsafe { webkit_security_manager_register_uri_scheme_as_secure(self.as_ptr(), scheme.as_ptr()) }
    }

    /// Other origins may fetch from `scheme` when its responses allow it.
    pub fn register_uri_scheme_as_cors_enabled(&self, scheme: &str) {
        let scheme = cstring(scheme);
        unsafe {
            webkit_security_manager_register_uri_scheme_as_cors_enabled(
                self.as_ptr(),
                scheme.as_ptr(),
            )
        }
    }
}

object!(
    /// One request for a URI of a scheme Danube serves.
    URISchemeRequest(WebKitURISchemeRequest)
);

impl URISchemeRequest {
    pub fn uri(&self) -> String {
        unsafe { string(webkit_uri_scheme_request_get_uri(self.as_ptr())) }.unwrap_or_default()
    }

    /// The view the request came from (its page, not necessarily the
    /// frame that asked).
    pub fn web_view(&self) -> Option<WebView> {
        unsafe { WebView::borrowed(webkit_uri_scheme_request_get_web_view(self.as_ptr())) }
    }

    /// The request's `name` header, when it has one.
    pub fn header(&self, name: &str) -> Option<String> {
        let name = cstring(name);
        unsafe {
            let headers = webkit_uri_scheme_request_get_http_headers(self.as_ptr());
            if headers.is_null() {
                return None;
            }
            string(soup_message_headers_get_one(headers, name.as_ptr()))
        }
    }

    /// Answers with `body` and the headers given, `content_type` among
    /// them.
    pub fn finish(&self, status: u32, content_type: &str, headers: &[(String, String)], body: &[u8]) {
        unsafe {
            // GLib copies the body (g_memdup) and frees its copy with the
            // stream.
            let copy = g_memdup2(body.as_ptr().cast(), body.len() as gsize);
            let stream = g_memory_input_stream_new_from_data(copy, body.len() as gssize, Some(g_free));
            let response = webkit_uri_scheme_response_new(stream, body.len() as i64);
            let reason = cstring(if status == 200 { "OK" } else { "Error" });
            webkit_uri_scheme_response_set_status(response, status, reason.as_ptr());
            let content_type = cstring(content_type);
            webkit_uri_scheme_response_set_content_type(response, content_type.as_ptr());
            let soup = soup_message_headers_new(SOUP_MESSAGE_HEADERS_RESPONSE);
            // Once headers are set, a script's `Response.headers` reads
            // them and not the content type above, and
            // `WebAssembly.instantiateStreaming` checks it there.
            let name = cstring("Content-Type");
            soup_message_headers_append(soup, name.as_ptr(), content_type.as_ptr());
            for (name, value) in headers {
                let (name, value) = (cstring(name), cstring(value));
                soup_message_headers_append(soup, name.as_ptr(), value.as_ptr());
            }
            webkit_uri_scheme_response_set_http_headers(response, soup);
            webkit_uri_scheme_request_finish_with_response(self.as_ptr(), response);
            g_object_unref(response.cast());
            g_object_unref(stream.cast());
        }
    }

    /// Fails the request with `message`.
    pub fn finish_error(&self, message: &str) {
        unsafe {
            let message = cstring(message);
            let error = g_error_new_literal(g_io_error_quark(), 1, message.as_ptr());
            webkit_uri_scheme_request_finish_error(self.as_ptr(), error);
            g_error_free(error);
        }
    }
}

/// Where a user script or style sheet goes: every frame or the top one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Frames {
    All,
    Top,
}

impl Frames {
    fn raw(self) -> WebKitUserContentInjectedFrames {
        match self {
            Frames::All => WEBKIT_USER_CONTENT_INJECT_ALL_FRAMES,
            Frames::Top => WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
        }
    }
}

/// A NULL-terminated array of C strings for a list of patterns; `None`
/// for an empty list, which WebKit reads as "everywhere".
struct StringList {
    _owned: Vec<CString>,
    ptrs: Vec<*const c_char>,
}

impl StringList {
    fn new(items: &[String]) -> Self {
        let owned: Vec<CString> = items.iter().map(|s| cstring(s)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
        ptrs.push(ptr::null());
        Self {
            _owned: owned,
            ptrs,
        }
    }

    fn as_ptr(&self) -> *const *const c_char {
        if self.ptrs.len() == 1 {
            ptr::null()
        } else {
            self.ptrs.as_ptr()
        }
    }
}

/// A world name as WebKit takes it: NULL for the page's own.
fn world(name: Option<&str>) -> Option<CString> {
    name.map(cstring)
}

fn world_ptr(name: &Option<CString>) -> *const c_char {
    name.as_ref().map_or(ptr::null(), |n| n.as_ptr())
}

counted!(
    /// A script run in every page whose URL the lists allow.
    UserScript(WebKitUserScript, webkit_user_script_ref, webkit_user_script_unref)
);

impl UserScript {
    /// `source` run in `world` (the page's own when `None`) at document
    /// start or end, in the frames `frames` says, on the pages `allow`
    /// matches and `block` does not.
    pub fn new(
        source: &str,
        frames: Frames,
        at_start: bool,
        world_name: Option<&str>,
        allow: &[String],
        block: &[String],
    ) -> Self {
        let source = cstring(source);
        let time = if at_start {
            WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START
        } else {
            WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_END
        };
        let (allow, block) = (StringList::new(allow), StringList::new(block));
        // The `_for_world` constructor refuses NULL for the page's own
        // world; the plain one is that world.
        let script = match world(world_name) {
            Some(name) => unsafe {
                webkit_user_script_new_for_world(
                    source.as_ptr(),
                    frames.raw(),
                    time,
                    name.as_ptr(),
                    allow.as_ptr(),
                    block.as_ptr(),
                )
            },
            None => unsafe {
                webkit_user_script_new(source.as_ptr(), frames.raw(), time, allow.as_ptr(), block.as_ptr())
            },
        };
        unsafe { Self::owned(script) }.expect("WebKit allocates or aborts")
    }
}

counted!(
    /// A style sheet applied to every page the lists allow.
    UserStyleSheet(
        WebKitUserStyleSheet,
        webkit_user_style_sheet_ref,
        webkit_user_style_sheet_unref
    )
);

impl UserStyleSheet {
    pub fn new(
        source: &str,
        frames: Frames,
        world_name: Option<&str>,
        allow: &[String],
        block: &[String],
    ) -> Self {
        let source = cstring(source);
        let (allow, block) = (StringList::new(allow), StringList::new(block));
        // As the page's own styles, which an extension's are meant to
        // override and be overridden by.
        let level = WEBKIT_USER_STYLE_LEVEL_AUTHOR;
        let sheet = match world(world_name) {
            Some(name) => unsafe {
                webkit_user_style_sheet_new_for_world(
                    source.as_ptr(),
                    frames.raw(),
                    level,
                    name.as_ptr(),
                    allow.as_ptr(),
                    block.as_ptr(),
                )
            },
            None => unsafe {
                webkit_user_style_sheet_new(source.as_ptr(), frames.raw(), level, allow.as_ptr(), block.as_ptr())
            },
        };
        unsafe { Self::owned(sheet) }.expect("WebKit allocates or aborts")
    }
}

/// The answer to a script message, kept until there is one.
pub struct Reply {
    reply: NonNull<WebKitScriptMessageReply>,
    context: NonNull<JSCContext>,
    _thread: PhantomData<*mut ()>,
}

impl Reply {
    /// Resolves the page's promise with `json` (a string the page parses).
    pub fn value(self, json: &str) {
        let json = cstring(json);
        unsafe {
            let value = jsc_value_new_string(self.context.as_ptr(), json.as_ptr());
            webkit_script_message_reply_return_value(self.reply.as_ptr(), value);
            g_object_unref(value.cast());
        }
    }

    /// Rejects it with `message`.
    pub fn error(self, message: &str) {
        let message = cstring(message);
        unsafe {
            webkit_script_message_reply_return_error_message(self.reply.as_ptr(), message.as_ptr())
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        unsafe {
            webkit_script_message_reply_unref(self.reply.as_ptr());
            g_object_unref(self.context.as_ptr().cast());
        }
    }
}

impl UserContentManager {
    pub fn add_script(&self, script: &UserScript) {
        unsafe { webkit_user_content_manager_add_script(self.as_ptr(), script.as_ptr()) }
    }

    pub fn remove_script(&self, script: &UserScript) {
        unsafe { webkit_user_content_manager_remove_script(self.as_ptr(), script.as_ptr()) }
    }

    pub fn add_style_sheet(&self, sheet: &UserStyleSheet) {
        unsafe { webkit_user_content_manager_add_style_sheet(self.as_ptr(), sheet.as_ptr()) }
    }

    pub fn remove_style_sheet(&self, sheet: &UserStyleSheet) {
        unsafe { webkit_user_content_manager_remove_style_sheet(self.as_ptr(), sheet.as_ptr()) }
    }

    /// `window.webkit.messageHandlers.<name>.postMessage(text)` in `world`
    /// (the page's own when `None`) calls `f` with the text and a
    /// [`Reply`] that settles the promise `postMessage` returned; `false`
    /// when the name is taken.
    pub fn register_message_handler<F: Fn(String, Reply) + 'static>(
        &self,
        name: &str,
        world_name: Option<&str>,
        f: F,
    ) -> bool {
        unsafe extern "C" fn trampoline<F: Fn(String, Reply)>(
            _: *mut WebKitUserContentManager,
            value: *mut JSCValue,
            reply: *mut WebKitScriptMessageReply,
            data: gpointer,
        ) -> gboolean {
            let text = unsafe {
                let s = jsc_value_to_string(value);
                let text = string(s).unwrap_or_default();
                g_free(s.cast());
                text
            };
            let context = unsafe { jsc_value_get_context(value) };
            let (Some(reply), Some(context)) = (NonNull::new(reply), NonNull::new(context)) else {
                return FALSE;
            };
            unsafe {
                webkit_script_message_reply_ref(reply.as_ptr());
                g_object_ref(context.as_ptr().cast());
            }
            let reply = Reply {
                reply,
                context,
                _thread: PhantomData,
            };
            unsafe { (*(data as *const F))(text, reply) };
            1
        }
        // Connected before registering, as WebKit's documentation asks,
        // so no message can arrive unheard.
        self.connect(
            &format!("script-message-with-reply-received::{name}"),
            trampoline::<F> as *const (),
            f,
        );
        let name = cstring(name);
        let world_name = world(world_name);
        unsafe {
            webkit_user_content_manager_register_script_message_handler_with_reply(
                self.as_ptr(),
                name.as_ptr(),
                world_ptr(&world_name),
            ) != FALSE
        }
    }
}

/// What a script evaluated in a page came to.
pub type Evaluated = Result<String, String>;

struct EvaluateFinish(Evaluated);

impl Finish for EvaluateFinish {
    unsafe fn finish(source: *mut GObject, result: *mut GAsyncResult) -> Self {
        let mut error = ptr::null_mut();
        let value = unsafe { webkit_web_view_evaluate_javascript_finish(source.cast(), result, &mut error) };
        if value.is_null() {
            return Self(Err(unsafe { take_error(error) }));
        }
        let json = unsafe {
            let s = jsc_value_to_json(value, 0);
            let json = string(s).unwrap_or_default();
            g_free(s.cast());
            g_object_unref(value.cast());
            json
        };
        Self(Ok(json))
    }
}

impl WebView {
    /// Runs `script` in the main frame, in `world_name` (the page's own
    /// when `None`); `done`, when given, gets the result as JSON.
    pub fn evaluate(
        &self,
        script: &str,
        world_name: Option<&str>,
        done: Option<Box<dyn FnOnce(Evaluated)>>,
    ) {
        let world_name = world(world_name);
        let (callback, data) = match done {
            Some(done) => boxed(move |EvaluateFinish(result)| done(result)),
            None => (None, ptr::null_mut()),
        };
        unsafe {
            webkit_web_view_evaluate_javascript(
                self.as_ptr(),
                script.as_ptr().cast(),
                script.len() as gssize,
                world_ptr(&world_name),
                ptr::null(),
                ptr::null_mut(),
                callback,
                data,
            )
        }
    }

    /// The URL patterns this view's pages may fetch without CORS checks
    /// (an extension's host permissions).
    pub fn set_cors_allowlist(&self, patterns: &[String]) {
        let list = StringList::new(patterns);
        unsafe { webkit_web_view_set_cors_allowlist(self.as_ptr(), list.as_ptr()) }
    }

    /// Starts saving `uri`, as a page's own link would.
    pub fn download_uri(&self, uri: &str) -> Download {
        let uri = cstring(uri);
        unsafe { Download::owned(webkit_web_view_download_uri(self.as_ptr(), uri.as_ptr())) }
            .expect("WebKit allocates or aborts")
    }

}

impl Download {
    /// The address being saved.
    pub fn uri(&self) -> String {
        unsafe {
            let request = webkit_download_get_request(self.as_ptr());
            if request.is_null() {
                return String::new();
            }
            string(webkit_uri_request_get_uri(request)).unwrap_or_default()
        }
    }
}

// What handing a download to aria2 needs (downloads.rs): the response's
// final address and size, the request's method and Referer, the cookies
// WebKit would send, and a way to stop WebKit's own transfer.

impl Download {
    /// Stops it; WebKit then emits `failed` (cancelled) and `finished`.
    pub fn cancel(&self) {
        unsafe { webkit_download_cancel(self.as_ptr()) }
    }

    /// The address the response came from, after any redirects; empty
    /// before there is a response.
    pub fn response_uri(&self) -> String {
        unsafe {
            let response = webkit_download_get_response(self.as_ptr());
            if response.is_null() {
                return String::new();
            }
            string(webkit_uri_response_get_uri(response)).unwrap_or_default()
        }
    }

    /// The size the server gave, if it gave one.
    pub fn content_length(&self) -> Option<u64> {
        let length = unsafe {
            let response = webkit_download_get_response(self.as_ptr());
            if response.is_null() {
                return None;
            }
            webkit_uri_response_get_content_length(response)
        };
        (length > 0).then_some(length)
    }

    /// The request's HTTP method; WebKit leaves it unset for a plain GET.
    pub fn method(&self) -> String {
        let method = unsafe {
            let request = webkit_download_get_request(self.as_ptr());
            if request.is_null() {
                None
            } else {
                string(webkit_uri_request_get_http_method(request))
            }
        };
        method.filter(|m| !m.is_empty()).unwrap_or_else(|| "GET".into())
    }

    /// One of the headers the request was sent with.
    pub fn request_header(&self, name: &str) -> Option<String> {
        let name = cstring(name);
        unsafe {
            let request = webkit_download_get_request(self.as_ptr());
            if request.is_null() {
                return None;
            }
            let headers = webkit_uri_request_get_http_headers(request);
            if headers.is_null() {
                return None;
            }
            string(soup_message_headers_get_one(headers, name.as_ptr()))
        }
    }

    /// Bytes saved so far.
    pub fn received(&self) -> u64 {
        unsafe { webkit_download_get_received_data_length(self.as_ptr()) }
    }

    /// `received-data`: more of it arrived.
    pub fn on_received<F: Fn(&Download) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&Download)>(
            download: *mut WebKitDownload,
            _length: u64,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&Download::passing(download)) }
        }
        self.connect("received-data", trampoline::<F> as *const (), f);
    }
}

/// The cookies for an address, as name and value.
struct CookiesFinish(Vec<(String, String)>);

impl Finish for CookiesFinish {
    unsafe fn finish(source: *mut GObject, result: *mut GAsyncResult) -> Self {
        let mut error = ptr::null_mut();
        let list = unsafe { webkit_cookie_manager_get_cookies_finish(source.cast(), result, &mut error) };
        if list.is_null() {
            // No cookies and an error look the same to a download: it is
            // asked for without any.
            if !error.is_null() {
                unsafe { take_error(error) };
            }
            return Self(Vec::new());
        }
        let mut cookies = Vec::new();
        let mut at = list;
        unsafe {
            while !at.is_null() {
                let cookie = (*at).data as *mut SoupCookie;
                cookies.push((
                    string(soup_cookie_get_name(cookie)).unwrap_or_default(),
                    string(soup_cookie_get_value(cookie)).unwrap_or_default(),
                ));
                soup_cookie_free(cookie);
                at = (*at).next;
            }
            g_list_free(list);
        }
        Self(cookies)
    }
}

impl NetworkSession {
    /// The cookies WebKit would send to `uri`, HttpOnly ones included.
    pub fn cookies<F: FnOnce(Vec<(String, String)>) + 'static>(&self, uri: &str, done: F) {
        let uri = cstring(uri);
        let (callback, data) = boxed(move |CookiesFinish(cookies)| done(cookies));
        unsafe {
            let manager = webkit_network_session_get_cookie_manager(self.as_ptr());
            webkit_cookie_manager_get_cookies(manager, uri.as_ptr(), ptr::null_mut(), callback, data);
        }
    }

    /// Starts saving `uri` with WebKit, outside any page.
    pub fn download_uri(&self, uri: &str) -> Download {
        let uri = cstring(uri);
        unsafe { Download::owned(webkit_network_session_download_uri(self.as_ptr(), uri.as_ptr())) }
            .expect("WebKit allocates or aborts")
    }
}

// GDBus, for the desktop's notifications (notify.rs): calls on the session
// bus with arguments built from [`Var`]s, and signals as [`Arg`]s.

object!(
    /// A connection to the session bus.
    Bus(GDBusConnection)
);

/// A D-Bus value to send, as GVariant takes it.
#[derive(Clone, Debug, PartialEq)]
pub enum Var {
    Str(String),
    U32(u32),
    I32(i32),
    Bool(bool),
    /// `as`
    Strings(Vec<String>),
    /// `a{sv}`
    Dict(Vec<(String, Var)>),
    /// An array of the given element type (`(us)`), which an empty one
    /// still needs.
    Array(&'static str, Vec<Var>),
    /// The call's arguments, or a struct inside them.
    Tuple(Vec<Var>),
}

impl Var {
    /// A floating GVariant, which whatever takes it consumes.
    unsafe fn to_gvariant(&self) -> *mut GVariant {
        unsafe {
            match self {
                Var::Str(s) => g_variant_new_string(cstring(s).as_ptr()),
                Var::U32(n) => g_variant_new_uint32(*n),
                Var::I32(n) => g_variant_new_int32(*n),
                Var::Bool(b) => g_variant_new_boolean(gbool(*b)),
                Var::Strings(list) => {
                    let owned: Vec<CString> = list.iter().map(|s| cstring(s)).collect();
                    let pointers: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
                    // Copies the strings.
                    g_variant_new_strv(pointers.as_ptr(), pointers.len() as gssize)
                }
                Var::Dict(entries) => {
                    let children: Vec<*mut GVariant> = entries
                        .iter()
                        .map(|(key, value)| {
                            g_variant_new_dict_entry(
                                g_variant_new_string(cstring(key).as_ptr()),
                                g_variant_new_variant(value.to_gvariant()),
                            )
                        })
                        .collect();
                    // G_VARIANT_TYPE("{sv}") is a cast of the type string.
                    g_variant_new_array(
                        c"{sv}".as_ptr().cast(),
                        children.as_ptr(),
                        children.len() as gsize,
                    )
                }
                Var::Array(element, items) => {
                    let children: Vec<*mut GVariant> = items.iter().map(|v| v.to_gvariant()).collect();
                    let element = cstring(element);
                    g_variant_new_array(element.as_ptr().cast(), children.as_ptr(), children.len() as gsize)
                }
                Var::Tuple(items) => {
                    let children: Vec<*mut GVariant> = items.iter().map(|v| v.to_gvariant()).collect();
                    g_variant_new_tuple(children.as_ptr(), children.len() as gsize)
                }
            }
        }
    }
}

/// A D-Bus value received: the kinds the notification signals and the
/// portals' replies carry, and anything else.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    U32(u32),
    Str(String),
    /// `as`
    Strings(Vec<String>),
    /// `a{sv}`, each value unwrapped from its variant.
    Dict(Vec<(String, Arg)>),
    Other,
}

impl Arg {
    /// The value under `key` in a [`Arg::Dict`].
    pub fn get(&self, key: &str) -> Option<&Arg> {
        match self {
            Arg::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// One GVariant as an [`Arg`]; `value` stays the caller's.
unsafe fn arg(value: *mut GVariant) -> Arg {
    unsafe {
        let kind = CStr::from_ptr(g_variant_get_type_string(value)).to_bytes();
        let child = |n| g_variant_get_child_value(value, n);
        match kind {
            b"u" => Arg::U32(g_variant_get_uint32(value)),
            b"s" | b"o" => Arg::Str(string(g_variant_get_string(value, ptr::null_mut())).unwrap_or_default()),
            b"v" => {
                let inner = g_variant_get_variant(value);
                let parsed = arg(inner);
                g_variant_unref(inner);
                parsed
            }
            b"as" => Arg::Strings(
                (0..g_variant_n_children(value))
                    .map(|n| {
                        let s = child(n);
                        let text = string(g_variant_get_string(s, ptr::null_mut())).unwrap_or_default();
                        g_variant_unref(s);
                        text
                    })
                    .collect(),
            ),
            b"a{sv}" => Arg::Dict(
                (0..g_variant_n_children(value))
                    .map(|n| {
                        let entry = child(n);
                        let (key, value) = (g_variant_get_child_value(entry, 0), g_variant_get_child_value(entry, 1));
                        let pair = (string(g_variant_get_string(key, ptr::null_mut())).unwrap_or_default(), arg(value));
                        g_variant_unref(key);
                        g_variant_unref(value);
                        g_variant_unref(entry);
                        pair
                    })
                    .collect(),
            ),
            _ => Arg::Other,
        }
    }
}

/// The children of a tuple GVariant (a reply's or a signal's arguments).
unsafe fn args(tuple: *mut GVariant) -> Vec<Arg> {
    if tuple.is_null() {
        return Vec::new();
    }
    let count = unsafe { g_variant_n_children(tuple) };
    (0..count)
        .map(|n| unsafe {
            let child = g_variant_get_child_value(tuple, n);
            let parsed = arg(child);
            g_variant_unref(child);
            parsed
        })
        .collect()
}

/// A call's reply arguments, or its error's message.
pub type Replied = Result<Vec<Arg>, String>;

struct CallFinish(Replied);

impl Finish for CallFinish {
    unsafe fn finish(source: *mut GObject, result: *mut GAsyncResult) -> Self {
        let mut error = ptr::null_mut();
        let reply = unsafe { g_dbus_connection_call_finish(source.cast(), result, &mut error) };
        if reply.is_null() {
            return Self(Err(unsafe { take_error(error) }));
        }
        let parsed = unsafe { args(reply) };
        unsafe { g_variant_unref(reply) };
        Self(Ok(parsed))
    }
}

impl Bus {
    /// The session bus, or why there is none (the sandbox may have been
    /// given no bus at all).
    pub fn session() -> Result<Self, String> {
        let mut error = ptr::null_mut();
        let bus = unsafe { g_bus_get_sync(G_BUS_TYPE_SESSION, ptr::null_mut(), &mut error) };
        unsafe { Self::owned(bus) }.ok_or_else(|| unsafe { take_error(error) })
    }

    /// Calls `method` on `dest` with `args` (a [`Var::Tuple`]), and hands
    /// `done` the reply.
    pub fn call<F: FnOnce(Replied) + 'static>(
        &self,
        dest: &str,
        path: &str,
        interface: &str,
        method: &str,
        args: &Var,
        done: F,
    ) {
        // The bus's default timeout.
        self.call_waiting(dest, path, interface, method, args, -1, done);
    }

    /// As [`Self::call`], waiting up to `timeout_ms` for the reply: for a
    /// service D-Bus starts on this call, which on a slow machine takes
    /// longer than the bus's default 25 seconds.
    #[allow(clippy::too_many_arguments)]
    pub fn call_waiting<F: FnOnce(Replied) + 'static>(
        &self,
        dest: &str,
        path: &str,
        interface: &str,
        method: &str,
        args: &Var,
        timeout_ms: i32,
        done: F,
    ) {
        let (dest, path, interface, method) = (cstring(dest), cstring(path), cstring(interface), cstring(method));
        let (callback, data) = boxed(move |CallFinish(reply)| done(reply));
        unsafe {
            g_dbus_connection_call(
                self.as_ptr(),
                dest.as_ptr(),
                path.as_ptr(),
                interface.as_ptr(),
                method.as_ptr(),
                args.to_gvariant(),
                ptr::null(),
                G_DBUS_CALL_FLAGS_NONE,
                timeout_ms,
                ptr::null_mut(),
                callback,
                data,
            )
        }
    }

    /// Calls `f` with the name and arguments of every signal `sender`
    /// emits on `interface` at `path`, for as long as the connection lives.
    pub fn on_signal<F: Fn(&str, Vec<Arg>) + 'static>(&self, sender: &str, interface: &str, path: &str, f: F) {
        self.subscribe(sender, interface, Some(path), move |_, signal, args| f(signal, args));
    }

    /// As [`Self::on_signal`], at any path, which `f` is told: a portal
    /// answers each request on an object of its own.
    pub fn on_signal_anywhere<F: Fn(&str, &str, Vec<Arg>) + 'static>(&self, sender: &str, interface: &str, f: F) {
        self.subscribe(sender, interface, None, f);
    }

    fn subscribe<F: Fn(&str, &str, Vec<Arg>) + 'static>(&self, sender: &str, interface: &str, path: Option<&str>, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&str, &str, Vec<Arg>)>(
            _: *mut GDBusConnection,
            _sender: *const c_char,
            path: *const c_char,
            _interface: *const c_char,
            signal: *const c_char,
            parameters: *mut GVariant,
            data: gpointer,
        ) {
            let path = unsafe { string(path) }.unwrap_or_default();
            let signal = unsafe { string(signal) }.unwrap_or_default();
            unsafe { (*(data as *const F))(&path, &signal, args(parameters)) }
        }
        unsafe extern "C" fn free<F>(data: gpointer) {
            drop(unsafe { Box::from_raw(data as *mut F) });
        }
        let (sender, interface) = (cstring(sender), cstring(interface));
        let path = path.map(cstring);
        let data = Box::into_raw(Box::new(f)) as gpointer;
        unsafe {
            g_dbus_connection_signal_subscribe(
                self.as_ptr(),
                sender.as_ptr(),
                interface.as_ptr(),
                ptr::null(),
                path.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
                ptr::null(),
                G_DBUS_SIGNAL_FLAGS_NONE,
                Some(trampoline::<F>),
                data,
                Some(free::<F>),
            );
        }
    }
}

impl Download {
    /// The view whose page started it, if a page did.
    pub fn web_view(&self) -> Option<WebView> {
        unsafe { WebView::borrowed(webkit_download_get_web_view(self.as_ptr())) }
    }
}

impl WebView {
    /// Whether `other` is this same view.
    pub fn is(&self, other: &WebView) -> bool {
        self.as_ptr() == other.as_ptr()
    }
}

// A page's file upload, its notifications, and finding text in it.

object!(
    /// A page's `<input type=file>` asking for files, kept until it is
    /// answered: dropping the last reference unanswered cancels it.
    FileChooserRequest(WebKitFileChooserRequest)
);

impl FileChooserRequest {
    /// The MIME types the input accepts, empty for any.
    pub fn mime_types(&self) -> Vec<String> {
        let list = unsafe { webkit_file_chooser_request_get_mime_types(self.as_ptr()) };
        unsafe { strings(list) }
    }

    pub fn select_multiple(&self) -> bool {
        unsafe { webkit_file_chooser_request_get_select_multiple(self.as_ptr()) != FALSE }
    }

    /// Answers with `paths`, which WebKit copies.
    pub fn select_files(&self, paths: &[String]) {
        let owned: Vec<CString> = paths.iter().map(|p| cstring(p)).collect();
        let mut pointers: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
        pointers.push(ptr::null());
        unsafe { webkit_file_chooser_request_select_files(self.as_ptr(), pointers.as_ptr()) }
    }

    pub fn cancel(&self) {
        unsafe { webkit_file_chooser_request_cancel(self.as_ptr()) }
    }
}

/// A NULL-terminated array of C strings WebKit keeps, copied out.
unsafe fn strings(list: *const *const c_char) -> Vec<String> {
    let mut out = Vec::new();
    if list.is_null() {
        return out;
    }
    let mut at = list;
    while let Some(s) = unsafe { string(*at) } {
        out.push(s);
        at = unsafe { at.add(1) };
    }
    out
}

object!(
    /// A notification a page asked to show.
    Notification(WebKitNotification)
);

impl Notification {
    pub fn title(&self) -> String {
        unsafe { string(webkit_notification_get_title(self.as_ptr())) }.unwrap_or_default()
    }

    pub fn body(&self) -> String {
        unsafe { string(webkit_notification_get_body(self.as_ptr())) }.unwrap_or_default()
    }

    /// The page's tag for it: a new notification with the same tag
    /// replaces the old one.
    pub fn tag(&self) -> Option<String> {
        unsafe { string(webkit_notification_get_tag(self.as_ptr())) }.filter(|t| !t.is_empty())
    }

    /// The person clicked it: the page's `onclick` runs.
    pub fn clicked(&self) {
        unsafe { webkit_notification_clicked(self.as_ptr()) }
    }

    /// The person dismissed it: the page's `onclose` runs.
    pub fn close(&self) {
        unsafe { webkit_notification_close(self.as_ptr()) }
    }

    /// `closed`: the page closed it, or [`Self::close`] did.
    pub fn on_closed<F: Fn(&Notification) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&Notification)>(
            notification: *mut WebKitNotification,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(&Notification::passing(notification)) }
        }
        self.connect("closed", trampoline::<F> as *const (), f);
    }
}

object!(
    /// Finding text in a page: one search at a time, highlighted in it.
    FindController(WebKitFindController)
);

/// The most matches a search counts and highlights.
const MATCHES: u32 = 1000;

impl FindController {
    /// Highlights every match of `text`, case-insensitively, wrapping round
    /// the page, and goes to the first; `found-text` or
    /// `failed-to-find-text` says how it went.
    pub fn search(&self, text: &str) {
        let text = cstring(text);
        let options = WEBKIT_FIND_OPTIONS_CASE_INSENSITIVE | WEBKIT_FIND_OPTIONS_WRAP_AROUND;
        unsafe { webkit_find_controller_search(self.as_ptr(), text.as_ptr(), options, MATCHES) }
    }

    pub fn next(&self) {
        unsafe { webkit_find_controller_search_next(self.as_ptr()) }
    }

    pub fn previous(&self) {
        unsafe { webkit_find_controller_search_previous(self.as_ptr()) }
    }

    /// Ends the search and takes the highlights away.
    pub fn finish(&self) {
        unsafe { webkit_find_controller_search_finish(self.as_ptr()) }
    }

    /// `found-text`, with how many matches there are.
    pub fn on_found<F: Fn(u32) + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(u32)>(
            _: *mut WebKitFindController,
            count: guint,
            data: gpointer,
        ) {
            unsafe { (*(data as *const F))(count) }
        }
        self.connect("found-text", trampoline::<F> as *const (), f);
    }

    /// `failed-to-find-text`.
    pub fn on_not_found<F: Fn() + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn()>(_: *mut WebKitFindController, data: gpointer) {
            unsafe { (*(data as *const F))() }
        }
        self.connect("failed-to-find-text", trampoline::<F> as *const (), f);
    }
}

impl WebView {
    /// `run-file-chooser`. The request is handed over for keeping; `f`
    /// returns `true` when it will be answered, `false` to let WebKit
    /// cancel it.
    pub fn on_run_file_chooser<F: Fn(&WebView, FileChooserRequest) -> bool + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView, FileChooserRequest) -> bool>(
            view: *mut WebKitWebView,
            request: *mut WebKitFileChooserRequest,
            data: gpointer,
        ) -> gboolean {
            let request = unsafe { FileChooserRequest::borrowed(request) }
                .expect("a signal never passes NULL");
            gbool(unsafe { (*(data as *const F))(&WebView::passing(view), request) })
        }
        self.connect("run-file-chooser", trampoline::<F> as *const (), f);
    }

    /// `show-notification`, the notification handed over for keeping; `f`
    /// returns `true` when it was shown, `false` to let WebKit drop it.
    pub fn on_show_notification<F: Fn(&WebView, Notification) -> bool + 'static>(&self, f: F) {
        unsafe extern "C" fn trampoline<F: Fn(&WebView, Notification) -> bool>(
            view: *mut WebKitWebView,
            notification: *mut WebKitNotification,
            data: gpointer,
        ) -> gboolean {
            let notification = unsafe { Notification::borrowed(notification) }
                .expect("a signal never passes NULL");
            gbool(unsafe { (*(data as *const F))(&WebView::passing(view), notification) })
        }
        self.connect("show-notification", trampoline::<F> as *const (), f);
    }

    pub fn find_controller(&self) -> FindController {
        unsafe { FindController::borrowed(webkit_web_view_get_find_controller(self.as_ptr())) }
            .expect("a web view has a find controller")
    }
}

impl WebContext {
    /// Lets every web process see `path`, read-only or not. Only before
    /// the first one starts.
    pub fn add_path_to_sandbox(&self, path: &Path, read_only: bool) {
        let path = cpath(path);
        unsafe { webkit_web_context_add_path_to_sandbox(self.as_ptr(), path.as_ptr(), gbool(read_only)) }
    }
}

impl Bus {
    /// This connection's own name on the bus (`:1.42`), which the portals
    /// put in the path of each request they answer.
    pub fn unique_name(&self) -> String {
        unsafe { string(g_dbus_connection_get_unique_name(self.as_ptr())) }.unwrap_or_default()
    }

    /// Calls `method` and waits for its reply, for a caller with no main
    /// loop running.
    pub fn call_sync(&self, dest: &str, path: &str, interface: &str, method: &str, arguments: &Var) -> Replied {
        let (dest, path, interface, method) = (cstring(dest), cstring(path), cstring(interface), cstring(method));
        let mut error = ptr::null_mut();
        let reply = unsafe {
            g_dbus_connection_call_sync(
                self.as_ptr(),
                dest.as_ptr(),
                path.as_ptr(),
                interface.as_ptr(),
                method.as_ptr(),
                arguments.to_gvariant(),
                ptr::null(),
                G_DBUS_CALL_FLAGS_NONE,
                -1,
                ptr::null_mut(),
                &mut error,
            )
        };
        if reply.is_null() {
            return Err(unsafe { take_error(error) });
        }
        let parsed = unsafe { args(reply) };
        unsafe { g_variant_unref(reply) };
        Ok(parsed)
    }
}
