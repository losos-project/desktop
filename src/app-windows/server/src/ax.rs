//! Windows and menus through Accessibility: raising and resizing a window,
//! closing it, and reading an app's menu bar for the host's global menu.
//!
//! Accessibility knows windows as elements of their app, not by the window
//! server's numbers; `_AXUIElementGetWindow` gives an element's number. It
//! is private, and every window manager for macOS (Rectangle, yabai,
//! AltTab) relies on it, for want of any public way to do the same.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use app_windows_protocol::{Menu, MenuEntry};
use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFNumber, CFRetained, CFString, CFType, CGPoint, CGSize,
};

use crate::apps;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn _AXUIElementGetWindow(element: &AXUIElement, window: *mut u32) -> AXError;
}

fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let mut value: *const CFType = ptr::null();
    let error = unsafe {
        element.copy_attribute_value(&CFString::from_str(name), NonNull::from(&mut value))
    };
    if error != AXError::Success || value.is_null() {
        return None;
    }
    Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(value as *mut CFType)) })
}

fn children(element: &AXUIElement, name: &str) -> Vec<CFRetained<AXUIElement>> {
    let Some(value) = attribute(element, name) else {
        return Vec::new();
    };
    let Some(array) = value.downcast_ref::<CFArray>() else {
        return Vec::new();
    };
    let array: &CFArray<AXUIElement> = unsafe { array.cast_unchecked() };
    array.iter().collect()
}

fn string(element: &AXUIElement, name: &str) -> Option<String> {
    attribute(element, name)?
        .downcast_ref::<CFString>()
        .map(|s| s.to_string())
}

fn window(pid: i32, id: u32) -> Option<CFRetained<AXUIElement>> {
    let app = unsafe { AXUIElement::new_application(pid) };
    children(&app, "AXWindows").into_iter().find(|w| {
        let mut number = 0u32;
        let found = unsafe { _AXUIElementGetWindow(w, &mut number) };
        found == AXError::Success && number == id
    })
}

/// Brings a window to the front of macOS's screen and its app forward, so
/// keys go to it and clicks land on it.
pub fn raise(pid: i32, id: u32) {
    apps::activate(pid);
    if let Some(w) = window(pid, id) {
        unsafe {
            w.set_attribute_value(&CFString::from_str("AXMain"), CFBoolean::new(true));
            w.perform_action(&CFString::from_str("AXRaise"));
        }
    }
}

/// Asks a window to be this size, in points; the app may keep it within
/// its own limits.
pub fn resize(pid: i32, id: u32, width: f64, height: f64) {
    let Some(w) = window(pid, id) else { return };
    // macOS keeps a window on its screen by shrinking a size that would
    // run past the screen's edge from where the window is, so it goes to
    // the top left first. It is pushed below the menu bar by macOS itself.
    let mut origin = CGPoint { x: 0.0, y: 0.0 };
    if let Some(value) = unsafe {
        AXValue::new(
            AXValueType::CGPoint,
            NonNull::from(&mut origin).cast::<c_void>(),
        )
    } {
        unsafe { w.set_attribute_value(&CFString::from_str("AXPosition"), &value) };
    }
    let mut size = CGSize { width, height };
    let Some(value) = (unsafe {
        AXValue::new(
            AXValueType::CGSize,
            NonNull::from(&mut size).cast::<c_void>(),
        )
    }) else {
        return;
    };
    // Some apps take a new size in steps, clamping it against the size they
    // had, so it is asked twice.
    for _ in 0..2 {
        unsafe { w.set_attribute_value(&CFString::from_str("AXSize"), &value) };
    }
}

/// Closes a window the way its close button does, so an app with unsaved
/// work asks about it first.
pub fn close(pid: i32, id: u32) {
    let Some(w) = window(pid, id) else { return };
    if let Some(button) = attribute(&w, "AXCloseButton") {
        if let Some(button) = button.downcast_ref::<AXUIElement>() {
            unsafe { button.perform_action(&CFString::from_str("AXPress")) };
        }
    }
}

