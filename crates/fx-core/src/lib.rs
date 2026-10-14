//! The parts of freeXplorer that don't care which operating system they run on:
//! block devices, the FATX file system, the Xbox 360 drive layout and backups.

pub mod backup;
pub mod copy;
pub mod device;
pub mod error;
pub mod fatx;
pub mod size;
pub mod sparse;
pub mod xbox360;

#[cfg(test)]
mod testing;

pub use device::{BlockDevice, Device};
pub use error::{Error, ErrorKind, Result};
