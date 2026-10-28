//! Which windows there are, and a video stream of each.
//!
//! Ten times a second the window thread asks the window server which windows
//! are on screen (CGWindowListCopyWindowInfo, cheap and in front-to-back
//! order) and tells watching clients what opened, moved, changed or closed.
//! A window it has not seen before gets a ScreenCaptureKit stream of its
//! own (`SCContentFilter(desktopIndependentWindow:)`), which sees the window
//! whole whatever covers it on macOS's screen, and only sends a picture when
//! the window's content changed. A popup is captured from the display
//! instead, cut to its own frame with nothing but it in the picture: menus
//! are drawn over a backdrop of the whole screen, which a window-alone
//! capture squeezes into the menu's size. Each picture is encoded by its own
//! VideoToolbox session: H.264, real time, no reordered frames, so every
//! picture can be shown the moment it arrives.

use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use app_windows_protocol::{Message, Video, WindowInfo, WindowKind, WindowRef};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass};
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowNumber,
    kCGWindowOwnerPID, CGDisplayCopyDisplayMode, CGDisplayMode, CGMainDisplayID,
    CGWindowListCopyWindowInfo, CGWindowListOption,
};
use objc2_core_media::{
    kCMSampleAttachmentKey_NotSync, kCMVideoCodecType_H264, CMSampleBuffer, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
};
use objc2_core_video::{
    kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType, SCWindow,
};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxFrameDelayCount, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_High_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl, VTCompressionSession,
    VTEncodeInfoFlags, VTSessionCopyProperty, VTSessionSetProperty,
};

use crate::apps;
use crate::server::{Hub, Tracked};

/// How often the window list is read: often enough that a menu shows
/// about when it would on a Mac.
const TICK: Duration = Duration::from_millis(100);

/// How long a window ScreenCaptureKit did not offer waits before it is
/// asked about again.
const RETRY: Duration = Duration::from_millis(200);
const RETRY_MAX: Duration = Duration::from_secs(10);

/// The most pictures a second any window sends.
const FPS: i32 = 60;

/// The smallest window that is an app's own, in points. macOS 15 puts a
/// small window of the app's over the corner of every window being
/// captured, and apps keep tiny helper windows of their own; neither is
/// anything a person would open.
const SMALLEST: (f64, f64) = (60.0, 30.0);

/// Apps whose windows are macOS's own furniture, never an app's window: the
/// Dock, the menu bar's extras, notifications, Spotlight, the wallpaper.
const FURNITURE: &[&str] = &[
    "com.apple.dock",
    "com.apple.WindowManager",
    "com.apple.controlcenter",
    "com.apple.systemuiserver",
    "com.apple.notificationcenterui",
    "com.apple.Spotlight",
    "com.apple.loginwindow",
    "com.apple.TextInputMenuAgent",
    "com.apple.TextInputSwitcher",
    "com.apple.wallpaper.agent",
    "com.apple.screencaptureui",
    "com.apple.ScreenContinuity",
    "com.apple.universalcontrol",
    apps::OWN_BUNDLE,
];

/// One window as the window server lists it.
struct Listed {
    id: u32,
    pid: i32,
    layer: i64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    title: String,
}

/// The window a sheet hangs from, if `w` is one: an untitled window of the
/// same app wholly inside the frontmost of that app's windows behind it, and
/// centred across it. macOS centres every sheet on its window this way; a
/// document window that happens to lie inside another has a title, and one
/// without is rarely centred to the point.
fn sheet_of(w: &Listed, behind: &[Listed]) -> Option<u32> {
    if !w.title.is_empty() {
        return None;
    }
    let under = behind.iter().find(|o| {
        o.pid == w.pid
            && kind_for_layer(o.layer) == Some(WindowKind::Normal)
            && o.width >= SMALLEST.0
            && o.height >= SMALLEST.1
    })?;
    let inside = w.x >= under.x
        && w.y >= under.y
        && w.x + w.width <= under.x + under.width
        && w.y + w.height <= under.y + under.height;
    let centred = ((w.x + w.width / 2.0) - (under.x + under.width / 2.0)).abs() <= 1.0;
    (inside && centred && (w.width, w.height) != (under.width, under.height)).then_some(under.id)
}

/// Whether the window server's layer is one of an app's own windows
/// (normal, floating, panels, utility windows) or what pops up over them
/// (menus, tooltips), as opposed to the Dock, menu bar and screen savers.
fn kind_for_layer(layer: i64) -> Option<WindowKind> {
    match layer {
        0 => Some(WindowKind::Normal),
        1..=19 | 101 | 102 | 200 => Some(WindowKind::Popup),
        _ => None,
    }
}

