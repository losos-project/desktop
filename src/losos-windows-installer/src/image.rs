//! Installing into a disk image laid out as Windows lays out its disk, on
//! Linux. Nothing ships this: it is how the install's own steps (the
//! partitions, their contents, the boot partition and the ESP) are tested
//! and then booted under QEMU without a Windows machine, with sfdisk standing
//! in for Windows' partition table calls. Only shrinking C: and the firmware
//! variable have no stand-in; docs/windows-installer.md says what that
//! leaves untested.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use miette::{IntoDiagnostic, Result, WrapErr, bail, miette};

use crate::disk::Device;
use crate::fat;
use crate::guid::{Guid, types};
use crate::install::{EspFile, Target};
use crate::layout::{Plan, Region};

#[derive(Debug, Clone)]
pub struct Entry {
    pub start: u64,
    pub size: u64,
    pub type_guid: Guid,
}

pub struct Image {
    path: PathBuf,
    device: Device,
    sector: u64,
    entries: Vec<Entry>,
    last_usable: u64,
    /// The table as sfdisk dumped it before anything was added.
    saved: Option<String>,
    esp_created: Vec<String>,
}

fn sfdisk(args: &[&str], path: &Path, input: Option<&str>) -> Result<String> {
    let mut child = Command::new("sfdisk")
        .args(args)
        .arg(path)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .into_diagnostic()
        .wrap_err("could not run sfdisk")?;
    if let Some(text) = input {
        child
            .stdin
            .take()
            .expect("piped")
            .write_all(text.as_bytes())
            .into_diagnostic()?;
    }
    let out = child.wait_with_output().into_diagnostic()?;
    if !out.status.success() {
        bail!(
            "sfdisk {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).into_diagnostic()
}

impl Image {
    pub fn open(path: &Path) -> Result<Image> {
        let json = sfdisk(&["--json"], path, None)?;
        let value: serde_json::Value = serde_json::from_str(&json).into_diagnostic()?;
        let table = &value["partitiontable"];
        if table["label"] != "gpt" {
            bail!("{} has no GPT", path.display());
        }
        let sector = table["sectorsize"].as_u64().unwrap_or(512);
        let last_usable = table["lastlba"]
            .as_u64()
            .ok_or_else(|| miette!("sfdisk reported no last LBA"))?;
        let entries = table["partitions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(Entry {
                    start: p["start"].as_u64()? * sector,
                    size: p["size"].as_u64()? * sector,
                    type_guid: Guid::parse_str(p["type"].as_str()?).ok()?,
                })
            })
            .collect();
        let file = fs::File::options()
            .read(true)
            .write(true)
            .open(path)
            .into_diagnostic()
            .wrap_err_with(|| path.display().to_string())?;
        Ok(Image {
            path: path.to_owned(),
            device: Device::new(file, sector),
            sector,
            entries,
            last_usable,
            saved: None,
            esp_created: Vec::new(),
        })
    }

    /// The free space directly behind the largest Microsoft basic data
    /// partition, which is where shrinking C: leaves it.
    pub fn free_behind_windows(&self) -> Result<Region> {
        let windows = self
            .entries
            .iter()
            .filter(|e| e.type_guid == types::MICROSOFT_BASIC_DATA)
            .max_by_key(|e| e.size)
            .ok_or_else(|| miette!("{} has no Windows partition", self.path.display()))?;
        let start = windows.start + windows.size;
        let end = self
            .entries
            .iter()
            .map(|e| e.start)
            .filter(|&s| s >= start)
            .min()
            .unwrap_or((self.last_usable + 1) * self.sector);
        Ok(Region { start, end })
    }

    fn esp(&self) -> Result<Entry> {
        self.entries
            .iter()
            .find(|e| e.type_guid == types::ESP)
            .cloned()
            .ok_or_else(|| miette!("{} has no ESP", self.path.display()))
    }
}

impl Target for Image {
    fn device(&mut self) -> &mut Device {
        &mut self.device
    }

    fn add_partitions(&mut self, plan: &Plan) -> Result<()> {
        self.saved = Some(sfdisk(&["--dump"], &self.path, None)?);
        let s = self.sector;
        let script: String = plan
            .partitions()
            .iter()
            .map(|p| {
                let bits: Vec<String> = (0..64)
                    .filter(|b| p.attributes & (1 << b) != 0)
                    .map(|b| format!("GUID:{b}"))
                    .collect();
                format!(
                    "start={}, size={}, type={}, uuid={}, name=\"{}\", attrs=\"{}\"\n",
                    p.start / s,
                    p.size / s,
                    p.type_guid,
                    p.uuid,
                    p.label,
                    bits.join(",")
                )
            })
            .collect();
        sfdisk(
            &["--append", "--no-reread", "--no-tell-kernel", "-q"],
            &self.path,
            Some(&script),
        )?;
        Ok(())
    }

    fn put_esp(&mut self, files: &[EspFile]) -> Result<Vec<String>> {
        let esp = self.esp()?;
        let created = fat::put(&mut self.device, esp.start, esp.size, files)?;
        self.esp_created.extend(created.iter().cloned());
        Ok(created)
    }

    fn add_boot_entry(&mut self, description: &str, loader: &str) -> Result<()> {
        // An image file has no firmware to tell. QEMU's boots the
        // removable-media path, which the QEMU check puts Limine at.
        tracing::info!("no firmware to add \"{description}\" ({loader}) to: this is an image");
        Ok(())
    }

    fn undo(&mut self) -> Result<()> {
        if !self.esp_created.is_empty() {
            let esp = self.esp()?;
            fat::remove(&mut self.device, esp.start, esp.size, &self.esp_created)?;
        }
        if let Some(dump) = self.saved.take() {
            sfdisk(
                &["--no-reread", "--no-tell-kernel", "-q"],
                &self.path,
                Some(&dump),
            )?;
        }
        Ok(())
    }
}
