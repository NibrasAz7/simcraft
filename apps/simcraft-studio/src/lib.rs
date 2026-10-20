//! SimCraft Studio — a minimal block-diagram editor (L7 app).
//!
//! Left: the `simblocks` catalog as a draggable palette. Centre: the
//! diagram canvas (drag blocks, drag port→port to wire, Del deletes,
//! double-click opens a params window). Right: inspector — model file,
//! solver settings and the selected block's params. Bottom: sink traces
//! from the last `sim.run`, drawn by [`plot`].
//!
//! Everything user-visible goes through `simcraft_engine::Session`
//! commands (`model.load`, `block.set`, `sim.run`); the canvas itself is a
//! [`Diagram`] that round-trips the `.simcraft` JSON format.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod canvas;
mod diagram;
mod plot;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Map, Value as Json};

use canvas::{CanvasState, Sel};
use diagram::{json_to_param, port_counts, Diagram};
use plot::TraceView;
use simcore::{Param, Solver};

const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x00, 0x72, 0xBD);

/// MatCraft house chrome: pale panels, white content, accent blue.
fn studio_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.panel_fill = egui::Color32::from_rgb(0xF2, 0xF4, 0xF7);
    v.window_fill = egui::Color32::WHITE;
    v.extreme_bg_color = egui::Color32::WHITE;
    v.faint_bg_color = egui::Color32::from_rgb(0xE9, 0xED, 0xF1);
    v.selection.bg_fill = egui::Color32::from_rgb(0xCD, 0xE3, 0xF3);
    v.hyperlink_color = ACCENT;
    v
}

/// Construction options for [`StudioApp`].
#[derive(Default)]
pub struct StudioOptions {
    /// A `.simcraft` file to open at startup.
    pub startup_model: Option<PathBuf>,
}

/// The Studio application state.
pub struct StudioApp {
    session: simcraft_engine::Session,
    diagram: Diagram,
    canvas: CanvasState,
    file_path: Option<PathBuf>,
    file_text: String,
    dirty: bool,
    status: String,
    /// Selection already mirrored into `path_edit`/`param_edits`.
    sel_sync: Option<String>,
    /// Rename buffer for the selected block's path.
    path_edit: String,
    /// Param text buffers keyed `(block path, key)` — shared by the
    /// inspector and the floating params window so both stay in sync.
    param_edits: BTreeMap<(String, String), String>,
    new_param: (String, String),
    traces: Vec<TraceView>,
    trace_sel: Option<usize>,
}

impl StudioApp {
    /// `eframe::run_native` entry point.
    pub fn new(cc: &eframe::CreationContext<'_>, opts: StudioOptions) -> Self {
        cc.egui_ctx.set_visuals(studio_visuals());
        let mut app = StudioApp {
            session: simcraft_engine::Session::new(),
            diagram: Diagram::default(),
            canvas: CanvasState::default(),
            file_path: None,
            file_text: String::new(),
            dirty: false,
            status: "no model — drag a block in or Open a .simcraft file".to_string(),
            sel_sync: None,
            path_edit: String::new(),
            param_edits: BTreeMap::new(),
            new_param: (String::new(), String::new()),
            traces: Vec::new(),
            trace_sel: None,
        };
        if let Some(p) = &opts.startup_model {
            app.open_path(p.clone());
        }
        app
    }

    fn exec(&mut self, id: &str, params: Json) -> Result<Json, String> {
        self.session.execute(id, params).map_err(|e| e.to_string())
    }

    fn reset_edit_state(&mut self) {
        self.canvas = CanvasState::default();
        self.sel_sync = None;
        self.path_edit.clear();
        self.param_edits.clear();
        self.new_param = (String::new(), String::new());
        self.traces.clear();
        self.trace_sel = None;
    }

    fn new_model(&mut self) {
        self.diagram = Diagram::default();
        self.file_path = None;
        self.file_text.clear();
        self.dirty = false;
        self.reset_edit_state();
        self.status = "new model".to_string();
    }

