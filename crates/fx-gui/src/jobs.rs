//! Everything that touches a drive happens on a thread of its own and reports back with messages,
//! so the window never stops responding while a backup runs.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use fx_core::Error;
use fx_core::backup::{self, BackupOptions, RestoreOptions};
use fx_core::copy::{Cancel, Progress};
use fx_core::device::{Device, FileDevice};
use fx_core::fatx::{self, FormatOptions};
use fx_core::size;
use fx_core::xbox360::formatter::{self, HddFormatOptions};
use fx_core::xbox360::scanner::{self, DetectedPartition};
use fx_core::xbox360::security::{self, SecuritySector};
use fx_platform::{DiskInfo, KeepAwake, safety};

use crate::model::{
    BackupRequest, FormatRequest, Loaded, MountState, PartRow, RestoreInfo, RestoreRequest, SourceKind, SourceRow,
    drive_summary,
};

/// What a worker tells the window.
pub enum Msg {
    Progress {
        fraction: f64,
        status: Option<String>,
    },
    Log(String),
    Status(String),
    Disks(Vec<DiskInfo>),
    Polled(Vec<DiskInfo>),
    Loaded {
        key: String,
        result: Result<Loaded, String>,
    },
    Mounted {
        key: String,
        part_id: String,
        result: Result<(fx_mount::Mounted, String), String>,
    },
    Unmounted {
        key: String,
        part_id: String,
        result: Result<(), String>,
    },
    Text {
        title: String,
        text: String,
    },
    Error(String),
    Reload(String),
    OpenRestore(RestoreInfo),
    DriverInstalled {
        restart_needed: bool,
    },
    /// the job is over, successfully or not
    Done,
}

/// What a worker uses to talk back.
#[derive(Clone)]
pub struct Reporter {
    tx: Sender<Msg>,
    egui: eframe::egui::Context,
    pub cancel: Cancel,
}

impl Reporter {
    pub fn new(tx: Sender<Msg>, egui: eframe::egui::Context, cancel: Cancel) -> Reporter {
        Reporter { tx, egui, cancel }
    }

    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.egui.request_repaint();
    }

    pub fn log(&self, text: impl Into<String>) {
        self.send(Msg::Log(text.into()));
    }

    pub fn progress(&self, fraction: f64, status: Option<String>) {
        self.send(Msg::Progress { fraction, status });
    }

    /// For `copy` and `backup`, which report as they go.
    pub fn copy_progress(&self) -> impl FnMut(&Progress) + use<> {
        let me = self.clone();
        move |p: &Progress| me.progress(p.fraction(), Some(format!("{} ({})", p.phase, p.summary())))
    }
}

/// Runs work on its own thread. A panic in it becomes an error message instead of a frozen window.
pub fn spawn(reporter: &Reporter, work: impl FnOnce(&Reporter) + Send + 'static) {
    let reporter = reporter.clone();
    std::thread::spawn(move || {
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| work(&reporter))) {
            let what = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "something unexpected".into());
            reporter.send(Msg::Error(format!("freeXplorer-rs hit a bug: {what}")));
        }
        reporter.send(Msg::Done);
    });
}

fn describe(err: impl std::fmt::Display) -> String {
    err.to_string()
}

pub fn open(source: &SourceRow, writable: bool) -> Result<Device, String> {
    match source.kind {
        SourceKind::Disk => {
            let disk = source.disk.as_ref().ok_or("that disk is gone")?;
            fx_platform::open_disk(disk, writable).map_err(describe)
        }
        SourceKind::Image => {
            let path = source.image.as_ref().ok_or("that file is gone")?;
            Ok(Arc::new(FileDevice::open(path, writable).map_err(describe)?))
        }
    }
}

// ---- looking ----

pub fn list_disks(r: &Reporter) {
    r.progress(0.0, Some("Looking for drives...".into()));
    let disks = fx_platform::list_disks();
    r.log(if disks.is_empty() {
        "No physical disks found.".to_string()
    } else {
        format!("Found {} disk{}.", disks.len(), if disks.len() == 1 { "" } else { "s" })
    });
    r.send(Msg::Disks(disks));
}

