use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use fx_core::copy::Cancel;
use fx_core::xbox360::layout;
use fx_platform::{DiskInfo, safety};

use crate::jobs::{self, Msg, Reporter};
use crate::model::{
    BackupForm, FormatForm, FormatInfo, Loaded, MountState, PartRow, RestoreForm, SourceKind, SourceRow,
    suggested_backup_name,
};
use crate::theme::{self, Palette};
use crate::{settings, ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
    Missing,
    Installed,
    Installing,
    RestartNeeded,
}

/// Something to do once the current job has finished.
enum Next {
    Load(String),
    Refresh,
}

/// What a yes to a question leads to.
#[derive(Clone)]
pub enum Then {
    MountWriting { key: String, part_id: String },
    InstallDriver,
    Quit,
    StopJob,
}

pub enum Modal {
    Confirm { title: String, message: String, then: Then },
    Error(String),
    Text { title: String, text: String },
    Format { form: Box<FormatForm>, key: String },
    Backup { form: Box<BackupForm>, key: String },
    Restore(Box<RestoreForm>),
}

struct Running {
    cancel: Cancel,
    can_cancel: bool,
    stop_warning: Option<&'static str>,
    stopping: bool,
}

pub struct App {
    egui: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    pub sources: Vec<SourceRow>,
    pub selected: Option<String>,
    pub selected_part: Option<String>,
    mounts: HashMap<(String, String), fx_mount::Mounted>,
    pub allow_writing: bool,

    job: Option<Running>,
    pub progress: f64,
    pub status: String,
    pub log: VecDeque<String>,
    pub driver_version: Option<String>,
    driver_installing: bool,
    driver_restart: bool,
    pub modal: Option<Modal>,
    pub dark: bool,
    pub palette: Palette,

