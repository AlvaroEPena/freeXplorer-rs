//! Presents a FatxVolume to WinFsp, which makes it show up as a drive in Explorer. Every callback
//! maps onto a FatxVolume call. Anything FATX can't express (streams, reparse points, ACLs) is
//! left out and WinFsp is told so through the volume flags.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::NaiveDateTime;
use fx_core::fatx::{Attributes, FatxVolume, NodeId, NodeInfo, name, time};
use fx_core::{Error, ErrorKind};
use winfsp::filesystem::{
    DirBuffer, DirBufferLock, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, ModificationDescriptor,
    OpenFileInfo, VolumeInfo, WideNameInfo,
};
use winfsp::{FspError, Result, U16CStr};

const ATTR_READ_ONLY: u32 = 0x01;
const ATTR_HIDDEN: u32 = 0x02;
const ATTR_SYSTEM: u32 = 0x04;
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_ARCHIVE: u32 = 0x20;
const ATTR_NORMAL: u32 = 0x80;
const ATTR_INVALID: u32 = 0xFFFF_FFFF;

const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
const CLEANUP_DELETE: u32 = 0x01;

/// write data, append data and delete: what a read-only volume must not be opened for
const WRITE_ACCESS: u32 = 0x0002 | 0x0004 | 0x0001_0000;

/// Explorer wants to park deleted files in a Recycle Bin folder on the drive itself, where they'd
/// keep using up space on a console partition. Saying no makes it delete for real, like on a USB stick.
const RECYCLE_BIN: &str = "$RECYCLE.BIN";

/// everyone gets full control, FATX has no owners
pub const OPEN_SECURITY: &str = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)";

const STATUS_END_OF_FILE: i32 = 0xC000_0011u32 as i32;
const STATUS_INVALID_PARAMETER: i32 = 0xC000_000Du32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;
const STATUS_OBJECT_NAME_INVALID: i32 = 0xC000_0033u32 as i32;
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;
const STATUS_OBJECT_NAME_COLLISION: i32 = 0xC000_0035u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003Au32 as i32;
const STATUS_DISK_FULL: i32 = 0xC000_007Fu32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BAu32 as i32;
const STATUS_MEDIA_WRITE_PROTECTED: i32 = 0xC000_00A2u32 as i32;
const STATUS_UNEXPECTED_IO_ERROR: i32 = 0xC000_00E9u32 as i32;
const STATUS_DIRECTORY_NOT_EMPTY: i32 = 0xC000_0101u32 as i32;
const STATUS_FILE_CORRUPT_ERROR: i32 = 0xC000_0102u32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103u32 as i32;
const STATUS_NAME_TOO_LONG: i32 = 0xC000_0106u32 as i32;
const STATUS_IO_DEVICE_ERROR: i32 = 0xC000_0185u32 as i32;
const STATUS_CANNOT_MAKE: i32 = 0xC000_02EAu32 as i32;
const STATUS_FILE_TOO_LARGE: i32 = 0xC000_0904u32 as i32;
const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005u32 as i32;

fn status(code: i32) -> FspError {
    FspError::NTSTATUS(code)
}

fn status_of(err: &Error) -> i32 {
    match err {
        Error::Fatx { kind, .. } => match kind {
            ErrorKind::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
            ErrorKind::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
            ErrorKind::NotADirectory => STATUS_NOT_A_DIRECTORY,
            ErrorKind::IsADirectory => STATUS_FILE_IS_A_DIRECTORY,
            ErrorKind::DirectoryNotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
            ErrorKind::InvalidName => STATUS_OBJECT_NAME_INVALID,
            ErrorKind::PathTooLong => STATUS_NAME_TOO_LONG,
            ErrorKind::DirectoryFull => STATUS_CANNOT_MAKE,
            ErrorKind::DiskFull => STATUS_DISK_FULL,
            ErrorKind::FileTooLarge => STATUS_FILE_TOO_LARGE,
            ErrorKind::ReadOnly => STATUS_MEDIA_WRITE_PROTECTED,
            ErrorKind::Corrupt => STATUS_FILE_CORRUPT_ERROR,
            _ => STATUS_INVALID_PARAMETER,
        },
        Error::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => STATUS_ACCESS_DENIED,
        Error::Io(_) => STATUS_IO_DEVICE_ERROR,
        _ => STATUS_UNEXPECTED_IO_ERROR,
    }
}