pub fn read_source(r: &Reporter, source: SourceRow) {
    r.progress(0.0, Some(format!("Reading {}...", source.title)));
    let result = (|| -> Result<Loaded, String> {
        let device = open(&source, false)?;
        let scan = scanner::scan(device.as_ref()).map_err(describe)?;
        let partitions = scan
            .partitions
            .iter()
            .map(|p| describe_partition(&device, p))
            .collect::<Vec<_>>();
        Ok(Loaded { scan, partitions })
    })();
    if let Ok(loaded) = &result {
        let usable = loaded.partitions.iter().filter(|p| p.can_mount()).count();
        let summary = drive_summary(&source.title, &loaded.scan, usable);
        r.send(Msg::Status(summary.clone()));
        r.log(summary);
    }
    r.send(Msg::Loaded {
        key: source.key.clone(),
        result,
    });
}

fn describe_partition(device: &Device, p: &DetectedPartition) -> PartRow {
    let mut format = String::new();
    if p.can_mount() {
        match scanner::open_partition(device, p, false).map(|v| {
            let kind = if v.flavor() == fatx::Flavor::Xbox360 {
                "XTAF"
            } else {
                "FATX"
            };
            format!(
                "{kind}, {} KB clusters, FAT{}",
                v.cluster_size() / 1024,
                v.geometry().fat_entry_size * 8
            )
        }) {
            Ok(text) => format = text,
            Err(err) => format = format!("Looks damaged: {err}"),
        }
    }
    PartRow {
        detected: p.clone(),
        format,
        mount: None,
    }
}

// ---- mounting ----

pub fn mount_one(r: &Reporter, source: SourceRow, part: DetectedPartition, write: bool) {
    r.progress(0.0, Some(format!("Mounting {}...", part.name())));
    let key = source.key.clone();
    let part_id = part.id().to_string();
    let result = (|| -> Result<(fx_mount::Mounted, String), String> {
        let device = open(&source, write)?;
        let mut volume = scanner::open_partition(&device, &part, write).map_err(describe)?;
        volume.scan_free_space(|_| {}).map_err(describe)?;
        let label = volume
            .label()
            .map_err(describe)?
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| part.name().to_string());
        let usage = format!(
            "{} used of {}",
            size::format(volume.total_bytes() - volume.free_bytes()),
            size::format(volume.total_bytes())
        );
        let mounted = fx_mount::mount(volume, &label, None).map_err(describe)?;
        Ok((mounted, usage))
    })();
    r.send(Msg::Mounted { key, part_id, result });
}

pub fn unmount(r: &Reporter, entries: Vec<(String, String, fx_mount::Mounted)>) {
    for (key, part_id, mounted) in entries {
        let point = mounted.mount_point().to_string();
        r.progress(0.0, Some(format!("Unmounting {point}...")));
        let result = mounted.unmount().map_err(describe);
        if result.is_ok() {
            r.send(Msg::Status(format!("Unmounted {point}")));
            r.log(format!("Unmounted {point}"));
        }
        r.send(Msg::Unmounted { key, part_id, result });
    }
}

// ---- checking and formatting ----

pub fn check(r: &Reporter, source: SourceRow, part: DetectedPartition) {
    r.progress(0.0, Some(format!("Checking {}...", part.name())));
    // a mounted volume is being modified through its own copy of the directory tree, so check it read-only
    let result = (|| -> Result<String, String> {
        let device = open(&source, false)?;
        let mut volume = scanner::open_partition(&device, &part, false).map_err(describe)?;
        let report = volume.check(false, |_| {}).map_err(describe)?;
        Ok(report.to_text(part.name()))
    })();
    match result {
        Ok(text) => {
            r.send(Msg::Text {
                title: format!("Check of {}", part.name()),
                text,
            });
            r.send(Msg::Status(format!("Checked {}.", part.name())));
            r.log(format!("Checked {}.", part.name()));
        }
        Err(err) => r.send(Msg::Error(err)),
    }
}

