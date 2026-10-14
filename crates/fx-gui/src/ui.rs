//! Drawing the main window.

use eframe::egui::{self, Align, Button, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke};
use egui_extras::{Column, TableBuilder};

use crate::app::{App, DriverState};
use crate::model::SourceKind;
use crate::theme::{self, Palette};

pub use crate::dialogs::modals;

pub fn draw(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let bar = Frame::new()
        .fill(p.bar)
        .inner_margin(Margin::symmetric(20, 10))
        .stroke(Stroke::new(1.0, p.border));

    egui::Panel::top("toolbar").frame(bar).show(ui, |ui| toolbar(app, ui));

    egui::Panel::bottom("status")
        .frame(
            Frame::new()
                .fill(p.bar)
                .inner_margin(Margin::symmetric(20, 6))
                .stroke(Stroke::new(1.0, p.border)),
        )
        .show(ui, |ui| status_bar(app, ui));

    egui::Panel::bottom("log")
        .frame(Frame::new().fill(p.window).inner_margin(Margin::symmetric(20, 8)))
        .exact_size(120.0)
        .resizable(false)
        .show(ui, |ui| log(app, ui));

    egui::Panel::left("drives")
        .frame(Frame::new().fill(p.window).inner_margin(Margin {
            left: 20,
            right: 6,
            top: 14,
            bottom: 6,
        }))
        .exact_size(332.0)
        .resizable(false)
        .show(ui, |ui| drives(app, ui));

    egui::CentralPanel::default()
        .frame(Frame::new().fill(p.window).inner_margin(Margin {
            left: 6,
            right: 20,
            top: 14,
            bottom: 6,
        }))
        .show(ui, |ui| partitions(app, ui));
}

fn card(p: &Palette) -> Frame {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

fn accent_button(text: &str, p: &Palette) -> Button<'static> {
    Button::new(RichText::new(text.to_string()).color(Color32::WHITE).strong()).fill(p.accent)
}

fn toolbar(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let busy = app.busy();
    let can_backup = !busy && app.selected_source().is_some_and(|s| s.kind == SourceKind::Disk);
    let part = app.selected_partition();
    let can_mount = !busy && part.is_some_and(|p| p.can_mount() && !p.is_mounted());
    let can_unmount = !busy && part.is_some_and(|p| p.is_mounted());
    let can_open = part.is_some_and(|p| p.is_mounted());
    let can_check = !busy && part.is_some_and(|p| p.can_mount());
    let can_format = !busy
        && app
            .selected_source()
            .is_some_and(|s| s.loaded.is_some() && !app_mounted_from(app, &s.key));

    ui.horizontal(|ui| {
        if ui.add_enabled(!busy, Button::new("Load backup...")).clicked() {
            app.load_backup_file();
        }
        if ui.add_enabled(can_backup, Button::new("Create backup...")).clicked() {
            app.create_backup();
        }
        if ui.add_enabled(!busy, Button::new("Restore backup...")).clicked() {
            app.restore_backup();
        }
        ui.add_space(8.0);
        if ui.add_enabled(!busy, Button::new("Refresh")).clicked() {
            app.refresh();
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            driver_button(app, ui);
            ui.label("Dark mode");
            let mut dark = app.dark;
            if theme::switch(ui, &mut dark, &p).changed() {
                app.set_dark(dark);
            }
        });
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(can_mount, accent_button("Mount", &p)).clicked() {
            app.mount_selected();
        }
        if ui.add_enabled(can_unmount, Button::new("Unmount")).clicked() {
            app.unmount_selected();
        }
        let open_label = if cfg!(windows) {
            "Open in Explorer"
        } else {
            "Open folder"
        };
        if ui.add_enabled(can_open, Button::new(open_label)).clicked() {
            app.open_selected_folder();
        }
        ui.add_space(8.0);
        ui.checkbox(&mut app.allow_writing, "Allow writing when mounting");

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled(can_format, Button::new("Format...")).clicked() {
                app.format_selected();
            }
            if ui.add_enabled(can_check, Button::new("Check")).clicked() {
                app.check_selected();
            }
        });
    });
}

fn app_mounted_from(app: &App, key: &str) -> bool {
    app.sources
        .iter()
        .find(|s| s.key == key)
        .and_then(|s| s.loaded.as_ref())
        .is_some_and(|l| l.partitions.iter().any(|p| p.is_mounted()))
}

fn driver_button(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let state = app.driver_state();
    let dot = match state {
        DriverState::Installed => p.good,
        DriverState::Installing | DriverState::RestartNeeded => p.wait,
        DriverState::Missing => p.bad,
    };
    // the dot is painted, the default font hasn't got a character for it
    let response = ui
        .add(Button::new(format!("      {}", app.driver_text())))
        .on_hover_text(app.driver_detail());
    ui.painter().circle_filled(
        egui::pos2(response.rect.left() + 17.0, response.rect.center().y),
        4.5,
        dot,
    );
    if response.clicked() {
        match state {
            DriverState::Missing if !app.busy() => app.install_driver(),
            _ => {
                let text = app.driver_detail();
                app.modal = Some(crate::app::Modal::Text {
                    title: "Driver".into(),
                    text,
                });
            }
        }
    }
}

