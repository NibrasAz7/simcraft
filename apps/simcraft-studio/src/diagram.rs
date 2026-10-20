//! The diagram canvas model: blocks with positions, wires between ports,
//! and the `.simcraft` document round-trip.
//!
//! Block positions live in a top-level `"layout"` key — `simmodel`
//! ignores unknown top-level keys, so Studio files stay loadable by
//! `simcraft-cli` and the engine unchanged. Ports are 1-based in the file
//! (`"path/port"`), matching `simmodel`'s wire endpoints.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value as Json};
use simcore::{Param, Params, SimConfig, SimError};

/// One placed block.
pub struct CanvasBlock {
    pub path: String,
    pub type_name: String,
    /// Canvas centre position.
    pub pos: egui::Pos2,
    pub params: Params,
}

/// A port-to-port edge; ports are 1-based.
pub struct CanvasWire {
    pub src: String,
    pub src_port: usize,
    pub dst: String,
    pub dst_port: usize,
}

/// Everything a `.simcraft` file round-trips plus the editor layout.
#[derive(Default)]
pub struct Diagram {
    pub blocks: Vec<CanvasBlock>,
    pub wires: Vec<CanvasWire>,
    pub solver: SimConfig,
    /// Free-text comment preserved on load/save.
    pub comment: Option<String>,
}

/// Palette groups: category → document type names (labels come from
/// `simblocks::Factory::catalog`).
pub const GROUPS: &[(&str, &[&str])] = &[
    ("Sources", &["constant", "step", "sine", "ramp", "clock"]),
    ("Sinks", &["scope", "to_workspace"]),
    ("Math", &["gain", "sum", "product", "abs", "saturation", "dead_zone", "switch"]),
    ("Logic", &["relational", "logical_operator"]),
    ("Continuous", &["integrator", "derivative", "transfer_fcn", "pid", "transport_delay"]),
    ("Discrete", &["unit_delay", "zero_order_hold"]),
    ("Routing", &["mux", "demux"]),
];

/// The params a freshly-placed block carries — the defaults documented on
/// each block's `new` (the public API doesn't expose them). Keeps the
/// inspector's param table useful and `transfer_fcn`'s required `den` set.
pub fn default_params(type_name: &str) -> Params {
    let mut p = Params::new();
    let mut num = |k: &str, v: f64| {
        p.insert(k.to_string(), Param::Num(v));
    };
    match type_name {
        "constant" => num("value", 1.0),
        "step" => {
            num("time", 0.0);
            num("before", 0.0);
            num("after", 1.0);
        }
        "sine" => {
            num("amplitude", 1.0);
            num("frequency", 1.0);
            num("phase", 0.0);
            num("bias", 0.0);
        }
        "ramp" => {
            num("slope", 1.0);
            num("start_time", 0.0);
            num("initial", 0.0);
        }
        "gain" => num("gain", 1.0),
        "sum" => {
            p.insert("signs".into(), Param::Str("++".into()));
        }
        "product" => {
            p.insert("ops".into(), Param::Str("**".into()));
        }
        "saturation" | "dead_zone" => {
            num("lower", -0.5);
            num("upper", 0.5);
        }
        "relational" => {
            p.insert("op".into(), Param::Str("<=".into()));
        }
        "logical_operator" => {
            num("inputs", 2.0);
            p.insert("op".into(), Param::Str("and".into()));
        }
        "pid" => {
            num("kp", 1.0);
            num("ki", 0.0);
            num("kd", 0.0);
            num("n", 100.0);
        }
        "transport_delay" => {
            num("delay", 1.0);
            num("initial_output", 0.0);
        }
        "switch" => {
            num("threshold", 0.0);
            p.insert("criteria".into(), Param::Str("u2>=threshold".into()));
        }
        "integrator" => num("x0", 0.0),
        "transfer_fcn" => {
            p.insert("num".into(), Param::Vec(vec![1.0]));
            p.insert("den".into(), Param::Vec(vec![1.0, 1.0]));
        }
        "unit_delay" => {
            num("sample_time", 1.0);
            num("x0", 0.0);
        }
        "zero_order_hold" => num("sample_time", 1.0),
        "mux" => num("inputs", 2.0),
        "demux" => num("outputs", 2.0),
        "to_workspace" => {
            p.insert("var".into(), Param::Str("simout".into()));
        }
        _ => {}
    }
    p
}

/// `(inputs, outputs)` for `type_name` under `params` — built through the
/// public factory so port counts always match what `Model::compile` sees.
/// `None` when the block fails to build (bad params).
pub fn port_counts(type_name: &str, params: &Params) -> Option<(usize, usize)> {
    use simcore::BlockFactory;
    let f = simblocks::Factory;
    f.make(type_name, params, "palette/probe").ok().map(|b| (b.inputs().len(), b.n_outputs()))
}

