use std::sync::Arc;

use super::*;
use crate::device::{BlockDevice, Device, MemoryDevice, read_vec};
use crate::error::{Error, ErrorKind};
use crate::testing::*;

fn kind<T>(result: crate::Result<T>) -> ErrorKind {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(err) => err.kind().expect("a file system error"),
    }
}

fn open(device: &Arc<MemoryDevice>) -> FatxVolume {
    let shared: Device = device.clone();
    FatxVolume::open(shared, true).unwrap()
}

// ---- formatting ----

#[test]
fn the_header_is_what_the_console_expects() {
    let device: Device = Arc::new(MemoryDevice::new(64 << 20));
    let options = FormatOptions {
        volume_id: Some(0x1122_3344),
        ..Default::default()
    };
    format(&device, 0, device.len(), &options, |_| {}).unwrap();

    let head = read_vec(device.as_ref(), 0, 16).unwrap();
    assert_eq!(&head[..4], b"XTAF");
    assert_eq!(head[4..8], [0x11, 0x22, 0x33, 0x44]);
    assert_eq!(head[8..12], [0, 0, 0, 0x20]); // sectors per cluster, big endian
    assert_eq!(head[12..16], [0, 0, 0, 1]); // root directory cluster

    // the rest of the page looks like a console-formatted drive: two zero bytes, then 0xFF
    let page = read_vec(device.as_ref(), 0, 0x1000).unwrap();
    assert_eq!(page[0x10..0x12], [0, 0]);
    assert!(page[0x12..].iter().all(|&b| b == 0xFF));
}

#[test]
fn a_32_bit_fat_starts_with_the_media_entry_and_a_closed_root() {
    let device: Device = Arc::new(MemoryDevice::new(2 << 30));
    format(&device, 0, device.len(), &FormatOptions::default(), |_| {}).unwrap();
    let fat = read_vec(device.as_ref(), 0x1000, 12).unwrap();
    assert_eq!(fat[0..4], [0xFF, 0xFF, 0xFF, 0xF8]);
    assert_eq!(fat[4..8], [0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(fat[8..12], [0, 0, 0, 0]);
}

#[test]
fn a_16_bit_fat_uses_16_bit_entries() {
    let device: Device = Arc::new(MemoryDevice::new(32 << 20));
    format(&device, 0, device.len(), &FormatOptions::default(), |_| {}).unwrap();
    assert_eq!(
        read_vec(device.as_ref(), 0x1000, 6).unwrap(),
        [0xFF, 0xF8, 0xFF, 0xFF, 0, 0]
    );
}

#[test]
fn the_root_directory_cluster_starts_empty() {
    let device: Device = Arc::new(MemoryDevice::new(64 << 20));
    let geometry = format(&device, 0, device.len(), &FormatOptions::default(), |_| {}).unwrap();
    let root = read_vec(
        device.as_ref(),
        geometry.cluster_offset(1),
        geometry.cluster_size as usize,
    )
    .unwrap();
    assert!(root.iter().all(|&b| b == 0xFF));
}

#[test]
fn formatting_stays_inside_the_requested_range() {
    let inner: Device = Arc::new(MemoryDevice::new(256 << 20));
    let recorder = Arc::new(RecordingDevice::new(inner));
    let device: Device = recorder.clone();
    format(&device, 64 << 20, 128 << 20, &FormatOptions::default(), |_| {}).unwrap();
    assert!(recorder.lowest_write() >= 64 << 20);
    assert!(recorder.highest_write_end() <= 192 << 20);
}

#[test]
fn a_volume_can_be_formatted_over_an_old_one() {
    let (device, mut volume) = create_default();
    let file = volume.create_file(volume.root(), "old.bin").unwrap();
    volume.write(file, 0, &random_bytes(100_000, 1)).unwrap();
    drop(volume);

    let shared: Device = device.clone();
    format(&shared, 0, shared.len(), &FormatOptions::default(), |_| {}).unwrap();
    let mut fresh = open(&device);
    assert!(fresh.children(fresh.root()).unwrap().is_empty());
}

#[test]
fn the_label_is_stored_in_name_txt() {
    let device = Arc::new(MemoryDevice::new(64 << 20));
    let shared: Device = device.clone();
    format(
        &shared,
        0,
        shared.len(),
        &FormatOptions {
            label: Some("My Drive".into()),
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    let mut volume = open(&device);
    assert_eq!(volume.label().unwrap().as_deref(), Some("My Drive"));
    let node = volume.find("name.txt").unwrap().unwrap();
    assert_eq!(read_all(&mut volume, node)[..4], [0xFE, 0xFF, 0, b'M']);
}

#[test]
fn a_label_can_be_changed_and_removed() {
    let (_, mut volume) = create_default();
    assert_eq!(volume.label().unwrap(), None);
    volume.set_label("First").unwrap();
    volume.set_label("Second").unwrap();
    assert_eq!(volume.label().unwrap().as_deref(), Some("Second"));
    assert!(volume.set_label("this label is too long").is_err());
    volume.set_label("").unwrap();
    assert_eq!(volume.label().unwrap(), None);
    assert_clean(&mut volume);
}

#[test]
fn a_read_only_device_is_refused() {
    let memory = MemoryDevice::new(64 << 20);
    memory.set_writable(false);
    let device: Device = Arc::new(memory);
    assert_eq!(
        kind(format(&device, 0, device.len(), &FormatOptions::default(), |_| {})),
        ErrorKind::ReadOnly
    );
}

#[test]
fn the_little_endian_flavor_writes_fatx() {
    let device = Arc::new(MemoryDevice::new(64 << 20));
    let shared: Device = device.clone();
    let options = FormatOptions {
        flavor: Flavor::Xbox,
        volume_id: Some(0x0102_0304),
        ..Default::default()
    };
    format(&shared, 0, shared.len(), &options, |_| {}).unwrap();
    let head = read_vec(device.as_ref(), 0, 16).unwrap();
    assert_eq!(&head[..4], b"FATX");
    assert_eq!(head[4..8], [4, 3, 2, 1]);
    assert_eq!(open(&device).flavor(), Flavor::Xbox);
}

#[test]
fn something_that_isnt_fatx_is_refused() {
    let device: Device = Arc::new(MemoryDevice::new(64 << 20));
    assert_eq!(kind(FatxVolume::open(device, false)), ErrorKind::NotFatx);
}

// ---- reading and writing ----

#[test]
fn a_fresh_volume_is_empty_and_clean() {
    let (_, mut volume) = create_default();
    assert!(volume.children(volume.root()).unwrap().is_empty());
    assert_eq!(volume.free_bytes(), volume.total_bytes() - volume.cluster_size() as u64);
    assert_clean(&mut volume);
}

#[test]
fn new_files_have_no_attribute_bits_like_the_consoles_own() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "plain.bin").unwrap();
    let dir = volume.create_directory(root, "folder").unwrap();
    assert_eq!(volume.info(file).attributes, Attributes::NONE);
    assert_eq!(volume.info(dir).attributes, Attributes::DIRECTORY);
}

#[test]
fn what_was_written_reads_back() {
    let (_, mut volume) = create_default();
    let dir = volume.create_directories("Content\\0000000000000000").unwrap();
    let file = volume.create_file(dir, "data.bin").unwrap();
    let data = random_bytes(100_000, 7);
    volume.write(file, 0, &data).unwrap();

    assert_eq!(volume.info(file).size, 100_000);
    assert_eq!(read_all(&mut volume, file), data);
    assert_clean(&mut volume);
}

#[test]
fn a_volume_survives_a_reopen() {
    let (device, mut volume) = create_default();
    let root = volume.root();
    let games = volume.create_directory(root, "Games").unwrap();
    let file = volume.create_file(games, "a.bin").unwrap();
    let data = random_bytes(333_333, 9);
    volume.write(file, 0, &data).unwrap();

    let mut volume = reopen(&device, volume);
    let again = volume.resolve("Games\\a.bin").unwrap();
    assert_eq!(read_all(&mut volume, again), data);
    assert_clean(&mut volume);
}

#[test]
fn partial_reads_at_odd_offsets() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "x").unwrap();
    let data = random_bytes(70_000, 3);
    volume.write(file, 0, &data).unwrap();

    let mut buf = vec![0u8; 5000];
    assert_eq!(volume.read(file, 16_380, &mut buf).unwrap(), 5000);
    assert_eq!(buf, data[16_380..21_380]);
    assert_eq!(volume.read(file, 69_000, &mut buf).unwrap(), 1000);
    assert_eq!(volume.read(file, 70_000, &mut buf).unwrap(), 0);
    assert_eq!(volume.read(file, 99_999, &mut buf).unwrap(), 0);
}