    queue: VecDeque<Next>,
    last_poll: Instant,
    polling: bool,
    closing_ok: bool,
    close_when_done: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let dark = settings::saved_dark().unwrap_or_else(settings::system_prefers_dark);
        theme::apply(&cc.egui_ctx, dark);
        let (tx, rx) = mpsc::channel();
        let mut app = App {
            egui: cc.egui_ctx.clone(),
            tx,
            rx,
            sources: Vec::new(),
            selected: None,
            selected_part: None,
            mounts: HashMap::new(),
            allow_writing: false,
            job: None,
            progress: 0.0,
            status: "Pick a drive on the left, or load a backup.".into(),
            log: VecDeque::new(),
            driver_version: None,
            driver_installing: false,
            driver_restart: false,
            modal: None,
            dark,
            palette: theme::palette(dark),
            queue: VecDeque::new(),
            last_poll: Instant::now(),
            polling: false,
            closing_ok: false,
            close_when_done: false,
        };
        app.refresh_driver();
        app.refresh();
        // backup files named on the command line are opened like ones loaded from the button
        for arg in std::env::args().skip(1).filter(|a| !a.starts_with("--")) {
            let path = PathBuf::from(arg);
            if path.is_file() {
                app.add_backup_source(&path);
            }
        }
        app
    }

    // ---- small things the drawing code asks ----

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn can_cancel(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.can_cancel && !j.stopping)
    }

    pub fn driver_state(&self) -> DriverState {
        if self.driver_installing {
            DriverState::Installing
        } else if self.driver_restart {
            DriverState::RestartNeeded
        } else if self.driver_version.is_none() {
            DriverState::Missing
        } else {
            DriverState::Installed
        }
    }

    pub fn driver_text(&self) -> String {
        match self.driver_state() {
            DriverState::Installed => "Driver installed".into(),
            DriverState::Installing => "Installing driver...".into(),
            DriverState::RestartNeeded => "Restart needed".into(),
            DriverState::Missing => "Install driver".into(),
        }
    }

    pub fn driver_detail(&self) -> String {
        let name = fx_mount::DRIVER_NAME;
        match self.driver_state() {
            DriverState::Installed => format!(
                "{name} {} is installed. It's the free driver that lets this computer show an Xbox 360 partition as a drive.",
                self.driver_version.as_deref().unwrap_or("")
            ),
            DriverState::Installing => format!("{name} is being installed."),
            DriverState::RestartNeeded => {
                format!("{name} is installed, but the computer needs a restart before it will work.")
            }
            DriverState::Missing => format!(
                "The {name} driver isn't installed. It's the free driver that lets this computer show an Xbox 360 partition as a drive, and mounting needs it. Click to install it."
            ),
        }
    }

    pub fn selected_source(&self) -> Option<&SourceRow> {
        let key = self.selected.as_ref()?;
        self.sources.iter().find(|s| &s.key == key)
    }

    pub fn partitions(&self) -> &[PartRow] {
        self.selected_source()
            .and_then(|s| s.loaded.as_ref())
            .map(|l| l.partitions.as_slice())
            .unwrap_or(&[])
    }

    pub fn selected_partition(&self) -> Option<&PartRow> {
        let id = self.selected_part.as_ref()?;
        self.partitions().iter().find(|p| p.id() == id)
    }

    fn mounted_from(&self, key: &str) -> bool {
        self.mounts.keys().any(|(k, _)| k == key)
    }

    // ---- jobs ----

    fn reporter(&self, cancel: &Cancel) -> Reporter {
        Reporter::new(self.tx.clone(), self.egui.clone(), cancel.clone())
    }

    /// Starts a job, unless one is running. The window leaves everything else alone while it does.
    fn run(
        &mut self,
        cancellable: bool,
        stop_warning: Option<&'static str>,
        work: impl FnOnce(&Reporter) + Send + 'static,
    ) {
        if self.job.is_some() {
            return;
        }
        let cancel = Cancel::new();
        let reporter = self.reporter(&cancel);
        self.job = Some(Running {
            cancel,
            can_cancel: cancellable,
            stop_warning,
            stopping: false,
        });
        self.progress = 0.0;
        jobs::spawn(&reporter, work);
    }

    pub fn refresh(&mut self) {
        self.refresh_driver();
        self.run(false, None, jobs::list_disks);
    }

    fn load_source(&mut self, key: &str) {
        if let Some(source) = self.sources.iter().find(|s| s.key == key).cloned() {
            self.run(false, None, move |r| jobs::read_source(r, source));
        }
    }

    fn refresh_driver(&mut self) {
        self.driver_version = fx_mount::driver_version();
    }

    fn add_log(&mut self, text: impl AsRef<str>) {
        self.log.push_back(format!(
            "{}  {}",
            chrono::Local::now().format("%H:%M:%S"),
            text.as_ref()
        ));
        while self.log.len() > 300 {
            self.log.pop_front();
        }
    }

    pub fn show_error(&mut self, message: impl Into<String>) {
        self.modal = Some(Modal::Error(message.into()));
    }

    fn confirm(&mut self, title: &str, message: &str, then: Then) {
        self.modal = Some(Modal::Confirm {
            title: title.into(),
            message: message.into(),
            then,
        });
    }

    // ---- what comes back from the jobs ----

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Progress { fraction, status } => {
                self.progress = fraction;
                if let Some(status) = status {
                    self.status = status;
                }
            }
            Msg::Log(text) => self.add_log(text),
            Msg::Status(text) => self.status = text,
            Msg::Disks(disks) => self.update_disks(disks),
            Msg::Polled(disks) => {
                self.polling = false;
                self.last_poll = Instant::now();
                if self.job.is_none() && self.modal.is_none() && !self.same_as_listed(&disks) {
                    self.refresh();
                }
            }
            Msg::Loaded { key, result } => self.loaded(key, result),
            Msg::Mounted { key, part_id, result } => self.mounted(key, part_id, result),
            Msg::Unmounted { key, part_id, result } => {
                if let Some(source) = self.sources.iter_mut().find(|s| s.key == key)
                    && let Some(loaded) = &mut source.loaded
                    && let Some(row) = loaded.partitions.iter_mut().find(|p| p.id() == part_id)
                {
                    row.mount = None;
                }
                if let Err(err) = result {
                    self.add_log(format!("Error: {err}"));
                    self.show_error(err);
                }
            }
            Msg::Text { title, text } => self.modal = Some(Modal::Text { title, text }),
            Msg::Error(text) => {
                self.status = "That didn't work.".into();
                self.add_log(format!("Error: {text}"));
                self.show_error(text);
            }
            Msg::Reload(key) => {
                if key == "*" {
                    self.queue.push_back(Next::Refresh);
                } else {
                    if let Some(source) = self.sources.iter_mut().find(|s| s.key == key) {
                        source.loaded = None;
                    }
                    self.queue.push_back(Next::Load(key));
                }
            }
            Msg::OpenRestore(info) => self.modal = Some(Modal::Restore(Box::new(RestoreForm::new(info)))),
            Msg::DriverInstalled { restart_needed } => {
                self.driver_installing = false;
                self.driver_restart = restart_needed;
                self.refresh_driver();
                if restart_needed {
                    self.status = "Restart the computer to finish installing the driver.".into();
                    self.add_log("Installed the driver. The computer wants a restart before it will work.");
                    self.modal = Some(Modal::Text {
                        title: "Restart needed".into(),
                        text: "The driver is installed, but the computer needs to restart before it can use it.\n\nRestart, then start freeXplorer-rs again.".into(),
                    });
                } else {
                    self.status = "The driver is installed. You can mount now.".into();
                    self.add_log(format!(
                        "Installed {} {}.",
                        fx_mount::DRIVER_NAME,
                        self.driver_version.as_deref().unwrap_or("")
                    ));
                }
            }
            Msg::Done => {
                if self.driver_installing {
                    // the install failed, an error was already shown
                    self.driver_installing = false;
                    self.refresh_driver();
                }
                self.job = None;
                self.progress = 0.0;
                if self.close_when_done {
                    self.closing_ok = true;
                    self.egui.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                if let Some(next) = self.queue.pop_front() {
                    match next {
                        Next::Load(key) => self.load_source(&key),
                        Next::Refresh => self.refresh(),
                    }
                }
            }
        }
    }

    fn update_disks(&mut self, disks: Vec<DiskInfo>) {
        let keep = self.selected.clone();
        // can't forget a disk that something is mounted from
        let mounted: HashSet<String> = self.mounts.keys().map(|(k, _)| k.clone()).collect();
        self.sources
            .retain(|s| s.kind != SourceKind::Disk || mounted.contains(&s.key));
        for (index, disk) in disks.iter().enumerate() {
            let key = format!("disk:{}", disk.id);
            if !self.sources.iter().any(|s| s.key == key) {
                self.sources.insert(index, SourceRow::from_disk(disk));
            }
        }
        if let Some(key) = keep {
            if let Some(source) = self.sources.iter_mut().find(|s| s.key == key) {
                source.loaded = None;
                self.queue.push_back(Next::Load(key));
            } else {
                self.selected = None;
                self.selected_part = None;
            }
        }
        self.status = "Ready.".into();
    }

    fn same_as_listed(&self, disks: &[DiskInfo]) -> bool {
        let listed: Vec<&DiskInfo> = self
            .sources
            .iter()
            .filter(|s| s.kind == SourceKind::Disk)
            .filter_map(|s| s.disk.as_ref())
            .collect();
        listed.len() == disks.len() && listed.iter().zip(disks).all(|(a, b)| *a == b)
    }

    fn loaded(&mut self, key: String, result: Result<Loaded, String>) {
        match result {
            Ok(mut loaded) => {
                for row in &mut loaded.partitions {
                    if let Some(m) = self.mounts.get(&(key.clone(), row.id().to_string())) {
                        row.mount = Some(MountState {
                            point: m.mount_point().to_string(),
                            read_only: m.read_only(),
                        });
                    }
                }
                if let Some(source) = self.sources.iter_mut().find(|s| s.key == key) {
                    source.loaded = Some(loaded);
                }
                if self.selected.as_deref() == Some(key.as_str()) {
                    self.pick_default_partition();
                }
            }
            Err(err) => {
                self.status = err.clone();
                self.add_log(err);
            }
        }
    }

    /// Content is what people come for, so start there when it's usable.
    fn pick_default_partition(&mut self) {
        let parts = self.partitions();
        self.selected_part = parts
            .iter()
            .find(|p| p.id() == layout::CONTENT_ID && p.can_mount())
            .or_else(|| parts.iter().find(|p| p.can_mount()))
            .or_else(|| parts.first())
            .map(|p| p.id().to_string());
    }

    fn mounted(&mut self, key: String, part_id: String, result: Result<(fx_mount::Mounted, String), String>) {
        match result {
            Ok((mounted, usage)) => {
                let point = mounted.mount_point().to_string();
                let read_only = mounted.read_only();
                let mut name = part_id.clone();
                if let Some(source) = self.sources.iter_mut().find(|s| s.key == key)
                    && let Some(loaded) = &mut source.loaded
                {
                    jobs::mark_mounted(&mut loaded.partitions, &part_id, &point, read_only);
                    if let Some(row) = loaded.partitions.iter().find(|p| p.id() == part_id) {
                        name = row.name().to_string();
                    }
                }
                self.mounts.insert((key, part_id), mounted);
                self.status = format!("{name} is now {point}");
                self.add_log(format!(
                    "{} ({}, {usage})",
                    self.status,
                    if read_only { "read-only" } else { "read/write" }
                ));
                fx_platform::open_in_shell(&shell_path(&point));
            }
            Err(err) => {
                self.status = "That didn't work.".into();
                self.add_log(format!("Error: {err}"));
                self.show_error(err);
            }
        }
    }

    // ---- what the buttons do ----

    pub fn select_source(&mut self, key: &str) {
        if self.selected.as_deref() == Some(key) {
            return;
        }
        self.selected = Some(key.to_string());
        self.selected_part = None;
        let loaded = self
            .sources
            .iter()
            .find(|s| s.key == key)
            .is_some_and(|s| s.loaded.is_some());
        if loaded {
            self.pick_default_partition();
        } else {
            self.load_source(key);
        }
    }

    pub fn load_backup_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Load a backup")
            .add_filter("Drive backups", &["img", "bin", "iso"])
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        self.add_backup_source(&path);
    }

    pub fn add_backup_source(&mut self, path: &std::path::Path) {
        let row = SourceRow::from_image(path);
        let key = row.key.clone();
        if !self.sources.iter().any(|s| s.key == key) {
            self.sources.push(row);
        }
        self.select_source(&key);
    }

    pub fn mount_selected(&mut self) {
        let (Some(source), Some(part)) = (self.selected_source().cloned(), self.selected_partition().cloned()) else {
            return;
        };
        // it may have been installed since the last look
        self.refresh_driver();
        match self.driver_state() {
            DriverState::Installed => {}
            DriverState::RestartNeeded => {
                self.show_error("The driver is installed, but the computer needs to restart before it will work.");
                return;
            }
            _ => {
                self.show_error(format!(
                    "The {} driver isn't installed yet. It's the free driver that lets this computer show these partitions as drives. Press \"Install driver\" at the top right, then try again.",
                    fx_mount::DRIVER_NAME
                ));
                return;
            }
        }

        if self.allow_writing {
            if let Some(disk) = &source.disk
                && let Err(why) = safety::check_write(disk, false)
            {
                self.show_error(why);
                return;
            }
            self.confirm(
                "Mount with writing turned on?",
                "Changes you make in the file manager are written straight to the drive. A bug here, or pulling the cable at the wrong moment, can corrupt it.\n\nMake sure anything you care about is backed up. Continue?",
                Then::MountWriting {
                    key: source.key.clone(),
                    part_id: part.id().to_string(),
                },
            );
            return;
        }
        self.start_mount(source, part, false);
    }

    fn start_mount(&mut self, source: SourceRow, part: PartRow, write: bool) {
        let detected = part.detected.clone();
        self.run(false, None, move |r| jobs::mount_one(r, source, detected, write));
    }

    pub fn unmount_selected(&mut self) {
        let (Some(key), Some(part_id)) = (self.selected.clone(), self.selected_part.clone()) else {
            return;
        };
        if let Some(mounted) = self.mounts.remove(&(key.clone(), part_id.clone())) {
            self.run(false, None, move |r| jobs::unmount(r, vec![(key, part_id, mounted)]));
        }
    }

    fn unmount_all(&mut self, then_close: bool) {
        let entries: Vec<_> = self
            .mounts
            .drain()
            .map(|((key, part), mounted)| (key, part, mounted))
            .collect();
        // the window closes once the last drive is let go of, not before
        self.close_when_done = then_close;
        self.run(false, None, move |r| jobs::unmount(r, entries));
    }

    pub fn open_selected_folder(&self) {
        if let Some(m) = self.selected_partition().and_then(|p| p.mount.as_ref()) {
            fx_platform::open_in_shell(&shell_path(&m.point));
        }
    }

    pub fn check_selected(&mut self) {
        let (Some(source), Some(part)) = (self.selected_source().cloned(), self.selected_partition().cloned()) else {
            return;
        };
        let detected = part.detected.clone();
        self.run(false, None, move |r| jobs::check(r, source, detected));
    }

    pub fn format_selected(&mut self) {
        let Some(source) = self.selected_source().cloned() else {
            return;
        };
        let Some(loaded) = source.loaded.clone() else {
            return;
        };
        if self.mounted_from(&source.key) {
            self.show_error("Unmount this drive's partitions before formatting it.");
            return;
        }
        if let Some(disk) = &source.disk
            && let Err(why) = safety::check_write(disk, false)
        {
            self.show_error(why);
            return;
        }
        let length = source
            .disk
            .as_ref()
            .map(|d| d.length)
            .or_else(|| {
                source
                    .image
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len())
            })
            .unwrap_or(0);
        let info = FormatInfo {
            title: source.title.clone(),
            disk: source.disk.clone(),
            layout_fits: length >= layout::MINIMUM_DRIVE_LENGTH,
            scan: loaded.scan,
        };
        self.modal = Some(Modal::Format {
            form: Box::new(FormatForm::new(info)),
            key: source.key,
        });
    }

    pub fn create_backup(&mut self) {
        let Some(source) = self.selected_source().cloned() else {
            return;
        };
        let Some(disk) = source.disk.clone() else {
            return;
        };
        if self.mounts.iter().any(|((k, _), m)| *k == source.key && !m.read_only()) {
            self.show_error(
                "A partition on this drive is mounted with writing turned on. Unmount it first, so the backup can't catch a file half way through changing.",
            );
            return;
        }
        let form = BackupForm::new(disk.clone(), &documents_dir(), &suggested_backup_name(&disk));
        self.modal = Some(Modal::Backup {
            form: Box::new(form),
            key: source.key,
        });
    }

    pub fn restore_backup(&mut self) {
        let backup = self
            .selected_source()
            .filter(|s| s.kind == SourceKind::Image)
            .and_then(|s| s.image.clone());
        let in_use: HashSet<String> = self.mounts.keys().map(|(k, _)| k.clone()).collect();
        self.run(false, None, move |r| jobs::prepare_restore(r, backup, in_use));
    }

    pub fn install_driver(&mut self) {
        let name = fx_mount::DRIVER_NAME;
        let text = if cfg!(windows) {
            format!(
                "freeXplorer-rs will ask the system's package manager to install {name}, the free, open source driver that lets this computer show an Xbox 360 partition as a drive.\n\nThis changes the computer. On Windows you can remove it again in Settings > Apps.\n\nInstall it now?"
            )
        } else if cfg!(target_os = "macos") {
            format!(
                "{name} can't be installed from this window, it needs your approval in System Settings and a restart. freeXplorer-rs will open its download page.\n\nOpen it now?"
            )
        } else {
            format!(
                "freeXplorer-rs will ask the package manager of this system (apt, dnf, pacman or zypper) to install {name}, the free, open source driver that lets this computer show an Xbox 360 partition as a folder. It needs an internet connection.\n\nThis changes the computer. You can remove it again with the same package manager.\n\nInstall it now?"
            )
        };
        self.confirm("Install the driver?", &text, Then::InstallDriver);
    }

    pub fn cancel_job(&mut self) {
        let Some(job) = &self.job else { return };
        if job.stopping {
            return;
        }
        if let Some(warning) = job.stop_warning {
            self.confirm("Stop?", warning, Then::StopJob);
            return;
        }
        self.stop_job();
    }

    fn stop_job(&mut self) {
        if let Some(job) = &mut self.job {
            job.cancel.cancel();
            job.stopping = true;
            self.status = "Stopping...".into();
        }
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
        self.palette = theme::palette(dark);
        theme::apply(&self.egui, dark);
        settings::save_dark(dark);
    }

    // ---- the answers from the windows that ask ----

    pub fn answer(&mut self, then: Then, yes: bool) {
        self.modal = None;
        if !yes {
            return;
        }
        match then {
            Then::MountWriting { key, part_id } => {
                let source = self.sources.iter().find(|s| s.key == key).cloned();
                let part = self
                    .sources
                    .iter()
                    .find(|s| s.key == key)
                    .and_then(|s| s.loaded.as_ref())
                    .and_then(|l| l.partitions.iter().find(|p| p.id() == part_id))
                    .cloned();
                if let (Some(source), Some(part)) = (source, part) {
                    self.start_mount(source, part, true);
                }
            }
            Then::InstallDriver => {
                self.driver_installing = true;
                self.run(false, None, jobs::install_driver);
                if self.job.is_none() {
                    self.driver_installing = false;
                }
            }
            Then::Quit => self.unmount_all(true),
            Then::StopJob => self.stop_job(),
        }
    }

    pub fn format_accepted(&mut self, key: String, form: &FormatForm) {
        self.modal = None;
        let (Some(source), Some(request)) = (
            self.sources.iter().find(|s| s.key == key).cloned(),
            form.build_request(),
        ) else {
            return;
        };
        self.run(false, None, move |r| jobs::format(r, source, request));
    }

    pub fn backup_accepted(&mut self, key: String, form: &BackupForm) {
        self.modal = None;
        let (Some(source), Some(request)) = (
            self.sources.iter().find(|s| s.key == key).cloned(),
            form.build_request(),
        ) else {
            return;
        };
        let Some(disk) = source.disk.clone() else { return };
        self.run(true, None, move |r| jobs::backup(r, source, disk, request));
    }

    pub fn restore_accepted(&mut self, form: &RestoreForm) {
        self.modal = None;
        let Some(request) = form.build_request() else { return };
        // the window already refuses these, this is the last look before anything is written
        if let Err(why) = safety::check_write(&request.target, false) {
            self.show_error(why);
            return;
        }
        let key = format!("disk:{}", request.target.id);
        if self.mounted_from(&key) {
            self.show_error("Unmount this drive's partitions before restoring a backup to it.");
            return;
        }
        self.run(
            true,
            Some("Stop restoring?\n\nThe drive has only been partly written. It won't work until you restore a backup to it again."),
            move |r| jobs::restore(r, request, None),
        );
    }

    // ---- once per frame ----

    fn poll_devices(&mut self) {
        if self.polling
            || self.job.is_some()
            || self.modal.is_some()
            || self.last_poll.elapsed() < Duration::from_secs(3)
        {
            return;
        }
        self.polling = true;
        let reporter = self.reporter(&Cancel::new());
        std::thread::spawn(move || reporter.send(Msg::Polled(fx_platform::list_disks())));
    }

    fn close_requested(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.closing_ok {
            return;
        }
        if self.busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.show_error(
                "freeXplorer-rs is still working. Wait for it to finish before quitting, a drive can be left half written otherwise.",
            );
        } else if !self.mounts.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            let n = self.mounts.len();
            self.confirm(
                "Quit freeXplorer-rs?",
                &format!(
                    "{n} partition{} still mounted. They'll be unmounted if you quit.",
                    if n == 1 { " is" } else { "s are" }
                ),
                Then::Quit,
            );
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_messages();
        self.poll_devices();
        self.close_requested(&ctx);

        ui::draw(self, ui);
        ui::modals(self, &ctx);

        // what a click changed has to show right away, not at the next scheduled redraw
        if ctx.input(|i| !i.events.is_empty()) {
            ctx.request_repaint();
        }
        // something may be coming from a worker, or the drives may have changed
        ctx.request_repaint_after(Duration::from_millis(if self.busy() { 100 } else { 1000 }));
    }
}

/// How the file manager wants the place of a mount: a bare drive letter needs its slash.
fn shell_path(point: &str) -> String {
    if cfg!(windows) && point.chars().count() <= 3 && !point.ends_with('\\') {
        format!("{point}\\")
    } else {
        point.to_string()
    }
}

/// Where a backup is suggested to go: Documents if there is one, else the home folder.
fn documents_dir() -> PathBuf {
    let Some(home) = home_dir() else {
        return PathBuf::from(".");
    };
    let documents = home.join("Documents");
    if documents.is_dir() { documents } else { home }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    let home = {
        // under sudo the files belong to the person who ran it, not to root
        let sudo_home = std::env::var_os("SUDO_USER")
            .filter(|u| u != "root")
            .map(|u| std::path::Path::new(if cfg!(target_os = "macos") { "/Users" } else { "/home" }).join(u))
            .filter(|p| p.is_dir());
        sudo_home.or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    };
    home.filter(|p| p.is_dir())
}
