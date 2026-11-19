//! The 360's layout is compiled in, so detecting one is a question of whether anything FATX
//! sits where the console would have put it. Scanning any drive at all starts in
//! [`crate::scan`], which rules out an original Xbox drive before getting here.

use super::layout;
use crate::device::BlockDevice;
use crate::error::Result;

pub use crate::scan::{
    DetectedPartition, DriveKind, DriveScan, SINGLE_PARTITION_ID, open_partition, scan, signature_at,
};

pub fn scan_drive(device: &dyn BlockDevice) -> Result<DriveScan> {
    let visible = layout::visible_length(device)?;
    let security = layout::looks_like_security_sector(device)?;
    let mut found = Vec::new();
    let mut any_fatx = false;

    for spec in layout::partitions(visible) {
        if spec.length == 0 || spec.end() > device.len() {
            continue;
        }
        if !spec.is_fatx() {
            found.push(DetectedPartition {
                spec,
                has_signature: false,
                flavor: None,
            });
            continue;
        }
        let flavor = signature_at(device, spec.offset)?;
        any_fatx |= flavor.is_some();
        found.push(DetectedPartition {
            spec,
            has_signature: flavor.is_some(),
            flavor,
        });
    }

    Ok(DriveScan {
        kind: if any_fatx {
            DriveKind::Xbox360Drive
        } else {
            DriveKind::Unknown
        },
        visible_length: visible,
        has_security_sector: security,
        partitions: found,
    })
}
