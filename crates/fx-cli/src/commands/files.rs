use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::NaiveDateTime;
use fx_core::fatx::{self, Attributes, FatxVolume, NodeId, NodeInfo, name, time};
use fx_core::{Error, ErrorKind, size};

use crate::args::Args;
use crate::target::{Session, split_partition};
use crate::{CliError, Context, Outcome, say, say_err};

const CHUNK: usize = 1 << 20;

fn not_found(path: &str) -> CliError {
    CliError::Failed(format!("{path} doesn't exist"))
}

pub fn ls(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["long"])?;
    let spec = a.require(0, "target")?;
    let (partition, rest) = split_partition(a.positional.get(1..).unwrap_or_default());
    let path = rest.first().map_or("\\", String::as_str);

    let mut session = Session::open(spec, partition.as_deref(), false, false)?;
    let vol = &mut session.volume;
    let node = vol.find(path)?.ok_or_else(|| not_found(path))?;
    let info = vol.info(node);

    if !info.is_directory {
        print_entry(ctx, &info, a.has("long"));
        return Ok(0);
    }
    let mut children = vol.children(node)?;
    children.sort_by_key(|c| (!c.is_directory, c.name.to_lowercase()));
    for child in &children {
        print_entry(ctx, child, a.has("long"));
    }
    Ok(0)
}

fn print_entry(ctx: &mut Context, node: &NodeInfo, detailed: bool) {
    let name = if node.is_directory {
        format!("{}\\", node.name)
    } else {
        node.name.clone()
    };
    if !detailed {
        say!(ctx, "{name}");
        return;
    }
    let size = if node.is_directory {
        "<dir>".to_string()
    } else {
        node.size.to_string()
    };
    let flag = |attr: Attributes, on: char| if node.attributes.contains(attr) { on } else { '-' };
    say!(
        ctx,
        "{}{}{} {:>12} {}  {name}",
        flag(Attributes::READ_ONLY, 'r'),
        flag(Attributes::HIDDEN, 'h'),
        flag(Attributes::SYSTEM, 's'),
        size,
        node.modified.format("%Y-%m-%d %H:%M")
    );
}

// ---- copying ----

struct Stats<'a, 'b> {
    ctx: &'a mut Context<'b>,
    quiet: bool,
    started: Instant,
    files: u64,
    failures: u64,
    bytes: u64,
}

impl<'a, 'b> Stats<'a, 'b> {
    fn new(ctx: &'a mut Context<'b>, quiet: bool) -> Self {
        Stats {
            ctx,
            quiet,
            started: Instant::now(),
            files: 0,
            failures: 0,
            bytes: 0,
        }
    }

    fn note(&mut self, line: &str) {
        if !self.quiet {
            say!(self.ctx, "{line}");
        }
    }

    fn fail(&mut self, line: &str) {
        self.failures += 1;
        say_err!(self.ctx, "{line}");
    }

    fn finish(&mut self, what: &str) {
        let seconds = self.started.elapsed().as_secs_f64().max(0.001);
        let failed = if self.failures > 0 {
            format!(", {} failed", self.failures)
        } else {
            String::new()
        };
        say!(
            self.ctx,
            "{} file{}, {} {what} ({}/s){failed}",
            self.files,
            if self.files == 1 { "" } else { "s" },
            size::format(self.bytes),
            size::format((self.bytes as f64 / seconds) as u64)
        );
    }
}

fn to_system_time(local: NaiveDateTime) -> SystemTime {
    let seconds = time::to_utc(local).and_utc().timestamp();
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    }
}

fn from_system_time(stamp: SystemTime) -> NaiveDateTime {
    time::from_utc(chrono::DateTime::<chrono::Utc>::from(stamp).naive_utc())
}

// get <target> [partition] <path> <dest>
pub fn get(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["force", "quiet"])?;
    let spec = a.require(0, "target")?;
    let (partition, rest) = split_partition(a.positional.get(1..).unwrap_or_default());
    let [path, dest] = rest.as_slice() else {
        return Err(CliError::Usage(
            "usage: get <target> [partition] <path on drive> <destination>".into(),
        ));
    };

    let mut session = Session::open(spec, partition.as_deref(), false, false)?;
    let vol = &mut session.volume;
    let source = vol.find(path)?.ok_or_else(|| not_found(path))?;
    let info = vol.info(source);
    let mut destination = Path::new(dest).to_path_buf();
    if destination.is_dir() && !info.is_root {
        destination.push(&info.name);
    }

    let mut stats = Stats::new(ctx, a.has("quiet"));
    copy_out(vol, source, &destination, a.has("force"), &mut stats)?;
    stats.finish("copied to this computer");
    Ok(i32::from(stats.failures > 0))
}

