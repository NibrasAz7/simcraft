//! The diagram canvas: block rects with edge ports, bezier wires, and the
//! pointer interaction model (place / drag / wire-drag / select / delete).

use crate::diagram::{port_counts, Diagram};

/// Canvas selection.
#[derive(Clone, PartialEq, Default)]
pub enum Sel {
    #[default]
    None,
    Block(String),
    Wire(usize),
}

/// Pointer/drag state owned by the app, driven by [`ui`].
#[derive(Default)]
pub struct CanvasState {
    pub sel: Sel,
    /// `(block path, grab offset)` while moving a block.
    pub grab: Option<(String, egui::Vec2)>,
    /// `(source block path, 1-based out port)` while a wire drag is live.
    pub pending_wire: Option<(String, usize)>,
    /// Palette type awaiting a canvas click (click-to-place).
    pub place_pending: Option<String>,
    /// Double-clicked block whose params window is open.
    pub param_win: Option<String>,
}

const BLOCK_W: f32 = 120.0;
const PORT_SPACING: f32 = 18.0;
const PORT_HIT: f32 = 9.0;
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x00, 0x72, 0xBD);

/// Block rect from its centre position and port counts.
fn block_rect(pos: egui::Pos2, ports: (usize, usize)) -> egui::Rect {
    let n = ports.0.max(ports.1) as f32;
    let h = (n * PORT_SPACING + 22.0).max(48.0);
    egui::Rect::from_center_size(pos, egui::vec2(BLOCK_W, h))
}

/// Centre of input/output port `i` (0-based) on `rect`.
fn port_pos(rect: egui::Rect, input: bool, i: usize, count: usize) -> egui::Pos2 {
    let y0 = rect.center().y - count as f32 * PORT_SPACING / 2.0;
    let y = y0 + i as f32 * PORT_SPACING + PORT_SPACING / 2.0;
    egui::pos2(if input { rect.left() } else { rect.right() }, y)
}

/// 24-point cubic bezier from `a` to `b` with horizontal tangents —
/// the Simulink-style elbow curve.
fn wire_points(a: egui::Pos2, b: egui::Pos2) -> Vec<egui::Pos2> {
    let dx = ((b.x - a.x) * 0.5).abs().max(40.0);
    let (c1, c2) = (egui::pos2(a.x + dx, a.y), egui::pos2(b.x - dx, b.y));
    (0..=24)
        .map(|k| {
            let t = k as f32 / 24.0;
            let s = 1.0 - t;
            egui::pos2(
                s * s * s * a.x + 3.0 * s * s * t * c1.x + 3.0 * s * t * t * c2.x + t * t * t * b.x,
                s * s * s * a.y + 3.0 * s * s * t * c1.y + 3.0 * s * t * t * c2.y + t * t * t * b.y,
            )
        })
        .collect()
}

struct Hit {
    block: Option<String>,
    out_port: Option<(String, usize)>,
    in_port: Option<(String, usize)>,
    wire: Option<usize>,
}

fn hit_test(d: &Diagram, p: egui::Pos2) -> Hit {
    let mut hit = Hit { block: None, out_port: None, in_port: None, wire: None };
    for b in &d.blocks {
        let Some(ports) = port_counts(&b.type_name, &b.params) else { continue };
        let r = block_rect(b.pos, ports);
        for i in 0..ports.1 {
            if port_pos(r, false, i, ports.1).distance(p) < PORT_HIT {
                hit.out_port = Some((b.path.clone(), i + 1));
            }
        }
        for i in 0..ports.0 {
            if port_pos(r, true, i, ports.0).distance(p) < PORT_HIT {
                hit.in_port = Some((b.path.clone(), i + 1));
            }
        }
        if r.contains(p) {
            hit.block = Some(b.path.clone()); // later blocks win (drawn on top)
        }
    }
    'w: for (i, w) in d.wires.iter().enumerate() {
        let (Some(sb), Some(db)) = (d.block(&w.src), d.block(&w.dst)) else { continue };
        let (Some(sp), Some(dp)) = (port_counts(&sb.type_name, &sb.params), port_counts(&db.type_name, &db.params)) else { continue };
        let a = port_pos(block_rect(sb.pos, sp), false, w.src_port - 1, sp.1);
        let b = port_pos(block_rect(db.pos, dp), true, w.dst_port - 1, dp.0);
        for pt in wire_points(a, b) {
            if pt.distance(p) < 6.0 {
                hit.wire = Some(i);
                break 'w;
            }
        }
    }
    hit
}