#[test]
fn overwriting_in_the_middle_keeps_the_rest() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "x").unwrap();
    let mut data = random_bytes(60_000, 4);
    volume.write(file, 0, &data).unwrap();

    let patch = random_bytes(9_000, 5);
    volume.write(file, 20_000, &patch).unwrap();
    data[20_000..29_000].copy_from_slice(&patch);

    assert_eq!(volume.info(file).size, 60_000);
    assert_eq!(read_all(&mut volume, file), data);
}

#[test]
fn appending_in_small_pieces() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "log.txt").unwrap();
    let mut all = Vec::new();
    for i in 0..300u64 {
        let piece = random_bytes(1 + (i as usize * 7) % 900, i);
        let end = volume.info(file).size;
        volume.write(file, end, &piece).unwrap();
        all.extend_from_slice(&piece);
    }
    assert_eq!(read_all(&mut volume, file), all);
    assert_clean(&mut volume);
}

#[test]
fn writing_past_the_end_zero_fills_the_gap() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "sparse").unwrap();
    volume.write(file, 50_000, &[1, 2, 3]).unwrap();

    let all = read_all(&mut volume, file);
    assert_eq!(all.len(), 50_003);
    assert!(all[..50_000].iter().all(|&b| b == 0));
    assert_eq!(all[50_000..], [1, 2, 3]);
}

#[test]
fn growing_a_file_never_exposes_old_data() {
    let (_, mut volume) = create(8 << 20, 0x20, Flavor::Xbox360);
    let root = volume.root();
    let junk = volume.create_file(root, "junk").unwrap();
    volume.write(junk, 0, &vec![0xAB; 1_000_000]).unwrap();
    volume.delete(junk).unwrap();

    let fresh = volume.create_file(root, "fresh").unwrap();
    volume.set_length(fresh, 600_000).unwrap();
    assert!(read_all(&mut volume, fresh).iter().all(|&b| b == 0));

    // and the same through a write that skips ahead
    let other = volume.create_file(root, "other").unwrap();
    volume.write(other, 300_000, &[9]).unwrap();
    assert!(read_all(&mut volume, other)[..300_000].iter().all(|&b| b == 0));
}

#[test]
fn truncating_down_frees_clusters_and_up_gives_zeros() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "t").unwrap();
    volume.write(file, 0, &random_bytes(200_000, 1)).unwrap();
    let used_before = volume.total_bytes() - volume.free_bytes();

    volume.set_length(file, 20_000).unwrap();
    assert_eq!(volume.info(file).size, 20_000);
    assert!(volume.total_bytes() - volume.free_bytes() < used_before);
    assert_clean(&mut volume);

    volume.set_length(file, 40_000).unwrap();
    assert!(read_all(&mut volume, file)[20_000..].iter().all(|&b| b == 0));

    volume.set_length(file, 0).unwrap();
    assert_eq!(volume.info(file).first_cluster, 0);
    assert_eq!(volume.free_bytes(), volume.total_bytes() - volume.cluster_size() as u64);
    assert_clean(&mut volume);
}

