//! How an Xbox 360 hard drive is laid out, and the things that follow from it.

pub mod formatter;
pub mod layout;
pub mod scanner;
pub mod security;

#[cfg(test)]
mod tests;

pub use formatter::{FormatStep, HddFormatOptions};
pub use layout::{PartitionFileSystem, PartitionSpec};
pub use scanner::{DetectedPartition, DriveKind, DriveScan};
pub use security::SecuritySector;
