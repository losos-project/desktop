//! The running Windows machine as an install target.
//!
//! Everything here asks Windows itself: the partition table through the
//! disk driver's layout calls, C:'s size through the Storage module's
//! `Resize-Partition` (which moves NTFS's own structures out of the way, as
//! Disk Management's "Shrink Volume" does), BitLocker through its WMI class,
//! the firmware's boot entries through the UEFI variable calls. PowerShell
//! is used only where Windows offers no call a program can make without
//! COM, and only for numbers, read back as JSON, so nothing depends on the
//! language Windows is set to.
//!
//! None of this has run on a Windows machine yet; docs/windows-installer.md
//! says what has been tested and how.
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::process::Command;

use miette::{IntoDiagnostic, Result, WrapErr, bail, miette};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ENVVAR_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, GetLastError, HANDLE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    SE_SYSTEM_ENVIRONMENT_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetLogicalDrives,
    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DISK_EXTENT, DISK_GEOMETRY_EX, DRIVE_LAYOUT_INFORMATION_EX, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
    IOCTL_DISK_GET_DRIVE_LAYOUT_EX, IOCTL_DISK_SET_DRIVE_LAYOUT_EX, IOCTL_DISK_UPDATE_PROPERTIES,
    PARTITION_INFORMATION_EX, PARTITION_INFORMATION_GPT, PARTITION_STYLE_GPT, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::SystemInformation::{
    FIRMWARE_TYPE, FirmwareTypeUefi, GetFirmwareType, GlobalMemoryStatusEx, IMAGE_FILE_MACHINE,
    IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64, MEMORYSTATUSEX,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2, OpenProcessToken};
