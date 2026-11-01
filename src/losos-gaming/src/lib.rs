//! The gaming machine (docs/gaming-machine.md): a Windows machine given a
//! graphics card of its own, while the desktop draws on the other card or
//! on the processor.
//!
//! gpu.rs picks the card and moves it to vfio-pci at boot, domain.rs writes
//! the machine's libvirt definition, and host.rs reads what the host has to
//! give it.

pub mod domain;
pub mod gpu;
pub mod host;
