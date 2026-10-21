//! Input from the host, posted into macOS as if from its own keyboard and
//! mouse (CGEvent), and the permissions that takes.

use std::time::{Duration, Instant};

use objc2_application_services::{
    kAXTrustedCheckOptionPrompt, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGMouseButton, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
    CGScrollEventUnit,
};

use crate::keymap;
use crate::server::Tracked;

/// What this program still needs someone to allow, in the words System
/// Settings uses for it.
pub fn missing_permissions() -> Vec<String> {
    let mut missing = Vec::new();
    if !CGPreflightScreenCaptureAccess() {
        missing.push("Screen Recording".to_owned());
    }
    if !unsafe { AXIsProcessTrusted() } {
        missing.push("Accessibility".to_owned());
    }
    missing
}

/// Has macOS ask for what is missing, which it does once per program, then
/// says what still is.
pub fn request_permissions() -> Vec<String> {
    if !CGPreflightScreenCaptureAccess() {
        CGRequestScreenCaptureAccess();
    }
    unsafe {
        let options = CFDictionary::<CFString, CFBoolean>::from_slices(
            &[kAXTrustedCheckOptionPrompt],
            &[CFBoolean::new(true)],
        );
        AXIsProcessTrustedWithOptions(Some(options.as_opaque()));
    }
    missing_permissions()
}

fn source() -> Option<CFRetained<CGEventSource>> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
}