use windows_sys::Win32::System::WindowsProgramming::{
    GetFirmwareEnvironmentVariableW, SetFirmwareEnvironmentVariableExW,
};
use windows_sys::Win32::UI::Shell::{IsUserAnAdmin, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::core::GUID;

use crate::disk::Device;
use crate::efi;
use crate::guid::{Guid, types};
use crate::install::{EspFile, Target};
use crate::layout::{NewPartition, Plan, Region};

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn to_guid(g: Guid) -> GUID {
    let (data1, data2, data3, data4) = g.as_fields();
    GUID {
        data1,
        data2,
        data3,
        data4: *data4,
    }
}

fn from_guid(g: &GUID) -> Guid {
    Guid::from_fields(g.data1, g.data2, g.data3, &g.data4)
}

// ---------------------------------------------------------------------------
// The machine

/// Asks for administrator rights by starting this program again through
/// UAC, the way a double-clicked installer is expected to. Returns false in
/// the process that did the asking, which then has nothing more to do.
pub fn ensure_admin() -> Result<bool> {
    // SAFETY: no arguments, no preconditions.
    if unsafe { IsUserAnAdmin() } != 0 {
        return Ok(true);
    }
    let exe = std::env::current_exe().into_diagnostic()?;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .map(|a| format!("\"{}\"", a.replace('"', "\\\"")))
        .collect();
    let (verb, file, params) = (wide("runas"), wide(&exe), wide(args.join(" ")));
    // SAFETY: zeroed is this struct's documented default.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 1; // SW_SHOWNORMAL
    // SAFETY: every pointer in info outlives the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        bail!("LosOS's installer needs administrator rights, and they were not given");
    }
    Ok(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BitLocker {
    Off,
    On,
    /// Encrypting or decrypting: the volume cannot be shrunk until it is
    /// done.
    Converting,
    /// The BitLocker WMI class is not there (an edition without it).
    Absent,
}

#[derive(Debug)]
pub struct Machine {
    pub uefi: bool,
    pub native: IMAGE_FILE_MACHINE,
    pub secure_boot: bool,
    pub ram: u64,
    pub fast_startup: bool,
    pub bitlocker: BitLocker,
}

impl Machine {
    pub fn arch(&self) -> Option<&'static str> {
        match self.native {
            IMAGE_FILE_MACHINE_AMD64 => Some("x86_64"),
            IMAGE_FILE_MACHINE_ARM64 => Some("aarch64"),
            _ => None,
        }
    }
}

const SECURE_BOOT_KEY: &str = r"SYSTEM\CurrentControlSet\Control\SecureBoot\State";
const POWER_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Power";

fn registry_dword(key: &str, value: &str) -> Option<u32> {
    let (key, value) = (wide(key), wide(value));
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: data and size are valid for the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

fn set_registry_dword(key: &str, value: &str, data: u32) -> Result<()> {
    let (key, value) = (wide(key), wide(value));
    // SAFETY: data lives across the call and the size matches.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            REG_DWORD,
            (&data as *const u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    if status != 0 {
        bail!(
            "could not set {value:?}: {}",
            io::Error::from_raw_os_error(status as i32)
        );
    }
    Ok(())
}

pub fn system_drive() -> String {
    std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into())
}

pub fn machine() -> Result<Machine> {
    let mut firmware: FIRMWARE_TYPE = 0;
    // SAFETY: firmware is valid for the call.
    let uefi = unsafe { GetFirmwareType(&mut firmware) } != 0 && firmware == FirmwareTypeUefi;
    let (mut process, mut native) = (0, 0);
    // SAFETY: both outputs are valid for the call.
    if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } == 0 {
        native = IMAGE_FILE_MACHINE_AMD64;
    }
    // SAFETY: zeroed with dwLength set is how the call is made.
    let mut memory: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    memory.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: memory is valid for the call.
    if unsafe { GlobalMemoryStatusEx(&mut memory) } == 0 {
        return Err(last_error())
            .into_diagnostic()
            .wrap_err("reading the RAM size");
    }
    Ok(Machine {
        uefi,
        native,
        secure_boot: registry_dword(SECURE_BOOT_KEY, "UEFISecureBootEnabled") == Some(1),
        ram: memory.ullTotalPhys,
        // Fast startup is on unless HiberbootEnabled says 0.
        fast_startup: registry_dword(POWER_KEY, "HiberbootEnabled") != Some(0),
        bitlocker: bitlocker(&system_drive()),
    })
}

/// Turns fast startup off. With it on, "Shut down" hibernates the kernel
/// with every disk's partition table in memory, and LosOS's first boot adds
/// partitions to this one; a Windows that resumed from that would write
/// back a table that no longer exists. Hibernate itself stays as it was.
pub fn disable_fast_startup() -> Result<()> {
    set_registry_dword(POWER_KEY, "HiberbootEnabled", 0)
}

// ---------------------------------------------------------------------------
// PowerShell, for the Storage and BitLocker calls

fn powershell(script: &str) -> Result<serde_json::Value> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let exe = PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let wrapped = format!(
        "$ErrorActionPreference = 'Stop'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
         & {{ {script} }} | ConvertTo-Json -Compress -Depth 3"
    );
    let out = Command::new(exe)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ])
        .arg(&wrapped)
        .output()
        .into_diagnostic()
        .wrap_err("could not run PowerShell")?;
    if !out.status.success() {
        bail!(
            "PowerShell failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(text.trim())
        .into_diagnostic()
        .wrap_err_with(|| format!("PowerShell printed {text}"))
}

fn letter(drive: &str) -> char {
    drive.chars().next().unwrap_or('C')
}

/// How far C: can be shrunk, as Disk Management's "Shrink Volume" computes
/// it: (smallest size, current maximum).
pub fn supported_size(drive: &str) -> Result<(u64, u64)> {
    let v = powershell(&format!(
        "Get-PartitionSupportedSize -DriveLetter {} | Select-Object SizeMin, SizeMax",
        letter(drive)
    ))?;
    let field = |name: &str| {
        v[name]
            .as_u64()
            .ok_or_else(|| miette!("Get-PartitionSupportedSize gave no {name}"))
    };
    Ok((field("SizeMin")?, field("SizeMax")?))
}

pub fn resize(drive: &str, size: u64) -> Result<()> {
    powershell(&format!(
        "Resize-Partition -DriveLetter {} -Size {size}",
        letter(drive)
    ))
    .wrap_err_with(|| format!("resizing {drive}"))?;
    Ok(())
}

const BITLOCKER: &str = "Get-CimInstance -Namespace root/CIMV2/Security/MicrosoftVolumeEncryption \
     -ClassName Win32_EncryptableVolume";

pub fn bitlocker(drive: &str) -> BitLocker {
    let Ok(v) = powershell(&format!(
        "{BITLOCKER} -Filter \"DriveLetter='{drive}'\" | \
         Select-Object ProtectionStatus, @{{n='Conversion';e={{($_ | Invoke-CimMethod -MethodName GetConversionStatus).ConversionStatus}}}}"
    )) else {
        return BitLocker::Absent;
    };
    // ConversionStatus: 0 fully decrypted, 1 fully encrypted, others moving.
    match (v["ProtectionStatus"].as_u64(), v["Conversion"].as_u64()) {
        (_, Some(c)) if c > 1 => BitLocker::Converting,
        (Some(1), _) => BitLocker::On,
        (Some(_), _) => BitLocker::Off,
        (None, _) => BitLocker::Absent,
    }
}

/// Suspends BitLocker's TPM protector until Windows has started `reboots`
/// times. The firmware now starts Limine, which starts Windows' boot
/// manager, so the TPM measures a different chain than the one BitLocker
/// sealed its key to; while suspended the key is readable, and when the
/// count runs out BitLocker seals it again to the chain it then boots
/// through. Without this, the next Windows boot asks for the recovery key.
pub fn suspend_bitlocker(drive: &str, reboots: u32) -> Result<()> {
    let v = powershell(&format!(
        "{BITLOCKER} -Filter \"DriveLetter='{drive}'\" | \
         Invoke-CimMethod -MethodName DisableKeyProtectors -Arguments @{{DisableCount=[uint32]{reboots}}} | \
         Select-Object ReturnValue"
    ))?;
    match v["ReturnValue"].as_u64() {
        Some(0) => Ok(()),
        other => bail!("BitLocker refused to suspend protection ({other:?})"),
    }
}

/// Free space on the drive holding `path`.
pub fn free_space(path: &Path) -> Result<u64> {
    let path = wide(path);
    let mut free = 0u64;
    // SAFETY: free is valid for the call; the other outputs may be null.
    if unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(last_error()).into_diagnostic();
    }
    Ok(free)
}

