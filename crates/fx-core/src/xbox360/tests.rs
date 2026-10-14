use std::sync::Arc;

use super::formatter::{self, HddFormatOptions};
use super::layout::{self, SECURITY_SECTOR_OFFSET};
use super::scanner::{self, DriveKind};
use crate::device::{BlockDevice, Device, MemoryDevice};
use crate::fatx::{Flavor, Geometry};
use crate::testing::*;

const GIB: u64 = 1 << 30;
const ALL: [&str; 4] = ["sysext", "sysext2", "compat", "content"];

fn drive(length: u64) -> (Arc<MemoryDevice>, Device) {
    let device = Arc::new(MemoryDevice::new(length));
    let shared: Device = device.clone();
    (device, shared)
}

fn claim(device: &MemoryDevice, length: u64) {
    device
        .write_at(SECURITY_SECTOR_OFFSET + 0x58, &((length / 512) as u32).to_le_bytes())
        .unwrap();
}

#[test]
fn partitions_sit_end_to_end() {
    let parts = layout::partitions(500 * GIB);
    assert_eq!(parts.len(), 6);
    for pair in parts.windows(2) {
        assert_eq!(pair[0].end(), pair[1].offset);
    }
    assert_eq!(parts[0].offset, 0x80000);
    assert_eq!(parts.last().unwrap().end(), 500 * GIB);
}

#[test]
fn only_the_caches_are_not_fatx() {
    let ids: Vec<&str> = layout::partitions(100 * GIB)
        .iter()
        .filter(|p| !p.is_fatx())
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, ["syscache", "gamecache"]);
}

#[test]
fn formatting_everything_then_scanning() {
    let (_, device) = drive(8 * GIB);
    formatter::format(&device, &ALL, &HddFormatOptions::default(), |_, _| {}).unwrap();

    let scan = scanner::scan(device.as_ref()).unwrap();
    assert_eq!(scan.kind, DriveKind::Xbox360Drive);
    let mountable: Vec<&str> = scan
        .partitions
        .iter()
        .filter(|p| p.can_mount())
        .map(|p| p.id())
        .collect();
    assert_eq!(mountable, ALL);

    let content = scan.partitions.iter().find(|p| p.id() == "content").unwrap();
    assert_eq!(content.length(), 8 * GIB - layout::CONTENT_OFFSET);
    assert!(
        scan.partitions
            .iter()
            .filter(|p| p.can_mount())
            .all(|p| p.flavor == Some(Flavor::Xbox360))
    );
}

#[test]
fn partitions_dont_trample_each_other() {
    let (_, device) = drive(8 * GIB);
    formatter::format(&device, &ALL, &HddFormatOptions::default(), |_, _| {}).unwrap();
    let scan = scanner::scan(device.as_ref()).unwrap();

    // fill each one with a recognisable file
    for p in scan.partitions.iter().filter(|p| p.can_mount()) {
        let mut volume = scanner::open_partition(&device, p, true).unwrap();
        let root = volume.root();
        let file = volume.create_file(root, "marker.bin").unwrap();
        let mark = p.id().as_bytes()[p.id().len() - 1];
        volume.write(file, 0, &vec![mark; 3_000_000]).unwrap();
    }

    for p in scan.partitions.iter().filter(|p| p.can_mount()) {
        let mut volume = scanner::open_partition(&device, p, false).unwrap();
        let file = volume.resolve("marker.bin").unwrap();
        let mark = p.id().as_bytes()[p.id().len() - 1];
        assert!(read_all(&mut volume, file).iter().all(|&b| b == mark), "{}", p.id());
        assert_clean(&mut volume);
    }
}

#[test]
fn formatting_leaves_the_drive_header_alone() {
    let inner: Device = Arc::new(MemoryDevice::new(8 * GIB));
    let recorder = Arc::new(RecordingDevice::new(inner));
    let device: Device = recorder.clone();
    formatter::format(&device, &ALL, &HddFormatOptions::default(), |_, _| {}).unwrap();
    assert!(
        recorder.lowest_write() >= layout::SYSTEM_EXT_OFFSET,
        "wrote at 0x{:X}",
        recorder.lowest_write()
    );
}

#[test]
fn formatting_one_partition_leaves_the_others_alone() {
    let (_, device) = drive(8 * GIB);
    formatter::format(&device, &ALL, &HddFormatOptions::default(), |_, _| {}).unwrap();
    {
        let scan = scanner::scan(device.as_ref()).unwrap();
        let content = scan.partitions.iter().find(|p| p.id() == "content").unwrap();
        let mut volume = scanner::open_partition(&device, content, true).unwrap();
        let root = volume.root();
        let file = volume.create_file(root, "keep.me").unwrap();
        volume.write(file, 0, &[0u8; 1000]).unwrap();
    }

    formatter::format(&device, &["compat"], &HddFormatOptions::default(), |_, _| {}).unwrap();

    let scan = scanner::scan(device.as_ref()).unwrap();
    let content = scan.partitions.iter().find(|p| p.id() == "content").unwrap();
    let mut check = scanner::open_partition(&device, content, false).unwrap();
    assert!(check.find("keep.me").unwrap().is_some());
}

#[test]
fn every_partition_gets_16k_clusters() {
    let (_, device) = drive(8 * GIB);
    let plan = formatter::plan(&device, &ALL, &HddFormatOptions::default()).unwrap();
    assert!(plan.iter().all(|s| s.geometry.cluster_size == 16384));
}