fn fail(err: Error) -> FspError {
    status(status_of(&err))
}

/// One open file or folder.
pub struct Open {
    node: NodeId,
    /// WinFsp's buffer of directory entries, kept between the calls that page through a listing
    listing: DirBuffer,
}

impl Open {
    fn new(node: NodeId) -> Open {
        Open {
            node,
            listing: DirBuffer::new(),
        }
    }
}

pub struct FatxFs {
    volume: Arc<Mutex<FatxVolume>>,
    label: Mutex<String>,
    security: Vec<u8>,
    read_only: bool,
    cluster_size: u32,
}

impl FatxFs {
    pub fn new(volume: Arc<Mutex<FatxVolume>>, label: String, security: Vec<u8>) -> FatxFs {
        let (read_only, cluster_size) = {
            let v = volume.lock().unwrap_or_else(|p| p.into_inner());
            (v.is_read_only(), v.cluster_size())
        };
        FatxFs {
            volume,
            label: Mutex::new(label),
            security,
            read_only,
            cluster_size,
        }
    }

    fn lock(&self) -> MutexGuard<'_, FatxVolume> {
        self.volume.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn writable(&self) -> Result<()> {
        if self.read_only {
            Err(status(STATUS_MEDIA_WRITE_PROTECTED))
        } else {
            Ok(())
        }
    }

    fn describe(&self, out: &mut FileInfo, info: &NodeInfo) {
        let size = if info.is_directory { 0 } else { info.size };
        let cluster = self.cluster_size as u64;
        let write = to_file_time(info.modified);
        out.file_attributes = windows_attributes(info);
        out.reparse_tag = 0;
        out.allocation_size = size.div_ceil(cluster) * cluster;
        out.file_size = size;
        out.creation_time = to_file_time(info.created);
        out.last_access_time = to_file_time(info.accessed);
        out.last_write_time = write;
        out.change_time = write;
        out.index_number = info.id.index() as u64;
        out.hard_links = 0;
        out.ea_size = 0;
    }

    fn copy_security(&self, buffer: Option<&mut [c_void]>) -> Result<u64> {
        let wanted = self.security.len();
        if let Some(buffer) = buffer {
            if buffer.len() < wanted {
                return Err(status(STATUS_BUFFER_OVERFLOW));
            }
            // SAFETY: the buffer is at least as long as the descriptor, and they can't overlap
            unsafe { std::ptr::copy_nonoverlapping(self.security.as_ptr(), buffer.as_mut_ptr().cast::<u8>(), wanted) };
        }
        Ok(wanted as u64)
    }

    fn fill_volume(&self, vol: &FatxVolume, out: &mut VolumeInfo) {
        out.total_size = vol.total_bytes();
        out.free_size = vol.free_bytes();
        out.set_volume_label(&*self.label.lock().unwrap_or_else(|p| p.into_inner()));
    }

    fn add_entry(&self, lock: &DirBufferLock<'_>, entry_name: &str, info: &NodeInfo) -> Result<()> {
        let mut entry = DirInfo::<255>::new();
        entry.set_name(entry_name)?;
        self.describe(entry.file_info_mut(), info);
        lock.write(&mut entry)
    }
}

fn split(path: &str) -> (&str, &str) {
    match path.rfind('\\') {
        None => ("\\", path),
        Some(0) => ("\\", &path[1..]),
        Some(cut) => (&path[..cut], &path[cut + 1..]),
    }
}

/// What to say about a name that isn't there: the folder is missing, or just the file.
fn missing(vol: &mut FatxVolume, path: &str) -> FspError {
    let (parent, _) = split(path);
    match vol.find(parent) {
        Ok(Some(id)) if vol.info(id).is_directory => status(STATUS_OBJECT_NAME_NOT_FOUND),
        _ => status(STATUS_OBJECT_PATH_NOT_FOUND),
    }
}

fn check_name(leaf: &str) -> Result<()> {
    if name::is_valid(leaf) {
        Ok(())
    } else if leaf.chars().count() > name::MAX_LENGTH {
        Err(status(STATUS_NAME_TOO_LONG))
    } else {
        Err(status(STATUS_OBJECT_NAME_INVALID))
    }
}