/// `Param` → document JSON (`number | [numbers] | string | bool`).
fn param_json(p: &Param) -> Json {
    match p {
        Param::Num(n) => json!(n),
        Param::Vec(v) => json!(v),
        Param::Str(s) => json!(s),
        Param::Bool(b) => json!(b),
    }
}

/// Editor JSON (number/bool/string/array-of-numbers) → `Param`, same rules
/// as `simmodel`'s loader.
pub fn json_to_param(v: &Json) -> Result<Param, String> {
    match v {
        Json::Number(n) => n.as_f64().map(Param::Num).ok_or_else(|| "number out of range".to_string()),
        Json::Bool(b) => Ok(Param::Bool(*b)),
        Json::String(s) => Ok(Param::Str(s.clone())),
        Json::Array(a) => {
            a.iter().map(|x| x.as_f64().ok_or_else(|| "arrays must be numbers".to_string())).collect::<Result<Vec<f64>, _>>().map(Param::Vec)
        }
        _ => Err("params must be numbers, arrays, strings or bools".to_string()),
    }
}

/// `"path/port"` (1-based) → `(path, port)`; `None` on malformed input.
fn split_endpoint(s: &str) -> Option<(String, usize)> {
    let (p, port) = s.rsplit_once('/')?;
    let n: usize = port.parse().ok()?;
    if n == 0 {
        return None;
    }
    Some((p.to_string(), n))
}

impl Diagram {
    pub fn block(&self, path: &str) -> Option<&CanvasBlock> {
        self.blocks.iter().find(|b| b.path == path)
    }

    pub fn block_mut(&mut self, path: &str) -> Option<&mut CanvasBlock> {
        self.blocks.iter_mut().find(|b| b.path == path)
    }

    /// First unused `type_name`/`type_name2`/… path.
    pub fn unique_path(&self, type_name: &str) -> String {
        if self.block(type_name).is_none() {
            return type_name.to_string();
        }
        let mut n = 2usize;
        loop {
            let p = format!("{type_name}{n}");
            if self.block(&p).is_none() {
                return p;
            }
            n += 1;
        }
    }

    /// Place a block of `type_name` at `pos` with documented default params.
    pub fn add_block(&mut self, type_name: &str, pos: egui::Pos2) -> String {
        let path = self.unique_path(type_name);
        self.blocks.push(CanvasBlock { path: path.clone(), type_name: type_name.to_string(), pos, params: default_params(type_name) });
        path
    }

    /// Remove `path` and every wire touching it.
    pub fn remove_block(&mut self, path: &str) {
        self.blocks.retain(|b| b.path != path);
        self.wires.retain(|w| w.src != path && w.dst != path);
    }

    /// Rename `old` → `new`, rewriting wire endpoints. No-op on collision.
    pub fn rename_block(&mut self, old: &str, new: &str) -> bool {
        if new.is_empty() || self.block(new).is_some() || self.block(old).is_none() {
            return false;
        }
        if let Some(b) = self.block_mut(old) {
            b.path = new.to_string();
        }
        for w in &mut self.wires {
            if w.src == old {
                w.src = new.to_string();
            }
            if w.dst == old {
                w.dst = new.to_string();
            }
        }
        true
    }

    /// Connect `src/src_port` → `dst/dst_port`, replacing any wire already
    /// on the destination input.
    pub fn add_wire(&mut self, src: &str, src_port: usize, dst: &str, dst_port: usize) {
        if src == dst || src_port == 0 || dst_port == 0 {
            return;
        }
        self.wires.retain(|w| !(w.dst == dst && w.dst_port == dst_port));
        self.wires.push(CanvasWire { src: src.to_string(), src_port, dst: dst.to_string(), dst_port });
    }

    /// Drop wires whose ports no longer exist (after a param edit that
    /// changed a port count, e.g. `sum.signs` or `mux.inputs`).
    pub fn prune_wires(&mut self) {
        let counts: BTreeMap<String, Option<(usize, usize)>> =
            self.blocks.iter().map(|b| (b.path.clone(), port_counts(&b.type_name, &b.params))).collect();
        self.wires.retain(|w| {
            let so = counts.get(&w.src).copied().flatten().map(|(_, o)| o).unwrap_or(0);
            let di = counts.get(&w.dst).copied().flatten().map(|(i, _)| i).unwrap_or(0);
            w.src_port <= so && w.dst_port <= di
        });
    }

