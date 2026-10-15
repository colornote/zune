//! `SectionLabel` — the 11pt UPPERCASE + tracking group label, with an optional
//! count and a hairline rule.

use eframe::egui::{Response, Sense, Ui, Vec2};

use crate::design::tokens::*;

/// `LABEL` + optional right-aligned count. Returns the response of its row.
pub fn section_label(ui: &mut Ui, text: &str, count: Option<&str>) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 18.0),
        Sense::hover(),
    );
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(tracked(text), label(), TEXT_3);
    painter.galley(
        eframe::egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley.clone(),
        TEXT_3,
    );
    if let Some(count) = count {
        let count_galley = painter.layout_no_wrap(count.to_owned(), mono(BODY_SM), TEXT_3);
        painter.galley(
            eframe::egui::pos2(
                rect.right() - count_galley.size().x,
                rect.center().y - count_galley.size().y / 2.0,
            ),
            count_galley,
            TEXT_3,
        );
    }
    response
}

/// The same label followed by a full-width hairline.
pub fn section_rule(ui: &mut Ui, text: &str, count: Option<&str>) {
    let response = section_label(ui, text, count);
    ui.add_space(SP_4);
    let rect = ui.available_rect_before_wrap();
    let y = rect.top();
    ui.painter().line_segment(
        [eframe::egui::pos2(rect.left(), y), eframe::egui::pos2(rect.right(), y)],
        eframe::egui::Stroke::new(1.0, DIVIDER),
    );
    ui.add_space(SP_8);
    let _ = response;
}
