//! Linux-PAM's module API as Rust: the few functions and constants the
//! module (src/pam.rs) uses, generated from the system's headers so they
//! can never drift from the libpam the module is loaded into.

use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/pam.h");
    println!("cargo:rustc-link-lib=pam");
    let bindings = bindgen::Builder::default()
        .header("src/pam.h")
        .allowlist_function("pam_get_user|pam_get_authtok|pam_prompt")
        .allowlist_type("pam_handle_t")
        .allowlist_var("PAM_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect(
            "Linux-PAM's headers (security/pam_modules.h, security/pam_ext.h) are needed to build",
        );
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    bindings
        .write_to_file(out.join("pam.rs"))
        .expect("OUT_DIR is writable");
}
