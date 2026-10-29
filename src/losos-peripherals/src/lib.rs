//! Peripherals in machines of their own (docs/peripherals.md).
//!
//! A USB device is code someone else wrote, talking to drivers that trust
//! it: a drive's filesystem, a camera's video, a Bluetooth radio's
//! firmware protocol. On LosOS none of that runs on the host. The USB
//! machine holds the USB controllers, the Bluetooth machine the Bluetooth
//! radio, the network machine network cards, the files machine drives and
//! phones, and anything else gets a disposable machine of its own, made
//! when it is plugged in and destroyed when it is pulled out. The host
//! keeps keyboards, pointers and security keys, which it needs to be used,
//! and the controller with the disk it booted from. A sound card's
//! machine is NixOS with a sound server, which the host plays to through a
//! socket the router makes for it.
//!
//! This crate is both halves: the host's router (`serve`, host.rs) and the
//! machines' (`usb-machine`, `receiver` and `forward`, guest.rs), plus `keep`, which
//! says before any controller moves which ones must stay.

pub mod guest;
pub mod host;
pub mod keep;
pub mod machines;
pub mod ports;
pub mod route;
pub mod splice;
pub mod uevent;
pub mod usb;
pub mod usbip;
pub mod vsock;
