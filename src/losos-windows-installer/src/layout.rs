//! Where LosOS goes in the space Windows gave up.
//!
//! An install from the ISO lays out a whole disk; this one shares it, and
//! puts three partitions at the front of the free space behind Windows:
//!
//! - an XBOOTLDR partition (the Boot Loader Specification's `$BOOT` beside
//!   the ESP) for the UKIs. Windows' ESP is usually 100 MB, a UKI is about
//!   45 MB and sysupdate keeps two, so they cannot live on the ESP the way
//!   they do on a disk of LosOS's own. GRUB reads UKIs from both,
//!   and update.nix writes UKIs to `$BOOT`, which is this partition when it
//!   exists and the ESP when it does not.
//! - slot A's `/usr` verity and data partitions, at disk.nix's sizes, holding
//!   the release, labelled with its version and carrying its UUIDs, as
//!   sysupdate would have left them.
//!
//! Everything else is first boot's, from disk.nix, exactly as for an image
//! written with dd: repart finds the ESP and slot A, and makes slot B, root,
//! /home and swap in the space that is left. It never touches a partition no
//! definition names, which is all of Windows'. So this is the one place that
//! has to know how much that space must be, and [`required`] adds it up.
use miette::{Result, bail};

use crate::guid::Guid;
use crate::release::{Arch, Release};

pub const MIB: u64 = 1 << 20;
pub const GIB: u64 = 1 << 30;

/// Room for the two UKIs sysupdate keeps (`InstancesMax=2`), the size
/// image.nix gives the ESP of a disk of LosOS's own.
pub const BOOT_SIZE: u64 = 512 * MIB;
/// disk.nix's verity partitions.
pub const VERITY_SIZE: u64 = 256 * MIB;
/// disk.nix's `SizeMinBytes=` for root and /home.
pub const ROOT_MIN: u64 = 8 * GIB;
pub const HOME_MIN: u64 = 4 * GIB;
/// What repart's alignment of five more partitions can cost, generously.
const SLACK: u64 = 64 * MIB;

/// GPT attribute bit 60, read-only: what sysupdate sets on a slot it wrote
/// (`ReadOnly=` in update.nix's transfers).
pub const GPT_READ_ONLY: u64 = 1 << 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub end: u64,
}