fn number(dict: &NSDictionary<NSString, AnyObject>, key: &CFString) -> Option<f64> {
    let key: &NSString = toll_free(key);
    let value = dict.objectForKey(key)?;
    let number = value.downcast_ref::<NSNumber>()?;
    Some(number.doubleValue())
}

/// A CoreFoundation object as the Foundation class it is toll-free bridged
/// with.
fn toll_free<C: ?Sized, F>(cf: &C) -> &F {
    // SAFETY: only used between toll-free bridged pairs (CFString and
    // NSString, CFDictionary and NSDictionary, CFArray and NSArray), which
    // are the same object.
    unsafe { &*(cf as *const C as *const F) }
}

/// The on-screen windows, front to back.
fn list() -> Vec<Listed> {
    let options =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(array) = CGWindowListCopyWindowInfo(options, 0) else {
        return Vec::new();
    };
    let array: &NSArray<NSDictionary<NSString, AnyObject>> = toll_free(&*array);
    let mut out = Vec::new();
    for dict in array.iter() {
        let (Some(id), Some(pid), Some(layer)) = (
            number(&dict, unsafe { kCGWindowNumber }),
            number(&dict, unsafe { kCGWindowOwnerPID }),
            number(&dict, unsafe { kCGWindowLayer }),
        ) else {
            continue;
        };
        // A window made fully transparent is there to catch the pointer,
        // not to be seen.
        if number(&dict, unsafe { kCGWindowAlpha }).unwrap_or(1.0) <= 0.0 {
            continue;
        }
        let bounds: &NSString = toll_free(unsafe { kCGWindowBounds });
        let Some(bounds) = dict.objectForKey(bounds) else {
            continue;
        };
        let Some(bounds) = bounds.downcast_ref::<NSDictionary>() else {
            continue;
        };
        let bounds: &NSDictionary<NSString, AnyObject> = toll_free(bounds);
        let field = |name: &str| {
            bounds
                .objectForKey(&NSString::from_str(name))
                .and_then(|v| v.downcast_ref::<NSNumber>().map(|n| n.doubleValue()))
                .unwrap_or(0.0)
        };
        let title_key: &NSString = toll_free(unsafe { kCGWindowName });
        let title = dict
            .objectForKey(title_key)
            .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()))
            .unwrap_or_default();
        out.push(Listed {
            id: id as u32,
            pid: pid as i32,
            layer: layer as i64,
            x: field("X"),
            y: field("Y"),
            width: field("Width"),
            height: field("Height"),
            title,
        });
    }
    out
}

/// Pixels per point on the main display: 2 on a Retina Mac, 1 in a
/// machine's virtual display.
fn scale() -> f64 {
    let mode = CGDisplayCopyDisplayMode(CGMainDisplayID());
    let points = CGDisplayMode::width(mode.as_deref());
    let pixels = CGDisplayMode::pixel_width(mode.as_deref());
    if points == 0 || pixels == 0 {
        1.0
    } else {
        pixels as f64 / points as f64
    }
}

/// An Objective-C object moved to another thread. ScreenCaptureKit's
/// objects are thread-safe; objc2 just cannot know that.
struct Sendable<T>(T);
// SAFETY: see above; only ScreenCaptureKit's immutable objects are wrapped.
unsafe impl<T> Send for Sendable<T> {}

/// ScreenCaptureKit's handle on each window, which is what a stream
/// captures, by window number, and on each display, which popups are cut
/// from.
struct Shareable {
    windows: HashMap<u32, Retained<SCWindow>>,
    displays: Vec<Retained<SCDisplay>>,
}

fn shareable() -> Shareable {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            if !error.is_null() {
                let error = unsafe { &*error };
                eprintln!(
                    "listing windows to capture: {}",
                    error.localizedDescription()
                );
            }
            let _ = tx.send(Sendable(unsafe { Retained::retain(content) }));
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &block,
        );
    }
    let Ok(Sendable(Some(content))) = rx.recv_timeout(Duration::from_secs(5)) else {
        return Shareable {
            windows: HashMap::new(),
            displays: Vec::new(),
        };
    };
    let windows = unsafe { content.windows() };
    Shareable {
        windows: windows
            .iter()
            .map(|w| (unsafe { w.windowID() }, w))
            .collect(),
        displays: unsafe { content.displays() }.iter().collect(),
    }
}

/// Where a popup is on its display, in the display's own points, and the
/// display's origin among all displays.
fn cut(info: &WindowInfo, display: CGRect) -> CGRect {
    CGRect {
        origin: CGPoint {
            x: info.x - display.origin.x,
            y: info.y - display.origin.y,
        },
        size: CGSize {
            width: info.width,
            height: info.height,
        },
    }
}