#[test]
fn small_partitions_use_a_16_bit_fat_and_content_a_32_bit_one() {
    let (_, device) = drive(8 * GIB);
    let plan = formatter::plan(&device, &ALL, &HddFormatOptions::default()).unwrap();
    let entry = |id: &str| {
        plan.iter()
            .find(|s| s.partition.id == id)
            .unwrap()
            .geometry
            .fat_entry_size
    };
    assert_eq!(
        (entry("sysext"), entry("sysext2"), entry("compat"), entry("content")),
        (2, 2, 2, 4)
    );
}

#[test]
fn a_drive_thats_too_small_is_rejected() {
    let (_, device) = drive(3 * GIB);
    assert!(formatter::plan(&device, &["content"], &HddFormatOptions::default()).is_err());
    assert!(formatter::plan(&device, &["compat"], &HddFormatOptions::default()).is_err());
}

#[test]
fn caches_and_nonsense_cant_be_formatted() {
    let (_, device) = drive(8 * GIB);
    assert!(formatter::plan(&device, &["syscache"], &HddFormatOptions::default()).is_err());
    assert!(formatter::plan(&device, &["nonsense"], &HddFormatOptions::default()).is_err());
}

#[test]
fn asking_for_a_partition_twice_formats_it_once() {
    let (_, device) = drive(8 * GIB);
    let plan = formatter::plan(
        &device,
        &["compat", "Compatibility", "xbox1"],
        &HddFormatOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.len(), 1);
}

#[test]
fn content_can_be_made_smaller_than_the_drive() {
    let (_, device) = drive(8 * GIB);
    let options = HddFormatOptions {
        content_length: 2 * GIB,
        ..Default::default()
    };
    let plan = formatter::plan(&device, &["content"], &options).unwrap();
    assert_eq!(plan[0].partition.length, 2 * GIB);
}

#[test]
fn the_content_label_is_written() {
    let (_, device) = drive(8 * GIB);
    let options = HddFormatOptions {
        content_label: Some("Games".into()),
        ..Default::default()
    };
    formatter::format(&device, &["content"], &options, |_, _| {}).unwrap();
    let scan = scanner::scan(device.as_ref()).unwrap();
    let content = scan.partitions.iter().find(|p| p.id() == "content").unwrap();
    let mut volume = scanner::open_partition(&device, content, false).unwrap();
    assert_eq!(volume.label().unwrap().as_deref(), Some("Games"));
}

#[test]
fn the_compatibility_partition_can_be_made_in_the_original_style() {
    let (_, device) = drive(8 * GIB);
    let options = HddFormatOptions {
        compat_flavor: Flavor::Xbox,
        ..Default::default()
    };
    formatter::format(&device, &ALL, &options, |_, _| {}).unwrap();
    let scan = scanner::scan(device.as_ref()).unwrap();
    let flavor = |id: &str| scan.partitions.iter().find(|p| p.id() == id).unwrap().flavor;
    assert_eq!(flavor("compat"), Some(Flavor::Xbox));
    assert_eq!(flavor("content"), Some(Flavor::Xbox360));
}

#[test]
fn a_partition_dump_is_recognised() {
    let (_, device) = drive(64 << 20);
    crate::fatx::format(&device, 0, device.len(), &Default::default(), |_| {}).unwrap();
    let scan = scanner::scan(device.as_ref()).unwrap();
    assert_eq!(scan.kind, DriveKind::SinglePartition);
    assert_eq!(scan.partitions.len(), 1);
    assert!(scan.partitions[0].can_mount());
    let mut volume = scanner::open_partition(&device, &scan.partitions[0], false).unwrap();
    let root = volume.root();
    assert!(volume.children(root).unwrap().is_empty());
}

#[test]
fn an_empty_disk_is_unknown_but_still_offers_the_layout() {
    let (_, device) = drive(10 * GIB);
    let scan = scanner::scan(device.as_ref()).unwrap();
    assert_eq!(scan.kind, DriveKind::Unknown);
    assert!(scan.partitions.iter().any(|p| p.id() == "content" && !p.has_signature));
}

// Content's FAT size depends on how long the partition is, and the console works that out
// from the security sector. If the idea of the length is wrong the volume looks broken.
#[test]
fn content_opens_with_the_length_the_console_would_use() {
    let (memory, device) = drive(10 * GIB);
    claim(&memory, 8 * GIB);

    formatter::format(&device, &["content"], &HddFormatOptions::default(), |_, _| {}).unwrap();
    let scan = scanner::scan(device.as_ref()).unwrap();
    let content = scan.partitions.iter().find(|p| p.id() == "content").unwrap();
    assert_eq!(content.length(), 8 * GIB - layout::CONTENT_OFFSET);
    let volume = scanner::open_partition(&device, content, false).unwrap();
    assert_eq!(
        volume.geometry().data_offset,
        Geometry::compute(content.length(), 0x20).unwrap().data_offset
    );
}

#[test]
fn content_falls_back_to_the_device_length() {
    // formatted on a 10 GiB drive, but then a security sector shows up claiming 8 GiB
    let (memory, device) = drive(10 * GIB);
    formatter::format(&device, &["content"], &HddFormatOptions::default(), |_, _| {}).unwrap();
    claim(&memory, 8 * GIB);

    let scan = scanner::scan(device.as_ref()).unwrap();
    let content = scan.partitions.iter().find(|p| p.id() == "content").unwrap();
    let volume = scanner::open_partition(&device, content, false).unwrap();
    let expected = Geometry::compute(10 * GIB - layout::CONTENT_OFFSET, 0x20).unwrap();
    assert_eq!(volume.geometry().data_offset, expected.data_offset);
}

#[test]
fn scanning_never_reads_past_the_end_of_a_small_file() {
    let (_, device) = drive(100_000);
    let scan = scanner::scan(device.as_ref()).unwrap();
    assert_eq!(scan.kind, DriveKind::Unknown);
    assert!(scan.partitions.is_empty());
}
