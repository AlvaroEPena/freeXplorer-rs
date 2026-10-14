//! Presents a FatxVolume through FUSE, so it shows up as a folder on Linux and macOS. Every call
//! maps onto a FatxVolume call, the same way the WinFsp side does it.
//!
//! FUSE talks in inode numbers. A node's number is its place in the volume's table plus one, so
//! the root is 1 as FUSE wants, and a number that comes back is looked up again with `node_at`.

use std::ffi::OsStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::NaiveDateTime;
use fuser::{
    AccessFlags, BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    LockOwner, OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
};
use fx_core::fatx::{Attributes, FatxVolume, NodeId, NodeInfo, name, time};
use fx_core::{Error, ErrorKind};

/// How long the kernel may remember what it was told. Nothing else writes to the volume, so a bit.
const TTL: Duration = Duration::from_secs(1);

pub struct FatxFuse {
    volume: Arc<Mutex<FatxVolume>>,
    read_only: bool,
    cluster_size: u32,
    uid: u32,
    gid: u32,
}

impl FatxFuse {
    pub fn new(volume: Arc<Mutex<FatxVolume>>, uid: u32, gid: u32) -> FatxFuse {
        let (read_only, cluster_size) = {
            let v = volume.lock().unwrap_or_else(|p| p.into_inner());
            (v.is_read_only(), v.cluster_size())
        };
        FatxFuse {
            volume,
            read_only,
            cluster_size,
            uid,
            gid,
        }
    }

    fn lock(&self) -> MutexGuard<'_, FatxVolume> {
        self.volume.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn attr(&self, info: &NodeInfo) -> FileAttr {
        let kind = if info.is_directory {
            FileType::Directory
        } else {
            FileType::RegularFile
        };
        let read_only = self.read_only || info.attributes.contains(Attributes::READ_ONLY);
        let perm = match (info.is_directory, read_only) {
            (true, false) => 0o755,
            (true, true) => 0o555,
            (false, false) => 0o644,
            (false, true) => 0o444,
        };
        let size = if info.is_directory { 0 } else { info.size };
        let cluster = self.cluster_size as u64;
        FileAttr {
            ino: ino_of(info.id),
            size,
            blocks: size.div_ceil(cluster) * cluster / 512,
            atime: to_system_time(info.accessed),
            mtime: to_system_time(info.modified),
            ctime: to_system_time(info.modified),
            crtime: to_system_time(info.created),
            kind,
            perm,
            nlink: if info.is_directory { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: self.cluster_size,
            flags: 0,
        }
    }
}

fn ino_of(id: NodeId) -> INodeNo {
    INodeNo(id.index() as u64 + 1)
}

fn node(vol: &FatxVolume, ino: INodeNo) -> Option<NodeId> {
    vol.node_at(usize::try_from(ino.0.checked_sub(1)?).ok()?)
}

pub fn errno(err: &Error) -> Errno {
    match err {
        Error::Fatx { kind, .. } => match kind {
            ErrorKind::NotFound => Errno::ENOENT,
            ErrorKind::AlreadyExists => Errno::EEXIST,
            ErrorKind::NotADirectory => Errno::ENOTDIR,
            ErrorKind::IsADirectory => Errno::EISDIR,
            ErrorKind::DirectoryNotEmpty => Errno::ENOTEMPTY,
            ErrorKind::InvalidName => Errno::EINVAL,
            ErrorKind::PathTooLong => Errno::ENAMETOOLONG,
            ErrorKind::DirectoryFull | ErrorKind::DiskFull => Errno::ENOSPC,
            ErrorKind::FileTooLarge => Errno::EFBIG,
            ErrorKind::ReadOnly => Errno::EROFS,
            _ => Errno::EIO,
        },
        Error::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Errno::EACCES,
        _ => Errno::EIO,
    }
}

fn to_system_time(local: NaiveDateTime) -> SystemTime {
    let seconds = time::to_utc(local).and_utc().timestamp();
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    }
}