// ---------------------------------------------------------------------------
// The disk and its partition table

fn ioctl(handle: HANDLE, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<u32> {
    let mut returned = 0u32;
    // SAFETY: the buffers are valid for their lengths for the call.
    let ok = unsafe {
        DeviceIoControl(
            handle,
            code,
            if input.is_empty() {
                std::ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if output.is_empty() {
                std::ptr::null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(last_error())
    } else {
        Ok(returned)
    }
}

/// A buffer aligned for the structs the layout calls fill.
fn buffer(bytes: usize) -> Vec<u64> {
    vec![0u64; bytes.div_ceil(8)]
}

fn bytes_of(buf: &mut [u64]) -> &mut [u8] {
    // SAFETY: u64 has no invalid bit patterns and u8 no alignment.
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast(), buf.len() * 8) }
}

/// Which disk a drive letter's volume is on, and where.
pub fn volume_extent(drive: &str) -> Result<DISK_EXTENT> {
    let file = OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(format!(r"\\.\{drive}"))
        .into_diagnostic()
        .wrap_err_with(|| format!("opening {drive}"))?;
    let mut out = buffer(size_of::<VOLUME_DISK_EXTENTS>() + 8 * size_of::<DISK_EXTENT>());
    ioctl(
        file.as_raw_handle() as HANDLE,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        &[],
        bytes_of(&mut out),
    )
    .into_diagnostic()
    .wrap_err_with(|| format!("finding {drive}'s disk"))?;
    // SAFETY: the call filled a VOLUME_DISK_EXTENTS at the buffer's start.
    let extents = unsafe { &*(out.as_ptr() as *const VOLUME_DISK_EXTENTS) };
    if extents.NumberOfDiskExtents != 1 {
        bail!(
            "{drive} spans {} disks (a dynamic or Storage Spaces volume), which LosOS cannot share",
            extents.NumberOfDiskExtents
        );
    }
    Ok(extents.Extents[0])
}

#[derive(Clone)]
pub struct Layout {
    header: DRIVE_LAYOUT_INFORMATION_EX,
    pub entries: Vec<PARTITION_INFORMATION_EX>,
}

impl Layout {
    pub fn usable(&self) -> Region {
        // SAFETY: only GPT layouts get this far (read_layout checks).
        let gpt = unsafe { self.header.Anonymous.Gpt };
        let start = gpt.StartingUsableOffset as u64;
        Region {
            start,
            end: start + gpt.UsableLength as u64,
        }
    }

    pub fn type_of(entry: &PARTITION_INFORMATION_EX) -> Guid {
        // SAFETY: as above, every entry of a GPT layout is a GPT entry.
        from_guid(&unsafe { entry.Anonymous.Gpt }.PartitionType)
    }

    pub fn uuid_of(entry: &PARTITION_INFORMATION_EX) -> Guid {
        // SAFETY: as above.
        from_guid(&unsafe { entry.Anonymous.Gpt }.PartitionId)
    }

    pub fn at(&self, offset: u64) -> Option<&PARTITION_INFORMATION_EX> {
        self.entries
            .iter()
            .find(|e| e.StartingOffset as u64 == offset)
    }

    pub fn esp(&self) -> Option<&PARTITION_INFORMATION_EX> {
        self.entries.iter().find(|e| Self::type_of(e) == types::ESP)
    }

    /// The free space directly behind the partition at `offset`.
    pub fn free_behind(&self, offset: u64) -> Option<Region> {
        let part = self.at(offset)?;
        let start = (part.StartingOffset + part.PartitionLength) as u64;
        let end = self
            .entries
            .iter()
            .map(|e| e.StartingOffset as u64)
            .filter(|&s| s >= start)
            .min()
            .unwrap_or(self.usable().end);
        Some(Region { start, end })
    }
}

pub struct Disk {
    pub number: u32,
    file: File,
    pub sector: u64,
    pub size: u64,
}

impl Disk {
    pub fn open(number: u32) -> Result<Disk> {
        let path = format!(r"\\.\PhysicalDrive{number}");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .into_diagnostic()
            .wrap_err_with(|| format!("opening {path}"))?;
        let mut out = buffer(size_of::<DISK_GEOMETRY_EX>() + 256);
        ioctl(
            file.as_raw_handle() as HANDLE,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            &[],
            bytes_of(&mut out),
        )
        .into_diagnostic()
        .wrap_err("reading the disk's geometry")?;
        // SAFETY: the call filled a DISK_GEOMETRY_EX at the buffer's start.
        let geometry = unsafe { &*(out.as_ptr() as *const DISK_GEOMETRY_EX) };
        Ok(Disk {
            number,
            file,
            sector: u64::from(geometry.Geometry.BytesPerSector),
            size: geometry.DiskSize as u64,
        })
    }

    fn handle(&self) -> HANDLE {
        self.file.as_raw_handle() as HANDLE
    }

    pub fn read_layout(&self) -> Result<Layout> {
        let entry = size_of::<PARTITION_INFORMATION_EX>();
        let head = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        let mut count = 128;
        loop {
            let mut out = buffer(head + count * entry);
            match ioctl(
                self.handle(),
                IOCTL_DISK_GET_DRIVE_LAYOUT_EX,
                &[],
                bytes_of(&mut out),
            ) {
                Ok(_) => {}
                Err(e) if e.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32) => {
                    count *= 2;
                    continue;
                }
                Err(e) => {
                    return Err(e)
                        .into_diagnostic()
                        .wrap_err("reading the partition table");
                }
            }
            // SAFETY: the call wrote a DRIVE_LAYOUT_INFORMATION_EX with
            // PartitionCount entries into the buffer, which is aligned for it.
            let header =
                unsafe { std::ptr::read(out.as_ptr() as *const DRIVE_LAYOUT_INFORMATION_EX) };
            if header.PartitionStyle != PARTITION_STYLE_GPT as u32 {
                bail!(
                    "the disk Windows is on has an MBR partition table; LosOS needs GPT, as UEFI machines use"
                );
            }
            let base = out.as_ptr() as *const u8;
            let entries = (0..header.PartitionCount as usize)
                .map(|i| {
                    // SAFETY: entry i lies within the bytes the call wrote.
                    unsafe {
                        std::ptr::read_unaligned(
                            base.add(head + i * entry) as *const PARTITION_INFORMATION_EX
                        )
                    }
                })
                // Unused GPT slots come back as zero-length entries.
                .filter(|e| e.PartitionLength > 0)
                .collect();
            return Ok(Layout { header, entries });
        }
    }

    pub fn write_layout(&self, layout: &Layout) -> Result<()> {
        let entry = size_of::<PARTITION_INFORMATION_EX>();
        let head = offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry);
        let mut header = layout.header;
        header.PartitionCount = layout.entries.len() as u32;
        let mut input = buffer(head + layout.entries.len().max(1) * entry);
        let bytes = bytes_of(&mut input);
        // SAFETY: both writes stay inside the buffer, which is sized for
        // the header and every entry.
        unsafe {
            std::ptr::write(
                bytes.as_mut_ptr() as *mut DRIVE_LAYOUT_INFORMATION_EX,
                header,
            );
            for (i, e) in layout.entries.iter().enumerate() {
                std::ptr::write_unaligned(
                    bytes.as_mut_ptr().add(head + i * entry) as *mut PARTITION_INFORMATION_EX,
                    *e,
                );
            }
        }
        let len = head + layout.entries.len() * entry;
        ioctl(
            self.handle(),
            IOCTL_DISK_SET_DRIVE_LAYOUT_EX,
            &bytes[..len],
            &mut [],
        )
        .into_diagnostic()
        .wrap_err("writing the partition table")?;
        ioctl(self.handle(), IOCTL_DISK_UPDATE_PROPERTIES, &[], &mut [])
            .into_diagnostic()
            .wrap_err("telling Windows the partition table changed")?;
        Ok(())
    }

    pub fn device(&self) -> Result<Device> {
        Ok(Device::new(
            self.file.try_clone().into_diagnostic()?,
            self.sector,
        ))
    }
}

