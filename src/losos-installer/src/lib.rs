//! The installer's pieces, as a library, so `tests/` can drive each one
//! against a fixture tree instead of a machine.
//!
//! The binary is the backend `derisk installer` talks to (`serve.rs`). What
//! it installs is not decided here: systemd-repart lays out the disk from
//! definitions the OS wrote, and systemd-sysupdate fills a /usr slot from
//! transfers the OS wrote (nixos/installer/default.nix). This crate offers
//! the disks and runs the two in order. Joining a network moved to derisk,
//! which does it the same way on the ISO and on an installed system.
pub mod disks;
pub mod github;
pub mod install;
pub mod report;
pub mod serve;
pub mod stage;