fn copy_out(
    vol: &mut FatxVolume,
    node: NodeId,
    destination: &Path,
    force: bool,
    stats: &mut Stats,
) -> Result<(), CliError> {
    let info = vol.info(node);
    if info.is_directory {
        fs::create_dir_all(destination)?;
        for child in vol.children(node)? {
            copy_out(vol, child.id, &destination.join(&child.name), force, stats)?;
        }
        return Ok(());
    }

    if destination.exists() && !force {
        stats.fail(&format!(
            "skipped {}, it already exists (use --force to overwrite)",
            destination.display()
        ));
        return Ok(());
    }
    match write_local(vol, &info, destination) {
        Ok(()) => {
            stats.files += 1;
            stats.bytes += info.size;
            stats.note(&format!("  {}", info.path));
        }
        Err(CopyError::Local(err)) => stats.fail(&format!("couldn't copy {}: {err}", info.path)),
        Err(CopyError::Volume(err)) => return Err(err.into()),
    }
    Ok(())
}

enum CopyError {
    Local(std::io::Error),
    Volume(Error),
}

fn write_local(vol: &mut FatxVolume, info: &NodeInfo, destination: &Path) -> Result<(), CopyError> {
    let mut output = File::create(destination).map_err(CopyError::Local)?;
    let mut buffer = vec![0u8; CHUNK];
    let mut offset = 0u64;
    while offset < info.size {
        let n = vol.read(info.id, offset, &mut buffer).map_err(CopyError::Volume)?;
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n]).map_err(CopyError::Local)?;
        offset += n as u64;
    }
    output
        .set_modified(to_system_time(info.modified))
        .map_err(CopyError::Local)?;
    Ok(())
}

// put <target> [partition] <source> <path>
pub fn put(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["force", "quiet", "allow-pc-disk"])?;
    let spec = a.require(0, "target")?;
    let (partition, rest) = split_partition(a.positional.get(1..).unwrap_or_default());
    let [source, path] = rest.as_slice() else {
        return Err(CliError::Usage(
            "usage: put <target> [partition] <file or folder on this computer> <folder on drive>".into(),
        ));
    };
    if !Path::new(source).exists() {
        return Err(CliError::Failed(format!("can't find \"{source}\"")));
    }

    let mut session = Session::open(spec, partition.as_deref(), true, a.has("allow-pc-disk"))?;
    let vol = &mut session.volume;

    let existing = vol.find(path)?.filter(|&id| vol.info(id).is_directory);
    let (parent, name) = match existing {
        Some(dir) => {
            let trimmed = source.trim_end_matches(['\\', '/']);
            let name = Path::new(trimmed)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (dir, name)
        }
        None => {
            let parts = name::split_path(path);
            let Some((last, folders)) = parts.split_last() else {
                return Err(CliError::Usage("give a destination folder on the drive".into()));
            };
            (vol.create_directories(&folders.join("\\"))?, (*last).to_string())
        }
    };

    let mut stats = Stats::new(ctx, a.has("quiet"));
    copy_in(vol, parent, Path::new(source), &name, a.has("force"), &mut stats)?;
    vol.flush()?;
    stats.finish("copied to the drive");
    Ok(i32::from(stats.failures > 0))
}

fn copy_in(
    vol: &mut FatxVolume,
    parent: NodeId,
    source: &Path,
    name: &str,
    force: bool,
    stats: &mut Stats,
) -> Result<(), CliError> {
    if source.is_dir() {
        if !name::is_valid(name) {
            stats.fail(&format!(
                "skipped folder {}, \"{name}\" isn't a name FATX can store",
                source.display()
            ));
            return Ok(());
        }
        let dir = match vol.find_child(parent, name)? {
            Some(existing) => existing,
            None => match vol.create_directory(parent, name) {
                Ok(created) => created,
                Err(err) if is_space_problem(&err) => {
                    stats.fail(&format!("couldn't copy {}: {err}", source.display()));
                    return Ok(());
                }
                Err(err) => return Err(err.into()),
            },
        };
        if !vol.info(dir).is_directory {
            stats.fail(&format!(
                "skipped {}, a file with that name is in the way",
                source.display()
            ));
            return Ok(());
        }
        let mut entries: Vec<_> = fs::read_dir(source)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let child_name = entry.file_name().to_string_lossy().into_owned();
            copy_in(vol, dir, &entry.path(), &child_name, force, stats)?;
        }
        return Ok(());
    }

    if !name::is_valid(name) {
        stats.fail(&format!(
            "skipped {}, \"{name}\" isn't a name FATX can store (42 characters, letters, digits and basic punctuation)",
            source.display()
        ));
        return Ok(());
    }
    let meta = fs::metadata(source)?;
    if meta.len() > fatx::MAX_FILE_SIZE {
        stats.fail(&format!(
            "skipped {}, FATX files can't be bigger than 4 GB",
            source.display()
        ));
        return Ok(());
    }

    match store_file(vol, parent, source, name, &meta, force) {
        Ok(Stored::Done(path)) => {
            stats.files += 1;
            stats.bytes += meta.len();
            stats.note(&format!("  {path}"));
        }
        Ok(Stored::Skipped(why)) => stats.fail(&format!("skipped {}, {why}", source.display())),
        Err(CopyError::Volume(err)) if is_space_problem(&err) => {
            stats.fail(&format!("couldn't copy {}: {err}", source.display()));
        }
        Err(CopyError::Volume(err)) => return Err(err.into()),
        Err(CopyError::Local(err)) => stats.fail(&format!("couldn't read {}: {err}", source.display())),
    }
    Ok(())
}