    /// Open `path`: parse via [`Diagram::from_text`] (which validates
    /// through `simmodel::Document`) and load the engine session through
    /// the `model.load` command.
    fn open_path(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| Diagram::from_text(&text).map_err(|e| e.to_string())) {
            Ok(d) => {
                self.diagram = d;
                self.file_path = Some(path.clone());
                self.file_text = path.display().to_string();
                self.reset_edit_state();
                self.dirty = false;
                match self.exec("model.load", json!({"path": path.to_string_lossy()})) {
                    Ok(r) => {
                        let (nb, nw) = (r.get("blocks").and_then(Json::as_u64).unwrap_or(0), r.get("wires").and_then(Json::as_u64).unwrap_or(0));
                        self.status = format!("loaded {} — {nb} blocks, {nw} wires", path.display());
                    }
                    Err(e) => self.status = format!("canvas loaded; engine rejected the model: {e}"),
                }
            }
            Err(e) => self.status = format!("cannot open {}: {e}", path.display()),
        }
    }

    fn save_as(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("SimCraft model", &["simcraft"]).save_file() {
            self.file_path = Some(p.clone());
            self.file_text = p.display().to_string();
            self.save();
        }
    }

    fn save(&mut self) {
        let Some(p) = self.file_path.clone() else {
            self.save_as();
            return;
        };
        match std::fs::write(&p, self.diagram.to_json()) {
            Ok(()) => {
                self.dirty = false;
                self.status = format!("saved {}", p.display());
            }
            Err(e) => self.status = format!("cannot save {}: {e}", p.display()),
        }
    }

    /// Run the diagram: canvas → `.simcraft` text → `Document` → engine
    /// session → the `sim.run` command. Sink traces land in `self.traces`.
    fn run(&mut self) {
        let doc = match simmodel::Document::parse(&self.diagram.to_json()) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("model error: {e}");
                return;
            }
        };
        if let Err(e) = self.session.load_document(doc) {
            self.status = format!("build error: {e}");
            return;
        }
        match self.exec("sim.run", json!({})) {
            Ok(r) => {
                self.traces = parse_traces(r.get("out").unwrap_or(&Json::Null));
                self.trace_sel = (!self.traces.is_empty()).then_some(0);
                self.status = format!(
                    "ran {} steps ({}, dt={}, tstop={}) — {} trace(s)",
                    r.get("steps").and_then(Json::as_u64).unwrap_or(0),
                    r.get("solver").and_then(Json::as_str).unwrap_or("?"),
                    r.get("dt").and_then(Json::as_f64).unwrap_or(0.0),
                    r.get("tstop").and_then(Json::as_f64).unwrap_or(0.0),
                    self.traces.len()
                );
            }
            Err(e) => self.status = format!("run failed: {e}"),
        }
    }

    /// Apply one edited param. Numbers/bools/arrays must parse as their
    /// existing kind; strings take raw text; new keys try JSON first and
    /// fall back to a string. Mirrored to the engine via `block.set`
    /// (best-effort — the session may not hold this block yet).
    fn commit_param(&mut self, path: &str, key: &str, text: &str) {
        let existing = self.diagram.block(path).and_then(|b| b.params.get(key));
        let param = match existing {
            Some(Param::Str(_)) => Some(Param::Str(text.to_string())),
            Some(Param::Bool(_)) => text.trim().parse::<bool>().ok().map(Param::Bool),
            Some(Param::Num(_)) | Some(Param::Vec(_)) | None => serde_json::from_str::<Json>(text).ok().and_then(|v| json_to_param(&v).ok()),
        };
        let param = match (param, existing.is_none()) {
            (Some(p), _) => p,
            (None, true) => Param::Str(text.to_string()),
            (None, false) => {
                self.status = format!("`{key}`: value doesn't parse ({text})");
                return;
            }
        };
        if let Some(b) = self.diagram.block_mut(path) {
            b.params.insert(key.to_string(), param.clone());
        }
        self.diagram.prune_wires();
        self.dirty = true;
        let mut one = Map::new();
        one.insert(key.to_string(), param_to_json(&param));
        let _ = self.exec("block.set", json!({"path": path, "params": Json::Object(one)}));
    }

    fn selected_block_path(&self) -> Option<String> {
        match &self.canvas.sel {
            Sel::Block(p) if self.diagram.block(p).is_some() => Some(p.clone()),
            _ => None,
        }
    }
}

/// `Param` → display text for the edit buffer (strings unquoted).
fn param_text(p: &Param) -> String {
    match p {
        Param::Str(s) => s.clone(),
        _ => serde_json::to_string(&param_to_json(p)).unwrap_or_default(),
    }
}

fn param_to_json(p: &Param) -> Json {
    match p {
        Param::Num(n) => json!(n),
        Param::Vec(v) => json!(v),
        Param::Str(s) => json!(s),
        Param::Bool(b) => json!(b),
    }
}

/// `sim.run` reply `out` object → trace list (stable name order).
fn parse_traces(out: &Json) -> Vec<TraceView> {
    let mut traces = Vec::new();
    if let Some(obj) = out.as_object() {
        let mut names: Vec<&String> = obj.keys().collect();
        names.sort();
        for name in names {
            let Some(rec) = obj.get(name) else { continue };
            let t: Vec<f64> = rec.get("t").and_then(Json::as_array).map(|a| a.iter().filter_map(Json::as_f64).collect()).unwrap_or_default();
            let y: Vec<Vec<f64>> = rec
                .get("y")
                .and_then(Json::as_array)
                .map(|a| a.iter().map(|row| row.as_array().map(|r| r.iter().filter_map(Json::as_f64).collect()).unwrap_or_default()).collect())
                .unwrap_or_default();
            traces.push(TraceView { name: name.clone(), t, y });
        }
    }
    traces
}

