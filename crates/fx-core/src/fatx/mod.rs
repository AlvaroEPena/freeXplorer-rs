//! The FATX file system the Xbox and Xbox 360 use.

mod check;
mod cluster_map;
mod dirs;
mod fat;
mod files;
mod format;
pub mod geometry;
pub mod name;
mod node;
pub mod time;
mod volume;

#[cfg(test)]
mod tests;

pub use check::{FsckIssue, FsckReport, Severity};
pub use format::{FormatOptions, format};
pub use geometry::Geometry;
pub use node::{Attributes, NodeId, NodeInfo};
pub use volume::{FatxVolume, MAX_ENTRIES_PER_DIRECTORY, MAX_FILE_SIZE};

/// XTAF is the 360 flavor (big endian). The original Xbox wrote FATX, little endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flavor {
    Xbox360,
    Xbox,
}

impl Flavor {
    pub fn signature(self) -> &'static [u8; 4] {
        match self {
            Flavor::Xbox360 => b"XTAF",
            Flavor::Xbox => b"FATX",
        }
    }

    pub fn is_big_endian(self) -> bool {
        self == Flavor::Xbox360
    }

    pub fn from_signature(bytes: &[u8]) -> Option<Flavor> {
        match bytes.get(..4)? {
            b"XTAF" => Some(Flavor::Xbox360),
            b"FATX" => Some(Flavor::Xbox),
            _ => None,
        }
    }
}
