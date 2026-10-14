use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sparse;

/// Anything that can be read and written by byte offset: an image file, a raw disk,
/// a slice of either. All calls take the whole range or fail, there are no short reads.
pub trait BlockDevice: Send + Sync {
    fn name(&self) -> &str;
    fn len(&self) -> u64;
    fn sector_size(&self) -> u32 {
        512
    }
    fn can_write(&self) -> bool;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()>;
    fn flush(&self) -> io::Result<()>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub type Device = Arc<dyn BlockDevice>;

pub fn read_vec(device: &dyn BlockDevice, offset: u64, count: usize) -> io::Result<Vec<u8>> {
    let mut data = vec![0u8; count];
    device.read_at(offset, &mut data)?;
    Ok(data)
}

pub(crate) fn check_range(name: &str, length: u64, offset: u64, count: usize) -> io::Result<()> {
    let end = offset.checked_add(count as u64);
    if end.is_none_or(|end| end > length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("range {offset}+{count} is outside {name} ({length} bytes)"),
        ));
    }
    Ok(())
}

// ---- memory ----

const MEMORY_BLOCK: u64 = 1 << 20;

/// Lives in RAM, but only the blocks that were written take memory, so a "400 GB drive"
/// costs nothing until it's filled. Mostly for tests and dry runs.
pub struct MemoryDevice {
    name: String,
    length: u64,
    writable: AtomicBool,
    blocks: Mutex<HashMap<u64, Box<[u8]>>>,
}

impl MemoryDevice {
    pub fn new(length: u64) -> Self {
        Self::named(length, "memory")
    }

    pub fn named(length: u64, name: &str) -> Self {
        MemoryDevice {
            name: name.to_string(),
            length,
            writable: AtomicBool::new(true),
            blocks: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_writable(&self, writable: bool) {
        self.writable.store(writable, Ordering::Relaxed);
    }

    pub fn allocated_bytes(&self) -> u64 {
        self.blocks.lock().unwrap().len() as u64 * MEMORY_BLOCK
    }
}

impl BlockDevice for MemoryDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn len(&self) -> u64 {
        self.length
    }

    fn can_write(&self) -> bool {
        self.writable.load(Ordering::Relaxed)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(&self.name, self.length, offset, buf.len())?;
        let blocks = self.blocks.lock().unwrap();
        let mut done = 0;
        while done < buf.len() {
            let pos = offset + done as u64;
            let index = pos / MEMORY_BLOCK;
            let within = (pos % MEMORY_BLOCK) as usize;
            let n = (buf.len() - done).min(MEMORY_BLOCK as usize - within);
            match blocks.get(&index) {
                Some(block) => buf[done..done + n].copy_from_slice(&block[within..within + n]),
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
        Ok(())
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.can_write() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "device is read-only"));
        }
        check_range(&self.name, self.length, offset, data.len())?;
        let mut blocks = self.blocks.lock().unwrap();
        let mut done = 0;
        while done < data.len() {
            let pos = offset + done as u64;
            let index = pos / MEMORY_BLOCK;
            let within = (pos % MEMORY_BLOCK) as usize;
            let n = (data.len() - done).min(MEMORY_BLOCK as usize - within);
            let block = blocks
                .entry(index)
                .or_insert_with(|| vec![0u8; MEMORY_BLOCK as usize].into_boxed_slice());
            block[within..within + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

// ---- image files ----

/// A raw disk image. The file never grows, writes past the end are rejected.
pub struct FileDevice {
    name: String,
    path: PathBuf,
    file: File,
    length: u64,
    writable: bool,
}

impl FileDevice {
    pub fn open(path: impl AsRef<Path>, writable: bool) -> io::Result<Self> {
        let path = path.as_ref();
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        share_mode(&mut options, writable);
        let file = options.open(path)?;
        Self::from_file(path, file, writable)
    }

    /// Makes a new image of the given size. Fails if something is already there.
    pub fn create_image(path: impl AsRef<Path>, length: u64, sparse: bool) -> io::Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
        if sparse {
            sparse::mark(&file);
        }
        file.set_len(length)?;
        Self::from_file(path, file, true)
    }

    fn from_file(path: &Path, file: File, writable: bool) -> io::Result<Self> {
        let length = file.metadata()?.len();
        let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(FileDevice {
            name: display_path(&full),
            path: full,
            file,
            length,
            writable,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Windows turns a canonical path into \\?\C:\..., which nobody wants to read.
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(text)
}

#[cfg(windows)]
fn share_mode(options: &mut OpenOptions, writable: bool) {
    use std::os::windows::fs::OpenOptionsExt;
    // others may read an image that's being written, and may do anything to one that's only read
    options.share_mode(if writable { 1 } else { 3 });
}

#[cfg(not(windows))]
fn share_mode(_options: &mut OpenOptions, _writable: bool) {}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_read(buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("unexpected end of file at {offset}"),
            ));
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        let n = file.seek_write(data, offset)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("nothing was written at {offset}"),
            ));
        }
        data = &data[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(unix)]
fn write_all_at(file: &File, data: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, data, offset)
}

impl BlockDevice for FileDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn len(&self) -> u64 {
        self.length
    }