impl StudioApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("New").clicked() {
                self.new_model();
            }
            if ui.button("Open…").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("SimCraft model", &["simcraft"]).pick_file() {
                    self.open_path(p);
                }
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            if ui.button("Save As…").clicked() {
                self.save_as();
            }
            ui.separator();
            if ui.button(egui::RichText::new("▶ Run").color(ACCENT).strong()).clicked() {
                self.run();
            }
            ui.separator();
            ui.label(format!(
                "{}{}",
                self.file_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "untitled".into()),
                if self.dirty { " *" } else { "" }
            ));
        });
    }

    fn palette(&mut self, ui: &mut egui::Ui) {
        ui.heading("Blocks");
        ui.small("drag to canvas, or click then click canvas");
        ui.separator();
        let labels: BTreeMap<&str, &str> = simblocks::Factory::catalog().iter().map(|b| (b.type_name, b.label)).collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (group, types) in diagram::GROUPS {
                egui::CollapsingHeader::new(*group).default_open(true).show(ui, |ui| {
                    for t in *types {
                        let label = labels.get(t).copied().unwrap_or(t);
                        let pending = self.canvas.place_pending.as_deref() == Some(t);
                        let resp = ui
                            .dnd_drag_source(ui.id().with(("pal", t)), (*t).to_string(), |ui| {
                                ui.label(label);
                            })
                            .response;
                        if pending {
                            ui.painter().rect_stroke(resp.rect, 2.0, egui::Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
                        }
                        if resp.clicked() {
                            self.canvas.place_pending = (!pending).then(|| (*t).to_string());
                        }
                        resp.on_hover_text(*t);
                    }
                });
            }
        });
    }

    /// The editable key/value param table for block `path` (shared by the
    /// inspector and the double-click params window).
    fn param_table(&mut self, ui: &mut egui::Ui, path: &str) {
        let keys: Vec<String> = self.diagram.block(path).map(|b| b.params.keys().cloned().collect()).unwrap_or_default();
        egui::Grid::new(ui.id().with(("pt", path))).num_columns(2).show(ui, |ui| {
            for key in keys {
                ui.label(&key);
                let default = self.diagram.block(path).and_then(|b| b.params.get(&key)).map(param_text).unwrap_or_default();
                let buf = self.param_edits.entry((path.to_string(), key.clone())).or_insert(default);
                if ui.add(egui::TextEdit::singleline(buf).desired_width(140.0)).changed() {
                    let text = buf.clone();
                    self.commit_param(path, &key, &text);
                }
                ui.end_row();
            }
            ui.label("+");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_param.0).hint_text("key").desired_width(60.0));
                ui.add(egui::TextEdit::singleline(&mut self.new_param.1).hint_text("value").desired_width(70.0));
                if ui.button("add").clicked() && !self.new_param.0.is_empty() {
                    let (k, v) = self.new_param.clone();
                    self.commit_param(path, &k, &v);
                    self.new_param = (String::new(), String::new());
                }
            });
            ui.end_row();
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.separator();
        ui.label("Model file");
        ui.horizontal(|ui| {
            let resp = ui.add(egui::TextEdit::singleline(&mut self.file_text).desired_width(f32::INFINITY).hint_text("untitled"));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.file_text.is_empty() {
                self.open_path(PathBuf::from(self.file_text.trim()));
            }
        });
        ui.add_space(6.0);
        ui.label("Solver");
        egui::Grid::new("solver").num_columns(2).show(ui, |ui| {
            ui.label("solver");
            egui::ComboBox::from_id_salt("solver_kind").selected_text(self.diagram.solver.solver.name()).show_ui(ui, |ui| {
                for &s in Solver::ALL {
                    if ui.selectable_label(self.diagram.solver.solver == s, s.name()).clicked() {
                        self.diagram.solver.solver = s;
                        self.dirty = true;
                    }
                }
            });
            ui.end_row();
            ui.label(if self.diagram.solver.solver == Solver::Rk45 { "dt (initial)" } else { "dt" });
            if ui.add(egui::DragValue::new(&mut self.diagram.solver.dt).speed(0.0005).range(1e-6..=1e6)).changed() {
                self.dirty = true;
            }
            ui.end_row();
            ui.label("tstop");
            if ui.add(egui::DragValue::new(&mut self.diagram.solver.tstop).speed(0.1).range(0.0..=1e6)).changed() {
                self.dirty = true;
            }
            ui.end_row();
            if self.diagram.solver.solver == Solver::Rk45 {
                ui.label("rtol");
                if ui.add(egui::DragValue::new(&mut self.diagram.solver.rtol).speed(0.0001).range(1e-12..=1.0)).changed() {
                    self.dirty = true;
                }
                ui.end_row();
                ui.label("atol");
                if ui.add(egui::DragValue::new(&mut self.diagram.solver.atol).speed(0.000001).range(0.0..=1.0)).changed() {
                    self.dirty = true;
                }
                ui.end_row();
                ui.label("min step (0=auto)");
                if ui.add(egui::DragValue::new(&mut self.diagram.solver.min_step).speed(0.00001).range(0.0..=1e6)).changed() {
                    self.dirty = true;
                }
                ui.end_row();
                ui.label("max step (0=auto)");
                if ui.add(egui::DragValue::new(&mut self.diagram.solver.max_step).speed(0.01).range(0.0..=1e6)).changed() {
                    self.dirty = true;
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        ui.separator();
        let sel = self.selected_block_path();
        if self.sel_sync != sel {
            self.sel_sync = sel.clone();
            if let Some(p) = &sel {
                self.path_edit = p.clone();
            }
        }
        match sel {
            Some(path) => {
                let (ty, params) = self.diagram.block(&path).map(|b| (b.type_name.clone(), b.params.clone())).unwrap_or_default();
                ui.heading(format!("Block: {ty}"));
                ui.horizontal(|ui| {
                    ui.label("path");
                    let resp = ui.add(egui::TextEdit::singleline(&mut self.path_edit).desired_width(120.0));
                    if resp.lost_focus() && self.path_edit != path {
                        if self.diagram.rename_block(&path, self.path_edit.trim()) {
                            self.canvas.sel = Sel::Block(self.path_edit.trim().to_string());
                            self.dirty = true;
                        } else {
                            self.status = format!("cannot rename `{path}` to `{}`", self.path_edit.trim());
                            self.path_edit = path.clone();
                        }
                    }
                });
                match port_counts(&ty, &params) {
                    Some((ni, no)) => {
                        ui.small(format!("{ni} in / {no} out"));
                    }
                    None => {
                        ui.colored_label(egui::Color32::from_rgb(0xC0, 0x39, 0x2B), "bad params — block won't build");
                    }
                }
                ui.add_space(4.0);
                self.param_table(ui, &path);
                ui.add_space(4.0);
                if ui.button("Delete block").clicked() {
                    self.diagram.remove_block(&path);
                    self.canvas.sel = Sel::None;
                    self.dirty = true;
                }
            }
            None => {
                ui.label("no block selected");
            }
        }
    }

    fn traces_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.status);
            ui.separator();
            ui.small(format!("{} blocks, {} wires", self.diagram.blocks.len(), self.diagram.wires.len()));
        });
        ui.separator();
        if self.traces.is_empty() {
            ui.label("Run ▶ to collect scope / to_workspace traces");
            return;
        }
        ui.columns(2, |cols| {
            cols[0].set_max_width(180.0);
            egui::ScrollArea::vertical().id_salt("traces").show(&mut cols[0], |ui| {
                for (i, tr) in self.traces.iter().enumerate() {
                    if ui.selectable_label(self.trace_sel == Some(i), format!("{} ({} pts)", tr.name, tr.t.len())).clicked() {
                        self.trace_sel = Some(i);
                    }
                }
            });
            if let Some(tr) = self.trace_sel.and_then(|i| self.traces.get(i)) {
                plot::trace_plot(&mut cols[1], tr);
            }
        });
    }
}

impl eframe::App for StudioApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::left("palette").resizable(true).default_size(160.0).show(ui, |ui| self.palette(ui));
        egui::Panel::right("inspector").resizable(true).default_size(230.0).show(ui, |ui| self.inspector(ui));
        egui::Panel::bottom("traces").resizable(true).default_size(190.0).show(ui, |ui| self.traces_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if canvas::ui(ui, &mut self.diagram, &mut self.canvas) {
                self.dirty = true;
            }
        });
        // Floating params window (double-click on a block).
        if let Some(path) = self.canvas.param_win.clone() {
            if self.diagram.block(&path).is_some() {
                let mut open = true;
                egui::Window::new(format!("Parameters — {path}"))
                    .open(&mut open)
                    .collapsible(false)
                    .default_width(240.0)
                    .show(ui.ctx(), |ui| self.param_table(ui, &path));
                if !open {
                    self.canvas.param_win = None;
                }
            } else {
                self.canvas.param_win = None;
            }
        }
    }
}
