//! The windows that stop to ask something.

use eframe::egui::{self, Button, Color32, CornerRadius, Frame, Key, Margin, RichText, Stroke, TextEdit};

use crate::app::{App, Modal};
use crate::model::{BackupForm, CLUSTER_CHOICES, FormatForm, RestoreForm};
use crate::theme::Palette;

/// What a window decided this frame.
enum Outcome {
    Open,
    Cancel,
    Accept,
}

pub fn modals(app: &mut App, ctx: &egui::Context) {
    let Some(modal) = app.modal.take() else { return };
    let p = app.palette;

    let back = match modal {
        Modal::Confirm { title, message, then } => match confirm(ctx, &p, &title, &message) {
            Some(yes) => {
                app.answer(then, yes);
                None
            }
            None => Some(Modal::Confirm { title, message, then }),
        },
        Modal::Error(message) => (!notice(ctx, &p, "freeXplorer-rs", &message, true)).then_some(Modal::Error(message)),
        Modal::Text { title, text } => (!notice(ctx, &p, &title, &text, false)).then_some(Modal::Text { title, text }),
        Modal::Format { mut form, key } => match format_window(ctx, &p, &mut form) {
            Outcome::Open => Some(Modal::Format { form, key }),
            Outcome::Cancel => None,
            Outcome::Accept => {
                app.format_accepted(key, &form);
                None
            }
        },
        Modal::Backup { mut form, key } => match backup_window(ctx, &p, &mut form) {
            Outcome::Open => Some(Modal::Backup { form, key }),
            Outcome::Cancel => None,
            Outcome::Accept => {
                app.backup_accepted(key, &form);
                None
            }
        },
        Modal::Restore(mut form) => match restore_window(ctx, &p, &mut form) {
            Outcome::Open => Some(Modal::Restore(form)),
            Outcome::Cancel => None,
            Outcome::Accept => {
                app.restore_accepted(&form);
                None
            }
        },
    };
    // a window that opened another one in answering keeps the new one
    if app.modal.is_none() {
        app.modal = back;
    }
}

fn frame(p: &Palette) -> Frame {
    Frame::new()
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(20))
}

/// The buttons along the bottom, pushed to the right and no taller than they need to be.
fn button_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 30.0),
        egui::Layout::right_to_left(egui::Align::Center),
        add,
    );
}
fn escape(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.key_pressed(Key::Escape))
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong().size(17.0));
}

fn muted(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).color(p.muted).size(12.0));
}

fn boxed(ui: &mut egui::Ui, fill: Color32, stroke: Color32, text: &str) {
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(text);
        });
}

fn danger_button(ui: &mut egui::Ui, p: &Palette, text: &str, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(text.to_string()).color(Color32::WHITE).strong())
            .fill(p.danger)
            .min_size(egui::vec2(90.0, 0.0)),
    )
    .clicked()
}

fn accent_button(ui: &mut egui::Ui, p: &Palette, text: &str, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        Button::new(RichText::new(text.to_string()).color(Color32::WHITE).strong())
            .fill(p.accent)
            .min_size(egui::vec2(110.0, 0.0)),
    )
    .clicked()
}

fn dialog<R>(ctx: &egui::Context, p: &Palette, width: f32, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Modal::new(egui::Id::new("freexplorer-dialog"))
        .frame(frame(p))
        .show(ctx, |ui| {
            ui.set_width(width);
            content(ui)
        })
        .inner
}

/// Yes or no. None while it's still open.
fn confirm(ctx: &egui::Context, p: &Palette, title: &str, message: &str) -> Option<bool> {
    let mut answer = None;
    dialog(ctx, p, 460.0, |ui| {
        heading(ui, title);
        ui.add_space(8.0);
        ui.label(message);
        ui.add_space(14.0);
        button_row(ui, |ui| {
            if ui.add(Button::new("Yes").min_size(egui::vec2(80.0, 0.0))).clicked() {
                answer = Some(true);
            }
            if ui.add(Button::new("No").min_size(egui::vec2(80.0, 0.0))).clicked() {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && escape(ctx) {
        answer = Some(false);
    }
    answer
}

/// A message with an OK button. True once it's been dismissed.
fn notice(ctx: &egui::Context, p: &Palette, title: &str, text: &str, error: bool) -> bool {
    let mut done = false;
    dialog(ctx, p, if error { 460.0 } else { 560.0 }, |ui| {
        heading(ui, title);
        ui.add_space(8.0);
        if error {
            ui.label(RichText::new(text).color(p.error));
        } else {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                ui.label(text);
            });
        }
        ui.add_space(14.0);
        button_row(ui, |ui| {
            if ui.add(Button::new("OK").min_size(egui::vec2(80.0, 0.0))).clicked() {
                done = true;
            }
            if !error && ui.button("Copy").clicked() {
                ui.ctx().copy_text(text.to_string());
            }
        });
    });
    done || escape(ctx)
}

fn confirm_line(ui: &mut egui::Ui, p: &Palette, phrase: &str, text: &mut String) {
    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        ui.label("To continue, type");
        ui.label(RichText::new(phrase).strong());
        ui.label("below.");
    });
    ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY));
    let _ = p;
}

