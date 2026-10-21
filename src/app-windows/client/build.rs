//! Rust declarations for the parts of FFmpeg the decoder uses, generated
//! from the headers of the FFmpeg it links, so struct layouts match
//! whichever version that is: nixpkgs' on LosOS, Homebrew's on a Mac
//! running the tests.

use std::env;
use std::path::PathBuf;

fn main() {
    let mut includes = Vec::new();
    for library in ["libavcodec", "libavutil", "libswscale"] {
        let found = pkg_config::Config::new()
            .probe(library)
            .unwrap_or_else(|e| panic!("FFmpeg's {library} is needed to build app-windows: {e}"));
        includes.extend(found.include_paths);
    }
    let bindings = bindgen::Builder::default()
        .header_contents(
            "ffmpeg.h",
            "#include <libavcodec/avcodec.h>\n\
             #include <libavutil/hwcontext.h>\n\
             #include <libavutil/imgutils.h>\n\
             #include <libswscale/swscale.h>\n",
        )
        .clang_args(includes.iter().map(|p| format!("-I{}", p.display())))
        .allowlist_function("avcodec_.*|av_packet_.*|av_frame_.*|av_hwdevice_ctx_create|av_hwframe_transfer_data|av_buffer_(ref|unref)|av_log_set_level|av_strerror|sws_.*")
        .allowlist_type("AVCodecContext|AVPacket|AVFrame|AVPixelFormat|AVHWDeviceType|AVColorRange|SwsContext")
        .allowlist_var("AV_.*|SWS_.*|AVERROR_.*|FF_THREAD_.*")
        .newtype_enum("AVPixelFormat|AVHWDeviceType|AVCodecID|AVColorRange")
        .layout_tests(false)
        .generate()
        .expect("generating FFmpeg's bindings");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ffmpeg.rs");
    bindings
        .write_to_file(out)
        .expect("writing FFmpeg's bindings");
}