fn drives(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let rows: Vec<(String, String, String, Option<String>)> = app
        .sources
        .iter()
        .map(|s| (s.key.clone(), s.title.clone(), s.subtitle.clone(), s.note.clone()))
        .collect();
    let selected = app.selected.clone();
    let enabled = !app.busy();

    card(&p).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.label(RichText::new("Drives").strong().size(15.0));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (key, title, subtitle, note) in rows {
                let is_selected = selected.as_deref() == Some(key.as_str());
                let id = ui.make_persistent_id(&key);
                let fill = if is_selected { p.selected } else { Color32::TRANSPARENT };
                let inner = Frame::new()
                    .fill(fill)
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&title).strong());
                        ui.label(RichText::new(&subtitle).color(p.muted).size(12.0));
                        if let Some(note) = &note {
                            ui.label(RichText::new(note).color(p.note).size(12.0));
                        }
                    });
                let response = ui.interact(inner.response.rect, id, Sense::click());
                if response.hovered() && !is_selected {
                    ui.painter().rect_filled(
                        inner.response.rect,
                        CornerRadius::same(4),
                        p.item_hover.gamma_multiply(0.6),
                    );
                }
                if response.clicked() && enabled {
                    app.select_source(&key);
                }
            }
        });
    });
}

fn partitions(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    let title = app
        .selected_source()
        .map(|s| s.title.clone())
        .unwrap_or_else(|| "Partitions".into());
    let selected = app.selected_part.clone();
    let rows: Vec<_> = app
        .partitions()
        .iter()
        .map(|r| {
            (
                r.id().to_string(),
                r.name().to_string(),
                r.size_text(),
                r.format.clone(),
                r.offset_text(),
                r.status(),
                r.is_mounted(),
            )
        })
        .collect();

    card(&p).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        ui.label(RichText::new(title).strong().size(15.0));
        ui.add_space(6.0);

        let mut clicked = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            TableBuilder::new(ui)
                .sense(Sense::click())
                .striped(false)
                .column(Column::exact(180.0))
                .column(Column::exact(80.0))
                .column(Column::exact(250.0))
                .column(Column::exact(130.0))
                .column(Column::remainder())
                .header(28.0, |mut header| {
                    for text in ["Partition", "Size", "Format", "Offset", "Status"] {
                        header.col(|ui| {
                            let rect = ui.max_rect().expand2(egui::vec2(8.0, 0.0));
                            ui.painter().rect_filled(rect, CornerRadius::ZERO, p.header);
                            ui.label(RichText::new(text).color(p.muted).strong());
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, rows.len(), |mut row| {
                        let (id, name, size, format, offset, status, mounted) = &rows[row.index()];
                        let is_selected = selected.as_deref() == Some(id.as_str());
                        let fill = match (*mounted, is_selected) {
                            (true, true) => Some(p.mounted_selected_row),
                            (true, false) => Some(p.mounted_row),
                            (false, true) => Some(p.selected),
                            _ => None,
                        };
                        for text in [name, size, format, offset, status] {
                            row.col(|ui| {
                                // each cell paints its own slice of the row, so the bar has no gaps
                                if let Some(fill) = fill {
                                    let slice = ui.max_rect().expand2(egui::vec2(0.0, 2.0));
                                    ui.painter().rect_filled(slice, CornerRadius::ZERO, fill);
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add_space(4.0);
                                    ui.add(egui::Label::new(text).truncate());
                                });
                            });
                        }
                        if row.response().clicked() {
                            clicked = Some(id.clone());
                        }
                    });
                });
        });
        if let Some(id) = clicked {
            app.selected_part = Some(id);
        }
    });
}

fn log(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    card(&p).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &app.log {
                    ui.label(RichText::new(line).monospace().color(p.muted).size(12.0));
                }
            });
    });
}

fn status_bar(app: &mut App, ui: &mut egui::Ui) {
    let p = app.palette;
    ui.horizontal(|ui| {
        ui.label(&app.status);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if cfg!(windows) {
                ui.hyperlink_to(
                    RichText::new("WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos")
                        .color(p.muted)
                        .size(12.0),
                    "https://github.com/winfsp/winfsp",
                );
            }
            if app.can_cancel() && ui.button("Stop").clicked() {
                app.cancel_job();
            }
            if app.busy() {
                // the track would be white on white without its own colour
                ui.scope(|ui| {
                    ui.visuals_mut().extreme_bg_color = p.progress_track;
                    ui.add(
                        egui::ProgressBar::new(app.progress as f32)
                            .desired_width(220.0)
                            .corner_radius(CornerRadius::same(4))
                            .animate(app.progress <= 0.0),
                    );
                });
            }
        });
    });
}
