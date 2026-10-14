//! Installing what mounting needs on Linux and macOS.
//!
//! Linux: the package manager of the system brings in FUSE 3. macOS: macFUSE needs a restart and
//! an approval in System Settings that only the person at the computer can give, so the most that
//! can be done there is to open its download page.

#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
use std::process::Command;
#[cfg(not(target_os = "macos"))]
use std::process::Stdio;

#[cfg(not(target_os = "macos"))]
const FUSE_PAGE: &str = "https://github.com/libfuse/libfuse";
#[cfg(target_os = "macos")]
const MACFUSE_PAGE: &str = "https://macfuse.github.io";

#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
}

#[cfg(not(target_os = "macos"))]
impl Manager {
    const ALL: [Manager; 4] = [Manager::Apt, Manager::Dnf, Manager::Pacman, Manager::Zypper];

    fn program(self) -> &'static str {
        match self {
            Manager::Apt => "apt-get",
            Manager::Dnf => "dnf",
            Manager::Pacman => "pacman",
            Manager::Zypper => "zypper",
        }
    }

    /// What has to run, in order. Only the last step has to work, the ones before it are a
    /// courtesy (refreshing the list of packages) and may fail on a computer that is offline.
    fn steps(self) -> &'static [&'static [&'static str]] {
        match self {
            Manager::Apt => &[&["apt-get", "update"], &["apt-get", "install", "-y", "fuse3"]],
            Manager::Dnf => &[&["dnf", "install", "-y", "fuse3"]],
            Manager::Pacman => &[&["pacman", "-S", "--noconfirm", "--needed", "fuse3"]],
            Manager::Zypper => &[&["zypper", "--non-interactive", "install", "fuse3"]],
        }
    }
}

/// The first package manager the system has, in the order apt, dnf, pacman, zypper.
#[cfg(not(target_os = "macos"))]
fn detect(has: impl Fn(&str) -> bool) -> Option<Manager> {
    Manager::ALL.into_iter().find(|m| has(m.program()))
}

/// The command with whatever it takes to run it as an administrator: nothing for root, the
/// graphical password prompt (pkexec) for anyone else, and no way at all when that's missing.
#[cfg(not(target_os = "macos"))]
fn with_privileges(step: &[&str], root: bool, pkexec: bool) -> Result<Vec<String>, String> {
    let mut command: Vec<String> = step.iter().map(|s| s.to_string()).collect();
    if root {
        Ok(command)
    } else if pkexec {
        command.insert(0, "pkexec".to_string());
        Ok(command)
    } else {
        Err(format!(
            "Installing needs administrator rights. Start freeXplorer-rs with sudo, or install FUSE 3 yourself with: sudo {}",
            step.join(" ")
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn find_program(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .chain(["/usr/bin", "/bin", "/usr/sbin", "/sbin", "/usr/local/bin"].map(PathBuf::from))
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

#[cfg(not(target_os = "macos"))]
pub fn install() -> Result<(), String> {
    let manager = detect(|program| find_program(program).is_some()).ok_or_else(|| {
        format!(
            "This system has no package manager this program knows (apt, dnf, pacman or zypper). Install FUSE 3 with yours, see {FUSE_PAGE}."
        )
    })?;
    // SAFETY: geteuid has no arguments and can't fail
    let root = unsafe { libc::geteuid() } == 0;
    let pkexec = find_program("pkexec").is_some();

    let steps = manager.steps();
    for (index, step) in steps.iter().enumerate() {
        let last = index + 1 == steps.len();
        let command = with_privileges(step, root, pkexec)?;
        let output = Command::new(&command[0])
            .args(&command[1..])
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("Couldn't start {}: {err}", command[0]))?;
        if last && !output.status.success() {
            // package managers say what went wrong at the end, and on stderr
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let text = if stderr.trim().is_empty() { stdout } else { stderr };
            let tail: Vec<&str> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(3).collect();
            return Err(format!(
                "The install didn't finish ({}). {}\nYou can install FUSE 3 yourself with: sudo {}",
                output.status,
                tail.into_iter().rev().collect::<Vec<_>>().join(" "),
                step.join(" ")
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn install() -> Result<(), String> {
    // opened for the person who started this with sudo, not for root
    let mut command = match std::env::var("SUDO_USER").ok().filter(|u| u != "root") {
        Some(user) => {
            let mut c = Command::new("sudo");
            c.args(["-u", &user, "open", MACFUSE_PAGE]);
            c
        }
        None => {
            let mut c = Command::new("open");
            c.arg(MACFUSE_PAGE);
            c
        }
    };
    let _ = command.status();
    Err(format!(
        "macFUSE can't be installed from here, it needs your approval in System Settings. Its download page ({MACFUSE_PAGE}) has been opened. Install it, allow it in System Settings > Privacy & Security, restart the Mac, then start freeXplorer-rs again."
    ))
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn the_first_package_manager_that_is_there_wins() {
        assert_eq!(detect(|p| p == "apt-get" || p == "dnf"), Some(Manager::Apt));
        assert_eq!(detect(|p| p == "dnf" || p == "zypper"), Some(Manager::Dnf));
        assert_eq!(detect(|p| p == "pacman"), Some(Manager::Pacman));
        assert_eq!(detect(|p| p == "zypper"), Some(Manager::Zypper));
        assert_eq!(detect(|_| false), None);
    }

    #[test]
    fn every_manager_ends_by_installing_fuse3_without_asking_questions() {
        for manager in Manager::ALL {
            let last = manager.steps().last().unwrap();
            assert_eq!(last[0], manager.program());
            assert!(last.contains(&"fuse3"));
            // a window can't answer a prompt
            assert!(
                last.iter()
                    .any(|a| matches!(*a, "-y" | "--noconfirm" | "--non-interactive")),
                "{manager:?} would stop to ask"
            );
        }
    }

    #[test]
    fn apt_refreshes_its_list_first() {
        let steps = Manager::Apt.steps();
        assert_eq!(steps[0], ["apt-get", "update"]);
        assert_eq!(steps.len(), 2);
    }

    #[test]
    fn root_runs_the_command_as_it_is() {
        let command = with_privileges(&["dnf", "install", "-y", "fuse3"], true, true).unwrap();
        assert_eq!(command, ["dnf", "install", "-y", "fuse3"]);
    }

    #[test]
    fn someone_else_goes_through_the_password_prompt() {
        let command = with_privileges(&["dnf", "install", "-y", "fuse3"], false, true).unwrap();
        assert_eq!(command, ["pkexec", "dnf", "install", "-y", "fuse3"]);
    }

    #[test]
    fn without_a_way_to_get_rights_the_error_says_what_to_type() {
        let err = with_privileges(&["apt-get", "install", "-y", "fuse3"], false, false).unwrap_err();
        assert!(err.contains("sudo apt-get install -y fuse3"), "{err}");
    }
}
