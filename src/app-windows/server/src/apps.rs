//! The Mac's apps: which are installed, their icons, starting and bringing
//! them forward, and the clipboard they share.

use std::path::{Path, PathBuf};
use std::process::Command;

use app_windows_protocol::AppInfo;
use base64::Engine;
use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard,
    NSPasteboardTypeString, NSRunningApplication, NSWorkspace,
};
use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFBoolean, CFRunLoop, CFString, CGPoint, CGRect, CGSize};
use objc2_foundation::{NSBundle, NSDictionary, NSProcessInfo, NSString};

/// This program's own bundle identifier, whose windows (it has none) and
/// whose entry in the app list are left out.
pub const OWN_BUNDLE: &str = "io.github.losos-project.macos-app-server";

/// Where macOS and people put apps. Utilities is inside Applications, and
/// Cryptexes hold Safari on recent macOS.
const FOLDERS: &[&str] = &[
    "/Applications",
    "/Applications/Utilities",
    "/System/Applications",
    "/System/Applications/Utilities",
    "/System/Volumes/Preboot/Cryptexes/App/System/Applications",
];

/// The side of the icons sent, in pixels: the largest the host's app grid
/// draws at twice its scale.
const ICON: f64 = 128.0;

/// Every app a person could open from the Finder, alphabetically, with its
/// icon. Agents and helpers that never show in the Dock are left out.
pub fn installed() -> Vec<AppInfo> {
    let mut folders: Vec<PathBuf> = FOLDERS.iter().map(PathBuf::from).collect();
    if let Ok(home) = std::env::var("HOME") {
        folders.push(Path::new(&home).join("Applications"));
    }
    let mut apps: Vec<AppInfo> = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "app") {
                continue;
            }
            let Some(app) = describe(&path) else { continue };
            if app.id != OWN_BUNDLE && !apps.iter().any(|a| a.id == app.id) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

fn describe(path: &Path) -> Option<AppInfo> {
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(path.to_str()?))?;
    let id = bundle.bundleIdentifier()?.to_string();
    let info = |key: &str| {
        bundle
            .objectForInfoDictionaryKey(&NSString::from_str(key))
            .and_then(|v| v.downcast::<NSString>().ok())
            .map(|s| s.to_string())
    };
    let flag = |key: &str| {
        info(key).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            || bundle
                .objectForInfoDictionaryKey(&NSString::from_str(key))
                .and_then(|v| v.downcast::<objc2_foundation::NSNumber>().ok())
                .is_some_and(|n| n.boolValue())
    };
    if flag("LSUIElement") || flag("LSBackgroundOnly") {
        return None;
    }
    let name = info("CFBundleDisplayName")
        .or_else(|| info("CFBundleName"))
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    Some(AppInfo {
        id,
        name,
        icon: icon(path),
    })
}

/// An app's icon as Finder shows it, as a base64 PNG.
fn icon(path: &Path) -> Option<String> {
    let workspace = NSWorkspace::sharedWorkspace();
    let image = workspace.iconForFile(&NSString::from_str(path.to_str()?));
    let mut rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: ICON,
            height: ICON,
        },
    };
    let cg = unsafe { image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(base64::engine::general_purpose::STANDARD.encode(png.to_vec()))
}

/// The bundle identifier of a running process, or `None` for one that is
/// not an app (a daemon, a command-line tool).
pub fn bundle_of(pid: i32) -> Option<String> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    Some(app.bundleIdentifier()?.to_string())
}

/// Starts an app, or brings it forward when it is running, as opening it
/// from the Finder does.
pub fn launch(id: &str) -> Result<(), String> {
    let status = Command::new("/usr/bin/open")
        .arg("-b")
        .arg(id)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("macOS could not open {id}"))
    }
}

/// Makes an app the active one. Since macOS 14 an app in the background
/// cannot simply take activation for another, so this asks through
/// Accessibility as well, which window managers do.
pub fn activate(pid: i32) {
    if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        #[allow(deprecated)]
        app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
    }
    let element = unsafe { AXUIElement::new_application(pid) };
    unsafe {
        element.set_attribute_value(&CFString::from_str("AXFrontmost"), CFBoolean::new(true))
    };
}

pub fn computer_name() -> String {
    Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Mac".to_owned())
}

pub fn system_version() -> String {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    if v.patchVersion == 0 {
        format!("macOS {}.{}", v.majorVersion, v.minorVersion)
    } else {
        format!(
            "macOS {}.{}.{}",
            v.majorVersion, v.minorVersion, v.patchVersion
        )
    }
}

/// The clipboard's text and its change count, which goes up with every
/// change anyone makes.
pub fn clipboard() -> (isize, Option<String>) {
    let board = NSPasteboard::generalPasteboard();
    let text = unsafe { board.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string());
    (board.changeCount(), text)
}

pub fn set_clipboard(text: &str) {
    let board = NSPasteboard::generalPasteboard();
    board.clearContents();
    unsafe { board.setString_forType(&NSString::from_str(text), NSPasteboardTypeString) };
}

/// Runs the main thread's run loop, forever.
pub fn run_main_loop() {
    CFRunLoop::run();
}

#[allow(dead_code)]
fn _unused(_: Retained<NSString>) {}