#[test]
fn deleting_gives_the_space_back() {
    let (_, mut volume) = create_default();
    let free = volume.free_bytes();
    let root = volume.root();
    let file = volume.create_file(root, "big").unwrap();
    volume.write(file, 0, &vec![0u8; 3_000_000]).unwrap();
    assert!(volume.free_bytes() < free - 2_900_000);

    volume.delete(file).unwrap();
    assert_eq!(volume.free_bytes(), free);
    assert_eq!(volume.find("big").unwrap(), None);
    assert!(volume.info(file).is_deleted);
    assert_eq!(kind(volume.read(file, 0, &mut [0u8; 4])), ErrorKind::NotFound);
    assert_clean(&mut volume);
}

#[test]
fn a_directory_has_to_be_empty_to_be_deleted() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let dir = volume.create_directory(root, "d").unwrap();
    let file = volume.create_file(dir, "f").unwrap();

    assert_eq!(kind(volume.delete(dir)), ErrorKind::DirectoryNotEmpty);
    volume.delete(file).unwrap();
    volume.delete(dir).unwrap();
    assert!(volume.children(root).unwrap().is_empty());
    assert_clean(&mut volume);
}

#[test]
fn names_ignore_case_but_keep_it() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    volume.create_file(root, "Hello.TXT").unwrap();
    assert!(volume.find("hello.txt").unwrap().is_some());
    let found = volume.find("HELLO.txt").unwrap().unwrap();
    assert_eq!(volume.info(found).name, "Hello.TXT");
    assert_eq!(kind(volume.create_file(root, "HELLO.TXT")), ErrorKind::AlreadyExists);
}

#[test]
fn bad_names_are_rejected() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    for bad in ["a?b", "", &"x".repeat(43), "tab\there", "\u{fc}"] {
        assert_eq!(kind(volume.create_file(root, bad)), ErrorKind::InvalidName, "{bad:?}");
    }
}

#[test]
fn long_paths_are_refused() {
    let (_, mut volume) = create_default();
    let mut dir = volume.root();
    let mut failure = None;
    for _ in 0..20 {
        match volume.create_directory(dir, &"d".repeat(30)) {
            Ok(next) => dir = next,
            Err(err) => {
                failure = Some(err);
                break;
            }
        }
    }
    assert_eq!(failure.unwrap().kind(), Some(ErrorKind::PathTooLong));
}

#[test]
fn directories_grow_past_one_cluster() {
    let (device, mut volume) = create_default();
    let root = volume.root();
    let dir = volume.create_directory(root, "many").unwrap();
    for i in 0..700 {
        volume.create_file(dir, &format!("file{i:04}.dat")).unwrap();
    }
    assert_eq!(volume.children(dir).unwrap().len(), 700);
    assert_clean(&mut volume);

    let mut volume = reopen(&device, volume);
    let dir = volume.resolve("many").unwrap();
    assert_eq!(volume.children(dir).unwrap().len(), 700);
    assert!(volume.find("many\\file0699.dat").unwrap().is_some());
    assert_clean(&mut volume);
}

#[test]
fn deleted_slots_get_reused() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let dir = volume.create_directory(root, "d").unwrap();
    let nodes: Vec<NodeId> = (0..50)
        .map(|i| volume.create_file(dir, &format!("f{i}")).unwrap())
        .collect();
    let clusters = volume.nodes[dir.0].map.as_ref().unwrap().count();

    for node in nodes.iter().step_by(2) {
        volume.delete(*node).unwrap();
    }
    for i in 0..25 {
        volume.create_file(dir, &format!("new{i}")).unwrap();
    }

    assert_eq!(volume.children(dir).unwrap().len(), 50);
    assert_eq!(volume.nodes[dir.0].map.as_ref().unwrap().count(), clusters);
    assert_clean(&mut volume);
}

#[test]
fn a_directory_stops_at_4096_entries() {
    let (_, mut volume) = create(64 << 20, 8, Flavor::Xbox360);
    let root = volume.root();
    let dir = volume.create_directory(root, "full").unwrap();
    for i in 0..4096 {
        volume.create_file(dir, &i.to_string()).unwrap();
    }
    assert_eq!(kind(volume.create_file(dir, "one-too-many")), ErrorKind::DirectoryFull);

    let gone = volume.find("full\\17").unwrap().unwrap();
    volume.delete(gone).unwrap();
    volume.create_file(dir, "fits-now").unwrap();
    assert_clean(&mut volume);
}

// ---- renaming ----

#[test]
fn renaming_in_place() {
    let (device, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "before.bin").unwrap();
    volume.write(file, 0, &random_bytes(5000, 1)).unwrap();

    volume.rename(file, root, "after.bin", false).unwrap();
    assert_eq!(volume.find("before.bin").unwrap(), None);
    assert_eq!(volume.find("after.bin").unwrap(), Some(file));

    let mut volume = reopen(&device, volume);
    let again = volume.resolve("after.bin").unwrap();
    assert_eq!(volume.info(again).size, 5000);
    assert_clean(&mut volume);
}

#[test]
fn a_rename_can_change_only_the_case() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "readme.txt").unwrap();
    volume.rename(file, root, "README.TXT", false).unwrap();
    let found = volume.find("readme.txt").unwrap().unwrap();
    assert_eq!(volume.info(found).name, "README.TXT");
}