pub fn format(r: &Reporter, source: SourceRow, request: FormatRequest) {
    r.progress(0.0, Some(format!("Formatting {}...", source.title)));
    let _awake = KeepAwake::new("Formatting a drive");
    let result = (|| -> Result<(), String> {
        let device = open(&source, true)?;
        let progress = |p: f64| r.progress(p, None);
        if request.whole_image {
            let length = device.len() / 512 * 512;
            let options = FormatOptions {
                sectors_per_cluster: request.content_sectors_per_cluster,
                label: request.label.clone(),
                ..FormatOptions::default()
            };
            fatx::format(&device, 0, length, &options, progress).map_err(describe)?;
        } else {
            let options = HddFormatOptions {
                content_sectors_per_cluster: request.content_sectors_per_cluster,
                content_length: request.content_length,
                content_label: request.label.clone(),
                compat_flavor: request.compat_flavor,
            };
            formatter::format(&device, &request.partition_ids, &options, |_, p| progress(p)).map_err(describe)?;
        }
        device.flush().map_err(describe)
    })();
    match result {
        Ok(()) => {
            r.log(format!("Formatted {}.", source.title));
            r.send(Msg::Reload(source.key));
        }
        Err(err) => r.send(Msg::Error(err)),
    }
}

// ---- backups ----

pub fn backup(r: &Reporter, source: SourceRow, disk: DiskInfo, request: BackupRequest) {
    r.progress(0.0, Some(format!("Backing up {}...", source.title)));
    let _awake = KeepAwake::new("Backing up a drive");
    let result = (|| -> Result<backup::BackupResult, Error> {
        let device = fx_platform::open_disk(&disk, false)?;
        backup::create(
            device.as_ref(),
            &request.path,
            &BackupOptions {
                verify: request.verify,
                sparse: true,
            },
            &mut r.copy_progress(),
            &r.cancel,
        )
    })();
    match result {
        Ok(done) => {
            r.send(Msg::Status(format!("Backup of {} is done.", source.title)));
            r.log(format!("Backed up {} to {}.", disk.display(), done.path.display()));
            r.send(Msg::Text {
                title: "Backup finished".into(),
                text: backup_summary(&disk, &done),
            });
        }
        Err(Error::Cancelled) => {
            r.send(Msg::Status("Backup stopped.".into()));
            r.log(format!(
                "Backup of {} stopped, the partly written file was deleted.",
                source.title
            ));
        }
        Err(err) => r.send(Msg::Error(err.to_string())),
    }
}

fn backup_summary(disk: &DiskInfo, result: &backup::BackupResult) -> String {
    let sums = backup::checksum_path(&result.path);
    let sums_name = sums
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "{}\n\nDrive: {}\nSize: {}, taking {} on disk (the empty parts of the drive aren't stored)\n{}\nSHA-256: {}\n\nA small file called {sums_name} was saved beside it. Keep the two together: Restore uses it to spot a damaged backup before anything is written to a drive.\n",
        result.path.display(),
        disk.display(),
        size::format(result.length),
        size::format(result.size_on_disk),
        if result.verified {
            "Checked: the file was read back and matches the drive."
        } else {
            "Checked: no, the file wasn't read back."
        },
        result.sha256
    )
}

/// Reads what the restore window needs to know about the drives it could write to.
pub fn prepare_restore(r: &Reporter, backup: Option<PathBuf>, in_use: std::collections::HashSet<String>) {
    r.progress(0.0, Some("Looking at the drives...".into()));
    let disks = fx_platform::list_disks();
    let mut sectors: HashMap<String, SecuritySector> = HashMap::new();
    // A drive that won't be read just has no answer.
    for disk in disks.iter().filter(|d| !d.is_system && !d.has_volumes) {
        if let Ok(device) = fx_platform::open_disk(disk, false)
            && let Ok(Some(sector)) = security::read(device.as_ref())
        {
            sectors.insert(disk.id.clone(), sector);
        }
    }
    r.send(Msg::Status("Ready.".into()));
    r.send(Msg::OpenRestore(RestoreInfo {
        disks,
        backup,
        in_use,
        sectors,
    }));
}