/// The display a window's top left corner is on.
fn display_of(displays: &[Retained<SCDisplay>], info: &WindowInfo) -> Option<Retained<SCDisplay>> {
    displays
        .iter()
        .find(|d| {
            let f = unsafe { d.frame() };
            info.x >= f.origin.x
                && info.y >= f.origin.y
                && info.x < f.origin.x + f.size.width
                && info.y < f.origin.y + f.size.height
        })
        .or(displays.first())
        .cloned()
}

/// One window's stream and what encodes it.
struct Stream {
    stream: Retained<SCStream>,
    output: Retained<WindowOutput>,
    /// The size in pixels the stream was last configured for.
    width: usize,
    height: usize,
    /// For a popup, the frame of the display it is cut from and the part
    /// cut, last configured.
    display: Option<CGRect>,
    source: Option<CGRect>,
}

/// The window thread: forever, the window list, its changes, the streams.
pub fn track(hub: &Arc<Hub>) {
    let mut streams: HashMap<u32, Stream> = HashMap::new();
    let mut bundles: HashMap<i32, Option<String>> = HashMap::new();
    // Windows ScreenCaptureKit did not offer, and when to ask again: it
    // learns of a new window a moment after the window server does, and a
    // menu is often asked about within that moment. One that stays
    // unoffered, as every window does without Screen Recording, is asked
    // about less and less often.
    let mut unshareable: HashMap<u32, (Instant, Duration)> = HashMap::new();
    loop {
        thread::sleep(TICK);
        let scale = scale();
        let listed = list();

        // Each popup belongs over the frontmost normal window of its app
        // behind it, which is the one it opened from.
        let mut current: BTreeMap<u32, (WindowInfo, i32)> = BTreeMap::new();
        for (i, w) in listed.iter().enumerate() {
            let Some(kind) = kind_for_layer(w.layer) else {
                continue;
            };
            if w.width < 1.0 || w.height < 1.0 {
                continue;
            }
            if kind == WindowKind::Normal && (w.width < SMALLEST.0 || w.height < SMALLEST.1) {
                continue;
            }
            let bundle = bundles
                .entry(w.pid)
                .or_insert_with(|| apps::bundle_of(w.pid))
                .clone();
            let Some(app) = bundle else { continue };
            if FURNITURE.contains(&app.as_str()) {
                continue;
            }
            let parent = match kind {
                WindowKind::Normal => None,
                WindowKind::Popup => {
                    let behind = listed[i + 1..].iter().find(|o| {
                        o.pid == w.pid
                            && kind_for_layer(o.layer) == Some(WindowKind::Normal)
                            && o.width >= SMALLEST.0
                            && o.height >= SMALLEST.1
                    });
                    match behind {
                        Some(o) => Some(o.id),
                        // A panel with nothing of its app under it is a
                        // window in its own right.
                        None if w.layer < 100 => None,
                        None => continue,
                    }
                }
            };
            // A sheet hangs from its window's title bar and is shown over
            // it the way a popup is. A window-alone capture of a sheet is of
            // the window it hangs from as well, squeezed into the sheet's
            // size, so a sheet is cut from the display as a popup is.
            let (kind, parent) = match (kind, parent) {
                (WindowKind::Popup, None) => (WindowKind::Normal, None),
                (WindowKind::Normal, _) => match sheet_of(w, &listed[i + 1..]) {
                    Some(id) => (WindowKind::Popup, Some(id)),
                    None => (WindowKind::Normal, None),
                },
                other => other,
            };
            current.insert(
                w.id,
                (
                    WindowInfo {
                        window: w.id,
                        app,
                        title: w.title.clone(),
                        x: w.x,
                        y: w.y,
                        width: w.width,
                        height: w.height,
                        scale,
                        kind,
                        parent,
                    },
                    w.pid,
                ),
            );
        }
        // Processes come and go with their numbers reused.
        bundles.retain(|pid, _| listed.iter().any(|w| w.pid == *pid));

        let watched = hub.watched();
        let mut events = Vec::new();
        {
            let mut windows = hub.windows.lock().unwrap();
            windows.retain(|id, _| {
                let keep = current.contains_key(id);
                if !keep {
                    events.push(Message::WindowClosed(WindowRef { window: *id }));
                }
                keep
            });
            for (id, (info, pid)) in &current {
                match windows.get_mut(id) {
                    Some(known) if known.info == *info => {}
                    Some(known) => {
                        known.info = info.clone();
                        events.push(Message::WindowChanged(info.clone()));
                    }
                    None => {
                        windows.insert(
                            *id,
                            Tracked {
                                info: info.clone(),
                                pid: *pid,
                                keyframe: Arc::new(AtomicBool::new(true)),
                            },
                        );
                        events.push(Message::WindowOpened(info.clone()));
                    }
                }
            }
        }
        for event in &events {
            hub.broadcast(event);
        }

        // Streams follow: none at all while nobody watches.
        streams.retain(|id, s| {
            let keep = watched && current.contains_key(id);
            if !keep {
                stop(s);
            }
            keep
        });
        unshareable.retain(|id, _| current.contains_key(id));
        if !watched {
            continue;
        }
        let missing: Vec<u32> = current
            .keys()
            .filter(|id| {
                !streams.contains_key(id)
                    && unshareable
                        .get(id)
                        .is_none_or(|(at, wait)| at.elapsed() >= *wait)
            })
            .copied()
            .collect();
        if !missing.is_empty() {
            let shareable = shareable();
            for id in missing {
                let Some(window) = shareable.windows.get(&id) else {
                    // ScreenCaptureKit does not offer every window the
                    // window server lists (without Screen Recording, none).
                    if !unshareable.contains_key(&id) {
                        eprintln!("window {id}: not offered by ScreenCaptureKit yet");
                    }
                    let wait = unshareable
                        .get(&id)
                        .map_or(RETRY, |(_, w)| (*w * 2).min(RETRY_MAX));
                    unshareable.insert(id, (Instant::now(), wait));
                    continue;
                };
                let Some(tracked) = hub.window(id) else {
                    continue;
                };
                let display = match tracked.info.kind {
                    WindowKind::Popup => display_of(&shareable.displays, &tracked.info),
                    WindowKind::Normal => None,
                };
                match start(hub, window, display.as_deref(), &tracked) {
                    Ok(stream) => {
                        eprintln!(
                            "window {id}: capturing {} \"{}\" at {}x{}{}",
                            tracked.info.app,
                            tracked.info.title,
                            stream.width,
                            stream.height,
                            if display.is_some() {
                                ", cut from its display"
                            } else {
                                ""
                            }
                        );
                        streams.insert(id, stream);
                    }
                    Err(e) => {
                        eprintln!("capturing window {id}: {e}");
                        let wait = unshareable
                            .get(&id)
                            .map_or(RETRY, |(_, w)| (*w * 2).min(RETRY_MAX));
                        unshareable.insert(id, (Instant::now(), wait));
                    }
                }
            }
        }
        for (id, stream) in streams.iter_mut() {
            stream.output.still();
            let Some((info, _)) = current.get(id) else {
                continue;
            };
            let (width, height) = pixel_size(info);
            let source = stream.display.map(|d| cut(info, d));
            if (width, height) != (stream.width, stream.height) || source != stream.source {
                stream.width = width;
                stream.height = height;
                stream.source = source;
                let config = configuration(width, height, source);
                unsafe {
                    stream
                        .stream
                        .updateConfiguration_completionHandler(&config, None)
                };
            }
        }
    }
}