fn is_space_problem(err: &Error) -> bool {
    matches!(
        err.kind(),
        Some(ErrorKind::DiskFull | ErrorKind::DirectoryFull | ErrorKind::PathTooLong)
    )
}

enum Stored {
    Done(String),
    Skipped(String),
}

fn store_file(
    vol: &mut FatxVolume,
    parent: NodeId,
    source: &Path,
    name: &str,
    meta: &fs::Metadata,
    force: bool,
) -> Result<Stored, CopyError> {
    let volume = CopyError::Volume;
    let node = match vol.find_child(parent, name).map_err(volume)? {
        Some(existing) => {
            let info = vol.info(existing);
            if info.is_directory || !force {
                return Ok(Stored::Skipped(format!(
                    "{} already exists (use --force to overwrite files)",
                    info.path
                )));
            }
            vol.set_length(existing, 0).map_err(CopyError::Volume)?;
            existing
        }
        None => vol.create_file(parent, name).map_err(CopyError::Volume)?,
    };

    let mut input = File::open(source).map_err(CopyError::Local)?;
    let mut buffer = vec![0u8; CHUNK];
    let mut offset = 0u64;
    loop {
        let n = input.read(&mut buffer).map_err(CopyError::Local)?;
        if n == 0 {
            break;
        }
        vol.write(node, offset, &buffer[..n]).map_err(CopyError::Volume)?;
        offset += n as u64;
    }
    if let Ok(modified) = meta.modified() {
        vol.set_times(node, None, Some(from_system_time(modified)), None)
            .map_err(CopyError::Volume)?;
    }
    vol.flush_node(node).map_err(CopyError::Volume)?;
    Ok(Stored::Done(vol.info(node).path))
}

pub fn mkdir(args: &[String], _ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["allow-pc-disk"])?;
    let spec = a.require(0, "target")?;
    let (partition, rest) = split_partition(a.positional.get(1..).unwrap_or_default());
    let [path] = rest.as_slice() else {
        return Err(CliError::Usage("usage: mkdir <target> [partition] <path>".into()));
    };

    let mut session = Session::open(spec, partition.as_deref(), true, a.has("allow-pc-disk"))?;
    session.volume.create_directories(path)?;
    session.volume.flush()?;
    Ok(0)
}

pub fn rm(args: &[String], ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["recursive", "allow-pc-disk"])?;
    let spec = a.require(0, "target")?;
    let (partition, rest) = split_partition(a.positional.get(1..).unwrap_or_default());
    let [path] = rest.as_slice() else {
        return Err(CliError::Usage("usage: rm <target> [partition] <path> [-r]".into()));
    };

    let mut session = Session::open(spec, partition.as_deref(), true, a.has("allow-pc-disk"))?;
    let vol = &mut session.volume;
    let node = vol.find(path)?.ok_or_else(|| not_found(path))?;
    let info = vol.info(node);
    if info.is_root {
        return Err(CliError::Usage(
            "that's the whole partition, use format if you want to wipe it".into(),
        ));
    }
    if info.is_directory && !a.has("recursive") && !vol.children(node)?.is_empty() {
        return Err(CliError::Usage(format!(
            "{} isn't empty, add -r to remove it and everything in it",
            info.path
        )));
    }

    let count = delete_tree(vol, node)?;
    vol.flush()?;
    say!(ctx, "removed {count} item{}", if count == 1 { "" } else { "s" });
    Ok(0)
}

fn delete_tree(vol: &mut FatxVolume, node: NodeId) -> Result<u64, Error> {
    let mut count = 0;
    if vol.info(node).is_directory {
        for child in vol.children(node)? {
            count += delete_tree(vol, child.id)?;
        }
    }
    vol.delete(node)?;
    Ok(count + 1)
}

pub fn mv(args: &[String], _ctx: &mut Context) -> Outcome {
    let a = Args::parse(args, &[])?;
    a.reject_unknown(&["force", "allow-pc-disk"])?;
    let spec = a.require(0, "target")?;
    let (partition, rest) = split_partition(a.positional.get(1..).unwrap_or_default());
    let [from, to] = rest.as_slice() else {
        return Err(CliError::Usage("usage: mv <target> [partition] <from> <to>".into()));
    };

    let mut session = Session::open(spec, partition.as_deref(), true, a.has("allow-pc-disk"))?;
    let vol = &mut session.volume;
    let node = vol.find(from)?.ok_or_else(|| not_found(from))?;
    let force = a.has("force");

    match vol.find(to)? {
        Some(target) if target != node && vol.info(target).is_directory => {
            let keep_name = vol.info(node).name;
            vol.rename(node, target, &keep_name, force)?;
        }
        _ => {
            let parts = name::split_path(to);
            let Some((last, folders)) = parts.split_last() else {
                return Err(CliError::Usage("give a new name or folder".into()));
            };
            let parent = vol
                .find(&folders.join("\\"))?
                .ok_or_else(|| CliError::Failed("the destination folder doesn't exist".into()))?;
            vol.rename(node, parent, last, force)?;
        }
    }
    vol.flush()?;
    Ok(0)
}