#[test]
fn moving_a_directory_takes_its_children_along() {
    let (device, mut volume) = create_default();
    let root = volume.root();
    let a = volume.create_directory(root, "a").unwrap();
    let b = volume.create_directory(root, "b").unwrap();
    let inner = volume.create_directory(a, "inner").unwrap();
    let file = volume.create_file(inner, "deep.bin").unwrap();
    let data = random_bytes(40_000, 11);
    volume.write(file, 0, &data).unwrap();

    volume.rename(a, b, "moved", false).unwrap();
    assert_eq!(volume.find("a").unwrap(), None);
    assert_eq!(volume.info(file).path, "\\b\\moved\\inner\\deep.bin");

    let mut volume = reopen(&device, volume);
    let moved = volume.resolve("b\\moved\\inner\\deep.bin").unwrap();
    assert_eq!(read_all(&mut volume, moved), data);
    assert_clean(&mut volume);
}

#[test]
fn a_directory_cant_move_into_itself() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let a = volume.create_directory(root, "a").unwrap();
    let b = volume.create_directory(a, "b").unwrap();
    assert!(volume.rename(a, b, "a", false).is_err());
    assert!(volume.rename(a, a, "a2", false).is_err());
}

#[test]
fn renaming_over_an_existing_file_only_when_asked() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let one = volume.create_file(root, "one").unwrap();
    let two = volume.create_file(root, "two").unwrap();
    volume.write(one, 0, &[1]).unwrap();
    volume.write(two, 0, &[2]).unwrap();

    assert_eq!(kind(volume.rename(one, root, "two", false)), ErrorKind::AlreadyExists);
    volume.rename(one, root, "two", true).unwrap();
    let now = volume.find("two").unwrap().unwrap();
    assert_eq!(read_all(&mut volume, now), [1]);
    assert!(volume.info(two).is_deleted);
    assert_clean(&mut volume);
}

// ---- trouble ----

#[test]
fn a_full_disk_leaves_everything_consistent() {
    let (_, mut volume) = create(6 << 20, 0x20, Flavor::Xbox360);
    let root = volume.root();
    let mut kept = Vec::new();
    let mut full = None;
    for i in 0..100u64 {
        let file = volume.create_file(root, &format!("f{i}")).unwrap();
        let data = random_bytes(500_000 + i as usize, i);
        match volume.write(file, 0, &data) {
            Ok(()) => kept.push((file, data)),
            Err(err) => {
                full = Some(err);
                break;
            }
        }
    }
    assert_eq!(full.unwrap().kind(), Some(ErrorKind::DiskFull));
    for (file, data) in &kept {
        assert_eq!(&read_all(&mut volume, *file), data);
    }
    assert_clean(&mut volume);

    // and space freed up is usable again
    volume.delete(kept[0].0).unwrap();
    let again = volume.create_file(root, "again").unwrap();
    volume.write(again, 0, &random_bytes(400_000, 1)).unwrap();
    assert_clean(&mut volume);
}

#[test]
fn a_failed_extend_changes_nothing() {
    let (_, mut volume) = create(4 << 20, 0x20, Flavor::Xbox360);
    let root = volume.root();
    let file = volume.create_file(root, "f").unwrap();
    volume.write(file, 0, &[0u8; 1000]).unwrap();
    let free = volume.free_bytes();

    assert!(volume.set_length(file, 3_900_000_000).is_err());
    assert!(volume.write(file, 0, &vec![0u8; 8_000_000]).is_err());
    assert_eq!(volume.info(file).size, 1000);
    assert_eq!(volume.free_bytes(), free);
    assert_clean(&mut volume);
}

#[test]
fn four_gigabytes_is_the_ceiling() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "huge").unwrap();
    assert_eq!(kind(volume.set_length(file, 0x1_0000_0000)), ErrorKind::FileTooLarge);
    assert_eq!(kind(volume.write(file, 0xFFFF_FFFF, &[0, 0])), ErrorKind::FileTooLarge);
}

#[test]
fn a_fragmented_file_reads_back_right() {
    let (device, mut volume) = create(32 << 20, 8, Flavor::Xbox360);
    let root = volume.root();
    let fillers: Vec<NodeId> = (0..200)
        .map(|i| {
            let f = volume.create_file(root, &format!("filler{i}")).unwrap();
            volume.write(f, 0, &[0u8; 4096]).unwrap();
            f
        })
        .collect();
    for f in fillers.iter().step_by(2) {
        volume.delete(*f).unwrap();
    }

    let big = volume.create_file(root, "big").unwrap();
    let data = random_bytes(600_000, 21);
    volume.write(big, 0, &data).unwrap();
    assert!(
        volume.nodes[big.0].map.as_ref().unwrap().extents().len() > 20,
        "the file needs to be spread out"
    );

    let mut volume = reopen(&device, volume);
    let big = volume.resolve("big").unwrap();
    assert_eq!(read_all(&mut volume, big), data);
    assert_clean(&mut volume);
}

#[test]
fn a_fresh_file_is_one_contiguous_run() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "seq").unwrap();
    for _ in 0..40 {
        let end = volume.info(file).size;
        volume.write(file, end, &[0u8; 50_000]).unwrap();
    }
    assert_eq!(volume.nodes[file.0].map.as_ref().unwrap().extents().len(), 1);
}

#[test]
fn read_only_volumes_refuse_changes() {
    let (device, mut volume) = create_default();
    let root = volume.root();
    volume.create_file(root, "a").unwrap();
    drop(volume);

    let shared: Device = device.clone();
    let mut ro = FatxVolume::open(shared, false).unwrap();
    assert!(ro.is_read_only());
    let a = ro.resolve("a").unwrap();
    let root = ro.root();
    assert_eq!(kind(ro.create_file(root, "b")), ErrorKind::ReadOnly);
    assert_eq!(kind(ro.write(a, 0, &[1])), ErrorKind::ReadOnly);
    assert_eq!(kind(ro.delete(a)), ErrorKind::ReadOnly);
    assert_eq!(kind(ro.rename(a, root, "c", false)), ErrorKind::ReadOnly);
}