/// A window's size in pixels, even, as 4:2:0 video needs.
fn pixel_size(info: &WindowInfo) -> (usize, usize) {
    let even = |v: f64| ((v * info.scale).round() as usize + 1) & !1;
    (even(info.width).max(2), even(info.height).max(2))
}

fn configuration(
    width: usize,
    height: usize,
    source: Option<CGRect>,
) -> Retained<SCStreamConfiguration> {
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(width);
        config.setHeight(height);
        if let Some(source) = source {
            config.setSourceRect(source);
        }
        config.setMinimumFrameInterval(CMTime {
            value: 1,
            timescale: FPS,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        });
        // NV12, which VideoToolbox encodes without converting.
        config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        // The host draws its own pointer over the window.
        config.setShowsCursor(false);
        // One more than the four it had: the window's latest picture is
        // kept (`OutputIvars::last`), and a stream whose surfaces are all
        // held stops sending.
        config.setQueueDepth(5);
        // macOS 14 and later draw a window's shadow into its picture unless
        // told not to; the host draws its own window's edges.
        if config.respondsToSelector(sel!(setIgnoreShadowsSingleWindow:)) {
            config.setIgnoreShadowsSingleWindow(true);
        }
        config
    }
}

fn start(
    hub: &Arc<Hub>,
    window: &SCWindow,
    display: Option<&SCDisplay>,
    tracked: &Tracked,
) -> Result<Stream, String> {
    let (width, height) = pixel_size(&tracked.info);
    let output = WindowOutput::new(OutputIvars {
        window: tracked.info.window,
        hub: hub.clone(),
        keyframe: tracked.keyframe.clone(),
        encoder: Mutex::new(None),
        epoch: Instant::now(),
        last: Mutex::new(None),
        samples: AtomicU32::new(0),
    });
    unsafe {
        let display_frame = display.map(|d| d.frame());
        let source = display_frame.map(|d| cut(&tracked.info, d));
        let filter = match display {
            Some(display) => SCContentFilter::initWithDisplay_includingWindows(
                SCContentFilter::alloc(),
                display,
                &NSArray::from_slice(&[window]),
            ),
            None => {
                SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
            }
        };
        let config = configuration(width, height, source);
        let delegate = ProtocolObject::<dyn SCStreamDelegate>::from_ref(&*output);
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            Some(delegate),
        );
        let queue = DispatchQueue::new(&format!("window-{}", tracked.info.window), None);
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
            .map_err(|e| e.localizedDescription().to_string())?;
        let id = tracked.info.window;
        let started = RcBlock::new(move |error: *mut NSError| {
            if let Some(error) = error.as_ref() {
                eprintln!(
                    "starting capture of window {id}: {}",
                    error.localizedDescription()
                );
            }
        });
        stream.startCaptureWithCompletionHandler(Some(&started));
        Ok(Stream {
            stream,
            output,
            width,
            height,
            display: display_frame,
            source,
        })
    }
}

