use fx_core::copy::{Progress, ProgressSink};

use crate::target::Target;
use crate::{Context, say};

pub mod backup;
pub mod files;
pub mod format;
pub mod info;
pub mod mount;

pub(crate) fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

/// A line every 10 per cent, which is plenty for something that takes half an hour.
pub(crate) struct ConsoleProgress<'a, 'b> {
    ctx: &'a mut Context<'b>,
    phase: Option<String>,
    shown: i32,
}

impl<'a, 'b> ConsoleProgress<'a, 'b> {
    pub fn new(ctx: &'a mut Context<'b>) -> Self {
        ConsoleProgress {
            ctx,
            phase: None,
            shown: -1,
        }
    }
}

impl ProgressSink for ConsoleProgress<'_, '_> {
    fn report(&mut self, p: &Progress) {
        if self.phase.as_deref() != Some(p.phase.as_str()) {
            self.phase = Some(p.phase.clone());
            self.shown = -1;
            say!(self.ctx, "{}...", p.phase);
        }
        let step = (p.fraction() * 10.0) as i32;
        if p.done == 0 || (step == self.shown && p.fraction() < 1.0) {
            return;
        }
        self.shown = step;
        say!(self.ctx, "  {:>4}  {}", percent(p.fraction()), p.summary());
    }
}

/// Asks before something destructive. A real disk wants its name typed, an image just a yes.
pub(crate) fn confirm(ctx: &mut Context, target: &Target, confirm_option: Option<&str>, yes: bool) -> bool {
    if let Some(disk) = &target.disk {
        let phrase = fx_platform::safety::confirmation_phrase(disk);
        let typed = match confirm_option {
            Some(text) => Some(text.to_string()),
            None => {
                let _ = write!(
                    ctx.out,
                    "This is a real disk: {}. Type \"{phrase}\" to go ahead: ",
                    disk.display()
                );
                let _ = ctx.out.flush();
                read_line(ctx)
            }
        };
        return fx_platform::safety::is_confirmed(disk, typed.as_deref());
    }

    if yes {
        return true;
    }
    let _ = write!(ctx.out, "Type yes to go ahead: ");
    let _ = ctx.out.flush();
    read_line(ctx).is_some_and(|line| line.trim().eq_ignore_ascii_case("yes"))
}

pub(crate) fn read_line(ctx: &mut Context) -> Option<String> {
    let mut line = String::new();
    match ctx.input.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
    }
}