#[test]
fn timestamps_are_set_and_kept() {
    use chrono::{Duration, Local, NaiveDate};

    let (device, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "t").unwrap();
    let age = Local::now().naive_local() - volume.info(file).created;
    assert!(age < Duration::minutes(2) && age > Duration::minutes(-2), "{age}");

    let stamp = NaiveDate::from_ymd_opt(2012, 3, 4)
        .unwrap()
        .and_hms_opt(5, 6, 8)
        .unwrap();
    volume
        .set_times(
            file,
            Some(stamp),
            Some(stamp + Duration::hours(1)),
            Some(stamp + Duration::hours(2)),
        )
        .unwrap();

    let mut volume = reopen(&device, volume);
    let back = volume.resolve("t").unwrap();
    let info = volume.info(back);
    assert_eq!(info.created, stamp);
    assert_eq!(info.modified, stamp + Duration::hours(1));
    assert_eq!(info.accessed, stamp + Duration::hours(2));
}

#[test]
fn attributes_keep_the_directory_bit() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let dir = volume.create_directory(root, "d").unwrap();
    volume.set_attributes(dir, Attributes::HIDDEN).unwrap();
    let info = volume.info(dir);
    assert!(info.is_directory);
    assert!(info.attributes.contains(Attributes::HIDDEN));
}

#[test]
fn a_32_bit_fat_with_small_clusters_works() {
    let (device, mut volume) = create(2 << 30, 8, Flavor::Xbox360);
    assert_eq!(volume.geometry().fat_entry_size, 4);
    let root = volume.root();
    let x = volume.create_directory(root, "x").unwrap();
    let file = volume.create_file(x, "y").unwrap();
    let data = random_bytes(2_500_000, 5);
    volume.write(file, 0, &data).unwrap();

    let mut volume = reopen(&device, volume);
    let file = volume.resolve("x\\y").unwrap();
    assert_eq!(read_all(&mut volume, file), data);
    assert_clean(&mut volume);
}

#[test]
fn a_16_bit_fat_works() {
    let (device, mut volume) = create(40 << 20, 0x20, Flavor::Xbox360);
    assert_eq!(volume.geometry().fat_entry_size, 2);
    let root = volume.root();
    let file = volume.create_file(root, "y").unwrap();
    let data = random_bytes(900_000, 6);
    volume.write(file, 0, &data).unwrap();

    let mut volume = reopen(&device, volume);
    let file = volume.resolve("y").unwrap();
    assert_eq!(read_all(&mut volume, file), data);
    assert_clean(&mut volume);
}

#[test]
fn little_endian_volumes_work_too() {
    use chrono::NaiveDate;

    let (device, mut volume) = create(64 << 20, 0x20, Flavor::Xbox);
    let root = volume.root();
    let dir = volume.create_directory(root, "Dir").unwrap();
    let file = volume.create_file(dir, "file.bin").unwrap();
    let data = random_bytes(123_456, 2);
    volume.write(file, 0, &data).unwrap();
    let when = NaiveDate::from_ymd_opt(2004, 7, 8)
        .unwrap()
        .and_hms_opt(9, 10, 12)
        .unwrap();
    volume.set_times(file, Some(when), Some(when), Some(when)).unwrap();

    let mut volume = reopen(&device, volume);
    let back = volume.resolve("Dir\\file.bin").unwrap();
    assert_eq!(volume.flavor(), Flavor::Xbox);
    assert_eq!(read_all(&mut volume, back), data);
    assert_eq!(volume.info(back).modified, when);
    assert_clean(&mut volume);
}

#[test]
fn entries_not_flushed_yet_are_written_by_flush() {
    let (device, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "f").unwrap();
    volume.write(file, 0, &[0u8; 100]).unwrap();
    volume.write(file, 10, &[0u8; 5]).unwrap(); // overwrite only, so the entry stays dirty
    let modified = volume.info(file).modified;
    volume.flush().unwrap();

    let shared: Device = device.clone();
    let mut other = FatxVolume::open(shared, false).unwrap();
    let again = other.resolve("f").unwrap();
    assert_eq!(other.info(again).modified, modified);
}

#[test]
fn reading_a_directory_as_a_file_or_the_other_way_round_is_refused() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let dir = volume.create_directory(root, "d").unwrap();
    let file = volume.create_file(root, "f").unwrap();
    assert_eq!(kind(volume.read(dir, 0, &mut [0u8; 4])), ErrorKind::IsADirectory);
    assert_eq!(kind(volume.children(file)), ErrorKind::NotADirectory);
    assert_eq!(kind(volume.create_file(file, "x")), ErrorKind::NotADirectory);
}

#[test]
fn a_cut_down_cache_changes_nothing_but_speed() {
    let (device, mut volume) = create(64 << 20, 8, Flavor::Xbox360);
    volume.fat = crate::fatx::fat::FatTable::with_cache(device.clone(), &volume.geometry.clone(), true, 4);
    let root = volume.root();
    let file = volume.create_file(root, "f").unwrap();
    let data = random_bytes(3_000_000, 3);
    volume.write(file, 0, &data).unwrap();
    assert_eq!(read_all(&mut volume, file), data);
    assert_clean(&mut volume);
}

// ---- the check ----