fn stop(stream: &Stream) {
    unsafe { stream.stream.stopCaptureWithCompletionHandler(None) };
    stream.output.ivars().encoder.lock().unwrap().take();
}

pub struct OutputIvars {
    window: u32,
    hub: Arc<Hub>,
    keyframe: Arc<AtomicBool>,
    encoder: Mutex<Option<Encoder>>,
    epoch: Instant,
    /// The window's latest picture. ScreenCaptureKit sends nothing while a
    /// window stays the same, so a keyframe asked for then is made from it.
    last: Mutex<Option<Last>>,
    /// Buffers ScreenCaptureKit has sent, for the log to say what the first
    /// few were.
    samples: AtomicU32,
}

#[derive(Clone)]
struct Last {
    image: CFRetained<objc2_core_video::CVImageBuffer>,
    at: Instant,
    /// Whether it has been pushed out of the encoder since it came.
    flushed: bool,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and WindowOutput
    // does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "LososWindowOutput"]
    #[ivars = OutputIvars]
    struct WindowOutput;

    unsafe impl NSObjectProtocol for WindowOutput {}

    unsafe impl SCStreamOutput for WindowOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                self.picture(sample);
            }
        }
    }

    unsafe impl SCStreamDelegate for WindowOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            eprintln!(
                "capture of window {} stopped: {}",
                self.ivars().window,
                error.localizedDescription()
            );
        }
    }
);

impl WindowOutput {
    fn new(ivars: OutputIvars) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    /// One picture from ScreenCaptureKit, on the window's own queue.
    fn picture(&self, sample: &CMSampleBuffer) {
        let ivars = self.ivars();
        let status = frame_status(sample);
        let image = unsafe { sample.image_buffer() };
        if ivars.samples.fetch_add(1, Ordering::Relaxed) < SAMPLES_LOGGED {
            eprintln!(
                "window {}: ScreenCaptureKit sent status {status:?}{}",
                ivars.window,
                if image.is_some() { "" } else { ", no picture" }
            );
        }
        // ScreenCaptureKit also sends a buffer when nothing changed (idle)
        // or the window is hidden. Complete frames carry a picture, and so
        // does the first a stream sends on some Macs, marked started.
        if !matches!(status, Some(FRAME_COMPLETE | FRAME_STARTED)) {
            return;
        }
        let Some(image) = image else {
            return;
        };
        let first = ivars.last.lock().unwrap().replace(Last {
            image: image.clone(),
            at: Instant::now(),
            flushed: false,
        });
        if first.is_none() {
            eprintln!("window {}: first picture", ivars.window);
        }
        self.encode(&image);
    }

    /// Called each tick, off the window's queue. Once a window has stayed
    /// the same a moment, its latest picture is pushed out of the encoder,
    /// which may hold a picture back until the next one comes, and that
    /// for a still window is never. It is made again as a keyframe if one
    /// was asked for meanwhile: by a connection that has just started
    /// watching, or a decoder that lost its place.
    fn still(&self) {
        let ivars = self.ivars();
        let last = ivars.last.lock().unwrap().clone();
        let Some(last) = last else { return };
        // A window that is changing sends its own pictures in a moment.
        if last.at.elapsed() < REPEAT_AFTER {
            return;
        }
        let repeat = ivars.keyframe.load(Ordering::Relaxed);
        if !repeat && last.flushed {
            return;
        }
        if repeat {
            self.encode(&last.image);
        }
        if let Some(encoder) = ivars.encoder.lock().unwrap().as_ref() {
            encoder.flush();
        }
        let mut now = ivars.last.lock().unwrap();
        // Unless a new picture came meanwhile, which is the encoder's again.
        if let Some(now) = now.as_mut().filter(|n| n.at == last.at) {
            now.flushed = true;
            if repeat {
                now.at = Instant::now();
            }
        }
    }

