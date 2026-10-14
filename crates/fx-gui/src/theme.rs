//! Light and dark mode.

use eframe::egui::{self, Color32, CornerRadius, Stroke};

// the whole set of colours is kept, even the ones this window doesn't use yet
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct Palette {
    pub window: Color32,
    pub card: Color32,
    pub bar: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub error: Color32,
    pub note: Color32,
    pub warn_background: Color32,
    pub warn_border: Color32,
    pub danger_background: Color32,
    pub danger_border: Color32,
    pub danger: Color32,
    pub danger_hover: Color32,
    pub selected: Color32,
    pub mounted_row: Color32,
    pub mounted_selected_row: Color32,
    pub good: Color32,
    pub bad: Color32,
    pub wait: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub item_hover: Color32,
    pub header: Color32,
    pub grid_line: Color32,
    pub input: Color32,
    pub switch_off: Color32,
    pub progress_track: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const LIGHT: Palette = Palette {
    window: rgb(0xF2F3F5),
    card: rgb(0xFFFFFF),
    bar: rgb(0xFFFFFF),
    border: rgb(0xD8DADF),
    accent: rgb(0x107C10),
    accent_hover: rgb(0x0C650C),
    text: rgb(0x1D2025),
    muted: rgb(0x6A7080),
    error: rgb(0xB3261E),
    note: rgb(0x9A5B00),
    warn_background: rgb(0xFFF4CE),
    warn_border: rgb(0xE8CB6B),
    danger_background: rgb(0xFDECEC),
    danger_border: rgb(0xE39A9A),
    danger: rgb(0xB3261E),
    danger_hover: rgb(0x8F1D17),
    selected: rgb(0xDEE6F2),
    mounted_row: rgb(0xD3EDD3),
    mounted_selected_row: rgb(0xB4DEB4),
    good: rgb(0x107C10),
    bad: rgb(0xC42B1C),
    wait: rgb(0xD98A00),
    control: rgb(0xFFFFFF),
    control_hover: rgb(0xEEF0F3),
    item_hover: rgb(0xF0F2F5),
    header: rgb(0xF7F8FA),
    grid_line: rgb(0xECEEF1),
    input: rgb(0xFFFFFF),
    switch_off: rgb(0xB8BDC7),
    progress_track: rgb(0xE3E6EB),
};

pub const DARK: Palette = Palette {
    window: rgb(0x1B1D21),
    card: rgb(0x25282D),
    bar: rgb(0x202328),
    border: rgb(0x3A3F47),
    accent: rgb(0x1E8E22),
    accent_hover: rgb(0x27A82C),
    text: rgb(0xE7E9ED),
    muted: rgb(0x9BA2AE),
    error: rgb(0xFF8F85),
    note: rgb(0xE5A646),
    warn_background: rgb(0x3A3217),
    warn_border: rgb(0x7C6A25),
    danger_background: rgb(0x3A2226),
    danger_border: rgb(0x8B3D44),
    danger: rgb(0xC73A32),
    danger_hover: rgb(0xDB4B42),
    selected: rgb(0x323C4D),
    mounted_row: rgb(0x1F4B2B),
    mounted_selected_row: rgb(0x2B6A3C),
    good: rgb(0x3FB950),
    bad: rgb(0xF0584D),
    wait: rgb(0xE8A33A),
    control: rgb(0x2C3037),
    control_hover: rgb(0x373C45),
    item_hover: rgb(0x2D3139),
    header: rgb(0x2B2F36),
    grid_line: rgb(0x30343B),
    input: rgb(0x1D2024),
    switch_off: rgb(0x5B626E),
    progress_track: rgb(0x353A42),
};

pub fn palette(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

pub fn apply(ctx: &egui::Context, dark: bool) {
    let p = palette(dark);
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    visuals.panel_fill = p.window;
    visuals.window_fill = p.window;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.extreme_bg_color = p.input;
    visuals.faint_bg_color = p.header;
    visuals.override_text_color = Some(p.text);
    visuals.hyperlink_color = p.muted;
    visuals.selection.bg_fill = p.accent;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.window_corner_radius = CornerRadius::same(8);

    let radius = CornerRadius::same(4);
    visuals.widgets.noninteractive.bg_fill = p.card;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.noninteractive.corner_radius = radius;
    for state in [&mut visuals.widgets.inactive, &mut visuals.widgets.open] {
        state.bg_fill = p.control;
        state.weak_bg_fill = p.control;
        state.bg_stroke = Stroke::new(1.0, p.border);
        state.fg_stroke = Stroke::new(1.0, p.text);
        state.corner_radius = radius;
    }
    visuals.widgets.hovered.bg_fill = p.control_hover;
    visuals.widgets.hovered.weak_bg_fill = p.control_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.hovered.corner_radius = radius;
    visuals.widgets.active.bg_fill = p.control_hover;
    visuals.widgets.active.weak_bg_fill = p.control_hover;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, p.border);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
    visuals.widgets.active.corner_radius = radius;

    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        // a click on text has to reach the row or card behind it, not start selecting the words
        style.interaction.selectable_labels = false;
    });
}

/// The on/off switch next to "Dark mode".
pub fn switch(ui: &mut egui::Ui, on: &mut bool, p: &Palette) -> egui::Response {
    let size = egui::vec2(36.0, 18.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let track = if *on { p.accent } else { p.switch_off };
    ui.painter().rect_filled(rect, CornerRadius::same(9), track);
    let knob_x = egui::lerp((rect.left() + 9.0)..=(rect.right() - 9.0), t);
    ui.painter()
        .circle_filled(egui::pos2(knob_x, rect.center().y), 6.5, Color32::WHITE);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