#[test]
fn a_leaked_cluster_is_found_and_released() {
    let (device, mut volume) = create_default();
    let root = volume.root();
    let file = volume.create_file(root, "f").unwrap();
    volume.write(file, 0, &[1u8; 100]).unwrap();
    volume.flush().unwrap();

    // mark a cluster used without any file owning it
    volume.set_fat(100, volume.geometry.end_of_chain()).unwrap();
    volume.flush().unwrap();

    let mut volume = reopen(&device, volume);
    let report = volume.check(false, |_| {}).unwrap();
    assert_eq!(report.orphan_clusters, 1);
    assert_eq!(report.errors(), 0);
    assert!(report.warnings() > 0);

    let fixed = volume.check(true, |_| {}).unwrap();
    assert_eq!(fixed.fixed_clusters, 1);
    assert_clean(&mut volume);
}

#[test]
fn a_cut_chain_is_an_error() {
    let (device, mut volume) = create(8 << 20, 8, Flavor::Xbox360);
    let root = volume.root();
    let file = volume.create_file(root, "f").unwrap();
    volume.write(file, 0, &random_bytes(100_000, 1)).unwrap();
    volume.flush().unwrap();

    // free the second cluster of the file
    let second = volume.nodes[file.0].map.as_ref().unwrap().cluster_at(1).unwrap();
    volume.set_fat(second, 0).unwrap();
    volume.flush().unwrap();

    let mut volume = reopen(&device, volume);
    let report = volume.check(false, |_| {}).unwrap();
    assert!(report.errors() > 0);
    assert!(report.issues.iter().any(|i| i.message.contains("\\f")));
}

#[test]
fn files_sharing_a_cluster_are_an_error() {
    let (device, mut volume) = create(8 << 20, 8, Flavor::Xbox360);
    let root = volume.root();
    let a = volume.create_file(root, "a").unwrap();
    let b = volume.create_file(root, "b").unwrap();
    volume.write(a, 0, &[1u8; 100]).unwrap();
    volume.write(b, 0, &[2u8; 100]).unwrap();
    let shared = volume.nodes[a.0].first_cluster;
    volume.nodes[b.0].first_cluster = shared;
    volume.write_entry(b).unwrap();
    volume.flush().unwrap();

    let mut volume = reopen(&device, volume);
    let report = volume.check(false, |_| {}).unwrap();
    assert!(
        report.issues.iter().any(|i| i.message.contains("already used")),
        "{:?}",
        report.issues
    );
}

#[test]
fn the_report_counts_what_is_there() {
    let (_, mut volume) = create_default();
    let root = volume.root();
    let dir = volume.create_directory(root, "d").unwrap();
    for i in 0..3 {
        let f = volume.create_file(dir, &format!("f{i}")).unwrap();
        volume.write(f, 0, &vec![0u8; 20_000]).unwrap();
    }
    let report = volume.check(false, |_| {}).unwrap();
    assert_eq!((report.directories, report.files, report.file_bytes), (2, 3, 60_000));
    assert!(report.is_clean());
}

// ---- random operations against a plain model, and power cuts ----

mod model {
    use std::collections::{HashMap, HashSet};

    use super::*;

    pub struct Model {
        pub files: HashMap<String, Vec<u8>>,
        pub dirs: HashSet<String>,
    }

    impl Model {
        pub fn new() -> Model {
            Model {
                files: HashMap::new(),
                dirs: HashSet::from([String::new()]),
            }
        }
    }

    const NAMES: [&str; 12] = [
        "a",
        "b",
        "c",
        "Content",
        "save.dat",
        "Game 1",
        "x.bin",
        "y.bin",
        "0000000000000000",
        "Headers",
        "TITLE",
        "t",
    ];

