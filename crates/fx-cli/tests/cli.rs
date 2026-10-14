use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use freexplorer_rs::{Context, run};
use fx_core::copy::Cancel;
use tempfile::TempDir;

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn exec(args: &[&str]) -> Run {
    exec_with_input(args, "")
}

fn exec_with_input(args: &[&str], input: &str) -> Run {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut stdin = Cursor::new(input.as_bytes().to_vec());
    let code = {
        let mut ctx = Context {
            out: &mut out,
            err: &mut err,
            input: &mut stdin,
            cancel: Cancel::new(),
        };
        run(&args, &mut ctx)
    };
    Run {
        code,
        out: String::from_utf8_lossy(&out).into_owned(),
        err: String::from_utf8_lossy(&err).into_owned(),
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// a single FATX volume image, small and quick
fn volume_image(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.path().join(name);
    let r = exec(&["new-image", s(&path), "--size", "64M", "--partition", "--label", "Test"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    path
}

fn local_tree(dir: &TempDir) -> PathBuf {
    let root = dir.path().join("Game");
    fs::create_dir_all(root.join("sub")).unwrap();
    let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 + i / 251) as u8).collect();
    fs::write(root.join("big.bin"), &big).unwrap();
    fs::write(root.join("sub").join("small.txt"), b"hello xbox").unwrap();
    fs::write(root.join("empty.dat"), b"").unwrap();
    root
}

#[test]
fn no_arguments_prints_help_and_asks_for_a_command() {
    let r = exec(&[]);
    assert_eq!(r.code, 2);
    assert!(r.out.contains("usage: freexplorer-rs"));
    assert_eq!(exec(&["help"]).code, 0);
}

#[test]
fn unknown_commands_and_options_are_usage_errors() {
    let r = exec(&["frobnicate"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("unknown command"));

    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let r = exec(&["ls", s(&image), "--bogus"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("--bogus"));
}

#[test]
fn a_new_full_layout_image_is_listed_by_info() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("hdd.img");
    let r = exec(&["new-image", s(&path), "--size", "6G"]);
    assert_eq!(r.code, 0, "{}", r.err);

    let r = exec(&["info", s(&path)]);
    assert_eq!(r.code, 0);
    assert!(r.out.contains("Xbox 360 drive"));
    assert!(r.out.contains("sysext2"));
    assert!(r.out.contains("XTAF, 16 KB clusters, FAT32"));
    assert!(r.out.contains("console cache, not FATX"));
}

#[test]
fn new_image_will_not_overwrite_and_checks_the_size() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("x.img");
    fs::write(&path, b"keep me").unwrap();
    assert_eq!(exec(&["new-image", s(&path), "--size", "6G"]).code, 2);
    assert_eq!(fs::read(&path).unwrap(), b"keep me");

    let small = dir.path().join("small.img");
    let r = exec(&["new-image", s(&small), "--size", "1G"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("needs at least"));
    assert!(!small.exists());
}

#[test]
fn files_go_in_and_come_out_the_same() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let tree = local_tree(&dir);

    let r = exec(&["put", s(&image), s(&tree), "\\Games\\Mine"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(r.out.contains("3 files"));

    let r = exec(&["ls", s(&image), "\\Games\\Mine", "-l"]);
    assert_eq!(r.code, 0);
    let lines: Vec<&str> = r.out.lines().collect();
    assert!(lines[0].ends_with("sub\\"), "folders first: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("300000") && l.ends_with("big.bin")));
    assert!(lines.iter().any(|l| l.ends_with("empty.dat")));

    let out = dir.path().join("out");
    let r = exec(&["get", s(&image), "\\Games\\Mine", s(&out), "--quiet"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(
        fs::read(out.join("big.bin")).unwrap(),
        fs::read(tree.join("big.bin")).unwrap()
    );
    assert_eq!(fs::read(out.join("sub").join("small.txt")).unwrap(), b"hello xbox");

    let r = exec(&["fsck", s(&image)]);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.out.contains("Clean."));
}

#[test]
fn existing_files_are_skipped_unless_forced() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let file = dir.path().join("save.bin");
    fs::write(&file, b"first").unwrap();
    // a destination that doesn't exist yet is the new name, folders on the way are made
    assert_eq!(exec(&["put", s(&image), s(&file), "\\Saves\\save.bin"]).code, 0);

    fs::write(&file, b"second version").unwrap();
    let r = exec(&["put", s(&image), s(&file), "\\Saves"]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("already exists"));

    assert_eq!(exec(&["put", s(&image), s(&file), "\\Saves", "--force"]).code, 0);
    let out = dir.path().join("back.bin");
    assert_eq!(exec(&["get", s(&image), "\\Saves\\save.bin", s(&out)]).code, 0);
    assert_eq!(fs::read(&out).unwrap(), b"second version");

    // and the same on the way out
    let r = exec(&["get", s(&image), "\\Saves\\save.bin", s(&out)]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("--force"));
}

#[test]
fn mkdir_mv_and_rm() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let file = dir.path().join("a.txt");
    fs::write(&file, b"data").unwrap();

    assert_eq!(exec(&["mkdir", s(&image), "\\One\\Two\\Three"]).code, 0);
    assert_eq!(exec(&["put", s(&image), s(&file), "\\One\\Two\\Three"]).code, 0);
    assert_eq!(
        exec(&["mv", s(&image), "\\One\\Two\\Three\\a.txt", "\\One\\b.txt"]).code,
        0
    );
    assert_eq!(exec(&["mv", s(&image), "\\One\\b.txt", "\\One\\Two"]).code, 0);

    let r = exec(&["ls", s(&image), "\\One\\Two"]);
    assert_eq!(r.out.lines().collect::<Vec<_>>(), ["Three\\", "b.txt"]);

    let r = exec(&["rm", s(&image), "\\One"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("isn't empty"));

    let r = exec(&["rm", s(&image), "\\One", "-r"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("removed 4 items"));
    assert_eq!(exec(&["ls", s(&image)]).out, "name.txt\n");

    let r = exec(&["rm", s(&image), "\\"]);
    assert_eq!(r.code, 2);
    assert_eq!(exec(&["fsck", s(&image)]).code, 0);
}

#[test]
fn names_fatx_cant_store_are_reported_not_fatal() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let tree = dir.path().join("mixed");
    fs::create_dir_all(&tree).unwrap();
    fs::write(tree.join("good.txt"), b"ok").unwrap();
    fs::write(tree.join("caf\u{e9}.txt"), b"nope").unwrap();

    let r = exec(&["put", s(&image), s(&tree), "\\Mixed"]);
    assert_eq!(r.code, 1);
    assert!(r.out.contains("1 file"));
    assert!(r.err.contains("isn't a name FATX can store"));
    assert!(r.out.contains("1 failed"));
}

#[test]
fn format_asks_first_and_dry_run_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let file = dir.path().join("a.txt");
    fs::write(&file, b"data").unwrap();
    assert_eq!(exec(&["put", s(&image), s(&file), "\\"]).code, 0);

    let r = exec(&["format", s(&image), "--whole", "--dry-run"]);
    assert_eq!(r.code, 0);
    assert!(r.out.contains("Dry run, nothing was written."));
    assert!(exec(&["ls", s(&image)]).out.contains("a.txt"));

    let r = exec_with_input(&["format", s(&image), "--whole"], "no\n");
    assert_eq!(r.code, 1);
    assert!(r.out.contains("Not confirmed"));
    assert!(exec(&["ls", s(&image)]).out.contains("a.txt"));

    let r = exec(&["format", s(&image), "--whole", "--yes", "--label", "Fresh"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(exec(&["ls", s(&image)]).out, "name.txt\n");
    assert!(exec(&["info", s(&image)]).out.contains("\"Fresh\""));
}

#[test]
fn format_wants_to_know_what_to_format() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    assert_eq!(exec(&["format", s(&image)]).code, 2);
    assert_eq!(exec(&["format", s(&image), "--all", "--whole"]).code, 2);
    // a single volume image has no console layout to format
    let r = exec(&["format", s(&image), "--all", "--dry-run"]);
    assert_ne!(r.code, 0);
}

#[test]
fn backup_and_restore_round_trip() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let file = dir.path().join("a.txt");
    fs::write(&file, b"precious").unwrap();
    assert_eq!(exec(&["put", s(&image), s(&file), "\\"]).code, 0);

    let backup = dir.path().join("a.bak");
    let r = exec(&["backup", s(&image), s(&backup)]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(r.out.contains("SHA-256"));
    assert!(Path::new(&format!("{}.sha256", s(&backup))).exists());

    // won't overwrite a backup
    assert_eq!(exec(&["backup", s(&image), s(&backup)]).code, 2);

    // a fresh file needs no confirmation
    let restored = dir.path().join("b.img");
    let r = exec(&["restore", s(&backup), s(&restored)]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(fs::read(&restored).unwrap(), fs::read(&image).unwrap());

    // an existing one does
    let r = exec_with_input(&["restore", s(&backup), s(&image)], "n\n");
    assert_eq!(r.code, 1);
    assert!(r.out.contains("Not confirmed"));
    let r = exec(&["restore", s(&backup), s(&image), "--yes"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
}

#[test]
fn a_damaged_backup_is_caught_before_anything_is_written() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let backup = dir.path().join("a.bak");
    assert_eq!(exec(&["backup", s(&image), s(&backup)]).code, 0);

    let mut bytes = fs::read(&backup).unwrap();
    bytes[100_000] ^= 0xFF;
    fs::write(&backup, bytes).unwrap();

    let target = dir.path().join("t.img");
    fs::copy(&image, &target).unwrap();
    let before = fs::read(&target).unwrap();
    let r = exec(&["restore", s(&backup), s(&target), "--yes"]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("checksum"), "{}", r.err);
    assert_eq!(fs::read(&target).unwrap(), before);
}

#[test]
fn restore_to_a_smaller_drive_is_refused() {
    let dir = TempDir::new().unwrap();
    let image = volume_image(&dir, "a.img");
    let backup = dir.path().join("a.bak");
    assert_eq!(exec(&["backup", s(&image), s(&backup)]).code, 0);

    let small = dir.path().join("small.img");
    assert_eq!(exec(&["new-image", s(&small), "--size", "32M", "--partition"]).code, 0);
    let r = exec(&["restore", s(&backup), s(&small), "--yes"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("at least as big"));
}

#[test]
fn a_target_that_does_not_exist_says_how_to_name_one() {
    let r = exec(&["ls", "no-such-file.img"]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("Give a disk like disk:3"));
}

#[test]
fn an_unknown_partition_lists_the_real_ones() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("hdd.img");
    assert_eq!(exec(&["new-image", s(&path), "--size", "6G"]).code, 0);

    // on a full drive there's more than one volume, so the partition has to be named
    let r = exec(&["ls", s(&path)]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("say which partition"));

    let r = exec(&["ls", s(&path), "content"]);
    assert_eq!(r.code, 0, "{}", r.err);
    let r = exec(&["ls", s(&path), "syscache"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("isn't a FATX partition"));
}