    fn encode(&self, image: &objc2_core_video::CVImageBuffer) {
        let ivars = self.ivars();
        let width = CVPixelBufferGetWidth(image);
        let height = CVPixelBufferGetHeight(image);
        let mut encoder = ivars.encoder.lock().unwrap();
        if encoder
            .as_ref()
            .is_none_or(|e| e.width != width || e.height != height)
        {
            *encoder = Encoder::new(
                width,
                height,
                ivars.window,
                ivars.hub.clone(),
                ivars.keyframe.clone(),
            );
            ivars.keyframe.store(true, Ordering::Relaxed);
        }
        let Some(encoder) = encoder.as_ref() else {
            return;
        };
        let keyframe = ivars.keyframe.swap(false, Ordering::Relaxed);
        let micros = ivars.epoch.elapsed().as_micros() as i64;
        encoder.encode(image, micros, keyframe);
    }
}

/// SCStreamFrameStatus values that carry a picture.
const FRAME_COMPLETE: i64 = 0;
const FRAME_STARTED: i64 = 4;

/// How long a window must have stayed the same before its last picture is
/// pushed out of the encoder, or made again for a keyframe asked for,
/// rather than the next picture waited for.
const REPEAT_AFTER: Duration = Duration::from_millis(150);

/// How many of a stream's first buffers the log describes: enough to see
/// how a stream starts on a Mac that shows a window late or never.
const SAMPLES_LOGGED: u32 = 3;

/// SCStreamFrameInfoStatus of a sample: 0 complete, 1 idle, 2 blank, ...
fn frame_status(sample: &CMSampleBuffer) -> Option<i64> {
    let attachments = unsafe { sample.sample_attachments_array(false) }?;
    let attachments: &NSArray<NSDictionary<NSString, AnyObject>> = toll_free(&*attachments);
    let first = attachments.firstObject()?;
    let status = first.objectForKey(unsafe { SCStreamFrameInfoStatus })?;
    Some(status.downcast_ref::<NSNumber>()?.integerValue() as i64)
}

/// Whether this Mac encodes in hardware: answered once, by an encoder made
/// for the purpose, for the welcome a client gets.
pub fn encoder_kind() -> &'static str {
    static KIND: OnceLock<&'static str> = OnceLock::new();
    KIND.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        match Encoder::open(1280, 720) {
            Some((session, true)) => {
                drop((session, flag));
                "hardware"
            }
            Some(_) => "software",
            None => "none",
        }
    })
}

/// What the encoder's output callback needs: where pictures go.
struct Sink {
    window: u32,
    width: u32,
    height: u32,
    hub: Arc<Hub>,
    keyframe: Arc<AtomicBool>,
    /// Whether a picture has left the encoder yet, for the log.
    sent: AtomicBool,
}

struct Encoder {
    session: CFRetained<VTCompressionSession>,
    // Boxed so its address, which the session holds, stays put; dropped
    // only after the session is invalidated (Drop below).
    sink: *mut Sink,
    width: usize,
    height: usize,
}

// SAFETY: a compression session may be used from any thread, one at a time,
// which the Mutex around each Encoder makes sure of.
unsafe impl Send for Encoder {}

impl Encoder {
    /// A compression session for pictures of this size, in hardware when
    /// the Mac has an encoder for it, and whether it does.
    fn open(width: usize, height: usize) -> Option<(CFRetained<VTCompressionSession>, bool)> {
        Self::open_with(width, height, None, ptr::null_mut())
    }

    fn open_with(
        width: usize,
        height: usize,
        callback: objc2_video_toolbox::VTCompressionOutputCallback,
        refcon: *mut c_void,
    ) -> Option<(CFRetained<VTCompressionSession>, bool)> {
        let yes = CFBoolean::new(true);
        // Low-latency rate control first, which Apple's hardware encoders
        // have; then any hardware encoder; then whatever encodes at all,
        // which in a machine without a media engine is the software one.
        let specs: [Option<CFRetained<CFDictionary<CFString, CFBoolean>>>; 3] = unsafe {
            [
                Some(CFDictionary::from_slices(
                    &[
                        kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
                        kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                    ],
                    &[yes, yes],
                )),
                Some(CFDictionary::from_slices(
                    &[kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder],
                    &[yes],
                )),
                None,
            ]
        };
        for spec in &specs {
            let mut raw: *mut VTCompressionSession = ptr::null_mut();
            let status = unsafe {
                VTCompressionSession::create(
                    None,
                    width as i32,
                    height as i32,
                    kCMVideoCodecType_H264,
                    spec.as_deref().map(|d| d.as_opaque()),
                    None,
                    None,
                    callback,
                    refcon,
                    NonNull::from(&mut raw),
                )
            };
            let Some(raw) = NonNull::new(raw) else {
                continue;
            };
            if status != 0 {
                continue;
            }
            let session = unsafe { CFRetained::from_raw(raw) };
            let hardware = unsafe {
                let mut value: *const CFType = ptr::null();
                VTSessionCopyProperty(
                    &session,
                    kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                    None,
                    (&mut value as *mut *const CFType).cast(),
                ) == 0
                    && !value.is_null()
                    && {
                        let value =
                            CFRetained::from_raw(NonNull::new_unchecked(value as *mut CFType));
                        value
                            .downcast_ref::<CFBoolean>()
                            .is_some_and(|b| b.as_bool())
                    }
            };
            return Some((session, hardware));
        }
        None
    }