fn from_system_time(stamp: SystemTime) -> NaiveDateTime {
    time::from_utc(chrono::DateTime::<chrono::Utc>::from(stamp).naive_utc())
}

fn resolve(when: TimeOrNow) -> NaiveDateTime {
    match when {
        TimeOrNow::SpecificTime(t) => from_system_time(t),
        TimeOrNow::Now => from_system_time(SystemTime::now()),
    }
}

impl Filesystem for FatxFuse {
    fn lookup(&self, _req: &Request, parent: INodeNo, leaf: &OsStr, reply: ReplyEntry) {
        let Some(leaf) = leaf.to_str() else {
            return reply.error(Errno::ENOENT);
        };
        let mut vol = self.lock();
        let Some(dir) = node(&vol, parent) else {
            return reply.error(Errno::ENOENT);
        };
        match vol.find_child(dir, leaf) {
            Ok(Some(id)) => reply.entry(&TTL, &self.attr(&vol.info(id)), Generation(0)),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let vol = self.lock();
        match node(&vol, ino) {
            Some(id) => reply.attr(&TTL, &self.attr(&vol.info(id))),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let mut vol = self.lock();
        let Some(id) = node(&vol, ino) else {
            return reply.error(Errno::ENOENT);
        };
        // FATX has no owners or permissions, so those changes are accepted and forgotten, which is
        // what most tools expect of a file system like that
        let changes_something = size.is_some() || atime.is_some() || mtime.is_some() || crtime.is_some();
        if changes_something && self.read_only {
            return reply.error(Errno::EROFS);
        }
        if let Some(size) = size {
            if vol.info(id).is_directory {
                return reply.error(Errno::EISDIR);
            }
            if let Err(err) = vol.set_length(id, size) {
                return reply.error(errno(&err));
            }
        }
        if (atime.is_some() || mtime.is_some() || crtime.is_some())
            && !vol.info(id).is_root
            && let Err(err) = vol.set_times(id, crtime.map(from_system_time), mtime.map(resolve), atime.map(resolve))
        {
            return reply.error(errno(&err));
        }
        reply.attr(&TTL, &self.attr(&vol.info(id)));
    }

    fn mkdir(&self, _req: &Request, parent: INodeNo, leaf: &OsStr, _mode: u32, _umask: u32, reply: ReplyEntry) {
        if self.read_only {
            return reply.error(Errno::EROFS);
        }
        let Some(leaf) = leaf.to_str() else {
            return reply.error(Errno::EINVAL);
        };
        let mut vol = self.lock();
        let Some(dir) = node(&vol, parent) else {
            return reply.error(Errno::ENOENT);
        };
        match vol.create_directory(dir, leaf) {
            Ok(id) => reply.entry(&TTL, &self.attr(&vol.info(id)), Generation(0)),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, leaf: &OsStr, reply: ReplyEmpty) {
        self.remove(parent, leaf, false, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, leaf: &OsStr, reply: ReplyEmpty) {
        self.remove(parent, leaf, true, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        leaf: &OsStr,
        new_parent: INodeNo,
        new_leaf: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if self.read_only {
            return reply.error(Errno::EROFS);
        }
        let (Some(leaf), Some(new_leaf)) = (leaf.to_str(), new_leaf.to_str()) else {
            return reply.error(Errno::EINVAL);
        };
        let mut vol = self.lock();
        let (Some(from_dir), Some(to_dir)) = (node(&vol, parent), node(&vol, new_parent)) else {
            return reply.error(Errno::ENOENT);
        };
        let id = match vol.find_child(from_dir, leaf) {
            Ok(Some(id)) => id,
            Ok(None) => return reply.error(Errno::ENOENT),
            Err(err) => return reply.error(errno(&err)),
        };
        if let Err(err) = name::require(new_leaf) {
            return reply.error(errno(&err));
        }
        // renaming over a file replaces it, like everywhere else, but a folder is never replaced
        let replace = match vol.find_child(to_dir, new_leaf) {
            Ok(Some(existing)) if existing != id => {
                if vol.info(existing).is_directory || vol.info(id).is_directory {
                    return reply.error(if vol.info(existing).is_directory {
                        Errno::ENOTEMPTY
                    } else {
                        Errno::EISDIR
                    });
                }
                true
            }
            Ok(_) => false,
            Err(err) => return reply.error(errno(&err)),
        };
        match vol.rename(id, to_dir, new_leaf, replace) {
            Ok(()) => reply.ok(),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let vol = self.lock();
        let Some(id) = node(&vol, ino) else {
            return reply.error(Errno::ENOENT);
        };
        if vol.info(id).is_directory {
            return reply.error(Errno::EISDIR);
        }
        if self.read_only && flags.acc_mode() != OpenAccMode::O_RDONLY {
            return reply.error(Errno::EROFS);
        }
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let mut vol = self.lock();
        let Some(id) = node(&vol, ino) else {
            return reply.error(Errno::ENOENT);
        };
        if offset >= vol.info(id).size {
            return reply.data(&[]);
        }
        let mut buffer = vec![0u8; size as usize];
        match vol.read(id, offset, &mut buffer) {
            Ok(n) => reply.data(&buffer[..n]),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        if self.read_only {
            return reply.error(Errno::EROFS);
        }
        let mut vol = self.lock();
        let Some(id) = node(&vol, ino) else {
            return reply.error(Errno::ENOENT);
        };
        match vol.write(id, offset, data) {
            Ok(()) => reply.written(data.len() as u32),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn flush(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, _lock_owner: LockOwner, reply: ReplyEmpty) {
        self.sync(Some(ino), reply);
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.sync(Some(ino), reply);
    }

    fn fsync(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, _datasync: bool, reply: ReplyEmpty) {
        self.sync(Some(ino), reply);
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let vol = self.lock();
        match node(&vol, ino) {
            Some(id) if vol.info(id).is_directory => reply.opened(FileHandle(0), FopenFlags::empty()),
            Some(_) => reply.error(Errno::ENOTDIR),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let mut vol = self.lock();
        let Some(id) = node(&vol, ino) else {
            return reply.error(Errno::ENOENT);
        };
        let children = match vol.children(id) {
            Ok(children) => children,
            Err(err) => return reply.error(errno(&err)),
        };
        let parent = vol.parent_of(id).unwrap_or(id);
        let mut entries = vec![
            (ino_of(id), FileType::Directory, ".".to_string()),
            (ino_of(parent), FileType::Directory, "..".to_string()),
        ];
        for child in children {
            let kind = if child.is_directory {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            entries.push((ino_of(child.id), kind, child.name));
        }
        // the offset a listing resumes from is the number of entries already handed out
        for (index, (entry_ino, kind, entry_name)) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(entry_ino, index as u64 + 1, kind, entry_name) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _flags: OpenFlags, reply: ReplyEmpty) {
        reply.ok();
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let vol = self.lock();
        let block = self.cluster_size as u64;
        let total = vol.total_bytes() / block;
        let free = vol.free_bytes() / block;
        reply.statfs(
            total,
            free,
            if self.read_only { 0 } else { free },
            0,
            0,
            self.cluster_size,
            name::MAX_LENGTH as u32,
            self.cluster_size,
        );
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        leaf: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        if self.read_only {
            return reply.error(Errno::EROFS);
        }
        let Some(leaf) = leaf.to_str() else {
            return reply.error(Errno::EINVAL);
        };
        let mut vol = self.lock();
        let Some(dir) = node(&vol, parent) else {
            return reply.error(Errno::ENOENT);
        };
        match vol.create_file(dir, leaf) {
            Ok(id) => reply.created(
                &TTL,
                &self.attr(&vol.info(id)),
                Generation(0),
                FileHandle(0),
                FopenFlags::empty(),
            ),
            Err(err) => reply.error(errno(&err)),
        }
    }

    fn access(&self, _req: &Request, ino: INodeNo, mask: AccessFlags, reply: ReplyEmpty) {
        let vol = self.lock();
        if node(&vol, ino).is_none() {
            return reply.error(Errno::ENOENT);
        }
        if self.read_only && mask.contains(AccessFlags::W_OK) {
            return reply.error(Errno::EROFS);
        }
        reply.ok();
    }
}

impl FatxFuse {
    fn remove(&self, parent: INodeNo, leaf: &OsStr, directory: bool, reply: ReplyEmpty) {
        if self.read_only {
            return reply.error(Errno::EROFS);
        }
        let Some(leaf) = leaf.to_str() else {
            return reply.error(Errno::ENOENT);
        };
        let mut vol = self.lock();
        let Some(dir) = node(&vol, parent) else {
            return reply.error(Errno::ENOENT);
        };
        let id = match vol.find_child(dir, leaf) {
            Ok(Some(id)) => id,
            Ok(None) => return reply.error(Errno::ENOENT),
            Err(err) => return reply.error(errno(&err)),
        };
        let is_directory = vol.info(id).is_directory;
        if directory && !is_directory {
            return reply.error(Errno::ENOTDIR);
        }
        if !directory && is_directory {
            return reply.error(Errno::EISDIR);
        }
        if is_directory && !vol.children(id).map(|c| c.is_empty()).unwrap_or(false) {
            return reply.error(Errno::ENOTEMPTY);
        }
        match vol.delete(id) {
            Ok(()) => reply.ok(),
            Err(err) => reply.error(errno(&err)),
        }
    }

    /// Writes down what was changed, for one file or all of it.
    fn sync(&self, ino: Option<INodeNo>, reply: ReplyEmpty) {
        if self.read_only {
            return reply.ok();
        }
        let mut vol = self.lock();
        let result = match ino.and_then(|i| node(&vol, i)) {
            Some(id) => vol.flush_node(id),
            None => vol.flush(),
        };
        match result {
            Ok(()) => reply.ok(),
            Err(err) => reply.error(errno(&err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatx_errors_become_the_errors_programs_expect() {
        let err = |kind| Error::fatx(kind, "x");
        assert_eq!(errno(&err(ErrorKind::NotFound)), Errno::ENOENT);
        assert_eq!(errno(&err(ErrorKind::DiskFull)), Errno::ENOSPC);
        assert_eq!(errno(&err(ErrorKind::DirectoryFull)), Errno::ENOSPC);
        assert_eq!(errno(&err(ErrorKind::DirectoryNotEmpty)), Errno::ENOTEMPTY);
        assert_eq!(errno(&err(ErrorKind::ReadOnly)), Errno::EROFS);
        assert_eq!(errno(&Error::invalid("x")), Errno::EIO);
    }

    #[test]
    fn the_root_is_inode_one_and_numbers_find_their_node_again() {
        let device: fx_core::Device = Arc::new(fx_core::device::MemoryDevice::new(64 << 20));
        fx_core::fatx::format(&device, 0, device.len(), &Default::default(), |_| {}).unwrap();
        let mut vol = FatxVolume::open(device, true).unwrap();
        let file = vol.create_file(vol.root(), "a.bin").unwrap();

        assert_eq!(ino_of(vol.root()), INodeNo(1));
        assert_eq!(node(&vol, INodeNo(1)), Some(vol.root()));
        assert_eq!(node(&vol, ino_of(file)), Some(file));
        assert_eq!(node(&vol, INodeNo(0)), None);
        assert_eq!(node(&vol, INodeNo(9999)), None);
    }

    #[test]
    fn times_survive_the_trip() {
        let when = chrono::NaiveDate::from_ymd_opt(2011, 6, 15)
            .unwrap()
            .and_hms_opt(12, 30, 0)
            .unwrap();
        assert_eq!(from_system_time(to_system_time(when)), when);
    }
}
