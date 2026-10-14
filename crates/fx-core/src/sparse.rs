//! Sparse files: a stretch of zeros that isn't stored. Backups of mostly empty drives
//! depend on this to stay small.

use std::fs::File;
use std::io;
use std::path::Path;

/// Tells the file system not to allocate the zeros of a big empty image. Quietly does
/// nothing where that isn't needed or isn't possible, the image just takes real space there.
#[cfg(windows)]
pub fn mark(file: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;

    let mut returned = 0u32;
    // SAFETY: the handle belongs to `file` and stays open for the whole call
    unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        ) != 0
    }
}

/// Unix file systems make holes by themselves when a file is stretched with set_len.
#[cfg(not(windows))]
pub fn mark(_file: &File) -> bool {
    true
}

/// What the file really takes on disk, which for a sparse file is far less than its length.
#[cfg(windows)]
pub fn size_on_disk(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, NO_ERROR};
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0u32;
    // SAFETY: `wide` is a nul terminated string that lives through the call
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    if low == u32::MAX && unsafe { GetLastError() } != NO_ERROR {
        return std::fs::metadata(path).map(|m| m.len());
    }
    Ok(((high as u64) << 32) | low as u64)
}

#[cfg(unix)]
pub fn size_on_disk(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.blocks() * 512)
}