fn format_window(ctx: &egui::Context, p: &Palette, form: &mut FormatForm) -> Outcome {
    let mut outcome = Outcome::Open;
    dialog(ctx, p, 520.0, |ui| {
        heading(ui, &form.heading());
        ui.add_space(10.0);
        boxed(ui, p.danger_background, p.danger_border, form.warning());
        ui.add_space(12.0);

        if form.show_modes() {
            if ui.radio(!form.compat_only, "Format the drive").clicked() {
                form.compat_only = false;
            }
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                muted(ui, p, "Content and the Xbox 1 Compatibility partition, plus the system partitions if the drive doesn't have them. Tick what you want below.");
            });
            if ui
                .radio(form.compat_only, "Only the Xbox 1 Compatibility partition")
                .clicked()
            {
                form.compat_only = true;
            }
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                muted(
                    ui,
                    p,
                    "Creates it if the drive hasn't got one, or erases it and makes it again. Nothing else is touched.",
                );
            });
            if form.compat_only {
                ui.label(RichText::new(form.compat_note()).strong());
            }
            ui.add_space(8.0);
        }

        if form.show_choices() {
            for choice in &mut form.choices {
                Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.checkbox(&mut choice.selected, RichText::new(&choice.name).strong());
                                muted(ui, p, &choice.description);
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                muted(ui, p, &choice.size_text());
                            });
                        });
                    });
            }
        }

        if form.show_content_options() {
            ui.add_space(8.0);
            egui::Grid::new("format-options")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Cluster size");
                    egui::ComboBox::from_id_salt("cluster")
                        .selected_text(CLUSTER_CHOICES[form.cluster].0)
                        .show_ui(ui, |ui| {
                            for (i, (text, _)) in CLUSTER_CHOICES.iter().enumerate() {
                                ui.selectable_value(&mut form.cluster, i, *text);
                            }
                        });
                    ui.end_row();

                    if form.show_choices() {
                        ui.checkbox(&mut form.limit_content, "Limit Content to (GB)");
                        ui.add_enabled(
                            form.limit_content,
                            TextEdit::singleline(&mut form.content_gigabytes).desired_width(100.0),
                        );
                        ui.end_row();
                    }

                    ui.label("Volume label");
                    ui.add(
                        TextEdit::singleline(&mut form.label)
                            .desired_width(f32::INFINITY)
                            .char_limit(30),
                    );
                    ui.end_row();
                });
        }
        if let Some(problem) = form.content_size_problem() {
            ui.label(RichText::new(problem).color(p.error));
        }
        if let Some(problem) = form.label_problem() {
            ui.label(RichText::new(problem).color(p.error));
        }

        if form.show_advanced() {
            ui.add_space(6.0);
            ui.collapsing("Advanced", |ui| {
                ui.checkbox(
                    &mut form.compat_original_style,
                    "Format the Xbox 1 Compatibility partition in the original Xbox (little endian) style",
                );
            });
        }

        let phrase = form.confirmation_phrase();
        confirm_line(ui, p, &phrase, &mut form.confirm);

        ui.add_space(14.0);
        button_row(ui, |ui| {
            if danger_button(ui, p, "Format", form.can_format()) {
                outcome = Outcome::Accept;
            }
            if ui.add(Button::new("Cancel").min_size(egui::vec2(80.0, 0.0))).clicked() {
                outcome = Outcome::Cancel;
            }
        });
    });
    if matches!(outcome, Outcome::Open) && escape(ctx) {
        outcome = Outcome::Cancel;
    }
    outcome
}

