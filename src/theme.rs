use eframe::egui::{self, Color32, CornerRadius, FontData, FontFamily, FontId, Margin, Stroke, TextStyle, Visuals};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(12, 14, 20);
pub const PANEL: Color32 = Color32::from_rgb(18, 21, 30);
pub const SURFACE: Color32 = Color32::from_rgb(26, 30, 43);
pub const SURFACE_HI: Color32 = Color32::from_rgb(35, 40, 58);
pub const BORDER: Color32 = Color32::from_rgb(38, 43, 61);
pub const BORDER_HI: Color32 = Color32::from_rgb(62, 69, 96);
pub const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);
pub const ACCENT_HI: Color32 = Color32::from_rgb(156, 132, 255);
pub const ACCENT_2: Color32 = Color32::from_rgb(56, 189, 248);
pub const SUCCESS: Color32 = Color32::from_rgb(34, 197, 94);
pub const DANGER: Color32 = Color32::from_rgb(239, 68, 68);
pub const WARN: Color32 = Color32::from_rgb(245, 158, 11);
pub const TEXT: Color32 = Color32::from_rgb(232, 234, 242);
pub const TEXT_DIM: Color32 = Color32::from_rgb(178, 183, 202);
pub const MUTED: Color32 = Color32::from_rgb(122, 128, 152);

pub const BOLD: &str = "bold";

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(BOLD.into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

fn read_first(paths: &[&str]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// Uses the platform's UI font when available (Segoe UI on Windows) for a
/// more native, modern look; falls back to egui's bundled fonts.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let regular = read_first(&[
        "C:/Windows/Fonts/segoeui.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ]);
    let semibold = read_first(&[
        "C:/Windows/Fonts/seguisb.ttf",
        "C:/Windows/Fonts/segoeuib.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    ]);
    let symbols = read_first(&["C:/Windows/Fonts/seguisym.ttf"]);

    let mut proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(bytes) = regular {
        fonts.font_data.insert("ui-regular".into(), Arc::new(FontData::from_owned(bytes)));
        proportional.insert(0, "ui-regular".into());
    }
    if let Some(bytes) = symbols {
        fonts.font_data.insert("ui-symbols".into(), Arc::new(FontData::from_owned(bytes)));
        proportional.push("ui-symbols".into());
    }
    let mut bold_family = proportional.clone();
    if let Some(bytes) = semibold {
        fonts.font_data.insert("ui-bold".into(), Arc::new(FontData::from_owned(bytes)));
        bold_family.insert(0, "ui-bold".into());
    }
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold_family);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut v = Visuals::dark();

    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(10, 12, 18);
    v.faint_bg_color = SURFACE;
    v.code_bg_color = SURFACE;
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0_f32, TEXT);
    v.hyperlink_color = ACCENT_HI;
    v.striped = false;

    let r = CornerRadius::same(8);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_DIM);

    v.widgets.inactive.corner_radius = r;
    v.widgets.inactive.bg_fill = SURFACE;
    v.widgets.inactive.weak_bg_fill = SURFACE;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);

    v.widgets.hovered.corner_radius = r;
    v.widgets.hovered.bg_fill = SURFACE_HI;
    v.widgets.hovered.weak_bg_fill = SURFACE_HI;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, BORDER_HI);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.corner_radius = r;
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.weak_bg_fill = ACCENT;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT_HI);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.widgets.active.expansion = 0.0;

    v.widgets.open = v.widgets.hovered;

    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.window_margin = Margin::same(20);
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.combo_width = 160.0;

    style.text_styles = [
        (TextStyle::Heading, bold(22.0)),
        (TextStyle::Body, regular(14.0)),
        (TextStyle::Button, regular(14.0)),
        (TextStyle::Small, regular(11.5)),
        (TextStyle::Monospace, FontId::monospace(12.5)),
    ]
    .into();

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_style_of(egui::Theme::Dark, style);
}
