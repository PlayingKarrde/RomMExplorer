//! Small custom-painted widgets used across the UI.

use crate::theme::*;
use eframe::egui::{
    self, text::LayoutJob, vec2, Align2, Color32, CursorIcon, FontId, Galley, Pos2, Rect, Response, RichText, Sense,
    Stroke, StrokeKind, TextFormat, Ui,
};
use std::sync::Arc;

pub fn truncated(ui: &Ui, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat { font_id: font, color, ..Default::default() });
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(10.0));
    ui.painter().layout_job(job)
}

/// A custom-painted pill button at an exact rect.
pub fn pill_button(ui: &mut Ui, rect: Rect, id: egui::Id, text: &str, fill: Color32, fg: Color32) -> Response {
    let resp = ui.interact(rect, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let fill = if resp.is_pointer_button_down_on() {
        fill.gamma_multiply(0.8)
    } else if resp.hovered() {
        lighten(fill, 0.12)
    } else {
        fill
    };
    let p = ui.painter();
    p.rect_filled(rect, rect.height() / 2.0, fill);
    p.text(rect.center(), Align2::CENTER_CENTER, text, bold(13.0), fg);
    resp
}

pub fn lighten(c: Color32, amt: f32) -> Color32 {
    let f = |x: u8| (x as f32 + (255.0 - x as f32) * amt) as u8;
    Color32::from_rgba_unmultiplied(f(c.r()), f(c.g()), f(c.b()), c.a())
}

/// Segmented control (a row of mutually exclusive options).
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(SURFACE)
        .corner_radius(9.0)
        .inner_margin(3)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (opt, label) in options {
                    let selected = *value == *opt;
                    let text = RichText::new(*label).color(if selected { Color32::WHITE } else { TEXT_DIM });
                    let btn = egui::Button::new(text)
                        .fill(if selected { ACCENT } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(7.0)
                        .min_size(vec2(0.0, 26.0));
                    if ui.add(btn).on_hover_cursor(CursorIcon::PointingHand).clicked() && !selected {
                        *value = *opt;
                        changed = true;
                    }
                }
            });
        });
    changed
}

pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    let btn = egui::Button::new(RichText::new(icon).size(16.0).color(TEXT_DIM))
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(9.0)
        .min_size(vec2(34.0, 32.0));
    ui.add(btn).on_hover_text(tooltip).on_hover_cursor(CursorIcon::PointingHand)
}

pub fn primary_button(ui: &mut Ui, text: &str, width: f32, height: f32) -> Response {
    let btn = egui::Button::new(RichText::new(text).font(bold(15.0)).color(Color32::WHITE))
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .corner_radius(10.0)
        .min_size(vec2(width, height));
    ui.add_sized(vec2(width, height), btn).on_hover_cursor(CursorIcon::PointingHand)
}

pub fn secondary_button(ui: &mut Ui, text: &str, color: Color32, width: f32) -> Response {
    let btn = egui::Button::new(RichText::new(text).color(color))
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(10.0)
        .min_size(vec2(width, 34.0));
    let r = if width > 0.0 { ui.add_sized(vec2(width, 34.0), btn) } else { ui.add(btn) };
    r.on_hover_cursor(CursorIcon::PointingHand)
}

/// A pill-shaped tag. Measured up front and allocated as one fixed-size
/// block, so in a wrapping layout a chip that doesn't fit moves to the next
/// row whole instead of its text wrapping inside it.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let fg = lighten(color, 0.35);
    let pad = vec2(9.0, 4.0);
    // Never wider than a full row; truncate with an ellipsis if it must.
    let max_text_w = (ui.max_rect().width() - pad.x * 2.0).max(20.0);
    let galley = truncated(ui, text, regular(12.0), fg, max_text_w);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.rect_filled(rect, rect.height() / 2.0, color.gamma_multiply(0.14));
        p.rect_stroke(rect, rect.height() / 2.0, Stroke::new(1.0_f32, color.gamma_multiply(0.35)), StrokeKind::Inside);
        p.galley(rect.min + pad, galley.clone(), fg);
    }
    if galley.elided {
        resp.on_hover_text(text)
    } else {
        resp
    }
}

pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).font(bold(11.0)).color(MUTED).extra_letter_spacing(1.2));
    ui.add_space(2.0);
}

/// Sidebar navigation row.
pub fn nav_item(ui: &mut Ui, icon: &str, label: &str, count: usize, installed: usize, selected: bool) -> Response {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = ui.painter().clone();
    if selected {
        p.rect_filled(rect, 8.0, ACCENT.gamma_multiply(0.20));
        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 9.0), vec2(3.0, 16.0)), 2.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, 8.0, SURFACE);
    }

    let mut x = rect.left() + 12.0;
    if !icon.is_empty() {
        p.text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, icon, regular(14.0), if selected { ACCENT_HI } else { MUTED });
        x += 24.0;
    }

    let count_text = crate::util::format_count(count);
    let count_galley = ui.painter().layout_no_wrap(count_text, regular(11.5), if selected { TEXT } else { MUTED });
    let pill_w = count_galley.size().x + 14.0;
    let pill = Rect::from_center_size(Pos2::new(rect.right() - 8.0 - pill_w / 2.0, rect.center().y), vec2(pill_w, 19.0));

    let mut right_edge = pill.left() - 6.0;
    if installed > 0 {
        let dot = Pos2::new(right_edge - 4.0, rect.center().y);
        p.circle_filled(dot, 3.5, SUCCESS);
        right_edge -= 12.0;
    }

    let color = if selected { Color32::WHITE } else { TEXT_DIM };
    let font = if selected { bold(14.0) } else { regular(14.0) };
    let galley = truncated(ui, label, font, color, right_edge - x - 4.0);
    p.galley(Pos2::new(x, rect.center().y - galley.size().y / 2.0), galley, color);

    p.rect_filled(pill, 10.0, if selected { ACCENT.gamma_multiply(0.45) } else { SURFACE_HI });
    p.galley(pill.center() - count_galley.size() / 2.0, count_galley, TEXT);

    if installed > 0 {
        resp.on_hover_text(format!("{installed} installed"))
    } else {
        resp
    }
}

pub fn progress_bar(painter: &egui::Painter, rect: Rect, fraction: f32, color: Color32) {
    painter.rect_filled(rect, rect.height() / 2.0, Color32::from_white_alpha(28));
    if fraction > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * fraction.clamp(0.0, 1.0)).max(rect.height()));
        painter.rect_filled(fill, rect.height() / 2.0, color);
    }
}

pub fn card_border(painter: &egui::Painter, rect: Rect, radius: f32, color: Color32, width: f32) {
    painter.rect_stroke(rect, radius, Stroke::new(width, color), StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chips in a narrow wrapping row must stay one line tall and move to the
    /// next row whole, never squashing their text.
    #[test]
    fn chips_wrap_without_squashing() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let tags = ["1998", "🌐 USA", "🌐 Europe", "En", "Role-playing (RPG)", "Turn-based strategy (TBS)", "Adventure", "Hack and slash/Beat 'em up", "A very very very long genre name that cannot fit on one row at all"];
        let mut rects = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.allocate_ui(vec2(300.0, 600.0), |ui| ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    for t in tags {
                        rects.push(chip(ui, t, ACCENT).rect);
                    }
                }));
            });
        });
        let h = rects[0].height();
        let left = rects.iter().map(|r| r.left()).fold(f32::INFINITY, f32::min);
        let rows: std::collections::BTreeSet<i32> = rects.iter().map(|r| r.top().round() as i32).collect();
        for (t, r) in tags.iter().zip(&rects) {
            assert!((r.height() - h).abs() < 0.5, "chip {t:?} is {} tall, expected {h}", r.height());
            assert!(r.right() <= left + 300.5, "chip {t:?} overflows the row: {r:?}");
        }
        assert!(rows.len() >= 3, "expected chips to wrap onto several rows, got {rows:?}");
    }
}
