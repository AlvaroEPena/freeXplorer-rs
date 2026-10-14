//! What the window knows and the rules it follows, with nothing about drawing in it so it can be
//! tested like any other code.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use fx_core::backup::{self, DestinationCheck};
use fx_core::device::FileDevice;
use fx_core::fatx::Flavor;
use fx_core::size;
use fx_core::xbox360::layout;
use fx_core::xbox360::scanner::{DetectedPartition, DriveKind, DriveScan};
use fx_core::xbox360::security::{self, SecuritySector};
use fx_platform::{DiskInfo, safety};

// ---- the lists ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Disk,
    Image,
}

/// A physical disk or a backup file in the list on the left.
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub key: String,
    pub kind: SourceKind,
    pub title: String,
    pub subtitle: String,
    pub note: Option<String>,
    pub disk: Option<DiskInfo>,
    pub image: Option<PathBuf>,
    pub loaded: Option<Loaded>,
}

impl SourceRow {
    pub fn from_disk(disk: &DiskInfo) -> SourceRow {
        let model = if disk.model.trim().is_empty() {
            "Unknown drive"
        } else {
            disk.model.as_str()
        };
        SourceRow {
            key: format!("disk:{}", disk.id),
            kind: SourceKind::Disk,
            title: format!("Disk {}", disk.id),
            subtitle: format!("{model}  ·  {}  ·  {}", size::format(disk.length), disk.bus),
            note: if disk.is_system {
                Some("This computer is running from this disk".into())
            } else if disk.has_volumes {
                Some("Has file systems mounted, probably a PC disk".into())
            } else {
                None
            },
            disk: Some(disk.clone()),
            image: None,
            loaded: None,
        }
    }

    pub fn from_image(path: &Path) -> SourceRow {
        let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let length = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
        SourceRow {
            key: full.to_string_lossy().into_owned(),
            kind: SourceKind::Image,
            title: full
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            subtitle: format!("Backup file  ·  {}", size::format(length)),
            note: None,
            disk: None,
            image: Some(full),
            loaded: None,
        }
    }
}

/// What reading a source found.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub scan: DriveScan,
    pub partitions: Vec<PartRow>,
}

#[derive(Debug, Clone)]
pub struct MountState {
    pub point: String,
    pub read_only: bool,
}

#[derive(Debug, Clone)]
pub struct PartRow {
    pub detected: DetectedPartition,
    pub format: String,
    pub mount: Option<MountState>,
}

impl PartRow {
    pub fn id(&self) -> &str {
        self.detected.id()
    }

    pub fn name(&self) -> &str {
        self.detected.name()
    }

    pub fn offset_text(&self) -> String {
        format!("0x{:X}", self.detected.offset())
    }

    pub fn size_text(&self) -> String {
        size::format(self.detected.length())
    }

    pub fn can_mount(&self) -> bool {
        self.detected.can_mount()
    }

    pub fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    pub fn status(&self) -> String {
        if let Some(m) = &self.mount {
            // a drive letter is short enough to show, a folder path isn't
            let place = if m.point.chars().count() <= 3 {
                format!("as {}", m.point)
            } else {
                "in a folder".to_string()
            };
            return format!(
                "Mounted {place} ({})",
                if m.read_only { "read-only" } else { "read/write" }
            );
        }
        if !self.detected.spec.is_fatx() {
            return "Cache, not browsable".into();
        }
        if !self.detected.has_signature {
            return "Not formatted".into();
        }
        "Ready to mount".into()
    }
}

pub fn drive_summary(title: &str, scan: &DriveScan, usable: usize) -> String {
    match scan.kind {
        DriveKind::Unknown => format!("{title}: no Xbox 360 partitions found. It may be blank, you can format it."),
        DriveKind::SinglePartition => format!("{title}: a single FATX partition."),
        DriveKind::Xbox360Drive => format!(
            "{title}: Xbox 360 drive, {usable} partition{} you can open.",
            if usable == 1 { "" } else { "s" }
        ),
    }
}

