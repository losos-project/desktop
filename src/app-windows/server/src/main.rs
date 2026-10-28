//! macOS's app server: what runs inside macOS so that LosOS shows each of
//! its windows as a window of its own (docs/app-windows.md).
//!
//!     macos-app-server [--listen ADDR:PORT] [--token-file PATH]
//!     macos-app-server permissions     ask for what it needs, print what is missing
//!     macos-app-server apps            print the apps it would list, as JSON
//!
//! It does for macOS what RemoteApp does for Windows under WinApps: lists
//! the apps, starts them, and sends every window on its own. Each window is
//! captured alone with ScreenCaptureKit, whatever covers it on macOS's own
//! screen, and encoded to H.264 by VideoToolbox, on the Mac's media engine
//! where it has one. Input comes back as Linux key codes and pointer
//! positions in the window, and goes in through CGEvent; windows are raised,
//! resized and closed, and their menus read and picked, through
//! Accessibility. The pointer's look goes along, so a text field shows the
//! I-beam on the host too. A file the host sends, such as an installer downloaded
//! there, lands in Downloads and opens.
//!
//! macOS asks the person once to allow Screen Recording and Accessibility
//! for it; until then a client is told what is missing (`Welcome::missing`).

#[cfg(target_os = "macos")]
mod apps;
#[cfg(target_os = "macos")]
mod ax;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod cursor;
// Plain file handling, tested on any system.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod files;
#[cfg(target_os = "macos")]
mod input;
// Plain tables, tested on any system.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod keymap;
#[cfg(target_os = "macos")]
mod server;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    server::main()
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("macos-app-server runs on macOS only");
    std::process::ExitCode::FAILURE
}
