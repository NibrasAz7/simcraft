//! Minimal XY trace plot — an egui-painter line chart with axes, grid and
//! labels (~90 lines; deliberately not matplot, which is a MatCraft crate).

/// A logged sink trace lifted out of a `sim.run` reply.
pub struct TraceView {
    /// Scope path or `to_workspace` var.
    pub name: String,
    pub t: Vec<f64>,
    /// `y[k][i]` — element `i` at sample `k`.
    pub y: Vec<Vec<f64>>,
}

const COLORS: &[egui::Color32] = &[
    egui::Color32::from_rgb(0x00, 0x72, 0xBD),
    egui::Color32::from_rgb(0xD9, 0x53, 0x19),
    egui::Color32::from_rgb(0xED, 0xB1, 0x20),
    egui::Color32::from_rgb(0x7E, 0x2F, 0x8E),
    egui::Color32::from_rgb(0x77, 0xAC, 0x30),
    egui::Color32::from_rgb(0x4D, 0xBE, 0xEE),
];

fn nice(v: f64) -> String {
    if v != 0.0 && (v.abs() >= 1e5 || v.abs() < 1e-3) {
        format!("{v:.1e}")
    } else {
        format!("{v:.3}")
    }
}

/// Draw `tr` into `ui`'s remaining space: every element of `y` as one
/// polyline (up to [`COLORS`] lines), grid + min/max axis labels.
pub fn trace_plot(ui: &mut egui::Ui, tr: &TraceView) {
    let (resp, painter) = ui.allocate_painter(ui.available_size_before_wrap(), egui::Sense::hover());
    let m = egui::vec2(58.0, 8.0);
    let plot = egui::Rect::from_min_max(resp.rect.min + m, resp.rect.max - egui::vec2(8.0, 26.0));
    if tr.t.len() < 2 || plot.width() < 40.0 || plot.height() < 40.0 {
        painter.text(plot.center(), egui::Align2::CENTER_CENTER, "no samples", egui::FontId::proportional(13.0), egui::Color32::GRAY);
        return;
    }
    let (mut lo_t, mut hi_t) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut lo_y, mut hi_y) = (f64::INFINITY, f64::NEG_INFINITY);
    for (k, tk) in tr.t.iter().enumerate() {
        lo_t = lo_t.min(*tk);
        hi_t = hi_t.max(*tk);
        if let Some(row) = tr.y.get(k) {
            for v in row.iter().take(COLORS.len()) {
                lo_y = lo_y.min(*v);
                hi_y = hi_y.max(*v);
            }
        }
    }
    let span_t = (hi_t - lo_t).max(1e-12);
    let span_y = (hi_y - lo_y).max(1e-12);
    lo_y -= span_y * 0.05;
    let hy = hi_y + span_y * 0.05;
    let to_screen = |t: f64, y: f64| {
        egui::pos2(plot.left() + ((t - lo_t) / span_t) as f32 * plot.width(), plot.bottom() - ((y - lo_y) / (hy - lo_y)) as f32 * plot.height())
    };
    // Grid + labels.
    let gray = egui::Color32::from_gray(0xE0);
    let dark = egui::Color32::from_gray(0x40);
    for k in 0..=4 {
        let fx = k as f32 / 4.0;
        let x = plot.left() + fx * plot.width();
        painter.line_segment([egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())], egui::Stroke::new(1.0, gray));
        painter.text(
            egui::pos2(x, plot.bottom() + 4.0),
            egui::Align2::CENTER_TOP,
            nice(lo_t + fx as f64 * span_t),
            egui::FontId::proportional(10.0),
            dark,
        );
        let y = plot.top() + fx * plot.height();
        painter.line_segment([egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)], egui::Stroke::new(1.0, gray));
        painter.text(
            egui::pos2(plot.left() - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            nice(hy - fx as f64 * (hy - lo_y)),
            egui::FontId::proportional(10.0),
            dark,
        );
    }
    painter.rect_stroke(plot, 0.0, egui::Stroke::new(1.0, egui::Color32::from_gray(0xA0)), egui::StrokeKind::Inside);
    // Traces: stride caps painter points for long runs.
    let n_elems = tr.y.first().map(|r| r.len()).unwrap_or(0).min(COLORS.len());
    let stride = (tr.t.len() / 2000).max(1);
    for (e, color) in COLORS.iter().enumerate().take(n_elems) {
        let pts: Vec<egui::Pos2> =
            tr.t.iter()
                .enumerate()
                .step_by(stride)
                .filter_map(|(k, tk)| tr.y.get(k).and_then(|row| row.get(e)).map(|v| to_screen(*tk, *v)))
                .collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(1.5, *color)));
    }
    painter.text(resp.rect.min + egui::vec2(58.0, 14.0), egui::Align2::LEFT_CENTER, &tr.name, egui::FontId::proportional(12.0), dark);
}
