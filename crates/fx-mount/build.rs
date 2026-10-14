fn main() {
    // WinFsp is only looked for when something is mounted, so the program has to start without it.
    // This matters for Windows builds only, whatever system the build runs on.
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winfsp::build::winfsp_link_delayload();
    }
}
