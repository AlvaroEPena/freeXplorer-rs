use std::io::{self, BufReader, Write};

use freexplorer_rs::{Context, run};
use fx_core::copy::Cancel;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let cancel = Cancel::new();
    let on_signal = cancel.clone();
    // a second Ctrl+C means it, don't wait for the job to notice
    let _ = ctrlc::set_handler(move || {
        if on_signal.is_cancelled() {
            std::process::exit(130);
        }
        on_signal.cancel();
    });

    // not locked: a mounted drive's threads print to the same streams, and a lock held
    // here for the whole run would make every one of them wait for it
    let mut out = io::stdout();
    let mut err = io::stderr();
    let mut input = BufReader::new(io::stdin());
    let mut ctx = Context {
        out: &mut out,
        err: &mut err,
        input: &mut input,
        cancel,
    };
    let code = run(&args, &mut ctx);
    drop(ctx);
    let _ = out.flush();
    std::process::exit(code);
}