// ---- format ----

#[derive(Debug, Clone)]
pub struct FormatInfo {
    pub title: String,
    pub disk: Option<DiskInfo>,
    pub layout_fits: bool,
    pub scan: DriveScan,
}

#[derive(Debug, Clone)]
pub struct PartitionChoice {
    pub id: String,
    pub name: String,
    pub description: String,
    pub length: u64,
    pub selected: bool,
}

impl PartitionChoice {
    pub fn size_text(&self) -> String {
        size::format(self.length)
    }
}

pub const CLUSTER_CHOICES: [(&str, u32); 4] = [
    ("Automatic (16 KB)", 0),
    ("16 KB", 0x20),
    ("32 KB", 0x40),
    ("64 KB", 0x80),
];

#[derive(Debug, Clone)]
pub struct FormatRequest {
    pub whole_image: bool,
    pub partition_ids: Vec<String>,
    pub content_sectors_per_cluster: u32,
    pub content_length: u64,
    pub label: Option<String>,
    pub compat_flavor: Flavor,
}

/// The format window. All the rules live here.
#[derive(Debug, Clone)]
pub struct FormatForm {
    pub info: FormatInfo,
    pub whole_image: bool,
    pub choices: Vec<PartitionChoice>,
    pub compat_is_formatted: bool,
    pub compat_only: bool,
    pub label: String,
    pub limit_content: bool,
    pub content_gigabytes: String,
    pub cluster: usize,
    pub compat_original_style: bool,
    pub confirm: String,
}

impl FormatForm {
    pub fn new(info: FormatInfo) -> FormatForm {
        let mut choices = Vec::new();
        let mut compat_is_formatted = false;
        if info.layout_fits {
            for spec in layout::partitions(info.scan.visible_length)
                .into_iter()
                .filter(|p| p.is_fatx())
            {
                if spec.length < 0x100_0000 {
                    continue;
                }
                let detected = info.scan.partitions.iter().find(|p| p.id() == spec.id);
                if spec.id == layout::COMPAT_ID {
                    compat_is_formatted = detected.is_some_and(|d| d.can_mount());
                }
                // Content and the compatibility partition are what a drive is formatted for. The two system
                // partitions are only ticked when they aren't there yet, an existing one is best left alone.
                let selected = spec.id == layout::CONTENT_ID
                    || spec.id == layout::COMPAT_ID
                    || detected.is_none_or(|d| !d.can_mount());
                choices.push(PartitionChoice {
                    id: spec.id.to_string(),
                    name: spec.name.to_string(),
                    description: spec.description.to_string(),
                    length: spec.length,
                    selected,
                });
            }
        }
        FormatForm {
            whole_image: !info.layout_fits,
            info,
            choices,
            compat_is_formatted,
            compat_only: false,
            label: String::new(),
            limit_content: false,
            content_gigabytes: String::new(),
            cluster: 0,
            compat_original_style: false,
            confirm: String::new(),
        }
    }

    pub fn show_modes(&self) -> bool {
        !self.whole_image
    }

    pub fn show_choices(&self) -> bool {
        !self.whole_image && !self.compat_only
    }

    pub fn show_content_options(&self) -> bool {
        !self.compat_only
    }

    pub fn show_advanced(&self) -> bool {
        !self.whole_image
    }

