//! Which windows there are, and a video stream of each.
//!
//! Ten times a second the window thread asks the window server which windows
//! are on screen (CGWindowListCopyWindowInfo, cheap and in front-to-back
//! order) and tells watching clients what opened, moved, changed or closed.
//! A window it has not seen before gets a ScreenCaptureKit stream of its
//! own (`SCContentFilter(desktopIndependentWindow:)`), which sees the window
//! whole whatever covers it on macOS's screen, and only sends a picture when
//! the window's content changed. Each picture is encoded by its own
//! VideoToolbox session: H.264, real time, no reordered frames, so every
//! picture can be shown the moment it arrives.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
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
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
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
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType, SCWindow,
};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
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

/// The most pictures a second any window sends.
const FPS: i32 = 60;

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
/// captures, by window number.
fn shareable_windows() -> HashMap<u32, Retained<SCWindow>> {
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
        return HashMap::new();
    };
    let windows = unsafe { content.windows() };
    windows
        .iter()
        .map(|w| (unsafe { w.windowID() }, w))
        .collect()
}

/// One window's stream and what encodes it.
struct Stream {
    stream: Retained<SCStream>,
    output: Retained<WindowOutput>,
    /// The size in pixels the stream was last configured for.
    width: usize,
    height: usize,
}

/// The window thread: forever, the window list, its changes, the streams.
pub fn track(hub: &Arc<Hub>) {
    let mut streams: HashMap<u32, Stream> = HashMap::new();
    let mut bundles: HashMap<i32, Option<String>> = HashMap::new();
    let mut unshareable: HashSet<u32> = HashSet::new();
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
                        o.pid == w.pid && kind_for_layer(o.layer) == Some(WindowKind::Normal)
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
            let kind = if kind == WindowKind::Popup && parent.is_none() {
                WindowKind::Normal
            } else {
                kind
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
        unshareable.retain(|id| current.contains_key(id));
        if !watched {
            continue;
        }
        let missing: Vec<u32> = current
            .keys()
            .filter(|id| !streams.contains_key(id) && !unshareable.contains(id))
            .copied()
            .collect();
        if !missing.is_empty() {
            let shareable = shareable_windows();
            for id in missing {
                let Some(window) = shareable.get(&id) else {
                    // ScreenCaptureKit does not offer every window the
                    // window server lists (without Screen Recording, none).
                    unshareable.insert(id);
                    continue;
                };
                let Some(tracked) = hub.window(id) else {
                    continue;
                };
                match start(hub, window, &tracked) {
                    Ok(stream) => {
                        streams.insert(id, stream);
                    }
                    Err(e) => {
                        eprintln!("capturing window {id}: {e}");
                        unshareable.insert(id);
                    }
                }
            }
        }
        for (id, stream) in streams.iter_mut() {
            let Some((info, _)) = current.get(id) else {
                continue;
            };
            let (width, height) = pixel_size(info);
            if (width, height) != (stream.width, stream.height) {
                stream.width = width;
                stream.height = height;
                let config = configuration(width, height);
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

fn configuration(width: usize, height: usize) -> Retained<SCStreamConfiguration> {
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(width);
        config.setHeight(height);
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
        config.setQueueDepth(4);
        // macOS 14 and later draw a window's shadow into its picture unless
        // told not to; the host draws its own window's edges.
        if config.respondsToSelector(sel!(setIgnoreShadowsSingleWindow:)) {
            config.setIgnoreShadowsSingleWindow(true);
        }
        config
    }
}

fn start(hub: &Arc<Hub>, window: &SCWindow, tracked: &Tracked) -> Result<Stream, String> {
    let (width, height) = pixel_size(&tracked.info);
    let output = WindowOutput::new(OutputIvars {
        window: tracked.info.window,
        hub: hub.clone(),
        keyframe: tracked.keyframe.clone(),
        encoder: Mutex::new(None),
        epoch: Instant::now(),
    });
    unsafe {
        let filter =
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window);
        let config = configuration(width, height);
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
        // ScreenCaptureKit also sends a buffer when nothing changed (idle)
        // or the window is hidden; only complete frames carry a picture.
        if frame_status(sample) != Some(0) {
            return;
        }
        let Some(image) = (unsafe { sample.image_buffer() }) else {
            return;
        };
        let width = CVPixelBufferGetWidth(&image);
        let height = CVPixelBufferGetHeight(&image);
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
        encoder.encode(&image, micros, keyframe);
    }
}

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
