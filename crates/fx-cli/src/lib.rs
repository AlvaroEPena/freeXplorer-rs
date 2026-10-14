//! The `freexplorer-rs` command line tool. Kept as a library with a thin main so the tests can run
//! whole commands in-process and read what they print.

use std::fmt;
use std::io::{self, BufRead, Write};

use fx_core::copy::Cancel;

mod args;
mod commands;
mod sizes;
mod target;

pub use sizes::parse as parse_size;

pub struct Context<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    pub input: &'a mut dyn BufRead,
    pub cancel: Cancel,
}

#[derive(Debug)]
pub enum CliError {
    /// the command was used wrong, exit code 2
    Usage(String),
    /// the command was fine but couldn't be done, exit code 1
    Failed(String),
    Cancelled,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Usage(m) | CliError::Failed(m) => f.write_str(m),
            CliError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl From<fx_core::Error> for CliError {
    fn from(err: fx_core::Error) -> Self {
        match err {
            fx_core::Error::Cancelled => CliError::Cancelled,
            other => CliError::Failed(other.to_string()),
        }
    }
}

impl From<io::Error> for CliError {
    fn from(err: io::Error) -> Self {
        CliError::Failed(err.to_string())
    }
}

type Outcome = Result<i32, CliError>;

/// print a line, a closed pipe isn't worth failing over
macro_rules! say {
    ($ctx:expr) => {{ let _ = writeln!($ctx.out); }};
    ($ctx:expr, $($arg:tt)*) => {{ let _ = writeln!($ctx.out, $($arg)*); }};
}
macro_rules! say_err {
    ($ctx:expr, $($arg:tt)*) => {{ let _ = writeln!($ctx.err, $($arg)*); }};
}
pub(crate) use {say, say_err};

pub fn run(args: &[String], ctx: &mut Context) -> i32 {
    let Some(first) = args.first() else {
        let _ = ctx.out.write_all(HELP.as_bytes());
        return 2;
    };
    if matches!(first.as_str(), "help" | "--help" | "-h" | "/?") {
        let _ = ctx.out.write_all(HELP.as_bytes());
        return 0;
    }

    let rest = &args[1..];
    let result = match first.to_lowercase().as_str() {
        "version" | "--version" => {
            say!(ctx, "freeXplorer-rs {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "list" => commands::info::list(rest, ctx),
        "info" => commands::info::info(rest, ctx),
        "fsck" => commands::info::fsck(rest, ctx),
        "ls" | "dir" => commands::files::ls(rest, ctx),
        "get" => commands::files::get(rest, ctx),
        "put" => commands::files::put(rest, ctx),
        "mkdir" => commands::files::mkdir(rest, ctx),
        "rm" => commands::files::rm(rest, ctx),
        "mv" => commands::files::mv(rest, ctx),
        "format" => commands::format::format(rest, ctx),
        "new-image" => commands::format::new_image(rest, ctx),
        "backup" => commands::backup::backup(rest, ctx),
        "restore" => commands::backup::restore(rest, ctx),
        "mount" => commands::mount::mount(rest, ctx),
        _ => Err(CliError::Usage(format!("unknown command \"{first}\", try: help"))),
    };

    match result {
        Ok(code) => code,
        Err(CliError::Usage(message)) => {
            say_err!(ctx, "freexplorer-rs: {message}");
            2
        }
        Err(CliError::Failed(message)) => {
            say_err!(ctx, "freexplorer-rs: {message}");
            1
        }
        Err(CliError::Cancelled) => {
            say_err!(ctx, "freexplorer-rs: cancelled");
            130
        }
    }
}

const HELP: &str = "\
freeXplorer-rs command line tool

usage: freexplorer-rs <command> [options]

Targets are a physical disk (disk:3 on Windows, disk:sdb or disk:disk2 elsewhere) or a raw
image file. Physical disks need an administrator prompt (or sudo). Partitions are content,
compat, sysext and sysext2.

looking around
  list [--scan]                         show the disks connected to this computer
  info <target> [--usage]               show the partitions on a drive or image
  fsck <target> [partition] [--fix]     check a partition for damage (--fix frees leaked clusters)
  ls <target> [partition] [path] [-l]   list a directory

moving files
  get <target> [partition] <path> <dest>        copy a file or folder to this computer
  put <target> [partition] <source> <path>      copy a file or folder onto the drive
  mkdir <target> [partition] <path>
  rm <target> [partition] <path> [-r]
  mv <target> [partition] <from> <to>

formatting
  format <target> (--all | --partitions a,b | --whole) [--label NAME] [--cluster-size 16K]
         [--content-size 100G] [--dry-run] [--yes] [--confirm \"disk 3\"]
  new-image <file> --size 8G [--partition]     make an empty drive image to practice on

backups (a backup is an exact copy of the whole drive, every partition included)
  backup <target> <file> [--no-verify] [--no-sparse]
  restore <file> <target> [--keep-header] [--no-verify] [--confirm \"disk 3\"] [--yes]
         The target has to be at least as big as the drive the backup came from.
         A .sha256 file is saved beside the backup and checked before a restore.
         --keep-header leaves the target's own first 512 KB, its security sector, as it
         is. Only for a drive the console sees at the same size as the original.

mounting (needs WinFsp on Windows, FUSE on Linux, macFUSE on macOS, see the readme)
  mount <target> [partition|all] [--rw] [--letter X: | --folder PATH] [--hold SECONDS]

Everything is read-only unless the command is one that changes things. Mounts are
read-only unless you pass --rw. On Windows a mount without --letter or --folder takes a
free drive letter, counting down from Z, and shows up in This PC. On Linux and macOS it
goes in a folder called freeXplorer-rs in your home folder unless you give one with --folder.

On Windows, mounting uses WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos
https://github.com/winfsp/winfsp
";