fn windows_attributes(info: &NodeInfo) -> u32 {
    let a = info.attributes;
    let mut result = 0;
    if info.is_directory {
        result |= ATTR_DIRECTORY;
    }
    if a.contains(Attributes::READ_ONLY) {
        result |= ATTR_READ_ONLY;
    }
    if a.contains(Attributes::HIDDEN) {
        result |= ATTR_HIDDEN;
    }
    if a.contains(Attributes::SYSTEM) {
        result |= ATTR_SYSTEM;
    }
    if a.contains(Attributes::ARCHIVE) && !info.is_directory {
        result |= ATTR_ARCHIVE;
    }
    if result == 0 { ATTR_NORMAL } else { result }
}

fn fatx_attributes(windows: u32) -> Attributes {
    let mut result = Attributes::NONE;
    for (flag, wanted) in [
        (ATTR_READ_ONLY, Attributes::READ_ONLY),
        (ATTR_HIDDEN, Attributes::HIDDEN),
        (ATTR_SYSTEM, Attributes::SYSTEM),
        (ATTR_ARCHIVE, Attributes::ARCHIVE),
    ] {
        if windows & flag != 0 {
            result = result | wanted;
        }
    }
    result
}

const FILETIME_UNIX_GAP: i64 = 11_644_473_600;

fn to_file_time(local: NaiveDateTime) -> u64 {
    let seconds = time::to_utc(local).and_utc().timestamp() + FILETIME_UNIX_GAP;
    seconds.max(0) as u64 * 10_000_000
}

fn from_file_time(stamp: u64) -> Option<NaiveDateTime> {
    if stamp == 0 || stamp == u64::MAX {
        return None;
    }
    let seconds = (stamp / 10_000_000) as i64 - FILETIME_UNIX_GAP;
    chrono::DateTime::from_timestamp(seconds, 0).map(|utc| time::from_utc(utc.naive_utc()))
}

impl FileSystemContext for FatxFs {
    type FileContext = Open;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> Result<FileSecurity> {
        let path = file_name.to_string_lossy();
        let mut vol = self.lock();
        let node = match vol.find(&path).map_err(fail)? {
            Some(id) => id,
            None => return Err(missing(&mut vol, &path)),
        };
        let attributes = windows_attributes(&vol.info(node));
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: self.copy_security(security_descriptor)?,
            attributes,
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> Result<Open> {
        let path = file_name.to_string_lossy();
        let mut vol = self.lock();
        let node = match vol.find(&path).map_err(fail)? {
            Some(id) => id,
            None => return Err(missing(&mut vol, &path)),
        };
        let info = vol.info(node);
        if create_options & FILE_DIRECTORY_FILE != 0 && !info.is_directory {
            return Err(status(STATUS_NOT_A_DIRECTORY));
        }
        if create_options & FILE_NON_DIRECTORY_FILE != 0 && info.is_directory {
            return Err(status(STATUS_FILE_IS_A_DIRECTORY));
        }
        // Same as a CD: say no when the file is opened, not when it's written. Otherwise Windows takes
        // the write into its cache, tells the program it worked, and only fails when it flushes, long
        // after anyone can do anything about it.
        if self.read_only && granted_access & WRITE_ACCESS != 0 {
            return Err(status(STATUS_MEDIA_WRITE_PROTECTED));
        }
        self.describe(file_info.as_mut(), &info);
        Ok(Open::new(node))
    }

    fn close(&self, _context: Open) {}

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: u32,
        file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> Result<Open> {
        self.writable()?;
        let path = file_name.to_string_lossy();
        let (parent_path, leaf) = split(&path);
        let mut vol = self.lock();

        let parent = vol
            .find(parent_path)
            .map_err(fail)?
            .ok_or_else(|| status(STATUS_OBJECT_PATH_NOT_FOUND))?;
        if !vol.info(parent).is_directory {
            return Err(status(STATUS_NOT_A_DIRECTORY));
        }
        check_name(leaf)?;
        if vol.find_child(parent, leaf).map_err(fail)?.is_some() {
            return Err(status(STATUS_OBJECT_NAME_COLLISION));
        }
        if parent == vol.root() && leaf.eq_ignore_ascii_case(RECYCLE_BIN) {
            return Err(status(STATUS_ACCESS_DENIED));
        }

        let node = if create_options & FILE_DIRECTORY_FILE != 0 {
            vol.create_directory(parent, leaf)
        } else {
            vol.create_file(parent, leaf)
        }
        .map_err(fail)?;

        let wanted = fatx_attributes(file_attributes);
        if wanted != Attributes::NONE {
            let keep = vol.info(node).attributes;
            vol.set_attributes(node, keep | wanted).map_err(fail)?;
        }
        self.describe(file_info.as_mut(), &vol.info(node));
        Ok(Open::new(node))
    }

    fn overwrite(
        &self,
        context: &Open,
        file_attributes: u32,
        replace_file_attributes: bool,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        self.writable()?;
        let mut vol = self.lock();
        vol.set_length(context.node, 0).map_err(fail)?;
        let keep = if replace_file_attributes {
            Attributes::NONE
        } else {
            vol.info(context.node).attributes
        };
        vol.set_attributes(context.node, keep | fatx_attributes(file_attributes))
            .map_err(fail)?;
        self.describe(file_info, &vol.info(context.node));
        Ok(())
    }

    fn cleanup(&self, context: &Open, _file_name: Option<&U16CStr>, flags: u32) {
        let mut vol = self.lock();
        let info = vol.info(context.node);
        // nothing to report errors to, cleanup has no return value. The next access sees the real state.
        if flags & CLEANUP_DELETE != 0 {
            if !self.read_only && !info.is_deleted && !info.is_root {
                let _ = vol.delete(context.node);
            }
        } else if !self.read_only {
            let _ = vol.flush_node(context.node);
        }
    }

    fn flush(&self, context: Option<&Open>, file_info: &mut FileInfo) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        let mut vol = self.lock();
        match context {
            None => vol.flush().map_err(fail),
            Some(open) => {
                vol.flush_node(open.node).map_err(fail)?;
                self.describe(file_info, &vol.info(open.node));
                Ok(())
            }
        }
    }

