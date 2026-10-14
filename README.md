# freeXplorer-rs

A rewrite of [freeXplorer](https://github.com/AlvaroEPena/freeXplorer) that isn't stuck on Windows. It reads and writes Xbox 360 hard drives (FATX/XTAF), formats them, backs them up and shows them as a normal drive. It comes as a window and as a small command line program, with no installer.

It was written from public documentation of the file system. No code from any other tool is in it.

## Where it is

Still being built. On Windows the window and the command line tool work. Linux works too, tried on Ubuntu under WSL2. macOS hasn't been run at all.

| | Windows | Linux | macOS |
|---|---|---|---|
| Image files (browse, copy, format, back up) | yes | yes | written, not run yet |
| Real drives | yes | yes | written, not run yet |
| Mounting as a folder or drive | yes, with WinFsp | yes, with FUSE | written, not run yet |
| Window | yes | yes | written, not run yet |

On Linux I formatted, wrote to, backed up and restored a virtual disk through its raw block device, mounted it with FUSE, and did the same things from the window. Volumes written there read back and check clean on Windows too. A real Xbox drive was also used under Linux: files were written through the raw device and through a read/write FUSE mount (copy, rename, delete), read back and checked, then removed again. Restoring a backup hasn't been run on Linux yet.

The Install driver button was tried on Ubuntu, where it installed FUSE 3 with apt. The dnf, pacman and zypper commands and the macOS download page are written but haven't been run.

"Written, not run yet" means it compiles for that system and the part that reads macOS `diskutil` output is tested on saved samples, but I haven't been able to run any of it on a Mac yet. Treat it as untested until that changes.

## Heads up

freeXplorer-rs writes to disks. Formatting and restoring a backup erase whatever is on the target, so check the disk number and keep a backup of anything you care about. Reading, mounting and writing files (from the command line and through a read/write mount, including renaming and deleting, on both Windows and Linux) have been tried on a real drive and checked clean afterwards. Backing up, formatting and restoring have only been run on image files and virtual disks so far, and I haven't tried a drive made with it in a console yet.

The program refuses to write to the disk the computer is running from, and to any disk that has drive letters unless you pass `--allow-pc-disk`.

## What you need

- Windows 10 or 11, 64-bit, for real drives and mounting. A graphics driver with OpenGL 2 (anything from the last ten years)
- The [Microsoft Visual C++ Redistributable](https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist) (2015-2022, x64). Most PCs have it already. If the program says `VCRUNTIME140.dll` is missing, install it, or run `winget install Microsoft.VCRedist.2015+.x64`
- [WinFsp](https://winfsp.dev), only for mounting. `winget install WinFsp.WinFsp` installs it, and so does the Install driver button in the window
- Administrator rights to open a real drive. The window asks for them when it starts (on Linux and macOS start it with sudo)
- On Linux, FUSE 3 (`sudo apt install fuse3`) for mounting. If it's missing, the Install driver button in the window installs it with apt, dnf, pacman or zypper. For the window, OpenGL and the usual desktop libraries (`libxkbcommon`, `libgl1`, `libegl1`, and the Wayland or X11 ones, which any desktop already has). The Browse buttons use your desktop's file dialog (`xdg-desktop-portal`); without one they do nothing, and you can type the path instead
- On macOS, [macFUSE](https://macfuse.github.io). It needs your approval in System Settings and a restart, so the Install driver button can only open its download page

## Using the window

Run `freexplorer-rs-gui`. Light or dark mode is the switch at the top right.

1. Pick a drive on the left. Its partitions show up on the right.
2. Select a partition and press Mount. It appears in This PC as a new drive and its row turns green. It's read-only unless you tick Allow writing when mounting. Unmount before you unplug the drive.
3. Create backup saves the whole drive to one file and puts a `.sha256` file next to it. Load backup opens a backup like a drive so you can look inside (you can also start the program with the file's name). Restore backup writes a backup onto another drive to make it an exact clone.
4. Format erases the partitions you tick, or only the Xbox 1 Compatibility partition. You have to type the disk name first.
5. Check looks for damage in the selected partition.

The drives that could hurt you (the one the computer runs from, or any with drive letters) can't be written to from the window.

## Using the command line

A target is `disk:N` (see `list`) or the path of an image file, and the partitions are called `content`, `compat`, `sysext` and `sysext2`.

```
freexplorer-rs list
freexplorer-rs info disk:3
freexplorer-rs ls disk:3 content \Content -l
freexplorer-rs get disk:3 content \Content\0000000000000000 C:\backup
freexplorer-rs put practice.img content C:\games\MyGame \Content\0000000000000000\4D5307E6
freexplorer-rs fsck disk:3 content
freexplorer-rs format practice.img --all --dry-run
freexplorer-rs format disk:3 --partitions compat
freexplorer-rs new-image practice.img --size 8G
freexplorer-rs backup disk:3 C:\backups\hdd.img
freexplorer-rs restore C:\backups\hdd.img disk:5
freexplorer-rs restore C:\backups\hdd.img disk:5 --keep-header
freexplorer-rs mount disk:3 content
freexplorer-rs mount practice.img content --rw --letter X: --hold 600
```

`freexplorer-rs help` lists every command. Anything that erases a disk asks you to type `disk N` first (or pass `--confirm "disk N"` in a script). `--dry-run` shows what a format would write and writes nothing. Image files need no special rights, so they're good for practice.

A backup is a raw copy of every sector, the same kind of image `dd` makes, with a `.sha256` file next to it that is checked before a restore. Empty parts aren't stored where the file system allows it.

## Building

You need a recent stable [Rust](https://rustup.rs) and a C linker (Visual Studio Build Tools on Windows, `build-essential` on Linux, the Xcode command line tools on macOS).

- **Windows:** also [LLVM](https://github.com/llvm/llvm-project/releases) (`winget install LLVM.LLVM`), which the WinFsp bindings use while they build. If the build can't find it, set `LIBCLANG_PATH` to its `bin` folder. It isn't needed to run the program.
- **Linux:** nothing else to build. The libraries listed above are needed to run the window.
- **macOS:** [macFUSE](https://macfuse.github.io) and `pkg-config` (`brew install pkgconf`), because the mounting library looks for macFUSE while it builds.

```
git clone https://github.com/AlvaroEPena/freeXplorer-rs.git
cd freeXplorer-rs
cargo build --release
cargo test
```

The programs, `freexplorer-rs` and `freexplorer-rs-gui`, end up in `target/release`.

## Built with

Rust, with a few small crates (`chrono`, `sha2`, `lru`, `thiserror`, `ctrlc`) and, on Windows, `windows-sys` and the [`winfsp`](https://crates.io/crates/winfsp) crate for mounting. WinFsp itself is loaded when you mount and isn't needed otherwise. On Linux and macOS mounting goes through FUSE with the [`fuser`](https://crates.io/crates/fuser) crate.

- `crates/fx-core` has the block devices, the FATX volume, the formatter, the Xbox 360 drive layout and backups. It doesn't know which operating system it's on.
- `crates/fx-platform` finds and opens the real disks.
- `crates/fx-mount` connects a volume to WinFsp, or to FUSE on Linux and macOS.
- `crates/fx-cli` is the `freexplorer-rs` command.
- `crates/fx-gui` is the window, made with [egui](https://github.com/emilk/egui).

## Sources

- Free60 wiki, [FATX](https://free60.org/System-Software/Systems/FATX/) and [the Xbox 360 hard drive](https://free60.org/Hardware/Console/HDD/), for the file system layout and the partition offsets
- [patatetom/xtaf](https://github.com/patatetom/xtaf), whose example output from a real drive was used to check the size calculations
- [Coding FATX support for 2.6](https://lkml.iu.edu/hypermail/linux/kernel/0709.2/3089.html) on the Linux kernel mailing list, for the general shape of the format
- [WinFsp](https://winfsp.dev) documentation and headers

## Not affiliated

freeXplorer-rs has nothing to do with FATXplorer or its author, or with Microsoft. Xbox and Xbox 360 are trademarks of Microsoft.

## License

GPL 3.0, see [LICENSE](LICENSE). It's GPL because the Rust WinFsp bindings it uses are.

Mounting is powered by WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos, https://github.com/winfsp/winfsp, see [THIRD-PARTY.txt](THIRD-PARTY.txt).
