//! Links WPE WebKit and its headless platform through pkg-config, and
//! writes the Rust declarations of their C API with bindgen from the
//! installed headers, so that every signature, enum value and struct
//! layout ffi.rs exposes is the one the library was built with rather
//! than one copied by hand.

use std::env;
use std::path::PathBuf;

fn main() {
    let mut include = Vec::new();
    for module in ["wpe-webkit-2.0", "wpe-platform-headless-2.0"] {
        let library = pkg_config::Config::new()
            .atleast_version("2.54")
            .probe(module)
            .unwrap_or_else(|error| panic!("{error}"));
        include.extend(library.include_paths);
    }

    // WebKit's own header pulls in GLib, GObject and GIO; the platform's
    // adds the headless display. Everything the crate declares comes
    // through these two.
    let header = "#include <wpe/webkit.h>\n#include <wpe/headless/wpe-headless.h>\n";
    let bindings = bindgen::Builder::default()
        .header_contents("danube.h", header)
        .clang_args(include.iter().map(|p| format!("-I{}", p.display())))
        // Only WebKit's and WPE's API, and the few GLib calls the engine
        // makes itself; the types they mention come along with them. Not
        // the whole of GLib, which would be most of the output.
        .allowlist_function("(webkit|wpe)_.*")
        .allowlist_function(
            "g_(main_loop_(new|run|quit|ref|unref)|idle_add_full|timeout_add_full|source_remove|free|memdup2|strfreev|error_(free|new_literal)|io_error_quark|bytes_(get_data|unref)|object_(new|ref|unref)|signal_connect_data|type_name_from_instance|file_new_for_path|memory_input_stream_new_from_data|cancellable_new)",
        )
        // libsoup's header objects, for the headers an extension's page is
        // served with (its content security policy) and the Referer it is
        // asked for with, and JavaScriptCore's values, which carry a
        // page's messages to the extension runtime.
        .allowlist_function("soup_message_headers_(new|append|unref|get_one)")
        // The cookies a download is handed to aria2 with (downloads.rs),
        // and GDBus with the GVariants it carries, for the notifications
        // downloads and sites show (notify.rs) and the file chooser portal
        // a page's upload button opens (portal.rs).
        .allowlist_function("soup_cookie_(get_name|get_value|free)|g_list_free")
        .allowlist_function("g_bus_get_sync|g_dbus_connection_(call|call_finish|call_sync|signal_subscribe|get_unique_name)")
        .allowlist_function(
            "g_variant_(new_(string|int32|uint32|boolean|variant|dict_entry|tuple|array|strv)|get_child_value|get_variant|get_uint32|get_string|get_type_string|n_children|unref)",
        )
        .allowlist_function("jsc_value_(to_string|to_json|new_string|get_context)")
        .allowlist_type("(JSC|Soup).*")
        .allowlist_type("(WebKit|WPE).*")
        .allowlist_var("(WEBKIT|WPE|SOUP)_.*")
        .allowlist_var("G_(BUS_TYPE|DBUS_CALL_FLAGS|DBUS_SIGNAL_FLAGS)_.*")
        // C enums as plain integer constants, as GLib's API takes them: a
        // Rust enum would be undefined behaviour for a value WebKit adds
        // in a later release.
        .prepend_enum_name(false)
        .layout_tests(false)
        .generate_comments(false)
        .generate()
        .expect("bindgen reads WPE WebKit's headers");
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("bindings.rs is written");
}