    fn get_file_info(&self, context: &Open, file_info: &mut FileInfo) -> Result<()> {
        let vol = self.lock();
        self.describe(file_info, &vol.info(context.node));
        Ok(())
    }

    fn get_security(&self, _context: &Open, security_descriptor: Option<&mut [c_void]>) -> Result<u64> {
        self.copy_security(security_descriptor)
    }

    // accepted and ignored, there's nowhere to keep it
    fn set_security(
        &self,
        _context: &Open,
        _security_information: u32,
        _descriptor: ModificationDescriptor,
    ) -> Result<()> {
        Ok(())
    }

    fn set_basic_info(
        &self,
        context: &Open,
        file_attributes: u32,
        creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        _last_change_time: u64,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        let mut vol = self.lock();
        self.describe(file_info, &vol.info(context.node));

        let wants_attributes = file_attributes != ATTR_INVALID;
        let wants_times = creation_time != 0 || last_access_time != 0 || last_write_time != 0;
        if !wants_attributes && !wants_times {
            return Ok(());
        }
        self.writable()?;
        if vol.info(context.node).is_root {
            return Ok(());
        }
        if wants_attributes {
            vol.set_attributes(context.node, fatx_attributes(file_attributes))
                .map_err(fail)?;
        }
        if wants_times {
            vol.set_times(
                context.node,
                from_file_time(creation_time),
                from_file_time(last_write_time),
                from_file_time(last_access_time),
            )
            .map_err(fail)?;
        }
        self.describe(file_info, &vol.info(context.node));
        Ok(())
    }