/// An app's menu bar, less the Apple menu, which is macOS's rather than
/// the app's. Each item's id is its path of indices from the menu bar, which
/// is how `activate_menu` finds it again.
pub fn menus(pid: i32) -> Vec<Menu> {
    let app = unsafe { AXUIElement::new_application(pid) };
    let Some(bar) = attribute(&app, "AXMenuBar") else {
        return Vec::new();
    };
    let Some(bar) = bar.downcast_ref::<AXUIElement>() else {
        return Vec::new();
    };
    children(bar, "AXChildren")
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(i, top)| {
            let label = string(top, "AXTitle")?;
            let menu = children(top, "AXChildren").into_iter().next()?;
            Some(Menu {
                title: label,
                entries: entries(&menu, &i.to_string(), 0),
            })
        })
        .collect()
}

fn entries(menu: &AXUIElement, path: &str, depth: usize) -> Vec<MenuEntry> {
    children(menu, "AXChildren")
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let path = format!("{path}/{i}");
            let label = string(item, "AXTitle").unwrap_or_default();
            if label.is_empty() {
                return MenuEntry::Separator;
            }
            let submenu = children(item, "AXChildren").into_iter().next();
            // Submenus that fill themselves only when opened (Open Recent
            // and the like) are read as far as they are; a depth limit
            // keeps a pathological app from taking all day.
            if let Some(submenu) = submenu.filter(|_| depth < 4) {
                return MenuEntry::Submenu {
                    label,
                    entries: entries(&submenu, &path, depth + 1),
                };
            }
            let enabled = attribute(item, "AXEnabled")
                .and_then(|v| v.downcast_ref::<CFBoolean>().map(|b| b.as_bool()))
                .unwrap_or(true);
            MenuEntry::Item {
                id: path,
                label,
                shortcut: shortcut(item),
                enabled,
            }
        })
        .collect()
}

/// An item's key equivalent as the host writes shortcuts, Cmd+Shift+S.
fn shortcut(item: &AXUIElement) -> Option<String> {
    let key = string(item, "AXMenuItemCmdChar").filter(|k| !k.trim().is_empty())?;
    // kAXMenuItemModifier*: Shift 1, Option 2, Control 4, and 8 for "no
    // Command", Command being there unless said otherwise.
    let modifiers = attribute(item, "AXMenuItemCmdModifiers")
        .and_then(|v| v.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()))
        .unwrap_or(0);
    let mut out = String::new();
    if modifiers & 4 != 0 {
        out.push_str("Ctrl+");
    }
    if modifiers & 2 != 0 {
        out.push_str("Option+");
    }
    if modifiers & 1 != 0 {
        out.push_str("Shift+");
    }
    if modifiers & 8 == 0 {
        out.insert_str(0, "Cmd+");
    }
    out.push_str(&key.to_uppercase());
    Some(out)
}

/// Picks the menu item at `path` ("2/5", "2/7/1").
pub fn activate_menu(pid: i32, path: &str) -> Result<(), String> {
    let app = unsafe { AXUIElement::new_application(pid) };
    let bar = attribute(&app, "AXMenuBar").ok_or("the app has no menu bar")?;
    let mut element: CFRetained<AXUIElement> = bar
        .downcast::<AXUIElement>()
        .map_err(|_| "the app's menu bar is not an element")?;
    let mut indices = path.split('/').map(|i| {
        i.parse::<usize>()
            .map_err(|_| format!("bad menu item {path}"))
    });
    let first = indices.next().ok_or("empty menu item")??;
    element = children(&element, "AXChildren")
        .into_iter()
        .nth(first)
        .ok_or("no such menu")?;
    for index in indices {
        // Below each title is its menu, and in that the items.
        let menu = children(&element, "AXChildren")
            .into_iter()
            .next()
            .ok_or("no such menu")?;
        element = children(&menu, "AXChildren")
            .into_iter()
            .nth(index?)
            .ok_or("no such menu item")?;
    }
    let error = unsafe { element.perform_action(&CFString::from_str("AXPress")) };
    if error == AXError::Success {
        Ok(())
    } else {
        Err(format!("the menu item did not respond ({error:?})"))
    }
}