    /// Serialize to `.simcraft` text — `simmodel::Document::parse`-able,
    /// with a `"layout"` key carrying block positions.
    pub fn to_json(&self) -> String {
        let mut obj = Map::new();
        obj.insert("format".into(), json!("simcraft"));
        obj.insert("version".into(), json!(1));
        if let Some(c) = &self.comment {
            obj.insert("comment".into(), json!(c));
        }
        obj.insert(
            "solver".into(),
            json!({
                "solver": self.solver.solver.name(), "dt": self.solver.dt, "tstop": self.solver.tstop,
                "rtol": self.solver.rtol, "atol": self.solver.atol,
                "min_step": self.solver.min_step, "max_step": self.solver.max_step
            }),
        );
        let blocks: Vec<Json> = self
            .blocks
            .iter()
            .map(|b| {
                let params: Map<String, Json> = b.params.iter().map(|(k, v)| (k.clone(), param_json(v))).collect();
                json!({"path": b.path, "type": b.type_name, "params": Json::Object(params)})
            })
            .collect();
        obj.insert("blocks".into(), Json::Array(blocks));
        let wires: Vec<Json> =
            self.wires.iter().map(|w| json!({"from": format!("{}/{}", w.src, w.src_port), "to": format!("{}/{}", w.dst, w.dst_port)})).collect();
        obj.insert("wires".into(), Json::Array(wires));
        let layout: Map<String, Json> = self.blocks.iter().map(|b| (b.path.clone(), json!([b.pos.x as f64, b.pos.y as f64]))).collect();
        obj.insert("layout".into(), Json::Object(layout));
        serde_json::to_string_pretty(&Json::Object(obj)).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse `.simcraft` text: validates through `simmodel::Document` (the
    /// same checks `simcraft-cli` applies) and reads `"layout"` positions.
    /// Blocks without a layout entry get an auto-layout slot.
    pub fn from_text(text: &str) -> Result<Diagram, SimError> {
        let doc = simmodel::Document::parse(text)?;
        let root: Json = serde_json::from_str(text).map_err(|e| SimError::Msg(format!("invalid JSON: {e}")))?;
        let layout = root.get("layout").and_then(Json::as_object);
        let pos_of = |path: &str| -> Option<egui::Pos2> {
            let a = layout.and_then(|l| l.get(path)).and_then(Json::as_array)?;
            Some(egui::pos2(a.first().and_then(Json::as_f64).unwrap_or(0.0) as f32, a.get(1).and_then(Json::as_f64).unwrap_or(0.0) as f32))
        };
        let mut d = Diagram { solver: doc.solver, comment: root.get("comment").and_then(Json::as_str).map(str::to_string), ..Default::default() };
        for (path, type_name, params) in doc.blocks() {
            let pos = pos_of(path).unwrap_or(egui::pos2(0.0, 0.0));
            d.blocks.push(CanvasBlock { path: path.to_string(), type_name: type_name.to_string(), pos, params: params.clone() });
        }
        for (from, to) in doc.wires() {
            if let (Some((src, sp)), Some((dst, dp))) = (split_endpoint(from), split_endpoint(to)) {
                d.wires.push(CanvasWire { src, src_port: sp, dst, dst_port: dp });
            }
        }
        if d.blocks.iter().any(|b| b.pos == egui::pos2(0.0, 0.0)) {
            d.auto_layout_missing();
        }
        Ok(d)
    }

    /// Give every block still at the origin a layered position: `x` follows
    /// wire depth (cycles capped), `y` stacks within the layer.
    fn auto_layout_missing(&mut self) {
        let mut depth: BTreeMap<String, usize> = BTreeMap::new();
        // Iterate relaxation bounded by block count (cycles just stop rising).
        for _ in 0..self.blocks.len().max(1) {
            for w in &self.wires {
                let d = depth.get(&w.src).copied().unwrap_or(0) + 1;
                let cur = depth.get(&w.dst).copied().unwrap_or(0);
                if d > cur {
                    depth.insert(w.dst.clone(), d);
                }
            }
        }
        let mut in_layer: BTreeMap<usize, usize> = BTreeMap::new();
        for b in &mut self.blocks {
            if b.pos == egui::pos2(0.0, 0.0) {
                let dep = *depth.get(&b.path).unwrap_or(&0);
                let row = in_layer.entry(dep).or_insert(0);
                b.pos = egui::pos2(80.0 + dep as f32 * 200.0, 80.0 + *row as f32 * 110.0);
                *row += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pendulum() -> Diagram {
        let mut d = Diagram::default();
        for (p, t, x, y) in [
            ("theta", "integrator", 100.0f32, 100.0f32),
            ("gl", "gain", 320.0, 100.0),
            ("theta_dot", "integrator", 540.0, 100.0),
            ("angle_scope", "scope", 100.0, 260.0),
            ("save_theta", "to_workspace", 320.0, 260.0),
        ] {
            d.blocks.push(CanvasBlock { path: p.into(), type_name: t.into(), pos: egui::pos2(x, y), params: default_params(t) });
        }
        d.add_wire("theta", 1, "gl", 1);
        d.add_wire("gl", 1, "theta_dot", 1);
        d.add_wire("theta_dot", 1, "theta", 1);
        d.add_wire("theta", 1, "angle_scope", 1);
        d.add_wire("theta", 1, "save_theta", 1);
        d
    }

    #[test]
    fn round_trip_preserves_blocks_wires_and_layout() {
        let d = pendulum();
        let text = d.to_json();
        let back = Diagram::from_text(&text).unwrap();
        assert_eq!(back.blocks.len(), 5);
        assert_eq!(back.wires.len(), 5);
        let theta = back.block("theta").unwrap();
        assert_eq!(theta.pos, egui::pos2(100.0, 100.0));
        assert_eq!(theta.type_name, "integrator");
        assert!(back.wires.iter().any(|w| w.src == "theta" && w.src_port == 1 && w.dst == "gl" && w.dst_port == 1));
    }

    #[test]
    fn saved_json_loads_through_simmodel() {
        let d = pendulum();
        let doc = simmodel::Document::parse(&d.to_json()).unwrap();
        let m = doc.to_model(&simblocks::Factory).unwrap();
        assert_eq!(m.len(), 5);
        assert_eq!(m.wires().len(), 5);
    }

    #[test]
    fn auto_layout_for_layoutless_files() {
        let text = r#"{"format":"simcraft","version":1,
            "blocks":[{"path":"a","type":"constant"},{"path":"b","type":"scope"}],
            "wires":[{"from":"a/1","to":"b/1"}]}"#;
        let d = Diagram::from_text(text).unwrap();
        let (a, b) = (d.block("a").unwrap(), d.block("b").unwrap());
        assert!(a.pos.x < b.pos.x, "source should sit left of its sink");
    }

    #[test]
    fn port_counts_match_block_library() {
        for (t, want) in [
            ("constant", (0, 1)),
            ("scope", (1, 0)),
            ("to_workspace", (1, 0)),
            ("gain", (1, 1)),
            ("sum", (2, 1)),
            ("switch", (3, 1)),
            ("integrator", (1, 1)),
            ("transfer_fcn", (1, 1)),
            ("unit_delay", (1, 1)),
            ("mux", (2, 1)),
            ("demux", (1, 2)),
        ] {
            assert_eq!(port_counts(t, &default_params(t)), Some(want), "{t}");
        }
        // Param-driven port counts.
        let mut p = Params::new();
        p.insert("signs".into(), Param::Str("+-+".into()));
        assert_eq!(port_counts("sum", &p), Some((3, 1)));
    }

    #[test]
    fn add_wire_replaces_destination_driver() {
        let mut d = Diagram::default();
        let a = d.add_block("constant", egui::pos2(0.0, 0.0));
        let b = d.add_block("constant", egui::pos2(0.0, 0.0));
        let s = d.add_block("scope", egui::pos2(0.0, 0.0));
        d.add_wire(&a, 1, &s, 1);
        d.add_wire(&b, 1, &s, 1);
        assert_eq!(d.wires.len(), 1);
        assert_eq!(d.wires[0].src, b);
    }

    #[test]
    fn shipped_pendulum_opens_with_layout() {
        let d = Diagram::from_text(include_str!("../../../examples/pendulum.simcraft")).unwrap();
        assert_eq!(d.blocks.len(), 5);
        assert_eq!(d.wires.len(), 5);
        assert_ne!(d.block("theta").unwrap().pos, egui::pos2(0.0, 0.0));
        // Saved back out, it still builds into a runnable model.
        let doc = simmodel::Document::parse(&d.to_json()).unwrap();
        assert!(doc.to_sim(&simblocks::Factory).is_ok());
    }

    #[test]
    fn prune_drops_out_of_range_ports() {
        let mut d = Diagram::default();
        let a = d.add_block("constant", egui::pos2(0.0, 0.0));
        let s = d.add_block("sum", egui::pos2(200.0, 0.0));
        d.add_wire(&a, 1, &s, 2);
        if let Some(b) = d.block_mut(&s) {
            b.params.insert("signs".into(), Param::Str("+".into()));
        }
        d.prune_wires();
        assert!(d.wires.is_empty());
    }
}