impl Region {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Boot,
    Verity,
    Usr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPartition {
    pub role: Role,
    pub type_guid: Guid,
    pub uuid: Guid,
    pub label: String,
    pub start: u64,
    pub size: u64,
    pub attributes: u64,
}

impl NewPartition {
    pub fn end(&self) -> u64 {
        self.start + self.size
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub boot: NewPartition,
    pub verity: NewPartition,
    pub usr: NewPartition,
    /// What is left behind them for first boot.
    pub left: u64,
}

impl Plan {
    pub fn partitions(&self) -> [&NewPartition; 3] {
        [&self.boot, &self.verity, &self.usr]
    }
}

/// The label the XBOOTLDR partition gets. Nothing looks it up by name; it is
/// what Windows' and Linux's partition tools show.
pub const BOOT_LABEL: &str = "LosOS boot";

/// Swap is the size of RAM, rounded up as losos-swap rounds it.
fn swap_size(ram: u64) -> u64 {
    ram.div_ceil(4096) * 4096
}

/// The least space LosOS can be given on a machine with `ram` bytes of RAM:
/// the three partitions written here and everything first boot adds.
pub fn required(usr_size: u64, ram: u64) -> u64 {
    BOOT_SIZE + 2 * (VERITY_SIZE + usr_size) + ROOT_MIN + HOME_MIN + swap_size(ram) + SLACK
}

fn align_up(value: u64, to: u64) -> u64 {
    value.div_ceil(to) * to
}

pub fn plan(
    region: Region,
    release: &Release,
    arch: &Arch,
    id: &str,
    usr_size: u64,
    ram: u64,
    boot_uuid: Guid,
) -> Result<Plan> {
    // 1 MiB, as Windows and repart both align partitions.
    let start = align_up(region.start, MIB);
    let need = required(usr_size, ram);
    if region.end < start || region.end - start < need {
        bail!(
            "LosOS needs {} of free space behind Windows and there is {}",
            size_text(need),
            size_text(region.end.saturating_sub(start))
        );
    }
    let label = release.label(id);
    let boot = NewPartition {
        role: Role::Boot,
        type_guid: crate::guid::types::XBOOTLDR,
        uuid: boot_uuid,
        label: BOOT_LABEL.to_owned(),
        start,
        size: BOOT_SIZE,
        attributes: 0,
    };
    let verity = NewPartition {
        role: Role::Verity,
        type_guid: arch.verity_type,
        uuid: release.verity.uuid,
        label: label.clone(),
        start: boot.end(),
        size: VERITY_SIZE,
        attributes: GPT_READ_ONLY,
    };
    let usr = NewPartition {
        role: Role::Usr,
        type_guid: arch.usr_type,
        uuid: release.usr.uuid,
        label,
        start: verity.end(),
        size: usr_size,
        attributes: GPT_READ_ONLY,
    };
    let left = region.end - usr.end();
    Ok(Plan {
        boot,
        verity,
        usr,
        left,
    })
}

/// Bytes as a person reads them on a disk: GB, one decimal, base 1024 as
/// Windows' own tools count.
pub fn size_text(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GIB as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::{File, Slot};

    fn release() -> Release {
        let file = |n: &str| File {
            name: n.into(),
            sha256: "0".repeat(64),
        };
        Release {
            version: "20261007.102859".into(),
            uki: file("uki"),
            usr: Slot {
                file: file("usr"),
                uuid: Guid::parse_str("a28067f9-25ea-dd94-670f-df04370bfc83").unwrap(),
            },
            verity: Slot {
                file: file("verity"),
                uuid: Guid::parse_str("80afbdea-1680-4d3e-322a-5f743f301a68").unwrap(),
            },
        }
    }

    #[test]
    fn adds_up_what_first_boot_will_make() {
        // 512M + 2 × (256M + 8G) + 8G + 4G + 16G of swap + slack.
        let need = required(8 * GIB, 16 * GIB);
        assert_eq!(
            need,
            512 * MIB + 2 * (256 * MIB + 8 * GIB) + 28 * GIB + 64 * MIB
        );
        assert_eq!(swap_size(4095), 4096);
    }

    #[test]
    fn places_three_partitions_at_the_front_of_the_space() {
        let arch = Arch::named("x86_64").unwrap();
        let boot = Guid::new_v4();
        let region = Region {
            start: 20 * GIB + 123,
            end: 80 * GIB,
        };
        let p = plan(
            region,
            &release(),
            &arch,
            "losos-desktop",
            8 * GIB,
            8 * GIB,
            boot,
        )
        .unwrap();
        assert_eq!(p.boot.start, 20 * GIB + MIB);
        assert_eq!(p.verity.start, p.boot.end());
        assert_eq!(p.usr.start, p.verity.end());
        assert_eq!(p.usr.size, 8 * GIB);
        assert_eq!(p.usr.uuid, release().usr.uuid);
        assert_eq!(p.usr.label, "losos-desktop_20261007.102859");
        assert_eq!(p.verity.attributes, GPT_READ_ONLY);
        assert_eq!(
            p.boot.type_guid.to_string(),
            "bc13c2ff-59e6-4262-a352-b275fd6f7172"
        );
        assert_eq!(p.left, 80 * GIB - p.usr.end());
        assert!(p.left + p.usr.end() - p.boot.start >= required(8 * GIB, 8 * GIB));
    }

    #[test]
    fn refuses_too_little_space() {
        let arch = Arch::named("x86_64").unwrap();
        let region = Region {
            start: 0,
            end: required(8 * GIB, 8 * GIB),
        };
        // Alignment from 0 costs nothing, so exactly enough fits...
        assert!(
            plan(
                region,
                &release(),
                &arch,
                "id",
                8 * GIB,
                8 * GIB,
                Guid::nil()
            )
            .is_ok()
        );
        // ...and a byte less does not.
        let short = Region {
            end: region.end - 1,
            ..region
        };
        assert!(
            plan(
                short,
                &release(),
                &arch,
                "id",
                8 * GIB,
                8 * GIB,
                Guid::nil()
            )
            .is_err()
        );
    }
}