    fn new(
        width: usize,
        height: usize,
        window: u32,
        hub: Arc<Hub>,
        keyframe: Arc<AtomicBool>,
    ) -> Option<Self> {
        let sink = Box::into_raw(Box::new(Sink {
            window,
            width: width as u32,
            height: height as u32,
            hub,
            keyframe,
            sent: AtomicBool::new(false),
        }));
        let Some((session, _)) = Self::open_with(width, height, Some(encoded), sink.cast()) else {
            drop(unsafe { Box::from_raw(sink) });
            eprintln!("no H.264 encoder for {width}x{height}");
            return None;
        };
        let set = |key: &CFString, value: &CFType| unsafe {
            VTSessionSetProperty(&session, key, Some(value));
        };
        unsafe {
            set(kVTCompressionPropertyKey_RealTime, CFBoolean::new(true));
            // No B-frames: each picture shows the moment it is decoded.
            set(
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false),
            );
            // Nor any picture held back to look ahead, which the software
            // encoder does otherwise: a window's last change would show
            // only with its next. Not every encoder takes it; `flush`
            // covers those.
            set(
                kVTCompressionPropertyKey_MaxFrameDelayCount,
                &CFNumber::new_i32(0),
            );
            set(
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_High_AutoLevel,
            );
            set(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::new_i32(FPS),
            );
            // Pictures only come when something changed, so keyframes are
            // asked for when a client needs one rather than on a timer.
            set(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::new_i32(i32::MAX),
            );
            // A tenth of a bit per pixel at the frame rate, the rate text
            // stays sharp at, within what a gigabit link carries easily.
            let rate = ((width * height) as f64 * FPS as f64 * 0.1).clamp(2e6, 80e6);
            set(
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::new_i32(rate as i32),
            );
            // Said in the stream itself so the host's decoder converts the
            // colours the way they were captured.
            set(
                kVTCompressionPropertyKey_ColorPrimaries,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
            );
            set(
                kVTCompressionPropertyKey_TransferFunction,
                kCVImageBufferTransferFunction_ITU_R_709_2,
            );
            set(
                kVTCompressionPropertyKey_YCbCrMatrix,
                kCVImageBufferYCbCrMatrix_ITU_R_709_2,
            );
            session.prepare_to_encode_frames();
        }
        Some(Encoder {
            session,
            sink,
            width,
            height,
        })
    }

    fn encode(&self, image: &objc2_core_video::CVImageBuffer, micros: i64, keyframe: bool) {
        let force;
        let properties = if keyframe {
            force = CFDictionary::<CFString, CFBoolean>::from_slices(
                &[unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }],
                &[CFBoolean::new(true)],
            );
            Some(force.as_opaque())
        } else {
            None
        };
        let time = CMTime {
            value: micros,
            timescale: 1_000_000,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        let status = unsafe {
            self.session.encode_frame(
                image,
                time,
                CMTime {
                    value: 0,
                    timescale: 0,
                    flags: CMTimeFlags::empty(),
                    epoch: 0,
                },
                properties,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status != 0 {
            eprintln!("encoding a picture of window {}: {status}", unsafe {
                (*self.sink).window
            });
        }
    }
}

impl Encoder {
    /// Has every picture given so far come out.
    fn flush(&self) {
        let all = CMTime {
            value: 0,
            timescale: 0,
            flags: CMTimeFlags::empty(),
            epoch: 0,
        };
        let status = unsafe { self.session.complete_frames(all) };
        if status != 0 {
            eprintln!("finishing pictures of window {}: {status}", unsafe {
                (*self.sink).window
            });
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            // Flushes and stops the callback before its sink goes.
            self.session.invalidate();
            drop(Box::from_raw(self.sink));
        }
    }
}

/// VideoToolbox's callback with each encoded picture.
unsafe extern "C-unwind" fn encoded(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    if status != 0 || refcon.is_null() {
        return;
    }
    let sink = unsafe { &*(refcon as *const Sink) };
    let Some(sample) = (unsafe { sample.as_ref() }) else {
        // A dropped frame: the next one has to start afresh.
        sink.keyframe.store(true, Ordering::Relaxed);
        return;
    };
    let Some((data, keyframe)) = annex_b(sample) else {
        return;
    };
    let micros = unsafe { sample.presentation_time_stamp() }.value.max(0) as u64;
    if !sink.sent.swap(true, Ordering::Relaxed) {
        eprintln!(
            "window {}: first picture sent, {} bytes{}",
            sink.window,
            data.len(),
            if keyframe { ", a keyframe" } else { "" }
        );
    }
    sink.hub.video(
        Video {
            window: sink.window,
            width: sink.width,
            height: sink.height,
            keyframe,
            micros,
            data,
        },
        &sink.keyframe,
    );
}

/// An encoded picture as H.264 Annex B, start codes before each NAL unit
/// and the parameter sets before a keyframe, and whether it is one.
/// VideoToolbox gives length-prefixed units with the parameter sets kept
/// aside in the format description.
fn annex_b(sample: &CMSampleBuffer) -> Option<(Vec<u8>, bool)> {
    const START: [u8; 4] = [0, 0, 0, 1];
    let keyframe = unsafe { sample.sample_attachments_array(false) }
        .and_then(|a| {
            let a: &NSArray<NSDictionary<NSString, AnyObject>> = toll_free(&*a);
            let key: &NSString = toll_free(unsafe { kCMSampleAttachmentKey_NotSync });
            a.firstObject().map(|d| {
                d.objectForKey(key)
                    .is_none_or(|v| v.downcast_ref::<NSNumber>().is_some_and(|n| !n.boolValue()))
            })
        })
        .unwrap_or(true);
    let mut out = Vec::new();
    if keyframe {
        let description = unsafe { sample.format_description() }?;
        let mut count = 0usize;
        unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                &description,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut count,
                ptr::null_mut(),
            );
        }
        for i in 0..count {
            let mut set: *const u8 = ptr::null();
            let mut size = 0usize;
            let status = unsafe {
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    &description,
                    i,
                    &mut set,
                    &mut size,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if status == 0 && !set.is_null() {
                out.extend_from_slice(&START);
                out.extend_from_slice(unsafe { std::slice::from_raw_parts(set, size) });
            }
        }
    }
    let block = unsafe { sample.data_buffer() }?;
    let length = unsafe { block.data_length() };
    let mut units = vec![0u8; length];
    let status =
        unsafe { block.copy_data_bytes(0, length, NonNull::new(units.as_mut_ptr().cast())?) };
    if status != 0 {
        return None;
    }
    let mut at = 0;
    while at + 4 <= units.len() {
        let size = u32::from_be_bytes(units[at..at + 4].try_into().unwrap()) as usize;
        at += 4;
        if at + size > units.len() {
            break;
        }
        out.extend_from_slice(&START);
        out.extend_from_slice(&units[at..at + size]);
        at += size;
    }
    Some((out, keyframe))
}