    fn join(dir: &str, name: &str) -> String {
        if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}\\{name}")
        }
    }

    fn parent_of(path: &str) -> String {
        path.rfind('\\').map_or(String::new(), |i| path[..i].to_string())
    }

    fn name_of(path: &str) -> String {
        path.rfind('\\').map_or(path.to_string(), |i| path[i + 1..].to_string())
    }

    fn content(rng: &mut Rng, max: usize) -> Vec<u8> {
        let length = match rng.below(100) {
            0..40 => rng.below(1000),
            40..80 => rng.below(65_000),
            80..95 => rng.below(300_000),
            _ => rng.below(1_500_000),
        }
        .min(max);
        let mut data = vec![0u8; length];
        rng.fill(&mut data);
        data
    }

    /// Does one random thing to the volume and the model. Running out of space or path
    /// length is allowed, and the volume then has to be exactly as it was.
    pub fn step(vol: &mut FatxVolume, m: &mut Model, rng: &mut Rng, allow_moves: bool, max_size: usize) {
        match apply(vol, m, rng, allow_moves, max_size) {
            Ok(()) => {}
            Err(Error::Fatx {
                kind: ErrorKind::DiskFull | ErrorKind::PathTooLong,
                ..
            }) => {}
            Err(other) => panic!("unexpected: {other}"),
        }
    }

    fn apply(
        vol: &mut FatxVolume,
        m: &mut Model,
        rng: &mut Rng,
        allow_moves: bool,
        max_size: usize,
    ) -> crate::Result<()> {
        let roll = rng.below(100);
        let mut dirs: Vec<String> = m.dirs.iter().cloned().collect();
        dirs.sort();
        let mut files: Vec<String> = m.files.keys().cloned().collect();
        files.sort();

        if roll < 22 {
            let dir = dirs[rng.below(dirs.len())].clone();
            let path = join(&dir, NAMES[rng.below(NAMES.len())]);
            if m.files.contains_key(&path) || m.dirs.contains(&path) {
                return Ok(());
            }
            let parent = vol.resolve(&dir)?;
            let node = vol.create_file(parent, &name_of(&path))?;
            m.files.insert(path.clone(), Vec::new());
            let data = content(rng, max_size);
            vol.write(node, 0, &data)?;
            m.files.insert(path, data);
        } else if roll < 36 && !files.is_empty() {
            let path = files[rng.below(files.len())].clone();
            let add = content(rng, max_size.min(100_000));
            let node = vol.resolve(&path)?;
            let end = vol.info(node).size;
            vol.write(node, end, &add)?;
            m.files.get_mut(&path).unwrap().extend_from_slice(&add);
        } else if roll < 48 && !files.is_empty() {
            let path = files[rng.below(files.len())].clone();
            let old = m.files[&path].clone();
            let offset = rng.below(old.len() + 5000);
            let patch = content(rng, max_size.min(30_000));
            if patch.is_empty() {
                return Ok(());
            }
            let node = vol.resolve(&path)?;
            vol.write(node, offset as u64, &patch)?;
            let mut next = vec![0u8; old.len().max(offset + patch.len())];
            next[..old.len()].copy_from_slice(&old);
            next[offset..offset + patch.len()].copy_from_slice(&patch);
            m.files.insert(path, next);
        } else if roll < 58 && !files.is_empty() {
            let path = files[rng.below(files.len())].clone();
            let old = m.files[&path].clone();
            let length = rng.below(old.len() + 20_000);
            let node = vol.resolve(&path)?;
            vol.set_length(node, length as u64)?;
            let mut next = vec![0u8; length];
            let keep = old.len().min(length);
            next[..keep].copy_from_slice(&old[..keep]);
            m.files.insert(path, next);
        } else if roll < 70 && !files.is_empty() {
            let path = files[rng.below(files.len())].clone();
            let node = vol.resolve(&path)?;
            vol.delete(node)?;
            m.files.remove(&path);
        } else if roll < 77 {
            let dir = dirs[rng.below(dirs.len())].clone();
            let path = join(&dir, NAMES[rng.below(NAMES.len())]);
            if m.files.contains_key(&path) || m.dirs.contains(&path) {
                return Ok(());
            }
            let parent = vol.resolve(&dir)?;
            vol.create_directory(parent, &name_of(&path))?;
            m.dirs.insert(path);
        } else if roll < 82 && dirs.len() > 1 {
            let path = dirs[rng.range(1, dirs.len())].clone();
            let empty = !m
                .files
                .keys()
                .chain(m.dirs.iter())
                .any(|p| *p != path && parent_of(p) == path);
            let node = vol.resolve(&path)?;
            if !empty {
                assert!(vol.delete(node).is_err(), "a directory with things in it was deleted");
                return Ok(());
            }
            vol.delete(node)?;
            m.dirs.remove(&path);
        } else if roll < 93 && !files.is_empty() {
            let from = files[rng.below(files.len())].clone();
            let to_dir = if allow_moves {
                dirs[rng.below(dirs.len())].clone()
            } else {
                parent_of(&from)
            };
            let to = join(&to_dir, NAMES[rng.below(NAMES.len())]);
            if m.dirs.contains(&to) {
                return Ok(());
            }
            let exists = m.files.contains_key(&to) && to != from;
            let replace = rng.below(2) == 0;
            let node = vol.resolve(&from)?;
            let target = vol.resolve(&to_dir)?;
            if exists && !replace {
                assert!(vol.rename(node, target, &name_of(&to), false).is_err());
                return Ok(());
            }
            vol.rename(node, target, &name_of(&to), replace)?;
            let data = m.files.remove(&from).unwrap();
            m.files.remove(&to);
            m.files.insert(to, data);
        } else if dirs.len() > 1 {
            let from = dirs[rng.range(1, dirs.len())].clone();
            let to_dir = if allow_moves {
                dirs[rng.below(dirs.len())].clone()
            } else {
                parent_of(&from)
            };
            let to = join(&to_dir, NAMES[rng.below(NAMES.len())]);
            let into_self = to_dir == from || to_dir.starts_with(&format!("{from}\\"));
            if m.files.contains_key(&to) || m.dirs.contains(&to) || into_self {
                if into_self && !m.files.contains_key(&to) && !m.dirs.contains(&to) {
                    let node = vol.resolve(&from)?;
                    let target = vol.resolve(&to_dir)?;
                    assert!(vol.rename(node, target, &name_of(&to), false).is_err());
                }
                return Ok(());
            }
            let node = vol.resolve(&from)?;
            let target = vol.resolve(&to_dir)?;
            vol.rename(node, target, &name_of(&to), false)?;

            let prefix = format!("{from}\\");
            let moved: Vec<String> = m.files.keys().filter(|p| p.starts_with(&prefix)).cloned().collect();
            for path in moved {
                let data = m.files.remove(&path).unwrap();
                m.files.insert(format!("{to}{}", &path[from.len()..]), data);
            }
            let moved: Vec<String> = m
                .dirs
                .iter()
                .filter(|p| **p == from || p.starts_with(&prefix))
                .cloned()
                .collect();
            for path in moved {
                m.dirs.remove(&path);
                m.dirs.insert(format!("{to}{}", &path[from.len()..]));
            }
        }
        Ok(())
    }

    /// The volume and the model have to say the same thing.
    pub fn compare(vol: &mut FatxVolume, m: &Model, wher: &str) {
        let mut seen_files = HashSet::new();
        let mut seen_dirs = HashSet::from([String::new()]);
        let mut pending = vec![(vol.root(), String::new())];
        while let Some((dir, path)) = pending.pop() {
            for child in vol.children(dir).unwrap() {
                let child_path = join(&path, &child.name);
                if child.is_directory {
                    seen_dirs.insert(child_path.clone());
                    pending.push((child.id, child_path));
                } else {
                    seen_files.insert(child_path.clone());
                    let expected = m
                        .files
                        .get(&child_path)
                        .unwrap_or_else(|| panic!("{wher}: unexpected file {child_path}"));
                    let actual = read_all(vol, child.id);
                    assert!(
                        *expected == actual,
                        "{wher}: contents of {child_path} differ ({} vs {} bytes)",
                        expected.len(),
                        actual.len()
                    );
                }
            }
        }
        assert_eq!(
            seen_files,
            m.files.keys().cloned().collect::<HashSet<_>>(),
            "{wher}: file lists differ"
        );
        assert_eq!(seen_dirs, m.dirs, "{wher}: directory lists differ");
    }
}

