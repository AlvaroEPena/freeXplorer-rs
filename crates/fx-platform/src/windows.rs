use std::collections::HashSet;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, Prefix};
use std::ptr::{null, null_mut};

use fx_core::backup::DestinationInfo;
use fx_core::device::Device;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Power::{PowerClearRequest, PowerCreateRequest, PowerSetRequest};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
};

use crate::DiskInfo;
use crate::raw::open_raw;

const SHARE_ALL: u32 = 7;
const IOCTL_DISK_GET_DRIVE_GEOMETRY_EX: u32 = 0x0007_00A0;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const FILE_SUPPORTS_SPARSE_FILES: u32 = 0x40;
const MAX_DISKS: u32 = 32;

fn physical_path(number: u32) -> String {
    format!(r"\\.\PhysicalDrive{number}")
}

pub fn normalize_id(rest: &str) -> Option<String> {
    rest.trim().parse::<u32>().ok().map(|n| n.to_string())
}

pub fn parse_path(spec: &str) -> Option<String> {
    let lower = spec.to_ascii_lowercase();
    lower.strip_prefix(r"\\.\physicaldrive").and_then(normalize_id)
}

/// A handle that can ask questions but not read or write, which needs no elevation.
fn open_query(path: &str) -> Option<File> {
    OpenOptions::new().access_mode(0).share_mode(SHARE_ALL).open(path).ok()
}

fn ioctl(file: &File, code: u32, input: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; out_len];
    let mut returned = 0u32;
    // SAFETY: the handle belongs to `file`, and both buffers outlive the call
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            code,
            if input.is_empty() {
                null()
            } else {
                input.as_ptr().cast::<c_void>()
            },
            input.len() as u32,
            out.as_mut_ptr().cast::<c_void>(),
            out_len as u32,
            &mut returned,
            null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    out.truncate(returned as usize);
    Some(out)
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn string_at(data: &[u8], offset: u32) -> String {
    let offset = offset as usize;
    if offset == 0 || offset >= data.len() {
        return String::new();
    }
    let end = data[offset..]
        .iter()
        .position(|&b| b == 0)
        .map_or(data.len(), |n| offset + n);
    String::from_utf8_lossy(&data[offset..end]).trim().to_string()
}

fn bus_name(bus: u32) -> &'static str {
    match bus {
        1 => "SCSI",
        2 => "ATAPI",
        3 => "ATA",
        4 => "IEEE 1394",
        5 => "SSA",
        6 => "Fibre",
        7 => "USB",
        8 => "RAID",
        9 => "iSCSI",
        10 => "SAS",
        11 => "SATA",
        12 => "SD",
        13 => "MMC",
        14 => "Virtual",
        15 => "File backed virtual",
        16 => "Storage Spaces",
        17 => "NVMe",
        _ => "Unknown",
    }
}

/// Which physical disks hold a given volume, by drive letter ("C:").
fn disks_of_volume(letter_and_colon: &str) -> HashSet<u32> {
    let mut found = HashSet::new();
    let Some(volume) = open_query(&format!(r"\\.\{letter_and_colon}")) else {
        return found;
    };
    if let Some(extents) = ioctl(&volume, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], 1024)
        && extents.len() >= 8
    {
        let count = u32_at(&extents, 0) as usize;
        for i in 0..count {
            let at = 8 + i * 24;
            if at + 4 > extents.len() {
                break;
            }
            found.insert(u32_at(&extents, at));
        }
    }
    found
}

fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn disks_with_volumes() -> HashSet<u32> {
    let mut found = HashSet::new();
    // SAFETY: no arguments
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = to_wide(&format!("{letter}:\\"));
        // SAFETY: `root` is a nul terminated string that lives through the call
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        if kind == DRIVE_REMOVABLE || kind == DRIVE_FIXED {
            found.extend(disks_of_volume(&format!("{letter}:")));
        }
    }
    found
}