    pub fn compat_note(&self) -> &'static str {
        if !self.compat_only {
            ""
        } else if self.compat_is_formatted {
            "This drive has an Xbox 1 Compatibility partition. It will be erased and made again."
        } else {
            "This drive has no Xbox 1 Compatibility partition yet. It will be created."
        }
    }

    pub fn heading(&self) -> String {
        match &self.info.disk {
            Some(d) => format!("Format {}", d.display()),
            None => format!("Format {}", self.info.title),
        }
    }

    /// what has to be typed before the Format button is enabled
    pub fn confirmation_phrase(&self) -> String {
        match &self.info.disk {
            Some(d) => safety::confirmation_phrase(d),
            None => "format".into(),
        }
    }

    pub fn warning(&self) -> &'static str {
        if self.whole_image {
            "The whole image will be erased and turned into one empty FATX volume."
        } else if self.compat_only {
            "Only the Xbox 1 Compatibility partition is touched. Everything else on the drive stays as it is, and so does its security sector."
        } else {
            "Everything on the partitions you tick will be erased. Other partitions are left alone, and so is the drive's security sector."
        }
    }

    pub fn label_problem(&self) -> Option<&'static str> {
        (!self.compat_only && self.label.chars().count() > 15).then_some("A label can be 15 characters at most.")
    }

    fn gigabytes(&self) -> Option<u64> {
        let gb: f64 = self.content_gigabytes.trim().parse().ok()?;
        (gb.is_finite() && gb > 0.0).then(|| (gb * (1u64 << 30) as f64) as u64 / 512 * 512)
    }

    pub fn content_size_problem(&self) -> Option<String> {
        if self.compat_only || !self.limit_content || self.content_gigabytes.trim().is_empty() {
            return None;
        }
        let Some(bytes) = self.gigabytes() else {
            return Some("Type a number of gigabytes.".into());
        };
        if let Some(content) = self.choices.iter().find(|c| c.id == layout::CONTENT_ID)
            && bytes > content.length
        {
            return Some(format!("That's more than the {} available.", content.size_text()));
        }
        if bytes < 0x100_0000 {
            return Some("That's too small.".into());
        }
        None
    }

    pub fn can_format(&self) -> bool {
        self.confirm.trim().eq_ignore_ascii_case(&self.confirmation_phrase())
            && self.label_problem().is_none()
            && self.content_size_problem().is_none()
            && (self.whole_image || self.compat_only || self.choices.iter().any(|c| c.selected))
            && (self.compat_only || !self.limit_content || !self.content_gigabytes.trim().is_empty())
    }

    pub fn build_request(&self) -> Option<FormatRequest> {
        if !self.can_format() {
            return None;
        }
        let flavor = if self.compat_original_style {
            Flavor::Xbox
        } else {
            Flavor::Xbox360
        };
        if self.compat_only {
            return Some(FormatRequest {
                whole_image: false,
                partition_ids: vec![layout::COMPAT_ID.to_string()],
                content_sectors_per_cluster: 0,
                content_length: 0,
                label: None,
                compat_flavor: flavor,
            });
        }
        let content_length = if self.limit_content {
            self.gigabytes().unwrap_or(0)
        } else {
            0
        };
        let label = self.label.trim();
        Some(FormatRequest {
            whole_image: self.whole_image,
            partition_ids: self
                .choices
                .iter()
                .filter(|c| c.selected)
                .map(|c| c.id.clone())
                .collect(),
            content_sectors_per_cluster: CLUSTER_CHOICES[self.cluster].1,
            content_length,
            label: (!label.is_empty()).then(|| label.to_string()),
            compat_flavor: flavor,
        })
    }
}

// ---- backup ----

#[derive(Debug, Clone)]
pub struct BackupRequest {
    pub path: PathBuf,
    pub verify: bool,
}

#[derive(Debug, Clone)]
pub struct BackupForm {
    pub disk: DiskInfo,
    pub path: String,
    pub verify: bool,
    checked: String,
    check: DestinationCheck,
}

impl BackupForm {
    pub fn new(disk: DiskInfo, start_folder: &Path, suggested_name: &str) -> BackupForm {
        let path = start_folder.join(suggested_name).to_string_lossy().into_owned();
        let check = check_destination(&path, disk.length);
        BackupForm {
            disk,
            checked: path.clone(),
            path,
            verify: true,
            check,
        }
    }