fn backup_window(ctx: &egui::Context, p: &Palette, form: &mut BackupForm) -> Outcome {
    form.refresh();
    let mut outcome = Outcome::Open;
    dialog(ctx, p, 520.0, |ui| {
        heading(ui, &form.heading());
        ui.add_space(6.0);
        muted(
            ui,
            p,
            "Saves an exact copy of the whole drive in one file, every partition included. Later you can load it to look inside, or restore it onto another drive. It can take a while, and you can stop at any time.",
        );
        ui.add_space(12.0);

        ui.label("Save it as");
        ui.horizontal(|ui| {
            if ui.button("Browse...").clicked() {
                let current = std::path::Path::new(&form.path);
                let mut dialog = rfd::FileDialog::new().set_title("Save the backup as");
                if let Some(folder) = current.parent().filter(|f| f.is_dir()) {
                    dialog = dialog.set_directory(folder);
                }
                if let Some(name) = current.file_name() {
                    dialog = dialog.set_file_name(name.to_string_lossy());
                }
                if let Some(chosen) = dialog.save_file() {
                    form.path = chosen.to_string_lossy().into_owned();
                }
            }
            ui.add(TextEdit::singleline(&mut form.path).desired_width(f32::INFINITY));
        });
        if let Some(problem) = form.problem() {
            ui.label(RichText::new(problem).color(p.error));
        }
        if let Some(warning) = form.warning() {
            ui.label(RichText::new(warning).color(p.note));
        }
        muted(ui, p, &format!("{} {}", form.size_text(), form.space_text()));

        ui.add_space(12.0);
        ui.checkbox(
            &mut form.verify,
            "Read the backup back afterwards and check it matches (takes a little longer)",
        );

        ui.add_space(14.0);
        button_row(ui, |ui| {
            if accent_button(ui, p, "Create backup", form.can_create()) {
                outcome = Outcome::Accept;
            }
            if ui.add(Button::new("Cancel").min_size(egui::vec2(80.0, 0.0))).clicked() {
                outcome = Outcome::Cancel;
            }
        });
    });
    if matches!(outcome, Outcome::Open) && escape(ctx) {
        outcome = Outcome::Cancel;
    }
    outcome
}

fn restore_window(ctx: &egui::Context, p: &Palette, form: &mut RestoreForm) -> Outcome {
    form.refresh();
    let mut outcome = Outcome::Open;
    dialog(ctx, p, 580.0, |ui| {
        heading(ui, "Restore a backup to a drive");
        ui.add_space(10.0);
        boxed(ui, p.danger_background, p.danger_border, &form.warning());
        ui.add_space(12.0);

        ui.label("Backup file");
        ui.horizontal(|ui| {
            if ui.button("Browse...").clicked()
                && let Some(chosen) = rfd::FileDialog::new()
                    .set_title("Pick the backup")
                    .add_filter("Drive backups", &["img", "bin", "iso"])
                    .add_filter("All files", &["*"])
                    .pick_file()
            {
                form.backup_path = chosen.to_string_lossy().into_owned();
                form.refresh();
            }
            ui.add(TextEdit::singleline(&mut form.backup_path).desired_width(f32::INFINITY));
        });
        form.refresh();
        if let Some(problem) = form.backup_problem() {
            ui.label(RichText::new(problem).color(p.error));
        }
        muted(ui, p, &form.backup_note());

        ui.add_space(10.0);
        ui.label("Restore it to");
        let mut pick = None;
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for (i, target) in form.targets.iter().enumerate() {
                        let selected = form.selected == Some(i);
                        ui.add_enabled_ui(target.enabled(), |ui| {
                            let response = ui.selectable_label(selected, RichText::new(target.disk.display()).strong());
                            if response.clicked() {
                                pick = Some(i);
                            }
                            if let Some(note) = &target.problem {
                                ui.label(RichText::new(note).color(p.note).size(12.0));
                            }
                        });
                    }
                });
            });
        if let Some(i) = pick {
            form.select(Some(i));
        }

        ui.add_space(10.0);
        ui.checkbox(
            &mut form.verify,
            "Read the drive back afterwards and check it matches the backup (takes about as long again)",
        );
        let can_keep = form.can_keep_header();
        let mut keep = form.keep_header();
        ui.add_enabled_ui(can_keep, |ui| {
            if ui
                .checkbox(&mut keep, "Keep this drive's own security sector (its first 512 KB)")
                .changed()
            {
                form.set_keep_header(keep);
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(22.0);
            ui.add(egui::Label::new(RichText::new(form.keep_header_note()).color(p.muted).size(12.0)).wrap());
        });

        let phrase = form.confirmation_phrase();
        confirm_line(ui, p, &phrase, &mut form.confirm);

        ui.add_space(14.0);
        button_row(ui, |ui| {
            if danger_button(ui, p, "Restore", form.can_restore()) {
                outcome = Outcome::Accept;
            }
            if ui.add(Button::new("Cancel").min_size(egui::vec2(80.0, 0.0))).clicked() {
                outcome = Outcome::Cancel;
            }
        });
    });
    if matches!(outcome, Outcome::Open) && escape(ctx) {
        outcome = Outcome::Cancel;
    }
    outcome
}