pub fn restore(r: &Reporter, request: RestoreRequest, loaded_key: Option<String>) {
    let _ = loaded_key;
    r.progress(0.0, Some(format!("Restoring to {}...", request.target.display())));
    let _awake = KeepAwake::new("Restoring a backup to a drive");
    let wrote = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&wrote);
    let result = (|| -> Result<backup::RestoreResult, String> {
        // the drive in the slot could have changed since the window looked at it
        safety::check_write(&request.target, false)?;
        let target = fx_platform::open_disk(&request.target, true).map_err(describe)?;
        if request.keep_header {
            let image = FileDevice::open(&request.backup, false).map_err(describe)?;
            if let Some(problem) = security::keep_problem_for(&image, target.as_ref()).map_err(describe)? {
                return Err(problem);
            }
        }
        let options = RestoreOptions {
            verify: request.verify,
            skip_first_bytes: if request.keep_header {
                security::HEADER_LENGTH
            } else {
                0
            },
            writing_started: Some(Box::new(move || flag.store(true, Ordering::Relaxed))),
        };
        backup::restore(
            &request.backup,
            target.as_ref(),
            options,
            &mut r.copy_progress(),
            &r.cancel,
        )
        .map_err(|e| match e {
            Error::Cancelled => "\u{0}cancelled".to_string(),
            other => other.to_string(),
        })
    })();

    let target = request.target.display();
    match result {
        Ok(done) => {
            r.send(Msg::Status(format!("{target} now matches the backup.")));
            let name = request
                .backup
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            r.log(format!(
                "Restored {name} to {target}{}{}",
                if done.skipped > 0 {
                    ", keeping the drive's own security sector"
                } else {
                    ""
                },
                if done.verified { ", read back and checked." } else { "." }
            ));
            if !done.checksum_file_used {
                r.log("There was no checksum file beside the backup, so the backup itself couldn't be checked for damage.");
            }
        }
        Err(text) if text == "\u{0}cancelled" => {
            r.send(Msg::Status("Restore stopped.".into()));
            if wrote.load(Ordering::Relaxed) {
                r.log(format!(
                    "Restore to {target} stopped part way. The drive holds a partial copy."
                ));
                r.send(Msg::Text {
                    title: "Restore stopped".into(),
                    text: "The drive was only partly written, so it won't work yet. Restore the backup to it again to finish the job.".into(),
                });
            } else {
                r.log(format!("Restore to {target} stopped before anything was written."));
            }
        }
        Err(text) => {
            if wrote.load(Ordering::Relaxed) {
                r.log(format!(
                    "{target} may hold a partial copy now, restore the backup to it again."
                ));
            }
            r.send(Msg::Error(text));
        }
    }
    // whatever happened, what the list knows about that drive is out of date now
    r.send(Msg::Reload("*".into()));
}

// ---- the driver ----

pub fn install_driver(r: &Reporter) {
    r.progress(0.0, Some("Installing the driver...".into()));
    let result = fx_platform::install_driver_package();
    match result {
        Ok(()) => {
            let installed = fx_mount::driver_installed();
            r.send(Msg::DriverInstalled {
                restart_needed: !installed,
            });
        }
        Err(err) => r.send(Msg::Error(err)),
    }
}

/// A short report of what a check found, in plain words.
pub trait ReportText {
    fn to_text(&self, name: &str) -> String;
}

impl ReportText for fx_core::fatx::FsckReport {
    fn to_text(&self, name: &str) -> String {
        let mut text = format!(
            "{name}: {} folders, {} files, {} of data.\n\n",
            self.directories,
            self.files,
            size::format(self.file_bytes)
        );
        if self.issues.is_empty() {
            text += "No problems found.\n";
        } else {
            for issue in &self.issues {
                let severity = match issue.severity {
                    fx_core::fatx::Severity::Info => "Info",
                    fx_core::fatx::Severity::Warning => "Warning",
                    fx_core::fatx::Severity::Error => "Error",
                };
                text += &format!("{severity}: {}\n", issue.message);
            }
            text += "\n";
            text += if self.errors() > 0 {
                "There is real damage. Copy your files somewhere safe before changing anything.\n"
            } else {
                "Nothing is lost, there is just some unused space that the command line tool can release (fsck --fix).\n"
            };
        }
        text
    }
}

pub fn mark_mounted(partitions: &mut [PartRow], part_id: &str, point: &str, read_only: bool) {
    if let Some(row) = partitions.iter_mut().find(|p| p.id() == part_id) {
        row.mount = Some(MountState {
            point: point.to_string(),
            read_only,
        });
    }
}