    fn can_write(&self) -> bool {
        self.writable
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(&self.name, self.length, offset, buf.len())?;
        read_exact_at(&self.file, buf, offset)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "image was opened read-only",
            ));
        }
        check_range(&self.name, self.length, offset, data.len())?;
        write_all_at(&self.file, data, offset)
    }

    fn flush(&self) -> io::Result<()> {
        if self.writable { self.file.sync_data() } else { Ok(()) }
    }
}

// ---- slices ----

/// A window into another device, so a partition can be handled with offsets starting at zero.
pub struct SliceDevice {
    inner: Device,
    start: u64,
    length: u64,
    writable: bool,
    name: String,
}

impl SliceDevice {
    pub fn new(inner: Device, start: u64, length: u64, writable: bool) -> io::Result<Self> {
        if start.checked_add(length).is_none_or(|end| end > inner.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "slice does not fit inside the device",
            ));
        }
        let name = format!("{}@0x{:X}", inner.name(), start);
        let writable = writable && inner.can_write();
        Ok(SliceDevice {
            inner,
            start,
            length,
            writable,
            name,
        })
    }

    pub fn start(&self) -> u64 {
        self.start
    }
}

impl BlockDevice for SliceDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn len(&self) -> u64 {
        self.length
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn can_write(&self) -> bool {
        self.writable
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(&self.name, self.length, offset, buf.len())?;
        self.inner.read_at(self.start + offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "slice is read-only"));
        }
        check_range(&self.name, self.length, offset, data.len())?;
        self.inner.write_at(self.start + offset, data)
    }

    fn flush(&self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ---- whole sectors only ----

/// What a raw disk handle gives you: reads and writes that are whole sectors at whole
/// sector offsets, nothing else.
pub trait SectorIo: Send + Sync {
    fn name(&self) -> &str;
    fn len(&self) -> u64;
    fn sector_size(&self) -> u32;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn can_write(&self) -> bool;
    fn read_sectors(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_sectors(&self, offset: u64, data: &[u8]) -> io::Result<()>;
    fn flush(&self) -> io::Result<()>;
}

/// Lets the rest of the code touch any byte range on something that only does whole
/// sectors. Partial sectors at either end are done with a read, change, write.
pub struct SectorAligned<T: SectorIo> {
    io: T,
    write_gate: Mutex<()>,
}

const MAX_CHUNK: usize = 1 << 20;

impl<T: SectorIo> SectorAligned<T> {
    pub fn new(io: T) -> Self {
        SectorAligned {
            io,
            write_gate: Mutex::new(()),
        }
    }
}

impl<T: SectorIo> BlockDevice for SectorAligned<T> {
    fn name(&self) -> &str {
        self.io.name()
    }

    fn len(&self) -> u64 {
        self.io.len()
    }

    fn sector_size(&self) -> u32 {
        self.io.sector_size()
    }

    fn can_write(&self) -> bool {
        self.io.can_write()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(self.io.name(), self.io.len(), offset, buf.len())?;
        let ss = self.io.sector_size() as usize;
        let mut done = 0;

        // ragged start
        let skip = (offset % ss as u64) as usize;
        if skip != 0 || buf.len() < ss {
            let start = offset - skip as u64;
            let span = (self.io.len() - start).min(ss as u64) as usize;
            let mut sector = vec![0u8; ss];
            self.io.read_sectors(start, &mut sector[..span])?;
            let n = buf.len().min(ss - skip);
            buf[..n].copy_from_slice(&sector[skip..skip + n]);
            done = n;
        }

        // whole sectors straight into the caller's buffer
        let mut pos = offset + done as u64;
        let whole = (buf.len() - done) / ss * ss;
        let end = done + whole;
        while done < end {
            let n = (end - done).min(MAX_CHUNK / ss * ss);
            self.io.read_sectors(pos, &mut buf[done..done + n])?;
            done += n;
            pos += n as u64;
        }

        // ragged end
        let rest = buf.len() - done;
        if rest > 0 {
            let mut sector = vec![0u8; ss];
            self.io.read_sectors(pos, &mut sector)?;
            buf[done..].copy_from_slice(&sector[..rest]);
        }
        Ok(())
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.can_write() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "disk was opened read-only",
            ));
        }
        check_range(self.io.name(), self.io.len(), offset, data.len())?;
        let ss = self.io.sector_size() as usize;
        let _gate = self.write_gate.lock().unwrap();

        let mut done = 0;
        let mut pos = offset;

        let skip = (pos % ss as u64) as usize;
        if skip != 0 || data.len() < ss {
            let start = pos - skip as u64;
            let mut sector = vec![0u8; ss];
            self.io.read_sectors(start, &mut sector)?;
            let n = data.len().min(ss - skip);
            sector[skip..skip + n].copy_from_slice(&data[..n]);
            self.io.write_sectors(start, &sector)?;
            done = n;
            pos += n as u64;
        }

        let whole = (data.len() - done) / ss * ss;
        let end = done + whole;
        while done < end {
            let n = (end - done).min(MAX_CHUNK / ss * ss);
            self.io.write_sectors(pos, &data[done..done + n])?;
            done += n;
            pos += n as u64;
        }

        let rest = data.len() - done;
        if rest > 0 {
            let mut sector = vec![0u8; ss];
            self.io.read_sectors(pos, &mut sector)?;
            sector[..rest].copy_from_slice(&data[done..]);
            self.io.write_sectors(pos, &sector)?;
        }
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        self.io.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_device_reads_zeros_until_written() {
        let device = MemoryDevice::new(10 << 20);
        let mut buf = [0xAAu8; 16];
        device.read_at(5 << 20, &mut buf).unwrap();
        assert_eq!(buf, [0u8; 16]);
        assert_eq!(device.allocated_bytes(), 0);
    }

    #[test]
    fn a_write_across_a_block_edge_comes_back_whole() {
        let device = MemoryDevice::new(4 << 20);
        let data: Vec<u8> = (0..4000u32).map(|i| (i % 251) as u8).collect();
        let at = MEMORY_BLOCK - 1000;
        device.write_at(at, &data).unwrap();
        assert_eq!(read_vec(&device, at, data.len()).unwrap(), data);
        assert_eq!(device.allocated_bytes(), 2 * MEMORY_BLOCK);
    }

    #[test]
    fn reads_and_writes_past_the_end_fail() {
        let device = MemoryDevice::new(1024);
        assert!(device.read_at(1020, &mut [0u8; 8]).is_err());
        assert!(device.write_at(1020, &[0u8; 8]).is_err());
        assert!(device.read_at(u64::MAX - 2, &mut [0u8; 8]).is_err());
    }

    #[test]
    fn read_only_devices_refuse_writes() {
        let device = MemoryDevice::new(1024);
        device.set_writable(false);
        assert!(device.write_at(0, &[1]).is_err());
    }

    #[test]
    fn a_slice_starts_at_its_own_zero() {
        let inner: Device = Arc::new(MemoryDevice::new(1 << 20));
        let slice = SliceDevice::new(inner.clone(), 4096, 8192, true).unwrap();
        slice.write_at(0, b"hello").unwrap();
        assert_eq!(read_vec(inner.as_ref(), 4096, 5).unwrap(), b"hello");
        assert!(slice.write_at(8190, &[0u8; 8]).is_err());
        assert!(SliceDevice::new(inner, 1 << 20, 1, true).is_err());
    }

    #[test]
    fn a_slice_of_a_read_only_device_is_read_only() {
        let memory = MemoryDevice::new(4096);
        memory.set_writable(false);
        let inner: Device = Arc::new(memory);
        let slice = SliceDevice::new(inner, 0, 4096, true).unwrap();
        assert!(!slice.can_write());
    }

    #[test]
    fn image_files_are_created_once_and_keep_their_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        let image = FileDevice::create_image(&path, 1 << 20, true).unwrap();
        assert_eq!(image.len(), 1 << 20);
        image.write_at(1000, b"abc").unwrap();
        assert!(image.write_at((1 << 20) - 1, b"xy").is_err());
        assert!(FileDevice::create_image(&path, 10, true).is_err());
        drop(image);

        let again = FileDevice::open(&path, false).unwrap();
        assert_eq!(read_vec(&again, 1000, 3).unwrap(), b"abc");
        assert!(again.write_at(0, b"x").is_err());
    }

    /// A pretend raw disk that insists on whole sectors.
    struct Picky {
        data: Mutex<Vec<u8>>,
    }

    impl SectorIo for Picky {
        fn name(&self) -> &str {
            "picky"
        }
        fn len(&self) -> u64 {
            self.data.lock().unwrap().len() as u64
        }
        fn sector_size(&self) -> u32 {
            512
        }
        fn can_write(&self) -> bool {
            true
        }
        fn read_sectors(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            assert_eq!(offset % 512, 0, "read offset {offset}");
            let data = self.data.lock().unwrap();
            buf.copy_from_slice(&data[offset as usize..offset as usize + buf.len()]);
            Ok(())
        }
        fn write_sectors(&self, offset: u64, data: &[u8]) -> io::Result<()> {
            assert_eq!(offset % 512, 0, "write offset {offset}");
            assert_eq!(data.len() % 512, 0, "write length {}", data.len());
            self.data.lock().unwrap()[offset as usize..offset as usize + data.len()].copy_from_slice(data);
            Ok(())
        }
        fn flush(&self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn odd_ranges_become_whole_sector_calls() {
        let disk = SectorAligned::new(Picky {
            data: Mutex::new(vec![0u8; 8192]),
        });
        let pattern: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 256) as u8).collect();
        disk.write_at(333, &pattern).unwrap();
        assert_eq!(read_vec(&disk, 333, pattern.len()).unwrap(), pattern);

        // the bytes around the write weren't disturbed
        assert_eq!(read_vec(&disk, 0, 333).unwrap(), vec![0u8; 333]);
        assert_eq!(read_vec(&disk, 333 + 3000, 100).unwrap(), vec![0u8; 100]);

        // a tiny read inside one sector, and a read ending on the last byte
        let mut one = [0u8; 1];
        disk.read_at(400, &mut one).unwrap();
        assert_eq!(one[0], pattern[400 - 333]);
        disk.read_at(8191, &mut one).unwrap();
    }
}