fn steps() -> usize {
    std::env::var("FREEXPLORER_FUZZ_STEPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500)
}

fn seeds() -> u64 {
    std::env::var("FREEXPLORER_FUZZ_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6)
}

#[test]
fn random_operations_match_the_model() {
    for seed in 1..=seeds() {
        // the big one has 4K clusters and a 32 bit FAT, the small one 16K clusters and a 16 bit FAT
        let big = seed % 2 == 0;
        let (device, mut volume) = if big {
            create(2 << 30, 8, Flavor::Xbox360)
        } else {
            create(96 << 20, 0x20, Flavor::Xbox360)
        };
        let mut m = model::Model::new();
        let mut rng = Rng::new(seed);

        for step in 1..=steps() {
            model::step(&mut volume, &mut m, &mut rng, true, 1_500_000);
            if step % 150 == 0 {
                model::compare(&mut volume, &m, &format!("seed {seed}, step {step}"));
            }
            if step % 500 == 0 {
                volume = reopen(&device, volume);
                model::compare(&mut volume, &m, &format!("seed {seed}, reopened at step {step}"));
            }
        }
        assert_clean(&mut volume);
    }
}

fn fresh_image() -> Arc<MemoryDevice> {
    let device = Arc::new(MemoryDevice::new(24 << 20));
    let shared: Device = device.clone();
    format(
        &shared,
        0,
        24 << 20,
        &FormatOptions {
            volume_id: Some(1),
            ..Default::default()
        },
        |_| {},
    )
    .unwrap();
    device
}

/// Power cut simulation. Replay the first N writes of a busy session onto a fresh image for
/// lots of values of N and make sure the result is always a volume that opens, lists, and
/// has no damage beyond some leaked clusters.
#[test]
fn every_prefix_of_the_writes_is_a_usable_volume() {
    for seed in 1..=4u64 {
        let image: Device = fresh_image();
        let live = Arc::new(JournalDevice::new(image));
        {
            let shared: Device = live.clone();
            let mut volume = FatxVolume::open(shared, true).unwrap();
            let mut m = model::Model::new();
            let mut rng = Rng::new(seed);
            for _ in 0..120 {
                model::step(&mut volume, &mut m, &mut rng, false, 200_000);
            }
            volume.flush().unwrap();
        }

        let writes = live.writes.lock().unwrap().clone();
        let replica = fresh_image();
        let mut looked_at = 0;
        for n in 0..=writes.len() {
            if n > 0 {
                replica.write_at(writes[n - 1].0, &writes[n - 1].1).unwrap();
            }
            // look at every state early on, then every few writes
            if n > 80 && n % 5 != 0 && n != writes.len() {
                continue;
            }

            let shared: Device = replica.clone();
            let mut seen = FatxVolume::open(shared, false).unwrap();
            let report = seen.check(false, |_| {}).unwrap();
            for issue in &report.issues {
                let leak = issue.message.contains("belong to nothing")
                    || issue.message.contains("more clusters than the size needs");
                assert!(
                    issue.severity != Severity::Error && leak,
                    "after {n} of {} writes: {:?}: {}",
                    writes.len(),
                    issue.severity,
                    issue.message
                );
            }
            looked_at += 1;
        }
        assert!(looked_at > 50, "the test didn't look at enough states to mean anything");
    }
}

/// The finished image has to be exactly what a replay of the whole journal produces.
#[test]
fn the_journal_covers_every_write() {
    let inner = fresh_image();
    let live = Arc::new(JournalDevice::new(inner.clone()));
    {
        let shared: Device = live.clone();
        let mut volume = FatxVolume::open(shared, true).unwrap();
        let mut m = model::Model::new();
        let mut rng = Rng::new(77);
        for _ in 0..60 {
            model::step(&mut volume, &mut m, &mut rng, false, 100_000);
        }
    }

    let replica = fresh_image();
    for (offset, data) in live.writes.lock().unwrap().iter() {
        replica.write_at(*offset, data).unwrap();
    }
    for pos in (0..24u64 << 20).step_by(1 << 20) {
        assert!(
            read_vec(inner.as_ref(), pos, 1 << 20).unwrap() == read_vec(replica.as_ref(), pos, 1 << 20).unwrap(),
            "images differ in the megabyte at {pos}"
        );
    }
}

// ---- finding things by number ----

#[test]
fn a_node_can_be_found_again_by_its_number() {
    let device = Arc::new(MemoryDevice::new(64 << 20));
    let shared: Device = device.clone();
    format(&shared, 0, shared.len(), &FormatOptions::default(), |_| {}).unwrap();
    let mut vol = open(&device);

    let dir = vol.create_directory(vol.root(), "Games").unwrap();
    let file = vol.create_file(dir, "a.bin").unwrap();

    assert_eq!(vol.node_at(vol.root().index()), Some(vol.root()));
    assert_eq!(vol.node_at(file.index()), Some(file));
    assert_eq!(vol.node_at(9999), None);
    assert_eq!(vol.parent_of(file), Some(dir));
    assert_eq!(vol.parent_of(dir), Some(vol.root()));
    assert_eq!(vol.parent_of(vol.root()), None);

    vol.delete(file).unwrap();
    assert_eq!(vol.node_at(file.index()), None, "a deleted node is gone");
}
