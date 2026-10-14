fn main() {
    // both of these are for Windows builds only, whatever system the build runs on
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // WinFsp is only looked for when something is mounted, so the program has to start without it
        winfsp::build::winfsp_link_delayload();

        // the icon Explorer shows for the .exe
        println!("cargo:rerun-if-changed=assets/icon.ico");
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set("FileDescription", "freeXplorer-rs")
            .set("ProductName", "freeXplorer-rs")
            .compile()
            .expect("could not add the icon to the program");
    }
}
