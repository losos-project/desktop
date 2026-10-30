//! What the host has to give: memory, processor cores, and the keyboards,
//! pointers and game pads plugged in now.

use std::fs;
use std::path::Path;

use crate::domain::Input;

/// Half the host's memory, at most 32 GiB: what a game wants beside what
/// the desktop and its other machines keep.
pub fn memory_mib(meminfo: &str) -> u64 {
    let kib: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(8 << 20);
    (kib / 1024 / 2).clamp(2048, 32 << 10)
}

/// `(cores, threads per core, AMD)` for the machine: every core but one,
/// or two on a host with more than eight, which the desktop, Looking
/// Glass's client and the host's interrupts keep.
pub fn processor(sys: &Path, cpuinfo: &str) -> (u32, u32, bool) {
    let online = fs::read_to_string(sys.join("devices/system/cpu/online"))
        .map(|s| count(s.trim()))
        .unwrap_or(2);
    let threads =
        fs::read_to_string(sys.join("devices/system/cpu/cpu0/topology/thread_siblings_list"))
            .map(|s| count(s.trim()))
            .unwrap_or(1)
            .max(1);
    let cores = online / threads;
    let spare = if cores > 8 { 2 } else { 1 };
    let amd = cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("vendor_id"))
        .is_some_and(|v| v.contains("AuthenticAMD"));
    (cores.saturating_sub(spare).max(1), threads, amd)
}

/// How many CPUs a list such as `0-7,16-23` names.
fn count(list: &str) -> u32 {
    list.split(',')
        .filter(|r| !r.is_empty())
        .map(|r| match r.split_once('-') {
            Some((a, b)) => {
                b.parse::<u32>()
                    .unwrap_or(0)
                    .saturating_sub(a.parse().unwrap_or(0))
                    + 1
            }
            None => 1,
        })
        .sum()
}

/// The keyboards and pointers plugged in, by their stable names.
pub fn inputs(by_id: &Path) -> Vec<Input> {
    let mut out: Vec<Input> = fs::read_dir(by_id)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let keyboard = name.ends_with("-event-kbd");
            (keyboard || name.ends_with("-event-mouse")).then(|| Input {
                path: e.path().to_string_lossy().into_owned(),
                keyboard,
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The USB game pads plugged in, `(vendor, product)`: each joystick
/// device's USB device, found by walking up from it in sysfs.
pub fn pads(sys: &Path) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(sys.join("class/input"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if !entry.file_name().to_string_lossy().starts_with("js") {
            continue;
        }
        let Ok(mut dir) = fs::canonicalize(entry.path()) else {
            continue;
        };
        while dir.pop() && dir.starts_with(sys) {
            let hex = |name: &str| {
                fs::read_to_string(dir.join(name))
                    .ok()
                    .and_then(|s| u16::from_str_radix(s.trim(), 16).ok())
            };
            if let (Some(vendor), Some(product)) = (hex("idVendor"), hex("idProduct")) {
                if !out.contains(&(vendor, product)) {
                    out.push((vendor, product));
                }
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_is_half_within_bounds() {
        assert_eq!(memory_mib("MemTotal:       32768000 kB\n"), 16000);
        assert_eq!(memory_mib("MemTotal:      134217728 kB\n"), 32 << 10);
    }

    #[test]
    fn cpu_lists_are_counted() {
        assert_eq!(count("0-15"), 16);
        assert_eq!(count("0,8"), 2);
        assert_eq!(count("0-3,8-11"), 8);
    }
}