fn entry_for(part: &NewPartition) -> PARTITION_INFORMATION_EX {
    // SAFETY: zeroed is this struct's documented default.
    let mut e: PARTITION_INFORMATION_EX = unsafe { std::mem::zeroed() };
    e.PartitionStyle = PARTITION_STYLE_GPT;
    e.StartingOffset = part.start as i64;
    e.PartitionLength = part.size as i64;
    e.RewritePartition = true;
    let mut name = [0u16; 36];
    for (slot, unit) in name.iter_mut().zip(part.label.encode_utf16()) {
        *slot = unit;
    }
    e.Anonymous.Gpt = PARTITION_INFORMATION_GPT {
        PartitionType: to_guid(part.type_guid),
        PartitionId: to_guid(part.uuid),
        Attributes: part.attributes,
        Name: name,
    };
    e
}

// ---------------------------------------------------------------------------
// The ESP

/// The ESP mounted at a free drive letter for as long as this lives, with
/// `mountvol`, which is the one way Windows offers to reach it.
pub struct MountedEsp {
    pub root: PathBuf,
    drive: String,
}

impl MountedEsp {
    pub fn mount(disk: u32, esp_offset: u64) -> Result<MountedEsp> {
        // SAFETY: no arguments.
        let used = unsafe { GetLogicalDrives() };
        let free = (b'D'..=b'Z')
            .rev()
            .find(|l| used & (1 << (l - b'A')) == 0)
            .ok_or_else(|| miette!("there is no free drive letter to mount the ESP at"))?;
        let drive = format!("{}:", free as char);
        let status = Command::new("mountvol")
            .args([drive.as_str(), "/S"])
            .status()
            .into_diagnostic()?;
        if !status.success() {
            bail!("mountvol could not mount the EFI system partition ({status})");
        }
        let mounted = MountedEsp {
            root: PathBuf::from(format!("{drive}\\")),
            drive: drive.clone(),
        };
        // mountvol mounts the ESP Windows booted from; it has to be the one
        // on the disk LosOS goes on, since that is where Limine looks for
        // LosOS and for Windows' boot manager.
        let extent = volume_extent(&drive)?;
        if extent.DiskNumber != disk || extent.StartingOffset as u64 != esp_offset {
            bail!(
                "Windows boots from an EFI system partition on another disk than the one it is installed on"
            );
        }
        Ok(mounted)
    }
}