    /// Looks at the place again when the text was edited. Cheap, so it can be called every frame.
    pub fn refresh(&mut self) {
        if self.checked != self.path {
            self.checked = self.path.clone();
            self.check = check_destination(&self.path, self.disk.length);
        }
    }

    pub fn heading(&self) -> String {
        format!("Back up {}", self.disk.display())
    }

    pub fn problem(&self) -> Option<&str> {
        if self.path.trim().is_empty() {
            Some("Say where to save it.")
        } else {
            self.check.problem.as_deref()
        }
    }

    pub fn warning(&self) -> Option<&str> {
        self.check.warning.as_deref()
    }

    pub fn space_text(&self) -> String {
        if self.check.free_bytes > 0 {
            format!("{} free there.", size::format(self.check.free_bytes))
        } else {
            String::new()
        }
    }

    pub fn size_text(&self) -> String {
        format!(
            "The drive is {}. {}",
            size::format(self.disk.length),
            if self.check.sparse {
                "Empty parts of it aren't stored, so the file takes less room than that."
            } else {
                "The file takes all of that, this disk can't leave out the empty parts."
            }
        )
    }

    pub fn can_create(&self) -> bool {
        self.problem().is_none()
    }

    pub fn build_request(&self) -> Option<BackupRequest> {
        self.can_create().then(|| BackupRequest {
            path: std::path::absolute(&self.path).unwrap_or_else(|_| PathBuf::from(&self.path)),
            verify: self.verify,
        })
    }
}

pub fn check_destination(path: &str, length: u64) -> DestinationCheck {
    if path.trim().is_empty() {
        return DestinationCheck {
            problem: Some("Say where to save it.".into()),
            warning: None,
            free_bytes: 0,
            sparse: false,
        };
    }
    backup::check_destination(Path::new(path), length, fx_platform::destination_info)
}

pub fn suggested_backup_name(disk: &DiskInfo) -> String {
    let size_text = size::format(disk.length).replace(' ', "");
    format!(
        "Xbox 360 drive {size_text} {}.img",
        chrono::Local::now().format("%Y-%m-%d")
    )
}

// ---- restore ----

#[derive(Debug, Clone)]
pub struct RestoreInfo {
    pub disks: Vec<DiskInfo>,
    pub backup: Option<PathBuf>,
    /// keys of the drives that have something mounted from them
    pub in_use: HashSet<String>,
    pub sectors: HashMap<String, SecuritySector>,
}

#[derive(Debug, Clone)]
pub struct RestoreTarget {
    pub disk: DiskInfo,
    pub problem: Option<String>,
}