fn post(event: Option<CFRetained<CGEvent>>) {
    if let Some(event) = event {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}

/// The pointer as one connection drives it: which buttons it holds, so a
/// move while one is down is a drag, and its recent clicks, so two quick
/// ones are a double click as macOS counts them.
#[derive(Default)]
pub struct PointerState {
    held: [bool; 3],
    last_click: Option<(u8, Instant, f64, f64, i64)>,
}

/// The double-click interval macOS uses unless someone changed it.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

fn global(window: &Tracked, x: f64, y: f64) -> CGPoint {
    CGPoint {
        x: window.info.x + x,
        y: window.info.y + y,
    }
}

fn mouse_button(button: u8) -> CGMouseButton {
    match button {
        1 => CGMouseButton::Right,
        2 => CGMouseButton::Center,
        _ => CGMouseButton::Left,
    }
}

impl PointerState {
    pub fn moved(&mut self, window: &Tracked, x: f64, y: f64) {
        let (kind, button) = if self.held[0] {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if self.held[1] {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if self.held[2] {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        post(CGEvent::new_mouse_event(
            source().as_deref(),
            kind,
            global(window, x, y),
            button,
        ));
    }

    pub fn button(&mut self, window: &Tracked, button: u8, pressed: bool, x: f64, y: f64) {
        let button = button.min(2);
        self.held[button as usize] = pressed;
        let kind = match (button, pressed) {
            (0, true) => CGEventType::LeftMouseDown,
            (0, false) => CGEventType::LeftMouseUp,
            (1, true) => CGEventType::RightMouseDown,
            (1, false) => CGEventType::RightMouseUp,
            (_, true) => CGEventType::OtherMouseDown,
            (_, false) => CGEventType::OtherMouseUp,
        };
        let point = global(window, x, y);
        // A press close in time and place to the last one of the same
        // button counts on from it; its release carries the same count.
        let clicks = match (pressed, self.last_click) {
            (true, Some((b, at, px, py, n)))
                if b == button
                    && at.elapsed() < DOUBLE_CLICK
                    && (px - point.x).abs() < 4.0
                    && (py - point.y).abs() < 4.0 =>
            {
                n + 1
            }
            (true, _) => 1,
            (false, Some((b, _, _, _, n))) if b == button => n,
            (false, _) => 1,
        };
        if pressed {
            self.last_click = Some((button, Instant::now(), point.x, point.y, clicks));
        }
        let event =
            CGEvent::new_mouse_event(source().as_deref(), kind, point, mouse_button(button));
        CGEvent::set_integer_value_field(
            event.as_deref(),
            CGEventField::MouseEventClickState,
            clicks,
        );
        post(event);
    }

    pub fn scroll(&mut self, window: &Tracked, x: f64, y: f64, dx: f64, dy: f64) {
        // Scrolling goes to what is under the pointer, so it goes there
        // first.
        self.moved(window, x, y);
        // Wayland counts down as positive, macOS up.
        let event = CGEvent::new_scroll_wheel_event2(
            source().as_deref(),
            CGScrollEventUnit::Pixel,
            2,
            (-dy).round() as i32,
            (-dx).round() as i32,
            0,
        );
        post(event);
    }
}

/// The keys one connection holds down, so that a press of a key already
/// down is marked as the keyboard's own repeat, which apps treat apart from
/// a fresh press (a held arrow scrolls; a held Return does not send twice
/// as fast). Events posted from outside the HID system never repeat on
/// their own, so the host sends the repeats and macOS is told they are.
#[derive(Default)]
pub struct Keyboard {
    held: std::collections::HashSet<u32>,
}

impl Keyboard {
    /// A key going down or up, with the modifiers held at the time and the
    /// text the host's layout types with it.
    pub fn key(&mut self, code: u32, pressed: bool, modifiers: u32, text: Option<&str>) {
        let repeat = if pressed {
            !self.held.insert(code)
        } else {
            self.held.remove(&code);
            false
        };
        key(code, pressed, modifiers, text, repeat);
    }

    /// Lets go of everything, for when the host's window loses the keyboard
    /// while keys are down and their releases will go elsewhere.
    pub fn release_all(&mut self) {
        for code in std::mem::take(&mut self.held) {
            key(code, false, 0, None, false);
        }
    }
}

/// A key going down or up.
pub fn key(code: u32, pressed: bool, modifiers: u32, text: Option<&str>, repeat: bool) {
    let Some(virtual_key) = keymap::mac_key(code) else {
        return;
    };
    let event = CGEvent::new_keyboard_event(source().as_deref(), virtual_key, pressed);
    let mut flags = keymap::mac_flags(modifiers);
    // A modifier's own event carries itself among the flags while down,
    // which is how macOS tells its press from its release.
    if keymap::is_modifier(code) && !pressed {
        flags &= !keymap::mac_flags(modifier_bit(code));
    } else if keymap::is_modifier(code) {
        flags |= keymap::mac_flags(modifier_bit(code));
    }
    if keymap::is_modifier(code) {
        if let Some(event) = &event {
            CGEvent::set_type(Some(event), CGEventType::FlagsChanged);
        }
    }
    // The character the person's own layout gives, so a guest set to
    // another layout types what they meant. Not with Command or Control
    // held: shortcuts go by the key's place, as they do on a Mac.
    let shortcut = modifiers
        & (app_windows_protocol::modifiers::CONTROL | app_windows_protocol::modifiers::SUPER)
        != 0;
    if let (Some(text), false) = (
        text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control)),
        shortcut,
    ) {
        let units: Vec<u16> = text.encode_utf16().take(20).collect();
        unsafe {
            CGEvent::keyboard_set_unicode_string(event.as_deref(), units.len() as _, units.as_ptr())
        };
    }
    if repeat {
        CGEvent::set_integer_value_field(
            event.as_deref(),
            CGEventField::KeyboardEventAutorepeat,
            1,
        );
    }
    CGEvent::set_flags(event.as_deref(), CGEventFlags(flags));
    post(event);
}

fn modifier_bit(code: u32) -> u32 {
    use app_windows_protocol::modifiers::*;
    match code {
        29 | 97 => CONTROL,
        42 | 54 => SHIFT,
        56 | 100 => ALT,
        125 | 126 => SUPER,
        58 => CAPS_LOCK,
        _ => 0,
    }
}

/// Text typed as it is: a key event carrying the characters, which macOS
/// delivers whatever its keyboard layout.
pub fn text(text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    // macOS takes at most 20 UTF-16 units per event.
    for chunk in units.chunks(20) {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(source().as_deref(), 0, down);
            unsafe {
                CGEvent::keyboard_set_unicode_string(
                    event.as_deref(),
                    chunk.len() as _,
                    chunk.as_ptr(),
                )
            };
            post(event);
        }
    }
}
