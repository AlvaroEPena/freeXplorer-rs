use std::fs::{File, OpenOptions};
use std::io;
use std::sync::Arc;

use fx_core::device::{Device, SectorAligned, SectorIo};

/// A raw disk or partition node opened as a plain file. The OS only wants whole sectors at
/// whole sector offsets, `SectorAligned` takes care of everything else.
struct RawDisk {
    file: File,
    name: String,
    length: u64,
    sector_size: u32,
    writable: bool,
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = file.seek_read(buf, offset)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
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
            return Err(io::ErrorKind::WriteZero.into());
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

impl SectorIo for RawDisk {
    fn name(&self) -> &str {
        &self.name
    }

    fn len(&self) -> u64 {
        self.length
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn can_write(&self) -> bool {
        self.writable
    }

    fn read_sectors(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        read_exact_at(&self.file, buf, offset)
            .map_err(|e| io::Error::new(e.kind(), format!("error reading {} at byte {offset}: {e}", self.name)))
    }

    fn write_sectors(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        write_all_at(&self.file, data, offset)
            .map_err(|e| io::Error::new(e.kind(), format!("error writing {} at byte {offset}: {e}", self.name)))
    }

    fn flush(&self) -> io::Result<()> {
        if self.writable { self.file.sync_all() } else { Ok(()) }
    }
}

pub(crate) fn open_raw(
    path: &str,
    name: &str,
    length: u64,
    sector_size: u32,
    writable: bool,
    configure: impl FnOnce(&mut OpenOptions),
) -> io::Result<Device> {
    if sector_size == 0 || length == 0 {
        return Err(io::Error::other(format!("{name} reports a size of zero")));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    configure(&mut options);
    let file = options.open(path).map_err(|e| describe(e, name))?;
    Ok(Arc::new(SectorAligned::new(RawDisk {
        file,
        name: path.to_string(),
        length,
        sector_size,
        writable,
    })))
}

fn describe(err: io::Error, name: &str) -> io::Error {
    match err.kind() {
        io::ErrorKind::PermissionDenied => io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the system won't let this program open {name}. Run it as an administrator (or with sudo)."),
        ),
        io::ErrorKind::NotFound => io::Error::new(io::ErrorKind::NotFound, format!("there is no {name}.")),
        _ if err.raw_os_error() == Some(32) => io::Error::other(format!("{name} is in use by another program.")),
        _ => err,
    }
}