/// Draw the diagram and handle canvas input. Returns true when the diagram
/// changed (marks the file dirty).
pub fn ui(ui: &mut egui::Ui, d: &mut Diagram, st: &mut CanvasState) -> bool {
    let (resp, painter) = ui.allocate_painter(ui.available_size_before_wrap(), egui::Sense::click_and_drag());
    let origin = resp.rect.min.to_vec2();
    let to_screen = |p: egui::Pos2| p + origin;
    let from_screen = |p: egui::Pos2| p - origin;
    let mut dirty = false;

    // Light dot grid for orientation.
    let step = 20.0;
    let mut gx = resp.rect.left();
    while gx < resp.rect.right() {
        let mut gy = resp.rect.top();
        while gy < resp.rect.bottom() {
            painter.circle_filled(egui::pos2(gx, gy), 1.0, egui::Color32::from_gray(0xDD));
            gy += step;
        }
        gx += step;
    }

    // ---- input -----------------------------------------------------------
    let ctx = ui.ctx();
    let pointer = ctx.input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()));
    let pressed = ctx.input(|i| i.pointer.primary_pressed());
    let released = ctx.input(|i| i.pointer.primary_released());
    let canvas_p = pointer.filter(|p| resp.rect.contains(*p)).map(from_screen);

    if let Some(p) = canvas_p {
        if let Some((path, _)) = &st.grab {
            if ctx.input(|i| i.pointer.primary_down()) {
                if let Some(b) = d.block_mut(path) {
                    let off = st.grab.as_ref().map(|g| g.1).unwrap_or(egui::Vec2::ZERO);
                    let np = p - off;
                    let snapped = egui::pos2((np.x / 10.0).round() * 10.0, (np.y / 10.0).round() * 10.0);
                    if b.pos != snapped {
                        b.pos = snapped;
                        dirty = true;
                    }
                }
            }
        }
        if resp.double_clicked() {
            let hit = hit_test(d, p);
            if let Some(path) = hit.block {
                st.sel = Sel::Block(path.clone());
                st.param_win = Some(path);
            }
        } else if pressed {
            if let Some(t) = st.place_pending.clone() {
                let path = d.add_block(&t, p);
                st.sel = Sel::Block(path);
                dirty = true;
                if !ctx.input(|i| i.modifiers.shift) {
                    st.place_pending = None;
                }
            } else {
                let hit = hit_test(d, p);
                if let Some((path, port)) = hit.out_port {
                    st.pending_wire = Some((path.clone(), port));
                    st.sel = Sel::Block(path);
                } else if let Some(path) = hit.block {
                    st.sel = Sel::Block(path.clone());
                    let bpos = d.block(&path).map(|b| b.pos).unwrap_or(p);
                    st.grab = Some((path, p - bpos));
                } else if let Some(i) = hit.wire {
                    st.sel = Sel::Wire(i);
                } else {
                    st.sel = Sel::None;
                }
            }
        }
    }
    if released {
        if let Some((src, sport)) = st.pending_wire.take() {
            if let Some(p) = canvas_p {
                if let Some((dst, dport)) = hit_test(d, p).in_port {
                    d.add_wire(&src, sport, &dst, dport);
                    dirty = true;
                }
            }
        }
        st.grab = None;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.pending_wire = None;
        st.place_pending = None;
    }
    if !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
        match st.sel.clone() {
            Sel::Block(path) => {
                d.remove_block(&path);
                st.sel = Sel::None;
                dirty = true;
            }
            Sel::Wire(i) => {
                if i < d.wires.len() {
                    d.wires.remove(i);
                    dirty = true;
                }
                st.sel = Sel::None;
            }
            Sel::None => {}
        }
    }

    // ---- wires -----------------------------------------------------------
    for (i, w) in d.wires.iter().enumerate() {
        let (Some(sb), Some(db)) = (d.block(&w.src), d.block(&w.dst)) else { continue };
        let (Some(sp), Some(dp)) = (port_counts(&sb.type_name, &sb.params), port_counts(&db.type_name, &db.params)) else { continue };
        let a = to_screen(port_pos(block_rect(sb.pos, sp), false, w.src_port - 1, sp.1));
        let b = to_screen(port_pos(block_rect(db.pos, dp), true, w.dst_port - 1, dp.0));
        let sel = st.sel == Sel::Wire(i);
        let stroke = egui::Stroke::new(if sel { 2.5 } else { 1.5 }, if sel { ACCENT } else { egui::Color32::from_gray(0x60) });
        painter.add(egui::Shape::line(wire_points(a, b), stroke));
    }
    // In-progress wire from a dragged output port.
    if let (Some((src, sport)), Some(p)) = (&st.pending_wire, canvas_p) {
        if let Some(sb) = d.block(src) {
            if let Some(sp) = port_counts(&sb.type_name, &sb.params) {
                let a = to_screen(port_pos(block_rect(sb.pos, sp), false, sport - 1, sp.1));
                painter.add(egui::Shape::line(wire_points(a, to_screen(p)), egui::Stroke::new(1.5, ACCENT)));
            }
        }
    }

    // ---- blocks ----------------------------------------------------------
    for b in &d.blocks {
        let ports = port_counts(&b.type_name, &b.params);
        let r = block_rect(to_screen(b.pos), ports.unwrap_or((1, 1)));
        let (fill, border) = match ports {
            Some(_) if st.sel == Sel::Block(b.path.clone()) => (egui::Color32::from_rgb(0xE3, 0xF0, 0xFB), ACCENT),
            Some(_) => (egui::Color32::WHITE, egui::Color32::from_gray(0x70)),
            None => (egui::Color32::from_rgb(0xFB, 0xE4, 0xE4), egui::Color32::from_rgb(0xC0, 0x39, 0x2B)),
        };
        painter.rect_filled(r, 4.0, fill);
        painter.rect_stroke(r, 4.0, egui::Stroke::new(1.5, border), egui::StrokeKind::Inside);
        painter.text(r.center(), egui::Align2::CENTER_CENTER, &b.type_name, egui::FontId::proportional(11.0), egui::Color32::from_gray(0x50));
        painter.text(
            egui::pos2(r.center().x, r.bottom() + 10.0),
            egui::Align2::CENTER_CENTER,
            &b.path,
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(0x20),
        );
        if let Some((ni, no)) = ports {
            for i in 0..no {
                painter.circle_filled(port_pos(r, false, i, no), 4.0, egui::Color32::from_rgb(0xD9, 0x53, 0x0D));
            }
            for i in 0..ni {
                painter.circle_filled(port_pos(r, true, i, ni), 4.0, egui::Color32::from_rgb(0x00, 0x72, 0xBD));
            }
        }
    }

    // Palette drag-and-drop: ghost preview + release places the block.
    if resp.dnd_hover_payload::<String>().is_some() {
        if let Some(p) = canvas_p {
            painter.rect_stroke(block_rect(to_screen(p), (1, 1)), 4.0, egui::Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
        }
    }
    if let Some(t) = resp.dnd_release_payload::<String>() {
        if let Some(p) = canvas_p {
            let path = d.add_block(t.as_str(), p);
            st.sel = Sel::Block(path);
            dirty = true;
        }
    }
    // Click-to-place ghost.
    if st.place_pending.is_some() {
        if let Some(p) = canvas_p {
            painter.rect_stroke(block_rect(to_screen(p), (1, 1)), 4.0, egui::Stroke::new(1.5, ACCENT.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        }
    }
    dirty
}