impl RestoreTarget {
    pub fn enabled(&self) -> bool {
        self.problem.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct RestoreRequest {
    pub backup: PathBuf,
    pub target: DiskInfo,
    pub verify: bool,
    pub keep_header: bool,
}

/// The window that asks which backup goes onto which drive. The disk can't be picked by accident:
/// the risky ones are greyed out, and the name has to be typed before the button works.
#[derive(Debug, Clone)]
pub struct RestoreForm {
    info: RestoreInfo,
    pub backup_path: String,
    looked_at: String,
    backup_length: u64,
    backup_visible: u64,
    backup_problem: Option<String>,
    pub verify: bool,
    keep_header: bool,
    pub targets: Vec<RestoreTarget>,
    pub selected: Option<usize>,
    pub confirm: String,
}

impl RestoreForm {
    pub fn new(info: RestoreInfo) -> RestoreForm {
        let path = info
            .backup
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut form = RestoreForm {
            info,
            backup_path: path,
            looked_at: String::from("\u{0}"),
            backup_length: 0,
            backup_visible: 0,
            backup_problem: None,
            verify: true,
            keep_header: false,
            targets: Vec::new(),
            selected: None,
            confirm: String::new(),
        };
        form.refresh();
        form
    }

    /// Looks at the backup again when its path was edited.
    pub fn refresh(&mut self) {
        if self.looked_at == self.backup_path {
            return;
        }
        self.looked_at = self.backup_path.clone();
        self.look_at_backup();
    }

    fn look_at_backup(&mut self) {
        self.backup_length = 0;
        self.backup_visible = 0;
        self.backup_problem = None;
        let path = self.backup_path.trim();
        if path.is_empty() {
            self.backup_problem = Some("Pick the backup file.".into());
        } else if !Path::new(path).is_file() {
            self.backup_problem = Some("That file doesn't exist.".into());
        } else {
            self.backup_length = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if self.backup_length == 0 {
                self.backup_problem = Some("That file is empty.".into());
            } else {
                self.backup_visible = visible_length_of(path);
            }
        }

        // the drives that can be used come first, the greyed out ones are only there so nobody wonders where a disk went
        let keep = self.selected.map(|i| self.targets[i].disk.id.clone());
        let mut targets: Vec<RestoreTarget> = self
            .info
            .disks
            .iter()
            .map(|d| RestoreTarget {
                disk: d.clone(),
                problem: self.problem_with(d),
            })
            .collect();
        targets.sort_by_key(|t| {
            (
                !t.enabled(),
                t.disk.id.parse::<u64>().unwrap_or(u64::MAX),
                t.disk.id.clone(),
            )
        });
        self.targets = targets;
        // keep the pick if it's still a good one
        self.selected = keep.and_then(|id| self.targets.iter().position(|t| t.disk.id == id && t.enabled()));
        self.fix_keep();
    }

    fn problem_with(&self, disk: &DiskInfo) -> Option<String> {
        if disk.is_system {
            return Some("This computer is running from this disk".into());
        }
        if disk.has_volumes {
            return Some("It has file systems mounted, so it looks like a PC disk".into());
        }
        if self.info.in_use.contains(&format!("disk:{}", disk.id)) {
            return Some("Partitions from it are mounted, unmount them first".into());
        }
        if self.backup_length > disk.length {
            return Some(format!("Too small, the backup is {}", size::format(self.backup_length)));
        }
        None
    }

    pub fn backup_problem(&self) -> Option<&str> {
        self.backup_problem.as_deref()
    }

    /// what is known about the backup file, shown under its name
    pub fn backup_note(&self) -> String {
        if self.backup_problem.is_some() {
            return String::new();
        }
        let length = size::format(self.backup_length);
        if backup::read_checksum(Path::new(self.backup_path.trim())).is_some() {
            format!(
                "{length}. Its checksum file was found, so it will be checked for damage before anything is written."
            )
        } else {
            format!("{length}. There's no checksum file beside it, so it can't be checked for damage first.")
        }
    }

    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index.filter(|&i| self.targets.get(i).is_some_and(RestoreTarget::enabled));
        self.fix_keep();
    }

    pub fn selected_target(&self) -> Option<&RestoreTarget> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    fn sector_of(&self, target: &RestoreTarget) -> Option<&SecuritySector> {
        self.info.sectors.get(&target.disk.id)
    }

    fn keep_problem(&self) -> Option<String> {
        let target = self.selected_target()?;
        security::keep_problem(self.backup_visible, self.sector_of(target))
    }

    /// Whether the drive can keep its own first 512 KB: it needs one, and the console has to see it at
    /// the same size as the drive the backup came from.
    pub fn can_keep_header(&self) -> bool {
        self.backup_problem.is_none() && self.selected_target().is_some() && self.keep_problem().is_none()
    }

    pub fn keep_header(&self) -> bool {
        self.keep_header && self.can_keep_header()
    }

    pub fn set_keep_header(&mut self, value: bool) {
        self.keep_header = value && self.can_keep_header();
    }

    fn fix_keep(&mut self) {
        if self.keep_header && !self.can_keep_header() {
            self.keep_header = false;
        }
    }

    pub fn keep_header_note(&self) -> String {
        let Some(target) = self.selected_target() else {
            return "Pick a drive to see whether it can keep its own.".into();
        };
        if self.backup_problem.is_some() {
            return String::new();
        }
        match self.keep_problem() {
            Some(problem) => problem,
            None => format!(
                "This drive has its own security sector (serial {}) and the console sees it at the same size as the original, so it can keep it.",
                self.sector_of(target).map(|s| s.serial.as_str()).unwrap_or("")
            ),
        }
    }

    pub fn confirmation_phrase(&self) -> String {
        match self.selected_target() {
            Some(t) => safety::confirmation_phrase(&t.disk),
            None => "the disk name".into(),
        }
    }

    pub fn warning(&self) -> String {
        let what = match self.selected_target() {
            Some(t) => t.disk.display(),
            None => "the drive you pick".into(),
        };
        if self.keep_header() {
            format!(
                "Everything on {what} except its own security sector (the first 512 KB) will be overwritten with the backup. That can't be undone."
            )
        } else {
            format!("Everything on {what} will be overwritten with the backup. That can't be undone.")
        }
    }

    pub fn can_restore(&self) -> bool {
        self.backup_problem.is_none()
            && self
                .selected_target()
                .is_some_and(|t| t.enabled() && safety::is_confirmed(&t.disk, Some(&self.confirm)))
    }

    pub fn build_request(&self) -> Option<RestoreRequest> {
        let target = self.selected_target()?;
        self.can_restore().then(|| RestoreRequest {
            backup: PathBuf::from(self.backup_path.trim()),
            target: target.disk.clone(),
            verify: self.verify,
            keep_header: self.keep_header(),
        })
    }
}

/// how big the console would see the drive this backup came from, which its security sector says
fn visible_length_of(path: &str) -> u64 {
    FileDevice::open(path, false)
        .ok()
        .and_then(|image| layout::visible_length(&image).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fx_core::device::MemoryDevice;
    use fx_core::xbox360::scanner;

    fn disk(id: &str, length: u64, system: bool, volumes: bool) -> DiskInfo {
        DiskInfo {
            id: id.into(),
            path: String::new(),
            length,
            sector_size: 512,
            model: "Test".into(),
            serial: String::new(),
            bus: "USB".into(),
            removable: true,
            has_volumes: volumes,
            is_system: system,
        }
    }

    fn console_scan(length: u64) -> DriveScan {
        let device = MemoryDevice::new(length);
        let device: fx_core::Device = std::sync::Arc::new(device);
        fx_core::xbox360::formatter::format(
            &device,
            &["sysext", "sysext2", "compat", "content"],
            &Default::default(),
            |_, _| {},
        )
        .unwrap();
        scanner::scan(device.as_ref()).unwrap()
    }

    fn format_form(disk: Option<DiskInfo>) -> FormatForm {
        FormatForm::new(FormatInfo {
            title: "Disk 3".into(),
            disk,
            layout_fits: true,
            scan: console_scan(6 << 30),
        })
    }

    #[test]
    fn a_disk_row_says_why_it_is_risky() {
        assert_eq!(
            SourceRow::from_disk(&disk("1", 1 << 30, true, true)).note.as_deref(),
            Some("This computer is running from this disk")
        );
        assert!(SourceRow::from_disk(&disk("2", 1 << 30, false, true)).note.is_some());
        assert!(SourceRow::from_disk(&disk("4", 1 << 30, false, false)).note.is_none());
    }

    #[test]
    fn partition_status_follows_what_is_there() {
        let scan = console_scan(6 << 30);
        let row = |id: &str| PartRow {
            detected: scan.partitions.iter().find(|p| p.id() == id).unwrap().clone(),
            format: String::new(),
            mount: None,
        };
        assert_eq!(row("syscache").status(), "Cache, not browsable");
        assert_eq!(row("content").status(), "Ready to mount");
        let mut mounted = row("content");
        mounted.mount = Some(MountState {
            point: "X:".into(),
            read_only: true,
        });
        assert_eq!(mounted.status(), "Mounted as X: (read-only)");
        mounted.mount = Some(MountState {
            point: "/mnt/xbox".into(),
            read_only: false,
        });
        assert_eq!(mounted.status(), "Mounted in a folder (read/write)");
    }

    #[test]
    fn format_needs_the_disk_to_be_typed() {
        let mut form = format_form(Some(disk("3", 6 << 30, false, false)));
        assert!(!form.can_format());
        assert_eq!(form.confirmation_phrase(), "disk 3");
        form.confirm = " Disk 3 ".into();
        assert!(form.can_format());
        assert!(form.build_request().is_some());
        form.confirm = "disk 4".into();
        assert!(!form.can_format());
    }

    #[test]
    fn an_image_only_needs_the_word_format() {
        let mut form = format_form(None);
        form.confirm = "format".into();
        assert!(form.can_format());
    }

    #[test]
    fn existing_system_partitions_are_left_unticked() {
        let form = format_form(None);
        let ticked: Vec<&str> = form
            .choices
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.id.as_str())
            .collect();
        // all four exist on this drive, so only content and compat are on
        assert_eq!(ticked, ["compat", "content"]);
        assert!(form.compat_is_formatted);
    }

    #[test]
    fn labels_and_sizes_are_checked() {
        let mut form = format_form(None);
        form.confirm = "format".into();
        form.label = "x".repeat(16);
        assert!(form.label_problem().is_some() && !form.can_format());
        form.label = "Games".into();
        assert!(form.can_format());

        form.limit_content = true;
        assert!(!form.can_format(), "a limit with no size isn't a choice yet");
        form.content_gigabytes = "abc".into();
        assert_eq!(
            form.content_size_problem().as_deref(),
            Some("Type a number of gigabytes.")
        );
        form.content_gigabytes = "0.001".into();
        assert_eq!(form.content_size_problem().as_deref(), Some("That's too small."));
        form.content_gigabytes = "9999".into();
        assert!(form.content_size_problem().unwrap().starts_with("That's more than"));
        form.content_gigabytes = "1".into();
        assert!(form.can_format());
        assert_eq!(form.build_request().unwrap().content_length, 1 << 30);
    }

    #[test]
    fn compat_only_ignores_the_rest() {
        let mut form = format_form(None);
        form.confirm = "format".into();
        form.compat_only = true;
        form.label = "x".repeat(40);
        let request = form.build_request().unwrap();
        assert_eq!(request.partition_ids, ["compat"]);
        assert!(request.label.is_none());
        assert!(form.compat_note().contains("erased and made again"));
        form.compat_original_style = true;
        assert_eq!(form.build_request().unwrap().compat_flavor, Flavor::Xbox);
    }

    #[test]
    fn an_image_that_is_too_small_for_the_layout_is_formatted_whole() {
        let form = FormatForm::new(FormatInfo {
            title: "tiny.img".into(),
            disk: None,
            layout_fits: false,
            scan: scanner::scan(&MemoryDevice::new(64 << 20)).unwrap(),
        });
        assert!(form.whole_image && !form.show_modes() && form.choices.is_empty());
    }

    #[test]
    fn the_backup_form_asks_for_a_place() {
        let dir = tempfile::tempdir().unwrap();
        let mut form = BackupForm::new(disk("4", 8 << 20, false, false), dir.path(), "a.img");
        assert!(form.can_create());
        form.path = String::new();
        form.refresh();
        assert_eq!(form.problem(), Some("Say where to save it."));
        form.path = dir
            .path()
            .join("no-such-folder")
            .join("a.img")
            .to_string_lossy()
            .into_owned();
        form.refresh();
        assert_eq!(form.problem(), Some("That folder doesn't exist."));
        std::fs::write(dir.path().join("taken.img"), b"x").unwrap();
        form.path = dir.path().join("taken.img").to_string_lossy().into_owned();
        form.refresh();
        assert!(!form.can_create());
        assert!(form.build_request().is_none());
    }

    fn restore_info(dir: &Path, disks: Vec<DiskInfo>) -> (RestoreInfo, PathBuf) {
        let backup = dir.join("b.img");
        std::fs::write(&backup, vec![1u8; 4 << 20]).unwrap();
        (
            RestoreInfo {
                disks,
                backup: Some(backup.clone()),
                in_use: ["disk:6".to_string()].into(),
                sectors: HashMap::new(),
            },
            backup,
        )
    }

    #[test]
    fn restore_greys_out_the_risky_disks_and_lists_good_ones_first() {
        let dir = tempfile::tempdir().unwrap();
        let (info, _) = restore_info(
            dir.path(),
            vec![
                disk("0", 100 << 20, false, true),
                disk("1", 100 << 20, true, true),
                disk("5", 100 << 20, false, false),
                disk("6", 100 << 20, false, false),
                disk("7", 1 << 20, false, false),
            ],
        );
        let form = RestoreForm::new(info);
        assert_eq!(form.targets[0].disk.id, "5");
        assert!(form.targets[1..].iter().all(|t| !t.enabled()));
        let problem = |id: &str| {
            form.targets
                .iter()
                .find(|t| t.disk.id == id)
                .unwrap()
                .problem
                .clone()
                .unwrap()
        };
        assert!(problem("1").contains("running from"));
        assert!(problem("0").contains("file systems mounted"));
        assert!(problem("6").contains("unmount"));
        assert!(problem("7").contains("Too small"));
    }

    #[test]
    fn restore_needs_the_disk_typed_and_ignores_greyed_picks() {
        let dir = tempfile::tempdir().unwrap();
        let (info, backup) = restore_info(
            dir.path(),
            vec![disk("5", 100 << 20, false, false), disk("1", 100 << 20, true, true)],
        );
        let mut form = RestoreForm::new(info);
        let bad = form.targets.iter().position(|t| t.disk.id == "1").unwrap();
        form.select(Some(bad));
        assert!(form.selected_target().is_none());

        form.select(Some(0));
        assert!(!form.can_restore());
        form.confirm = "disk 5".into();
        assert!(form.can_restore());
        let request = form.build_request().unwrap();
        assert_eq!(request.backup, backup);
        assert!(!request.keep_header);
    }

    #[test]
    fn restore_without_a_backup_is_not_possible() {
        let dir = tempfile::tempdir().unwrap();
        let (mut info, _) = restore_info(dir.path(), vec![disk("5", 100 << 20, false, false)]);
        info.backup = None;
        let mut form = RestoreForm::new(info);
        assert_eq!(form.backup_problem(), Some("Pick the backup file."));
        form.select(Some(0));
        form.confirm = "disk 5".into();
        assert!(!form.can_restore());
        form.backup_path = "nothing-here.img".into();
        form.refresh();
        assert_eq!(form.backup_problem(), Some("That file doesn't exist."));
    }

    #[test]
    fn keeping_the_header_needs_a_matching_security_sector() {
        let dir = tempfile::tempdir().unwrap();
        let (mut info, _) = restore_info(dir.path(), vec![disk("5", 100 << 20, false, false)]);
        let mut form = RestoreForm::new(info.clone());
        form.select(Some(0));
        form.set_keep_header(true);
        assert!(!form.keep_header(), "no security sector on the drive, nothing to keep");

        // a drive whose console size matches the backup's visible length (no claim in the backup = whole file)
        info.sectors.insert(
            "5".into(),
            SecuritySector {
                serial: "WD-TEST".into(),
                claimed_length: 4 << 20,
            },
        );
        let mut form = RestoreForm::new(info);
        form.select(Some(0));
        assert!(form.can_keep_header());
        form.set_keep_header(true);
        assert!(form.keep_header());
        assert!(form.build_request().is_none());
        form.confirm = "disk 5".into();
        assert!(form.build_request().unwrap().keep_header);
    }
}
