//! H.264 to pixels, through FFmpeg: on the GPU's video decoder through
//! VA-API where the host has one, in software otherwise, then converted to
//! the 32-bit BGRX a Wayland shared-memory buffer holds.

use std::ffi::{c_int, CStr};
use std::ptr;

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    clippy::all
)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/ffmpeg.rs"));
}

use ffi::{AVCodecID, AVColorRange, AVHWDeviceType, AVPixelFormat};

/// libswscale's bilinear filter. FFmpeg 8's headers declare its flags as an
/// enum, which bindgen names differently from FFmpeg 7's macros; the value
/// is the same in both.
const SWS_BILINEAR: c_int = 2;

/// A decoded picture, rows of `stride` bytes, four per pixel in B, G, R, X
/// order, which is wl_shm's XRGB8888 on a little-endian machine.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixels: Vec<u8>,
}

/// One window's decoder. Every picture of a window depends on the ones
/// before it back to a keyframe, so each window has its own.
pub struct Decoder {
    context: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    /// Where a picture decoded on the GPU is copied down to.
    transferred: *mut ffi::AVFrame,
    scaler: *mut ffi::SwsContext,
    scaler_for: (c_int, c_int, AVPixelFormat),
    hardware: bool,
}

// SAFETY: FFmpeg's decoding context may move between threads; it is only
// ever used from one at a time, by `&mut self`.
unsafe impl Send for Decoder {}

fn error(code: c_int) -> String {
    let mut buffer = [0 as std::ffi::c_char; 128];
    unsafe {
        ffi::av_strerror(code, buffer.as_mut_ptr(), buffer.len());
        CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

/// FFmpeg's choice of output format: the GPU's when it offers it, which it
/// does when the device opened and the GPU decodes this profile and size.
unsafe extern "C" fn pick_format(
    _context: *mut ffi::AVCodecContext,
    formats: *const AVPixelFormat,
) -> AVPixelFormat {
    let mut first = AVPixelFormat::AV_PIX_FMT_NONE;
    let mut at = formats;
    unsafe {
        while *at != AVPixelFormat::AV_PIX_FMT_NONE {
            if *at == AVPixelFormat::AV_PIX_FMT_VAAPI {
                return *at;
            }
            if first == AVPixelFormat::AV_PIX_FMT_NONE && *at != AVPixelFormat::AV_PIX_FMT_VAAPI {
                first = *at;
            }
            at = at.add(1);
        }
    }
    first
}

impl Decoder {
    /// A decoder, on the GPU when `hardware` is asked for and VA-API opens.
    pub fn new(hardware: bool) -> Result<Self, String> {
        unsafe {
            // FFmpeg's own chatter about every frame would drown the log.
            ffi::av_log_set_level(ffi::AV_LOG_ERROR as c_int);
            let codec = ffi::avcodec_find_decoder(AVCodecID::AV_CODEC_ID_H264);
            if codec.is_null() {
                return Err("this FFmpeg has no H.264 decoder".into());
            }
            let context = ffi::avcodec_alloc_context3(codec);
            if context.is_null() {
                return Err("allocating a decoder".into());
            }
            // Each picture as soon as it is in, no frame held back to look
            // ahead: the stream has no reordered frames to wait for.
            (*context).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as c_int;
            (*context).thread_type = ffi::FF_THREAD_SLICE as c_int;
            let mut on_gpu = false;
            if hardware {
                let mut device = ptr::null_mut();
                let status = ffi::av_hwdevice_ctx_create(
                    &mut device,
                    AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                    ptr::null(),
                    ptr::null_mut(),
                    0,
                );
                if status >= 0 {
                    (*context).hw_device_ctx = device;
                    (*context).get_format = Some(pick_format);
                    on_gpu = true;
                }
            }
            let status = ffi::avcodec_open2(context, codec, ptr::null_mut());
            if status < 0 {
                let mut context = context;
                ffi::avcodec_free_context(&mut context);
                return Err(format!("opening the H.264 decoder: {}", error(status)));
            }
            Ok(Decoder {
                context,
                packet: ffi::av_packet_alloc(),
                frame: ffi::av_frame_alloc(),
                transferred: ffi::av_frame_alloc(),
                scaler: ptr::null_mut(),
                scaler_for: (0, 0, AVPixelFormat::AV_PIX_FMT_NONE),
                hardware: on_gpu,
            })
        }
    }

    /// Whether pictures are decoded on the GPU.
    pub fn on_gpu(&self) -> bool {
        self.hardware
    }

    /// Decodes one message's worth of H.264, and returns the picture it
    /// completes, if it completes one.
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
        unsafe {
            // Not reference-counted, so FFmpeg copies what it keeps.
            (*self.packet).data = data.as_ptr().cast_mut();
            (*self.packet).size = data.len() as c_int;
            let status = ffi::avcodec_send_packet(self.context, self.packet);
            (*self.packet).data = ptr::null_mut();
            (*self.packet).size = 0;
            if status < 0 {
                return Err(format!("decoding: {}", error(status)));
            }
            let mut latest = None;
            loop {
                let status = ffi::avcodec_receive_frame(self.context, self.frame);
                if status < 0 {
                    // EAGAIN: it wants more input; anything else, nothing
                    // more to give either.
                    break;
                }
                latest = Some(self.convert()?);
                ffi::av_frame_unref(self.frame);
            }
            Ok(latest)
        }
    }

    unsafe fn convert(&mut self) -> Result<Picture, String> {
        unsafe {
            let mut source = self.frame;
            if AVPixelFormat((*source).format as _) == AVPixelFormat::AV_PIX_FMT_VAAPI {
                ffi::av_frame_unref(self.transferred);
                let status = ffi::av_hwframe_transfer_data(self.transferred, source, 0);
                if status < 0 {
                    return Err(format!("copying a picture from the GPU: {}", error(status)));
                }
                source = self.transferred;
            }
            let (width, height) = ((*source).width, (*source).height);
            let format = AVPixelFormat((*source).format as _);
            if self.scaler.is_null() || self.scaler_for != (width, height, format) {
                ffi::sws_freeContext(self.scaler);
                self.scaler = ffi::sws_getContext(
                    width,
                    height,
                    format,
                    width,
                    height,
                    AVPixelFormat::AV_PIX_FMT_BGR0,
                    SWS_BILINEAR,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                );
                if self.scaler.is_null() {
                    return Err(format!("no conversion from {format:?} to BGRX"));
                }
                // The guest encodes in BT.709, as screens are; swscale's
                // default would be the older BT.601's colours.
                let full = (*source).color_range == AVColorRange::AVCOL_RANGE_JPEG;
                let bt709 = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709 as c_int);
                ffi::sws_setColorspaceDetails(
                    self.scaler,
                    bt709,
                    c_int::from(full),
                    bt709,
                    1,
                    0,
                    1 << 16,
                    1 << 16,
                );
                self.scaler_for = (width, height, format);
            }
            let stride = width as usize * 4;
            let mut pixels = vec![0u8; stride * height as usize];
            let destination = [
                pixels.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            ];
            let strides = [stride as c_int, 0, 0, 0];
            ffi::sws_scale(
                self.scaler,
                (*source).data.as_ptr() as *const *const u8,
                (*source).linesize.as_ptr(),
                0,
                height,
                destination.as_ptr(),
                strides.as_ptr(),
            );
            Ok(Picture {
                width: width as u32,
                height: height as u32,
                stride,
                pixels,
            })
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            ffi::sws_freeContext(self.scaler);
            ffi::av_frame_free(&mut self.frame);
            ffi::av_frame_free(&mut self.transferred);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.context);
        }
    }
}
