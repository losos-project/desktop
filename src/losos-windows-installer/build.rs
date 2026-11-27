//! What the OS's configuration decides, compiled in.
//!
//! nixos/modules/windows-installer.nix builds this program from the image's
//! own settings, as installer.nix builds the ISO's: the channel's URL, the
//! release key, the image's id and architecture, the /usr slot size, and the
//! Limine limine.nix puts on an ESP. Each comes in as an environment
//! variable naming a value or a file. Unset, as in a plain `cargo test`, the
//! program has no channel and no key, and says so when run rather than when
//! built.
use std::env;
use std::fmt::Write as _;
use std::path::PathBuf;

fn var(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={name}");
    env::var(name).ok().filter(|v| !v.is_empty())
}

fn file(name: &str) -> String {
    match var(name) {
        Some(path) => {
            println!("cargo:rerun-if-changed={path}");
            format!("Some(include_bytes!({path:?}))")
        }
        None => "None".into(),
    }
}

fn main() {
    let mut out = String::new();
    let text = |v: Option<String>| match v {
        Some(v) => format!("Some({v:?})"),
        None => "None".into(),
    };
    writeln!(
        out,
        "pub const UPDATE_URL: Option<&str> = {};",
        text(var("LOSOS_UPDATE_URL"))
    )
    .unwrap();
    // The channel's index on the documentation site, for an OS whose own
    // updates come from GitHub Releases through it (update-source.nix);
    // then there is no UPDATE_URL a Windows machine could reach.
    writeln!(
        out,
        "pub const UPDATE_INDEX: Option<&str> = {};",
        text(var("LOSOS_UPDATE_INDEX"))
    )
    .unwrap();
    writeln!(
        out,
        "pub const PUBRING: Option<&[u8]> = {};",
        file("LOSOS_PUBRING")
    )
    .unwrap();
    writeln!(
        out,
        "pub const LOADER: Option<&[u8]> = {};",
        file("LOSOS_LOADER")
    )
    .unwrap();
    writeln!(
        out,
        "pub const IMAGE_ID: &str = {:?};",
        var("LOSOS_IMAGE_ID").unwrap_or_else(|| "losos-desktop".into())
    )
    .unwrap();
    writeln!(
        out,
        "pub const ARCH: Option<&str> = {};",
        text(var("LOSOS_ARCH"))
    )
    .unwrap();
    // losos.usrSize as repart reads it: a number with an optional K, M, G
    // or T, each 1024 of the one before.
    let usr = var("LOSOS_USR_SIZE").map_or(8 << 30, |v| {
        let (digits, unit) = v.split_at(v.trim_end_matches(char::is_alphabetic).len());
        let shift = match unit {
            "" | "B" => 0,
            "K" => 10,
            "M" => 20,
            "G" => 30,
            "T" => 40,
            _ => panic!("LOSOS_USR_SIZE={v}: unknown unit {unit}"),
        };
        digits
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("LOSOS_USR_SIZE={v} is not a size"))
            << shift
    });
    writeln!(out, "pub const USR_SIZE: u64 = {usr};").unwrap();
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("config.rs");
    std::fs::write(path, out).unwrap();
}
