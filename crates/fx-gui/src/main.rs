// no console window behind the program in release builds
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod dialogs;
mod jobs;
mod model;
mod settings;
mod theme;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    #[cfg(windows)]
    if !fx_platform::is_elevated() && !std::env::args().any(|a| a == "--no-elevate") {
        // reading a real drive needs administrator rights, so ask for them once, up front
        if relaunch_elevated() {
            return Ok(());
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("freeXplorer-rs")
            .with_icon(icon())
            .with_inner_size([1180.0, 740.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "freeXplorer-rs",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}

/// The freeXplorer icon with a small RS in the corner.
fn icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).expect("the built in icon is a valid PNG")
}

/// Starts this program again with a request for administrator rights. False if that wasn't granted,
/// the window then runs as it is and can still open image files.
#[cfg(windows)]
fn relaunch_elevated() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let Ok(exe) = std::env::current_exe() else { return false };
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide("runas".as_ref());
    let file = wide(exe.as_os_str());
    // SAFETY: all strings are nul terminated and live through the call
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    // anything over 32 means it started
    result as usize > 32
}