impl Drop for MountedEsp {
    fn drop(&mut self) {
        let _ = Command::new("mountvol")
            .args([self.drive.as_str(), "/D"])
            .status();
    }
}

// ---------------------------------------------------------------------------
// Firmware variables

fn enable_firmware_access() -> Result<()> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: token is valid for the call.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    } == 0
    {
        return Err(last_error()).into_diagnostic();
    }
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: the name is a static wide string; luid is valid.
    let found =
        unsafe { LookupPrivilegeValueW(std::ptr::null(), SE_SYSTEM_ENVIRONMENT_NAME, &mut luid) };
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: token is open and privileges valid for the call.
    let adjusted = found != 0
        && unsafe {
            AdjustTokenPrivileges(
                token,
                0,
                &privileges,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0;
    // AdjustTokenPrivileges succeeds without granting a privilege the
    // account does not hold, and says so only in the last error.
    // SAFETY: no arguments.
    let error = unsafe { GetLastError() };
    // SAFETY: token was opened above.
    unsafe { CloseHandle(token) };
    if !adjusted || error != 0 {
        bail!("Windows did not allow access to the firmware's boot entries");
    }
    Ok(())
}

fn get_variable(name: &str) -> Result<Option<Vec<u8>>> {
    let (name_w, guid) = (wide(name), wide(efi::GLOBAL_VARIABLE));
    let mut buf = vec![0u8; 4096];
    // SAFETY: buf is valid for its length.
    let n = unsafe {
        GetFirmwareEnvironmentVariableW(
            name_w.as_ptr(),
            guid.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
        )
    };
    if n == 0 {
        // SAFETY: no arguments.
        if unsafe { GetLastError() } == ERROR_ENVVAR_NOT_FOUND {
            return Ok(None);
        }
        return Err(last_error())
            .into_diagnostic()
            .wrap_err_with(|| format!("reading the firmware variable {name}"));
    }
    buf.truncate(n as usize);
    Ok(Some(buf))
}

/// Sets `name`, or deletes it when `data` is empty.
fn set_variable(name: &str, data: &[u8]) -> Result<()> {
    let (name_w, guid) = (wide(name), wide(efi::GLOBAL_VARIABLE));
    // SAFETY: data is valid for its length.
    let ok = unsafe {
        SetFirmwareEnvironmentVariableExW(
            name_w.as_ptr(),
            guid.as_ptr(),
            data.as_ptr().cast(),
            data.len() as u32,
            efi::ATTRIBUTES,
        )
    };
    if ok == 0 {
        return Err(last_error())
            .into_diagnostic()
            .wrap_err_with(|| format!("writing the firmware variable {name}"));
    }
    Ok(())
}

/// A `Boot####` number and its load option.
type BootEntry = (u16, Vec<u8>);

/// Every `Boot####` the firmware's order names, with its contents.
fn boot_entries() -> Result<(Vec<u16>, Vec<BootEntry>)> {
    let order = efi::parse_order(&get_variable("BootOrder")?.unwrap_or_default());
    let mut entries = Vec::new();
    for &n in &order {
        if let Some(data) = get_variable(&efi::boot_variable(n))? {
            entries.push((n, data));
        }
    }
    Ok((order, entries))
}

fn is_ours(option: &[u8], loader: &str) -> bool {
    efi::file_path(option).is_some_and(|p| p.eq_ignore_ascii_case(loader))
}

// ---------------------------------------------------------------------------
// The target

pub struct WindowsTarget {
    disk: Disk,
    device: Device,
    esp_offset: u64,
    esp_uuid: Guid,
    /// The table before LosOS's partitions went in, for undo.
    before: Option<Layout>,
    esp_created: Vec<String>,
    boot_entry: Option<(u16, Vec<u16>)>,
}

impl WindowsTarget {
    pub fn new(disk: Disk, layout: &Layout) -> Result<WindowsTarget> {
        let esp = layout
            .esp()
            .ok_or_else(|| miette!("the disk Windows is on has no EFI system partition"))?;
        Ok(WindowsTarget {
            device: disk.device()?,
            esp_offset: esp.StartingOffset as u64,
            esp_uuid: Layout::uuid_of(esp),
            disk,
            before: None,
            esp_created: Vec::new(),
            boot_entry: None,
        })
    }
}

impl Target for WindowsTarget {
    fn device(&mut self) -> &mut Device {
        &mut self.device
    }

    fn add_partitions(&mut self, plan: &Plan) -> Result<()> {
        let before = self.disk.read_layout()?;
        // SAFETY: GPT layout, checked by read_layout.
        let max = unsafe { before.header.Anonymous.Gpt.MaxPartitionCount } as usize;
        if before.entries.len() + 3 > max {
            bail!("the partition table has no room for three more partitions");
        }
        let mut after = before.clone();
        after
            .entries
            .extend(plan.partitions().iter().map(|p| entry_for(p)));
        self.before = Some(before);
        self.disk.write_layout(&after)?;
        // Read back: the driver can accept a layout and keep another.
        let written = self.disk.read_layout()?;
        for p in plan.partitions() {
            let found = written.at(p.start).is_some_and(|e| {
                e.PartitionLength as u64 == p.size
                    && Layout::type_of(e) == p.type_guid
                    && Layout::uuid_of(e) == p.uuid
            });
            if !found {
                bail!(
                    "Windows did not keep the partition {} at {}",
                    p.label,
                    p.start
                );
            }
        }
        Ok(())
    }

    fn put_esp(&mut self, files: &[EspFile]) -> Result<Vec<String>> {
        let esp = MountedEsp::mount(self.disk.number, self.esp_offset)?;
        let mut created = Vec::new();
        for file in files {
            let path = esp.root.join(file.path.replace('/', "\\"));
            let existed = path.exists();
            if existed && !file.replace {
                continue;
            }
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)
                    .into_diagnostic()
                    .wrap_err_with(|| dir.display().to_string())?;
            }
            fs::write(&path, file.bytes)
                .into_diagnostic()
                .wrap_err_with(|| format!("writing {} on the ESP", file.path))?;
            if !existed {
                created.push(file.path.clone());
            }
        }
        self.esp_created.extend(created.iter().cloned());
        Ok(created)
    }

    fn add_boot_entry(&mut self, description: &str, loader: &str) -> Result<()> {
        enable_firmware_access()?;
        let (order, entries) = boot_entries()?;
        // Windows Boot Manager's entry leads the firmware to this ESP in the
        // form it is known to boot; LosOS's is the same with its own file.
        let windows = entries.iter().find(|(_, data)| {
            efi::file_path(data)
                .is_some_and(|p| p.eq_ignore_ascii_case(r"\EFI\Microsoft\Boot\bootmgfw.efi"))
        });
        let option = windows
            .and_then(|(_, data)| efi::beside(data, self.esp_uuid, description, loader))
            .map(Ok)
            .unwrap_or_else(|| -> Result<Vec<u8>> {
                let layout = self.disk.read_layout()?;
                let esp = layout
                    .esp()
                    .ok_or_else(|| miette!("the ESP is gone from the partition table"))?;
                Ok(efi::load_option(
                    description,
                    &efi::Partition {
                        number: esp.PartitionNumber,
                        start_lba: esp.StartingOffset as u64 / self.disk.sector,
                        size_lba: esp.PartitionLength as u64 / self.disk.sector,
                        uuid: self.esp_uuid,
                    },
                    loader,
                ))
            })?;
        // A reinstall reuses LosOS's entry rather than adding another.
        let number = match entries.iter().find(|(_, d)| is_ours(d, loader)) {
            Some((n, _)) => *n,
            None => (0..=0xffffu16)
                .find(|n| {
                    !order.contains(n)
                        && get_variable(&efi::boot_variable(*n))
                            .ok()
                            .flatten()
                            .is_none()
                })
                .ok_or_else(|| miette!("the firmware has no free boot entry number"))?,
        };
        set_variable(&efi::boot_variable(number), &option)?;
        set_variable(
            "BootOrder",
            &efi::encode_order(&efi::with_first(&order, number)),
        )?;
        self.boot_entry = Some((number, order));
        Ok(())
    }

    fn undo(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        if let Some((number, order)) = self.boot_entry.take()
            && let Err(e) = set_variable("BootOrder", &efi::encode_order(&order))
                .and_then(|()| set_variable(&efi::boot_variable(number), &[]))
        {
            errors.push(format!("{e:?}"));
        }
        if !self.esp_created.is_empty() {
            match MountedEsp::mount(self.disk.number, self.esp_offset) {
                Ok(esp) => {
                    for path in self.esp_created.drain(..) {
                        let _ = fs::remove_file(esp.root.join(path.replace('/', "\\")));
                    }
                }
                Err(e) => errors.push(format!("{e:?}")),
            }
        }
        if let Some(before) = self.before.take()
            && let Err(e) = self.disk.write_layout(&before)
        {
            errors.push(format!("{e:?}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("undoing the install failed: {}", errors.join("; "))
        }
    }
}

// ---------------------------------------------------------------------------
// Uninstalling

/// LosOS's partitions on `layout`: the run of partitions of LosOS's types
/// directly behind Windows' at `windows_end`. Only that run, so another
/// Linux elsewhere on the disk is left alone.
pub fn losos_partitions(layout: &Layout, windows_end: u64, usr: &[Guid]) -> Vec<usize> {
    let ours =
        |g: Guid| g == types::XBOOTLDR || g == types::HOME || g == types::SWAP || usr.contains(&g);
    let mut sorted: Vec<usize> = (0..layout.entries.len()).collect();
    sorted.sort_by_key(|&i| layout.entries[i].StartingOffset);
    sorted
        .into_iter()
        .skip_while(|&i| (layout.entries[i].StartingOffset as u64) < windows_end)
        .take_while(|&i| ours(Layout::type_of(&layout.entries[i])))
        .collect()
}

pub fn remove_partitions(disk: &Disk, layout: &Layout, which: &[usize]) -> Result<()> {
    let mut after = layout.clone();
    let mut keep = Vec::new();
    for (i, e) in layout.entries.iter().enumerate() {
        if !which.contains(&i) {
            keep.push(*e);
        }
    }
    after.entries = keep;
    disk.write_layout(&after)
}

pub fn remove_esp_files(disk: u32, esp_offset: u64) -> Result<Vec<String>> {
    let esp = MountedEsp::mount(disk, esp_offset)?;
    let marker = esp.root.join(crate::install::MARKER.replace('/', "\\"));
    let Ok(list) = fs::read_to_string(&marker) else {
        return Ok(Vec::new());
    };
    let mut removed = Vec::new();
    for path in list.lines().filter(|l| !l.trim().is_empty()) {
        let full = esp.root.join(path.replace('/', "\\"));
        if fs::remove_file(&full).is_ok() {
            removed.push(path.to_owned());
        }
        // The directories it leaves empty go too; a full one stays.
        let mut dir = full.parent();
        while let Some(d) = dir {
            if d == esp.root || fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    Ok(removed)
}

pub fn remove_boot_entry(loader: &str) -> Result<bool> {
    enable_firmware_access()?;
    let (order, entries) = boot_entries()?;
    let Some((number, _)) = entries.iter().find(|(_, d)| is_ours(d, loader)) else {
        return Ok(false);
    };
    let order: Vec<u16> = order.into_iter().filter(|n| n != number).collect();
    set_variable("BootOrder", &efi::encode_order(&order))?;
    set_variable(&efi::boot_variable(*number), &[])?;
    Ok(true)
}
