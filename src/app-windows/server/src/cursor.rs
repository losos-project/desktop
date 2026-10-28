//! The pointer's look on the Mac, for the host to draw over the guest's
//! windows (`protocol::Cursor`).
//!
//! macOS tells nobody when the pointer changes look: an app sets it for
//! its own windows, and the window server shows it. `currentSystemCursor`
//! is what it shows, whichever app set it, as a picture and a hot spot,
//! so it is read a few times a second while a connection wants it. A
//! picture that is one of AppKit's own named cursors goes by its CSS name,
//! which the host draws from its own cursor theme in its own size and
//! style; any other, such as an app's custom pointer or the spinning wait
//! cursor, goes as the picture itself.
//!
//! Apple has marked `currentSystemCursor` as going to answer nothing in a
//! future macOS. Then nothing is sent, and the host keeps its own arrow,
//! as it did before this.

use std::thread;
use std::time::Duration;

use app_windows_protocol::{Cursor, CursorImage};
use base64::Engine;
use objc2::rc::{autoreleasepool, Retained};
use objc2::AllocAnyThread;
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSCursor, NSImage};
use objc2_foundation::{NSDictionary, NSPoint, NSSize};

use crate::server::Hub;

/// How often the pointer is looked at while someone watches it: a change
/// shows within a frame or three, and a read costs a small TIFF.
const POLL: Duration = Duration::from_millis(50);
/// How often to check whether anyone watches at all.
const IDLE: Duration = Duration::from_millis(500);

/// One of AppKit's named cursors, as the window server would show it.
struct Named {
    name: &'static str,
    tiff: Vec<u8>,
    size: NSSize,
    hotspot: NSPoint,
}

pub fn watch(hub: &Hub) {
    let named = autoreleasepool(|_| named());
    // The picture last sent, so an unchanged pointer sends nothing.
    let mut last: Option<Vec<u8>> = None;
    loop {
        if !hub.cursor_watched() {
            // Whoever watches next is sent the pointer as it is then.
            last = None;
            thread::sleep(IDLE);
            continue;
        }
        if let Some(cursor) = autoreleasepool(|_| look(&named, &mut last)) {
            hub.cursor(cursor);
        }
        thread::sleep(POLL);
    }
}

/// The pointer's look, if it changed since `last`.
fn look(named: &[Named], last: &mut Option<Vec<u8>>) -> Option<Cursor> {
    #[allow(deprecated)] // The only way to see another app's pointer; see above.
    let cursor = NSCursor::currentSystemCursor()?;
    let image = cursor.image();
    let tiff = image.TIFFRepresentation()?.to_vec();
    if last.as_ref() == Some(&tiff) {
        return None;
    }
    let (size, hotspot) = (image.size(), cursor.hotSpot());
    *last = Some(tiff);
    let tiff = last.as_deref().unwrap_or_default();
    if let Some(shape) = name_of(named, tiff, size, hotspot) {
        return Some(Cursor {
            shape: shape.to_owned(),
            image: None,
        });
    }
    eprintln!(
        "pointer: a picture of its own, {}x{} with its hot spot at {},{}",
        size.width, size.height, hotspot.x, hotspot.y
    );
    match picture(&image, hotspot) {
        Some(image) => Some(Cursor {
            shape: String::new(),
            image: Some(image),
        }),
        // Better the arrow than the last pointer, which is wrong now.
        None => Some(Cursor {
            shape: "default".to_owned(),
            image: None,
        }),
    }
}

/// Which named cursor a picture is: the one with the very same bytes, or
/// else the only one of its size with its hot spot in the same place,
/// since the window server may hand back the same picture encoded another
/// way.
fn name_of(named: &[Named], tiff: &[u8], size: NSSize, hotspot: NSPoint) -> Option<&'static str> {
    if let Some(n) = named.iter().find(|n| n.tiff == tiff) {
        return Some(n.name);
    }
    let close = |a: f64, b: f64| (a - b).abs() < 0.5;
    let mut alike = named.iter().filter(|n| {
        close(n.size.width, size.width)
            && close(n.size.height, size.height)
            && close(n.hotspot.x, hotspot.x)
            && close(n.hotspot.y, hotspot.y)
    });
    match (alike.next(), alike.next()) {
        (Some(n), None) => Some(n.name),
        _ => None,
    }
}

/// AppKit's named cursors with the CSS names Wayland's cursor-shape
/// protocol uses for them. The resize cursors are the ones from before
/// macOS 15, which apps still set.
#[allow(deprecated)]
fn named() -> Vec<Named> {
    let cursors: [(Retained<NSCursor>, &'static str); 17] = [
        (NSCursor::arrowCursor(), "default"),
        (NSCursor::IBeamCursor(), "text"),
        (NSCursor::IBeamCursorForVerticalLayout(), "vertical-text"),
        (NSCursor::pointingHandCursor(), "pointer"),
        (NSCursor::crosshairCursor(), "crosshair"),
        (NSCursor::openHandCursor(), "grab"),
        (NSCursor::closedHandCursor(), "grabbing"),
        (NSCursor::operationNotAllowedCursor(), "not-allowed"),
        (NSCursor::dragCopyCursor(), "copy"),
        (NSCursor::dragLinkCursor(), "alias"),
        (NSCursor::contextualMenuCursor(), "context-menu"),
        (NSCursor::resizeLeftRightCursor(), "ew-resize"),
        (NSCursor::resizeUpDownCursor(), "ns-resize"),
        (NSCursor::resizeLeftCursor(), "w-resize"),
        (NSCursor::resizeRightCursor(), "e-resize"),
        (NSCursor::resizeUpCursor(), "n-resize"),
        (NSCursor::resizeDownCursor(), "s-resize"),
    ];
    cursors
        .into_iter()
        .filter_map(|(cursor, name)| {
            let image = cursor.image();
            Some(Named {
                name,
                tiff: image.TIFFRepresentation()?.to_vec(),
                size: image.size(),
                hotspot: cursor.hotSpot(),
            })
        })
        .collect()
}

/// The pointer's picture as a PNG at its sharpest, with its hot spot in
/// that PNG's pixels.
fn picture(image: &NSImage, hotspot: NSPoint) -> Option<CursorImage> {
    let size = image.size();
    if size.width < 1.0 || size.height < 1.0 {
        return None;
    }
    // No rectangle asks for the image's best representation.
    let cg =
        unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
    let scale = (rep.pixelsWide() as f64 / size.width).round().max(1.0);
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(CursorImage {
        png: base64::engine::general_purpose::STANDARD.encode(png.to_vec()),
        scale: scale as u32,
        hotspot_x: (hotspot.x * scale).round().max(0.0) as u32,
        hotspot_y: (hotspot.y * scale).round().max(0.0) as u32,
    })
}
