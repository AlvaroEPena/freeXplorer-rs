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
use fx_core::fatx::{self, FormatOptions, Severity};
use fx_core::og;
use fx_core::size;
use fx_core::xbox360::formatter::{self, HddFormatOptions};
use fx_core::xbox360::scanner::{self, DetectedPartition, DriveKind};
use fx_core::xbox360::security::{self, SecuritySector};
use fx_platform::{DiskInfo, KeepAwake, safety};

use crate::model::{
    BackupRequest, CheckState, FormatRequest, Loaded, MountState, PartRow, RestoreInfo, RestoreRequest, SourceKind,
    SourceRow, drive_summary,
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
    /// one partition's read-only check finished, from the background pass or the Check button
    Checked {
        key: String,
        part_id: String,
        result: Result<CheckState, String>,
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
    /// a block of text for the log file only. The panel keeps the one-line verdict; the file keeps
    /// the whole report, which is what a bug report needs and what a closed dialog no longer has.
    LogDetail(String),
    /// a background pass of the kind that never takes the job slot has finished, so the drive is
    /// free to be opened again
    SideDone,
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

/// Runs a pass that must not take the window's job slot, so the drive stays usable while it works.
///
/// It reports `SideDone` rather than `Done`: that is what tells the window the drive has been let
/// go of, and mounting and checking both want the device to themselves, so anything queued behind
/// this pass waits for it — including when the pass panics.
pub fn spawn_side(reporter: &Reporter, work: impl FnOnce(&Reporter) + Send + 'static) {
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
        reporter.send(Msg::SideDone);
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
        let has_table = scan.kind == DriveKind::OgDrive
            && og::mbr::read(device.as_ref())
                .map_err(describe)?
                .is_some();
        let partitions = scan
            .partitions
            .iter()
            .map(|p| describe_partition(&device, p))
            .collect::<Vec<_>>();
        Ok(Loaded {
            scan,
            partitions,
            has_table,
        })
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

/// Checks every partition in turn on a thread of its own, reporting each one as it finishes.
///
/// This is deliberately not a `run` job: the drive stays usable while it works, the list fills in
/// as answers arrive, and a slow partition no longer holds up the window. It is one thread and one
/// device handle rather than one per partition — the checks are reads from the same disk, so they
/// would queue on the drive anyway, and the notes in AGENTS.md are clear about what two readers on
/// one marginal drive cost.
pub fn check_partitions(r: &Reporter, source: SourceRow, parts: Vec<DetectedPartition>) {
    let device = match open(&source, false) {
        Ok(device) => device,
        Err(err) => {
            for part in parts {
                r.send(Msg::Checked {
                    key: source.key.clone(),
                    part_id: part.id().to_string(),
                    result: Err(err.clone()),
                });
            }
            return;
        }
    };
    for part in parts {
        if r.cancel.is_cancelled() {
            return;
        }
        let result = scanner::open_partition(&device, &part, false)
            .and_then(|mut volume| volume.check_with(false, &r.cancel, |_| {}))
            .map(|report| state_of(&report))
            .map_err(describe);
        if r.cancel.is_cancelled() {
            // the user wants the drive; a half-finished walk has nothing worth reporting
            return;
        }
        r.send(Msg::Checked {
            key: source.key.clone(),
            part_id: part.id().to_string(),
            result,
        });
    }
}

fn describe_partition(device: &Device, p: &DetectedPartition) -> PartRow {
    let mut format = String::new();
    if p.can_mount() {
        match scanner::open_partition(device, p, false) {
            Ok(volume) => {
                let kind = if volume.flavor() == fatx::Flavor::Xbox360 {
                    "XTAF"
                } else {
                    "FATX"
                };
                format = format!(
                    "{kind}, {} KB clusters, FAT{}",
                    volume.cluster_size() / 1024,
                    volume.geometry().fat_entry_size * 8
                );
            }
            Err(err) => format = format!("Looks damaged: {err}"),
        }
    }
    PartRow {
        detected: p.clone(),
        format,
        mount: None,
        check: None,
        checking: false,
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
    let result = (|| -> Result<(String, CheckState), String> {
        let device = open(&source, false)?;
        let mut volume = scanner::open_partition(&device, &part, false).map_err(describe)?;
        let report = volume.check_with(false, &r.cancel, |_| {}).map_err(describe)?;
        Ok((report.to_text(part.name()), state_of(&report)))
    })();
    match result {
        Ok((text, state)) => {
            // the button's answer belongs on the row as well as in the dialog, so the list keeps it
            r.send(Msg::Checked {
                key: source.key.clone(),
                part_id: part.id().to_string(),
                result: Ok(state),
            });
            r.send(Msg::Text {
                title: format!("Check of {}", part.name()),
                text,
            });
            r.send(Msg::Status(format!("Checked {}.", part.name())));
            r.log(format!("Checked {}.", part.name()));
        }
        Err(err) => {
            r.send(Msg::Checked {
                key: source.key.clone(),
                part_id: part.id().to_string(),
                result: Err(err.clone()),
            });
            r.send(Msg::Error(err));
        }
    }
}

/// What a check found, in the shape the partition list keeps. Both the background pass and the
/// Check button go through this, so a row says the same thing whichever of them filled it in.
fn state_of(report: &fx_core::fatx::FsckReport) -> CheckState {
    let blocked = report.incomplete;
    CheckState {
        errors: report.errors(),
        warnings: report.warnings(),
        reclaimable_bytes: report.orphan_clusters * report.bytes_per_cluster,
        blocked,
        reason: if blocked {
            report
                .issues
                .iter()
                .find(|i| i.severity == Severity::Error)
                .map(|i| i.message.clone())
                .unwrap_or_else(|| "the volume couldn't be read completely".to_string())
        } else {
            String::new()
        },
    }
}

/// Releases clusters the FAT calls used that no file owns. The volume refuses by itself when it
/// couldn't be read completely, so a click here can't cut a file in half.
pub fn fix(r: &Reporter, source: SourceRow, part: DetectedPartition) {
    r.progress(0.0, Some(format!("Fixing {}...", part.name())));
    let result = (|| -> Result<(String, String), String> {
        let device = open(&source, true)?;
        let mut volume = scanner::open_partition(&device, &part, true).map_err(describe)?;
        let report = volume.check(true, |_| {}).map_err(describe)?;
        volume.flush().map_err(describe)?;
        // read it back rather than trusting the fix: this is the figure the console will see
        let after = volume.check(false, |_| {}).map_err(describe)?;

        // one line saying what actually happened. "Fixed F." over a fix that released nothing is
        // exactly how a no-op came to look like a repair.
        let verdict = if report.fixed_clusters > 0 {
            format!(
                "Fixed {}: released {}, {} clusters.",
                part.name(),
                size::format(report.released_bytes()),
                report.fixed_clusters
            )
        } else if !report.fixable() {
            format!(
                "{} wasn't fixed: the volume couldn't be read completely, so nothing was released.",
                part.name()
            )
        } else if report.orphan_clusters > 0 {
            format!("{} wasn't fixed: nothing was released.", part.name())
        } else {
            format!(
                "{} needed no fix: nothing was marked used without a file owning it.",
                part.name()
            )
        };

        let mut text = report.to_text(part.name());
        if report.fixed_clusters > 0 {
            text += &format!(
                "\nReleased {} clusters, {} of space.\n",
                report.fixed_clusters,
                size::format(report.released_bytes())
            );
        } else if !report.fixable() {
            text += "\nNothing was released. This volume needs a reformat, not a fix.\n";
        } else if report.orphan_clusters > 0 {
            text += "\nNothing was released.\n";
        }
        text += &format!(
            "In use now: {}, free: {} of {}.\n",
            size::format(after.used_bytes()),
            size::format(after.free_bytes()),
            size::format(after.used_bytes() + after.free_bytes())
        );
        let verdict_after = if after.is_clean() {
            "Checked again afterwards: no problems found.\n".to_string()
        } else {
            format!(
                "Checked again afterwards: {} errors and {} warnings remain.\n",
                after.errors(),
                after.warnings()
            )
        };
        text += &verdict_after;
        Ok((text, verdict))
    })();
    match result {
        Ok((text, verdict)) => {
            // the panel gets the one-line answer; the log file gets the whole report as well, which
            // is what a bug report needs and what the dialog is too easy to close without
            r.log(&verdict);
            r.send(Msg::LogDetail(text.clone()));
            r.send(Msg::Status(verdict));
            r.send(Msg::Text {
                title: format!("Fix of {}", part.name()),
                text,
            });
            r.send(Msg::Reload(source.key.clone()));
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
        match request {
            FormatRequest::Xbox360 {
                whole_image,
                partition_ids,
                content_sectors_per_cluster,
                content_length,
                label,
                compat_flavor,
            } => {
                refuse_an_og_drive(&device)?;
                if whole_image {
                    let length = device.len() / 512 * 512;
                    let options = FormatOptions {
                        sectors_per_cluster: content_sectors_per_cluster,
                        label,
                        ..FormatOptions::default()
                    };
                    fatx::format(&device, 0, length, &options, progress).map_err(describe)?;
                } else {
                    let options = HddFormatOptions {
                        content_sectors_per_cluster,
                        content_length,
                        content_label: label,
                        compat_flavor,
                    };
                    formatter::format(&device, &partition_ids, &options, |_, p| progress(p))
                        .map_err(describe)?;
                }
            }
            FormatRequest::Og {
                relayout,
                partition_ids,
                layout,
                sectors_per_cluster,
                label,
            } => format_og(
                &device,
                relayout,
                &partition_ids,
                layout,
                sectors_per_cluster,
                label,
                &progress,
            )?,
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

/// The one thing the 360 formatter must never be pointed at: an original Xbox drive.
///
/// Its layout is compiled in and it writes XTAF volumes at 360 offsets without ever touching
/// sector zero. On an original Xbox drive that leaves the table in place and puts a system cache
/// across X, Y and part of Z, so the drive comes back as a hybrid that is neither console's.
fn refuse_an_og_drive(device: &Device) -> Result<(), String> {
    let scan = scanner::scan(device.as_ref()).map_err(describe)?;
    if scan.kind == DriveKind::OgDrive {
        return Err("This is an original Xbox drive, whose partitions are the drive's own. Lay it \
                    out as an original Xbox drive instead."
            .into());
    }
    Ok(())
}

/// Writes an original Xbox drive: the table and every partition, or just the partitions that were
/// ticked, and then reads each one back before calling it done.
#[allow(clippy::too_many_arguments)]
fn format_og(
    device: &Device,
    relayout: bool,
    partition_ids: &[String],
    layout: og::Layout,
    sectors_per_cluster: u32,
    label: Option<String>,
    progress: &dyn Fn(f64),
) -> Result<(), String> {
    let scan = scanner::scan(device.as_ref()).map_err(describe)?;
    if relayout {
        // a blank drive is the point of laying one out; a drive that already belongs to the other
        // console is not something to convert by accident
        if matches!(
            scan.kind,
            DriveKind::Xbox360Drive | DriveKind::SinglePartition
        ) {
            return Err("This drive already has an Xbox 360 layout on it, so it can't be laid out \
                        as an original Xbox drive from here."
                .into());
        }
    } else if scan.kind != DriveKind::OgDrive {
        return Err(
            "This drive isn't an original Xbox drive, so there's no layout to reformat in place."
                .into(),
        );
    }
    let options = og::Options {
        layout,
        sectors_per_cluster,
        label,
    };
    let steps = if relayout {
        og::steps(device.len(), &options)
    } else {
        if og::mbr::read(device.as_ref()).map_err(describe)?.is_none() {
            return Err("This drive has no original Xbox partition table, so there's nothing to \
                        reformat in place. Lay it out again instead."
                .into());
        }
        let mut specs = Vec::with_capacity(partition_ids.len());
        for id in partition_ids {
            let part = scan
                .partitions
                .iter()
                .find(|p| p.id() == id)
                .ok_or_else(|| format!("this drive has no partition \"{id}\""))?;
            specs.push(part.spec.clone());
        }
        og::steps_for(&specs, &options)
    }
    .map_err(describe)?;

    if relayout {
        og::drive(device, &steps, &options, |_, p| progress(p)).map_err(describe)?;
    } else {
        og::volumes(device, &steps, &options, |_, p| progress(p)).map_err(describe)?;
    }
    device.flush().map_err(describe)?;

    // the house standard: a format isn't finished until what it wrote reads back clean
    og::verify(device, &steps, &options).map_err(describe)
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
            } else if self.fixed_clusters > 0 {
                "There was no damage: the leftover space has been released.\n"
            } else {
                "There is no damage. The leftover space is marked used by nothing, and Fix releases it.\n"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SourceRow;
    use fx_core::device::FileDevice;
    use fx_core::fatx::Flavor;
    use std::sync::mpsc;

    /// A blank image with an original Xbox layout already written to it, and the source row the
    /// window would have built for it.
    fn og_image(length: u64) -> (tempfile::TempDir, std::path::PathBuf, SourceRow) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drive.img");
        FileDevice::create_image(&path, length, true).unwrap();
        let device: Device = Arc::new(FileDevice::open(&path, true).unwrap());
        let options = og::Options::default();
        let steps = og::steps(device.len(), &options).unwrap();
        og::drive(&device, &steps, &options, |_, _| {}).unwrap();
        let source = SourceRow::from_image(&path);
        (dir, path, source)
    }

    fn reporter() -> (Reporter, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel();
        (
            Reporter::new(tx, eframe::egui::Context::default(), Cancel::new()),
            rx,
        )
    }

    fn said(rx: &mpsc::Receiver<Msg>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Error(text) => out.push(format!("error: {text}")),
                Msg::Log(text) => out.push(text),
                _ => {}
            }
        }
        out
    }

    /// The 360 formatter never touches sector zero, so pointing it at an original Xbox drive would
    /// leave the table in place and write a system cache across X, Y and part of Z.
    #[test]
    fn a_360_format_refuses_an_original_xbox_drive() {
        let (_dir, _path, source) = og_image(10_000_000_000);
        let (r, rx) = reporter();
        format(
            &r,
            source,
            FormatRequest::Xbox360 {
                whole_image: false,
                partition_ids: vec!["content".into()],
                content_sectors_per_cluster: 0,
                content_length: 0,
                label: None,
                compat_flavor: Flavor::Xbox360,
            },
        );
        let said = said(&rx);
        assert!(
            said.iter().any(|line| line.contains("original Xbox drive")),
            "{said:?}"
        );
    }

    /// Formatting a blank drive as new is the other half of what the window is for.
    #[test]
    fn a_blank_image_can_be_laid_out_as_an_original_xbox() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blank.img");
        FileDevice::create_image(&path, 10_000_000_000, true).unwrap();
        let source = SourceRow::from_image(&path);
        let (r, rx) = reporter();
        format(
            &r,
            source,
            FormatRequest::Og {
                relayout: true,
                partition_ids: Vec::new(),
                layout: og::Layout::Auto,
                sectors_per_cluster: 0,
                label: Some("XBOX DATA".into()),
            },
        );
        let said = said(&rx);
        assert!(
            !said.iter().any(|line| line.starts_with("error:")),
            "{said:?}"
        );

        let device: Device = Arc::new(FileDevice::open(&path, false).unwrap());
        let scan = scanner::scan(device.as_ref()).unwrap();
        assert_eq!(scan.kind, DriveKind::OgDrive);
        let e = scan.partitions.iter().find(|p| p.id() == "e").unwrap();
        assert!(e.can_mount(), "E was formatted but doesn't read back");
    }

    #[test]
    fn reformatting_one_partition_leaves_the_table_and_the_rest_alone() {
        let (_dir, path, source) = og_image(10_000_000_000);
        let (r, rx) = reporter();
        format(
            &r,
            source,
            FormatRequest::Og {
                relayout: false,
                partition_ids: vec!["f".into()],
                layout: og::Layout::Auto,
                sectors_per_cluster: 0,
                label: None,
            },
        );
        let said = said(&rx);
        assert!(!said.iter().any(|line| line.starts_with("error:")), "{said:?}");

        let device: Device = Arc::new(FileDevice::open(&path, false).unwrap());
        let scan = scanner::scan(device.as_ref()).unwrap();
        assert_eq!(scan.kind, DriveKind::OgDrive);
        let f = scan.partitions.iter().find(|p| p.id() == "f").unwrap();
        assert!(f.can_mount(), "F was formatted but doesn't read back");
    }
}