#[allow(dead_code)]
fn unused(_: DispatchRetained<DispatchQueue>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(
        id: u32,
        pid: i32,
        (x, y, width, height): (f64, f64, f64, f64),
        title: &str,
    ) -> Listed {
        Listed {
            id,
            pid,
            layer: 0,
            x,
            y,
            width,
            height,
            title: title.into(),
        }
    }

    #[test]
    fn a_sheet_hangs_from_the_window_behind_it() {
        // Chess's Game Center sheet over its board, as macOS 15 lists them.
        let chess = window(40, 7, (0.0, 25.0, 872.0, 682.0), "Game 1");
        let sheet = window(51, 7, (187.0, 225.0, 498.0, 220.0), "");
        assert_eq!(sheet_of(&sheet, &[chess]), Some(40));
    }

    #[test]
    fn other_windows_are_not_sheets() {
        let big = || window(1, 7, (0.0, 0.0, 800.0, 600.0), "Untitled");
        // A titled document window inside another.
        let titled = window(2, 7, (250.0, 100.0, 300.0, 200.0), "Untitled 2");
        assert_eq!(sheet_of(&titled, &[big()]), None);
        // One off centre.
        let aside = window(3, 7, (10.0, 100.0, 300.0, 200.0), "");
        assert_eq!(sheet_of(&aside, &[big()]), None);
        // Another app's window behind.
        let other = window(4, 8, (250.0, 100.0, 300.0, 200.0), "");
        assert_eq!(sheet_of(&other, &[big()]), None);
        // Sticking out of the window behind.
        let wide = window(5, 7, (-100.0, 100.0, 1000.0, 200.0), "");
        assert_eq!(sheet_of(&wide, &[big()]), None);
    }
}