    fn set_file_size(
        &self,
        context: &Open,
        new_size: u64,
        set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> Result<()> {
        let mut vol = self.lock();
        self.describe(file_info, &vol.info(context.node));
        self.writable()?;

        // Clusters are handed out as the file grows, so a bigger allocation is just a promise.
        // A smaller one cuts the file.
        if !set_allocation_size || new_size < vol.info(context.node).size {
            vol.set_length(context.node, new_size).map_err(fail)?;
        }
        self.describe(file_info, &vol.info(context.node));
        Ok(())
    }

    /// Only says whether the delete may go ahead. The deleting itself happens in cleanup.
    fn set_delete(&self, context: &Open, _file_name: &U16CStr, delete_file: bool) -> Result<()> {
        if !delete_file {
            return Ok(());
        }
        self.writable()?;
        let mut vol = self.lock();
        let info = vol.info(context.node);
        if info.is_root {
            return Err(status(STATUS_ACCESS_DENIED));
        }
        if info.is_directory && !vol.children(context.node).map_err(fail)?.is_empty() {
            return Err(status(STATUS_DIRECTORY_NOT_EMPTY));
        }
        Ok(())
    }

    fn rename(
        &self,
        context: &Open,
        _file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> Result<()> {
        self.writable()?;
        let new_path = new_file_name.to_string_lossy();
        let (parent_path, leaf) = split(&new_path);
        let mut vol = self.lock();

        let parent = vol
            .find(parent_path)
            .map_err(fail)?
            .ok_or_else(|| status(STATUS_OBJECT_PATH_NOT_FOUND))?;
        check_name(leaf)?;
        if let Some(existing) = vol.find_child(parent, leaf).map_err(fail)?
            && existing != context.node
        {
            if !replace_if_exists {
                return Err(status(STATUS_OBJECT_NAME_COLLISION));
            }
            if vol.info(existing).is_directory || vol.info(context.node).is_directory {
                return Err(status(STATUS_ACCESS_DENIED));
            }
        }
        vol.rename(context.node, parent, leaf, replace_if_exists).map_err(fail)
    }

    fn read(&self, context: &Open, buffer: &mut [u8], offset: u64) -> Result<u32> {
        let mut vol = self.lock();
        if offset >= vol.info(context.node).size {
            return Err(status(STATUS_END_OF_FILE));
        }
        vol.read(context.node, offset, buffer).map(|n| n as u32).map_err(fail)
    }

    fn write(
        &self,
        context: &Open,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> Result<u32> {
        let mut vol = self.lock();
        self.describe(file_info, &vol.info(context.node));
        self.writable()?;

        let size = vol.info(context.node).size;
        let (mut offset, mut length) = (offset, buffer.len());
        if constrained_io {
            // writes caused by the cache manager never grow the file
            if offset >= size {
                return Ok(0);
            }
            length = length.min((size - offset).min(u32::MAX as u64) as usize);
        } else if write_to_eof {
            offset = size;
        }

        if length > 0 {
            vol.write(context.node, offset, &buffer[..length]).map_err(fail)?;
        }
        self.describe(file_info, &vol.info(context.node));
        Ok(length as u32)
    }

    fn read_directory(
        &self,
        context: &Open,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> Result<u32> {
        // WinFsp keeps what is filled in here and hands out slices of it as the caller pages through
        match context.listing.acquire(marker.is_none(), None) {
            Ok(lock) => {
                let mut vol = self.lock();
                let dir = vol.info(context.node);
                if !dir.is_root {
                    let (parent_path, _) = split(&dir.path);
                    let parent = match vol.find(parent_path) {
                        Ok(Some(id)) => vol.info(id),
                        _ => dir.clone(),
                    };
                    self.add_entry(&lock, ".", &dir)?;
                    self.add_entry(&lock, "..", &parent)?;
                }
                for child in vol.children(context.node).map_err(fail)? {
                    self.add_entry(&lock, &child.name, &child)?;
                }
            }
            // the buffer was filled by an earlier call and only has to be read from
            Err(e) if e.to_ntstatus() >= 0 => {}
            Err(e) => return Err(e),
        }
        Ok(context.listing.read(marker, buffer))
    }

    fn get_volume_info(&self, out: &mut VolumeInfo) -> Result<()> {
        let vol = self.lock();
        self.fill_volume(&vol, out);
        Ok(())
    }

    fn set_volume_label(&self, label: &U16CStr, out: &mut VolumeInfo) -> Result<()> {
        self.writable()?;
        let wanted = label.to_string_lossy();
        let mut vol = self.lock();
        vol.set_label(&wanted).map_err(fail)?;
        *self.label.lock().unwrap_or_else(|p| p.into_inner()) = wanted;
        self.fill_volume(&vol, out);
        Ok(())
    }
}
