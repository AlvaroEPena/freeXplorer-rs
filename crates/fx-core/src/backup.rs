//! A backup is an exact copy of a whole drive in one file, every sector from the first to the
//! last, so the security sector, the cache partitions and everything else comes along. Next to
//! it goes a small .sha256 file, which is how a damaged backup is spotted before it overwrites
//! a drive.

use std::fs;
use std::path::{Path, PathBuf};

use crate::copy::{self, Cancel, CopyOptions, ProgressSink};
use crate::device::{BlockDevice, FileDevice};
use crate::error::{Error, Result};
use crate::size;
use crate::sparse;

/// A restore writes in long runs, give the drive a chance to say it has caught up now and then.
const FLUSH_EVERY: u64 = 128 << 20;

#[derive(Debug, Clone, Copy)]
pub struct BackupOptions {
    pub verify: bool,
    pub sparse: bool,
}

impl Default for BackupOptions {
    fn default() -> Self {
        BackupOptions {
            verify: true,
            sparse: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupResult {
    pub path: PathBuf,
    pub length: u64,
    pub sha256: String,
    pub verified: bool,
    pub size_on_disk: u64,
}

pub struct RestoreOptions {
    pub verify: bool,
    /// How much of the start of the drive to leave as it is. The backup's own copy of that
    /// part is never written, and never read back either.
    pub skip_first_bytes: u64,
    /// Called once, right before the first byte goes to the drive. Until then a failure
    /// leaves it exactly as it was.
    pub writing_started: Option<Box<dyn FnOnce() + Send>>,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        RestoreOptions {
            verify: true,
            skip_first_bytes: 0,
            writing_started: None,
        }
    }
}

/// `sha256` is of what was written, which is everything after `skip_first_bytes`.
#[derive(Debug, Clone)]
pub struct RestoreResult {
    pub length: u64,
    pub sha256: String,
    pub verified: bool,
    pub checksum_file_used: bool,
    pub skipped: u64,
}

/// What the operating system knows about the place a backup is going.
#[derive(Debug, Clone, Default)]
pub struct DestinationInfo {
    /// the drive or mount point, for messages
    pub root: String,
    pub free_bytes: Option<u64>,
    pub file_system: String,
    /// holes in a file cost nothing here
    pub supports_sparse: bool,
    /// the biggest file this file system takes, if it has a limit
    pub max_file_size: Option<u64>,
    pub network: bool,
}

/// Whether a backup can be written to the place that was picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationCheck {
    pub problem: Option<String>,
    pub warning: Option<String>,
    pub free_bytes: u64,
    pub sparse: bool,
}

impl DestinationCheck {
    fn problem(text: impl Into<String>) -> Self {
        DestinationCheck {
            problem: Some(text.into()),
            warning: None,
            free_bytes: 0,
            sparse: false,
        }
    }
}

pub fn checksum_path(image: &Path) -> PathBuf {
    let mut name = image.as_os_str().to_os_string();
    name.push(".sha256");
    PathBuf::from(name)
}

pub fn check_destination(path: &Path, length: u64, info: impl FnOnce(&Path) -> DestinationInfo) -> DestinationCheck {
    let Ok(full) = std::path::absolute(path) else {
        return DestinationCheck::problem("That isn't a usable file name.");
    };
    if full.file_name().is_none() {
        return DestinationCheck::problem("That isn't a usable file name.");
    }
    match full.parent() {
        Some(folder) if folder.is_dir() => {}
        _ => return DestinationCheck::problem("That folder doesn't exist."),
    }
    if full.exists() {
        return DestinationCheck::problem("There's already something with that name.");
    }

    let info = info(&full);
    if info.network {
        return DestinationCheck {
            problem: None,
            warning: Some("This is a network location, so there's no way to tell how much room it has.".into()),
            free_bytes: 0,
            sparse: false,
        };
    }
    let Some(free) = info.free_bytes else {
        return DestinationCheck {
            problem: None,
            warning: Some("Couldn't tell how much room there is.".into()),
            free_bytes: 0,
            sparse: false,
        };
    };
    let root = &info.root;

    if info.max_file_size.is_some_and(|max| length > max) {
        return DestinationCheck {
            problem: Some(format!(
                "{root} is formatted as {}, which can't hold a file that big. Pick another drive.",
                info.file_system
            )),
            warning: None,
            free_bytes: free,
            sparse: false,
        };
    }

    if !info.supports_sparse {
        return if free < length {
            DestinationCheck {
                problem: Some(format!(
                    "{root} has {} free and a backup needs {}. This drive can't skip the empty parts of the backup.",
                    size::format(free),
                    size::format(length)
                )),
                warning: None,
                free_bytes: free,
                sparse: false,
            }
        } else {
            DestinationCheck {
                problem: None,
                warning: None,
                free_bytes: free,
                sparse: false,
            }
        };
    }

    if free < (64 << 20) {
        return DestinationCheck {
            problem: Some(format!("{root} is almost full.")),
            warning: None,
            free_bytes: free,
            sparse: true,
        };
    }
    let warning = (free < length).then(|| {
        format!(
            "The backup needs room for everything on the drive that isn't empty, up to {}. {root} has {} free. \
             If it runs out, the backup stops and the partial file is deleted.",
            size::format(length),
            size::format(free)
        )
    });
    DestinationCheck {
        problem: None,
        warning,
        free_bytes: free,
        sparse: true,
    }
}

pub fn create(
    source: &dyn BlockDevice,
    path: &Path,
    options: &BackupOptions,
    progress: &mut dyn ProgressSink,
    cancel: &Cancel,
) -> Result<BackupResult> {
    let length = source.len();
    let checksum = checksum_path(path);
    let mut created = false;
    let mut finished = false;

    let outcome = (|| -> Result<BackupResult> {
        // create_new, so a file that's already there is never touched
        let image = FileDevice::create_image(path, length, options.sparse)?;
        created = true;
        let hash = copy::copy(
            source,
            &image,
            length,
            &CopyOptions {
                skip_zero_blocks: options.sparse,
                flush_every: 0,
            },
            progress,
            cancel,
            "Copying the drive",
            0,
        )?;
        drop(image);

        if options.verify {
            let again = FileDevice::open(path, false)?;
            let back = copy::hash(&again, length, progress, cancel, "Checking the backup", 0)?;
            if back != hash {
                return Err(Error::Backup(
                    "The backup file doesn't read back the same as the drive did, so it was deleted. \
                     The disk it was saved on may be failing, try saving it somewhere else."
                        .into(),
                ));
            }
        }

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        fs::write(&checksum, format!("{hash}  {name}\n"))?;
        finished = true;
        Ok(BackupResult {
            path: std::path::absolute(path)?,
            length,
            sha256: hash,
            verified: options.verify,
            size_on_disk: sparse::size_on_disk(path).unwrap_or(length),
        })
    })();

    if created && !finished {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(&checksum);
    }
    outcome
}

pub fn restore(
    path: &Path,
    target: &dyn BlockDevice,
    mut options: RestoreOptions,
    progress: &mut dyn ProgressSink,
    cancel: &Cancel,
) -> Result<RestoreResult> {
    if !target.can_write() {
        return Err(Error::Unsupported(format!("{} was opened read-only", target.name())));
    }

    let image = FileDevice::open(path, false)?;
    let length = image.len();
    if length == 0 {
        return Err(Error::Backup("That backup file is empty.".into()));
    }
    if length > target.len() {
        return Err(Error::invalid(format!(
            "The backup is {} and the drive is only {}. A drive has to be at least as big as the one that was backed up.",
            size::format(length),
            size::format(target.len())
        )));
    }

    let skip = options.skip_first_bytes;
    if skip >= length {
        return Err(Error::invalid(format!(
            "The backup is only {}, there's nothing left to write after the first {}.",
            size::format(length),
            size::format(skip)
        )));
    }

    // a damaged backup is found out before the drive is touched
    let expected = read_checksum(path);
    if let Some(expected) = &expected {
        let before = copy::hash(&image, length, progress, cancel, "Checking the backup", 0)?;
        if &before != expected {
            return Err(Error::Backup(
                "The backup file doesn't match its checksum file, so it has been damaged since it was made. \
                 Nothing was written to the drive."
                    .into(),
            ));
        }
    }

    if let Some(started) = options.writing_started.take() {
        started();
    }
    let hash = copy::copy(
        &image,
        target,
        length,
        &CopyOptions {
            skip_zero_blocks: false,
            flush_every: FLUSH_EVERY,
        },
        progress,
        cancel,
        "Writing the drive",
        skip,
    )?;

    if options.verify {
        let back = copy::hash(target, length, progress, cancel, "Checking the drive", skip)?;
        if back != hash {
            return Err(Error::Backup(
                "The drive doesn't read back the same as the backup. It was not copied correctly, so don't use it. \
                 Try again, and if it keeps happening the drive or its cable may be failing."
                    .into(),
            ));
        }
    }
    Ok(RestoreResult {
        length,
        sha256: hash,
        verified: options.verify,
        checksum_file_used: expected.is_some(),
        skipped: skip,
    })
}

/// The hash from the .sha256 file next to a backup, or None when there isn't one that makes sense.
pub fn read_checksum(image: &Path) -> Option<String> {
    let text = fs::read_to_string(checksum_path(image)).ok()?;
    text.lines().find_map(|line| {
        let line = line.trim();
        let head = line.get(..64)?;
        head.bytes()
            .all(|b| b.is_ascii_hexdigit())
            .then(|| head.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::copy::NoProgress;
    use crate::device::{Device, MemoryDevice, read_vec};

    fn drive(length: u64, seed: u8) -> MemoryDevice {
        let device = MemoryDevice::new(length);
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed))
            .collect();
        device.write_at(2000, &data[..2000]).unwrap();
        if length > (5 << 20) + 200_000 {
            device.write_at(5 << 20, &data).unwrap();
        }
        device.write_at(length - 512, &data[..512]).unwrap();
        device
    }

    #[test]
    fn a_backup_is_the_whole_drive_plus_a_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.img");
        let source = drive(24 << 20, 1);
        let result = create(
            &source,
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();

        assert_eq!(fs::metadata(&path).unwrap().len(), 24 << 20);
        assert!(result.verified);
        let sums = fs::read_to_string(dir.path().join("saved.img.sha256")).unwrap();
        assert_eq!(sums, format!("{}  saved.img\n", result.sha256));
        assert_eq!(read_checksum(&path).as_deref(), Some(result.sha256.as_str()));

        let image = FileDevice::open(&path, false).unwrap();
        assert_eq!(
            read_vec(&image, 5 << 20, 4096).unwrap(),
            read_vec(&source, 5 << 20, 4096).unwrap()
        );
    }

    #[test]
    fn a_file_that_is_already_there_is_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mine.img");
        fs::write(&path, "precious").unwrap();
        let source = drive(1 << 20, 1);
        assert!(
            create(
                &source,
                &path,
                &BackupOptions::default(),
                &mut NoProgress,
                &Cancel::new()
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "precious");
    }

    #[test]
    fn a_stopped_backup_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stopped.img");
        let source = drive(24 << 20, 1);
        let cancel = Cancel::new();
        cancel.cancel();
        let result = create(&source, &path, &BackupOptions::default(), &mut NoProgress, &cancel);
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(!path.exists());
        assert!(!dir.path().join("stopped.img.sha256").exists());
    }

    #[test]
    fn a_restore_gives_an_exact_clone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        let source = drive(24 << 20, 1);
        create(
            &source,
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();

        let target = drive(24 << 20, 99);
        let result = restore(
            &path,
            &target,
            RestoreOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert!(result.verified && result.checksum_file_used);
        assert_eq!(
            read_vec(&target, 0, 24 << 20).unwrap(),
            read_vec(&source, 0, 24 << 20).unwrap()
        );
    }

    #[test]
    fn a_bigger_drive_keeps_what_comes_after_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        create(
            &drive(8 << 20, 1),
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();

        let target = MemoryDevice::new(12 << 20);
        target.write_at(10 << 20, &[0x77; 1000]).unwrap();
        restore(
            &path,
            &target,
            RestoreOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(read_vec(&target, 10 << 20, 1000).unwrap(), vec![0x77; 1000]);
    }

    #[test]
    fn a_drive_that_is_too_small_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        create(
            &drive(8 << 20, 1),
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();

        let target = drive(4 << 20, 5);
        let before = read_vec(&target, 0, 4 << 20).unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let flag = started.clone();
        let options = RestoreOptions {
            writing_started: Some(Box::new(move || flag.store(true, Ordering::Relaxed))),
            ..Default::default()
        };
        assert!(restore(&path, &target, options, &mut NoProgress, &Cancel::new()).is_err());
        assert!(!started.load(Ordering::Relaxed));
        assert_eq!(read_vec(&target, 0, 4 << 20).unwrap(), before);
    }

    #[test]
    fn a_damaged_backup_is_refused_and_the_drive_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        create(
            &drive(8 << 20, 1),
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        {
            let image = FileDevice::open(&path, true).unwrap();
            image.write_at((5 << 20) + 100, &[0xEE]).unwrap();
        }
        let target = drive(8 << 20, 5);
        let before = read_vec(&target, 0, 8 << 20).unwrap();
        let err = restore(
            &path,
            &target,
            RestoreOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("damaged"), "{err}");
        assert_eq!(read_vec(&target, 0, 8 << 20).unwrap(), before);
    }

    #[test]
    fn without_a_checksum_file_a_restore_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        create(
            &drive(8 << 20, 1),
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        fs::remove_file(dir.path().join("a.img.sha256")).unwrap();
        let target = MemoryDevice::new(8 << 20);
        let result = restore(
            &path,
            &target,
            RestoreOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        assert!(!result.checksum_file_used);
    }

    #[test]
    fn the_start_of_the_drive_can_be_left_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        let source = drive(8 << 20, 1);
        create(
            &source,
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();

        let target = drive(8 << 20, 50);
        let start = read_vec(&target, 0, 0x80000).unwrap();
        let options = RestoreOptions {
            skip_first_bytes: 0x80000,
            ..Default::default()
        };
        let result = restore(&path, &target, options, &mut NoProgress, &Cancel::new()).unwrap();
        assert_eq!(result.skipped, 0x80000);
        assert_eq!(read_vec(&target, 0, 0x80000).unwrap(), start);
        assert_eq!(
            read_vec(&target, 0x80000, 1 << 20).unwrap(),
            read_vec(&source, 0x80000, 1 << 20).unwrap()
        );
    }

    #[test]
    fn skipping_the_whole_drive_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        create(
            &drive(1 << 20, 1),
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        let target = MemoryDevice::new(1 << 20);
        let options = RestoreOptions {
            skip_first_bytes: 1 << 20,
            ..Default::default()
        };
        assert!(restore(&path, &target, options, &mut NoProgress, &Cancel::new()).is_err());
    }

    #[test]
    fn a_bad_write_is_caught_by_the_read_back() {
        struct Flaky(Device);
        impl BlockDevice for Flaky {
            fn name(&self) -> &str {
                "flaky"
            }
            fn len(&self) -> u64 {
                self.0.len()
            }
            fn can_write(&self) -> bool {
                true
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
                self.0.read_at(offset, buf)
            }
            fn write_at(&self, offset: u64, data: &[u8]) -> std::io::Result<()> {
                self.0.write_at(offset, data)?;
                if offset <= (6 << 20) && offset + data.len() as u64 > (6 << 20) {
                    self.0.write_at(6 << 20, &[0xFF ^ data[(6 << 20) - offset as usize]])?;
                }
                Ok(())
            }
            fn flush(&self) -> std::io::Result<()> {
                self.0.flush()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.img");
        create(
            &drive(8 << 20, 1),
            &path,
            &BackupOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        let target = Flaky(Arc::new(MemoryDevice::new(8 << 20)));
        let err = restore(
            &path,
            &target,
            RestoreOptions::default(),
            &mut NoProgress,
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("doesn't read back the same"), "{err}");
    }

    #[test]
    fn checksum_files_are_read_in_the_sha256sum_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.img");
        let hash = "AB".repeat(32);
        fs::write(checksum_path(&path), format!("{hash}  x.img\n")).unwrap();
        assert_eq!(read_checksum(&path), Some(hash.to_lowercase()));
        fs::write(checksum_path(&path), "not a hash\n").unwrap();
        assert_eq!(read_checksum(&path), None);
    }

    fn info(root: &str, free: u64, sparse: bool, max: Option<u64>) -> DestinationInfo {
        DestinationInfo {
            root: root.into(),
            free_bytes: Some(free),
            file_system: if max.is_some() { "FAT32" } else { "NTFS" }.into(),
            supports_sparse: sparse,
            max_file_size: max,
            network: false,
        }
    }

    #[test]
    fn bad_destinations_are_problems() {
        let dir = tempfile::tempdir().unwrap();
        let take = dir.path().join("taken.img");
        fs::write(&take, "x").unwrap();
        let ok = |_: &Path| info("C:\\", 1 << 40, true, None);
        assert!(
            check_destination(&take, 1 << 20, ok)
                .problem
                .unwrap()
                .contains("already something")
        );
        assert!(
            check_destination(&dir.path().join("no/such/folder/x.img"), 1 << 20, ok)
                .problem
                .unwrap()
                .contains("folder")
        );
        assert!(
            check_destination(&dir.path().join("fine.img"), 1 << 20, ok)
                .problem
                .is_none()
        );
    }

    #[test]
    fn not_enough_room_warns_where_sparse_files_work_and_blocks_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.img");
        let sparse = check_destination(&target, 100 << 30, |_| info("C:\\", 50 << 30, true, None));
        assert!(sparse.problem.is_none() && sparse.warning.is_some() && sparse.sparse);
        let plain = check_destination(&target, 100 << 30, |_| info("E:\\", 50 << 30, false, None));
        assert!(plain.problem.unwrap().contains("can't skip the empty parts"));
    }

    #[test]
    fn a_file_system_with_a_size_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let check = check_destination(&dir.path().join("x.img"), 8 << 30, |_| {
            info("F:\\", 1 << 40, false, Some(0xFFFF_FFFF))
        });
        assert!(check.problem.unwrap().contains("FAT32"));
    }

    #[test]
    fn a_nearly_full_disk_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        let check = check_destination(&dir.path().join("x.img"), 1 << 30, |_| {
            info("C:\\", 10 << 20, true, None)
        });
        assert!(check.problem.unwrap().contains("almost full"));
    }
}