pub fn list_disks() -> Vec<DiskInfo> {
    let with_volumes = disks_with_volumes();
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let system = disks_of_volume(&system_drive);

    let mut disks = Vec::new();
    for number in 0..MAX_DISKS {
        let path = physical_path(number);
        let Some(handle) = open_query(&path) else {
            continue;
        };
        let Some(geometry) = ioctl(&handle, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], 128) else {
            continue;
        };
        if geometry.len() < 32 {
            continue;
        }

        let (mut model, mut serial, mut bus, mut removable) = (String::new(), String::new(), "Unknown", false);
        let query = [0u8; 12];
        if let Some(d) = ioctl(&handle, IOCTL_STORAGE_QUERY_PROPERTY, &query, 1024)
            && d.len() >= 36
        {
            removable = d[10] != 0;
            let vendor = string_at(&d, u32_at(&d, 12));
            let product = string_at(&d, u32_at(&d, 16));
            model = format!("{vendor} {product}").trim().to_string();
            serial = string_at(&d, u32_at(&d, 24));
            bus = bus_name(u32_at(&d, 28));
        }

        disks.push(DiskInfo {
            id: number.to_string(),
            path,
            length: u64_at(&geometry, 24),
            sector_size: u32_at(&geometry, 20),
            model,
            serial,
            bus: bus.to_string(),
            removable,
            has_volumes: with_volumes.contains(&number),
            is_system: system.contains(&number),
        });
    }
    disks
}

pub fn open_disk(disk: &DiskInfo, writable: bool) -> io::Result<Device> {
    // A list on screen can be old, and disk numbers get reshuffled when something is plugged in or
    // pulled out. A USB adapter keeps its name and serial whichever drive is inside it, so the size
    // is what tells the drives apart.
    let now = list_disks().into_iter().find(|d| d.id == disk.id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("Disk {} isn't connected any more.", disk.id),
        )
    })?;
    if now.length != disk.length || now.sector_size != disk.sector_size {
        return Err(io::Error::other(format!(
            "Disk {} is a different drive now. It was {} bytes when the list was loaded and it is {} bytes now.",
            disk.id, disk.length, now.length
        )));
    }
    open_raw(
        &now.path,
        &format!("disk {}", now.id),
        now.length,
        now.sector_size,
        writable,
        |options| {
            options.share_mode(3);
        },
    )
}

pub fn is_elevated() -> bool {
    // SAFETY: the token handle is closed before returning and the output struct is the right size
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

pub fn destination_info(path: &Path) -> DestinationInfo {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let (root, unc) = match full.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => (format!("{}:\\", letter as char), false),
            Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => (
                format!(r"\\{}\{}\", server.to_string_lossy(), share.to_string_lossy()),
                true,
            ),
            _ => return DestinationInfo::default(),
        },
        _ => return DestinationInfo::default(),
    };

    let wide: Vec<u16> = std::ffi::OsStr::new(&root).encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a nul terminated string that lives through every call below
    unsafe {
        let network = unc || GetDriveTypeW(wide.as_ptr()) == DRIVE_REMOTE;

        let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
        let free_bytes =
            (GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) != 0).then_some(available);

        let mut name = [0u16; 64];
        let (mut longest, mut flags) = (0u32, 0u32);
        let known = GetVolumeInformationW(
            wide.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            &mut longest,
            &mut flags,
            name.as_mut_ptr(),
            name.len() as u32,
        ) != 0;
        let file_system = if known {
            String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())])
        } else {
            String::new()
        };

        DestinationInfo {
            max_file_size: file_system
                .to_ascii_uppercase()
                .starts_with("FAT")
                .then_some(u32::MAX as u64),
            supports_sparse: known && flags & FILE_SUPPORTS_SPARSE_FILES != 0,
            root,
            free_bytes,
            file_system,
            network,
        }
    }
}

/// A power request, which unlike SetThreadExecutionState doesn't belong to one thread. The work
/// being protected hops between threads all the time.
pub struct Awake {
    handle: isize,
    _reason: Vec<u16>,
}

impl Awake {
    pub fn new(reason: &str) -> Awake {
        let mut text = to_wide(reason);
        let context = REASON_CONTEXT {
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: text.as_mut_ptr(),
            },
        };
        // SAFETY: the context and the string it points to are alive for the call
        let handle = unsafe { PowerCreateRequest(&context) };
        let mut awake = Awake {
            handle: handle as isize,
            _reason: text,
        };
        // if Windows won't give us one the work just goes ahead without it
        // SAFETY: only called with the handle PowerCreateRequest returned
        if handle.is_null() || handle == INVALID_HANDLE_VALUE || unsafe { PowerSetRequest(handle, 1) } == 0 {
            awake.release();
        }
        awake
    }

    fn release(&mut self) {
        let handle = self.handle as HANDLE;
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is ours and gets cleared so it can't be closed twice
            unsafe {
                PowerClearRequest(handle, 1);
                CloseHandle(handle);
            }
        }
        self.handle = 0;
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.release();
    }
}
