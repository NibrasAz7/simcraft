//! The SimCraft engine.
//!
//! Every user-visible behaviour is a command with a stable id
//! (`model.load`, `sim.run`, `block.set`, …) run with JSON parameters via
//! [`Session::execute`] — the same ArtCraft pattern as MatCraft's engine,
//! reachable from the CLI, the control channel and MCP.
//!
//! A [`Session`] owns one loaded `.simcraft` model plus an optional linked
//! `matlang::Session`: after `sim.run`, every `to_workspace` block's trace
//! lands in that workspace as a `{t, y}` struct and the run summary as the
//! `out` struct — `plot(out.t, out.y)` then just works.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod cmd;

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use serde_json::Value as Json;

use matcore::Matrix;
use matlang::Value;
use simcore::{Model, Params, Sim, SimConfig};
use simmodel::Document;

pub use cmd::{command_specs, find_command, CommandSpec};
pub use simblocks::Trace;
pub use simcore::{SimError, Solver};

/// Errors commands return; they reach the user as a message, never a panic.
#[derive(Debug)]
pub enum EngineError {
    UnknownCommand(String),
    BadParams {
        cmd: String,
        msg: String,
    },
    Sim(SimError),
    /// A command panicked; the loaded model was dropped (the document it
    /// came from is safe to reload) and the session stays usable.
    Panic(String),
    Other(String),
}

impl EngineError {
    pub fn bad_params(cmd: &str, msg: impl Into<String>) -> Self {
        EngineError::BadParams { cmd: cmd.to_string(), msg: msg.into() }
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::UnknownCommand(id) => write!(f, "unknown command `{id}`"),
            EngineError::BadParams { cmd, msg } => write!(f, "invalid parameters for `{cmd}`: {msg}"),
            EngineError::Sim(e) => write!(f, "{e}"),
            EngineError::Panic(cmd) => write!(f, "internal error in `{cmd}` (the loaded model was dropped)"),
            EngineError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<SimError> for EngineError {
    fn from(e: SimError) -> Self {
        EngineError::Sim(e)
    }
}

fn no_model() -> SimError {
    SimError::Msg("no model loaded — `model.load` a `.simcraft` file first".into())
}

/// Solver overrides accepted by `sim.run` (`{solver?, dt?, tstop?}`); unset
/// fields fall back to the document's `solver` block.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunOpts {
    pub solver: Option<Solver>,
    pub dt: Option<f64>,
    pub tstop: Option<f64>,
}

/// One sink trace lifted out of a finished run.
pub struct NamedTrace {
    /// The `to_workspace` block's `var`, or the scope's block path.
    pub name: String,
    /// True for `to_workspace` sinks — these also bind their `var`
    /// top-level in the linked MatCraft workspace.
    pub to_workspace: bool,
    pub trace: Trace,
}

/// What a `sim.run` hands back: the sampled time grid plus every sink trace.
pub struct SimRun {
    pub t: Vec<f64>,
    /// Final flat continuous state.
    pub x: Vec<f64>,
    pub steps: u64,
    pub solver: Solver,
    pub dt: f64,
    pub tstop: f64,
    pub traces: Vec<NamedTrace>,
}

/// `model.check` results — the model compiled and the solver grid is valid.
pub struct CheckInfo {
    pub blocks: usize,
    pub wires: usize,
    pub states: usize,
    pub steps: u64,
    pub solver: SimConfig,
}

/// A SimCraft session: the loaded document + built model, optionally linked
/// to a MatCraft workspace that receives `to_workspace` traces.
pub struct Session {
    doc: Option<Document>,
    /// Built from `doc`; kept so `model.list`/`block.*` stay cheap and so
    /// `sim.run` can run more than once.
    model: Option<Model>,
    /// Linked MatCraft session — the model's compute context. `None` until
    /// [`Session::link`] (hosts that already own a matlang session publish
    /// traces into it directly via [`publish`] instead).
    mat: Option<matlang::Session>,
}

impl Session {
    pub fn new() -> Self {
        Session { doc: None, model: None, mat: None }
    }

    /// Attach a MatCraft session as this model's workspace (`session.link`).
    pub fn link(&mut self, mat: matlang::Session) {
        self.mat = Some(mat);
    }

    /// Detach the linked MatCraft session (`session.unlink`).
    pub fn unlink(&mut self) {
        self.mat = None;
    }

    /// The linked MatCraft session, when attached.
    pub fn mat(&self) -> Option<&matlang::Session> {
        self.mat.as_ref()
    }

    /// True when a model is loaded (`model.load` succeeded).
    pub fn loaded(&self) -> bool {
        self.model.is_some()
    }

    /// The loaded document (for `model.list`/`block.get`).
    pub fn doc(&self) -> Option<&Document> {
        self.doc.as_ref()
    }

    /// Parse and build a `.simcraft` file. Replaces the current model.
    /// Returns the block count.
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<usize, SimError> {
        self.load_document(Document::load(path.as_ref())?)
    }

    /// Build `doc` into a runnable model. Replaces the current model.
    pub fn load_document(&mut self, doc: Document) -> Result<usize, SimError> {
        let model = doc.to_model(&simblocks::Factory)?;
        let n = model.len();
        self.doc = Some(doc);
        self.model = Some(model);
        Ok(n)
    }

    /// Rebuild the model from the document — after `block.set`, or to
    /// recover a runnable model after a failed run consumed one.
    fn rebuild(&mut self) -> Result<(), SimError> {
        match &self.doc {
            Some(doc) => {
                self.model = Some(doc.to_model(&simblocks::Factory)?);
                Ok(())
            }
            None => Err(no_model()),
        }
    }

    /// `block.set`: merge `params` into a block's parameter map, then rebuild
    /// so bad values surface here, not at the next `sim.run`.
    pub fn set_block_params(&mut self, path: &str, params: &Params) -> Result<(), SimError> {
        match self.doc.as_mut() {
            Some(doc) => {
                doc.set_block_params(path, params)?;
                self.rebuild()
            }
            None => Err(no_model()),
        }
    }

    /// `model.check`: build a fresh copy and compile it through `Sim::new`
    /// (which also validates the solver grid against sample times) — the
    /// stored model is left untouched.
    pub fn check(&self) -> Result<CheckInfo, SimError> {
        let doc = self.doc.as_ref().ok_or_else(no_model)?;
        let model = doc.to_model(&simblocks::Factory)?;
        let cfg = doc.solver;
        let sim = Sim::new(model, cfg)?;
        let m = sim.model();
        Ok(CheckInfo { blocks: m.len(), wires: m.wires().len(), states: sim.state().len(), steps: (cfg.tstop / cfg.dt).round() as u64, solver: cfg })
    }

    /// `sim.run`: run the loaded model over `[0, tstop]`, collect every
    /// Scope/ToWorkspace trace and publish them into the linked MatCraft
    /// session when one is attached. The model is kept afterwards (states
    /// reset on the next run — `compile` re-reads `x0`).
    pub fn run(&mut self, ov: &RunOpts) -> Result<SimRun, SimError> {
        let mut cfg = match &self.doc {
            Some(d) => d.solver,
            None => return Err(no_model()),
        };
        if let Some(s) = ov.solver {
            cfg.solver = s;
        }
        if let Some(dt) = ov.dt {
            cfg.dt = dt;
        }
        if let Some(t) = ov.tstop {
            cfg.tstop = t;
        }
        let model = self.model.take().ok_or_else(no_model)?;
        let mut sim = match Sim::new(model, cfg) {
            Ok(s) => s,
            Err(e) => {
                // The moved-out model was dropped; restore a fresh build so
                // the session isn't left half-loaded.
                let _ = self.rebuild();
                return Err(e);
            }
        };
        let result = sim.run();
        let mut model = sim.into_model();
        let out = match result {
            Ok(o) => o,
            Err(e) => {
                self.model = Some(model);
                return Err(e);
            }
        };
        let traces = collect_traces(&mut model);
        self.model = Some(model);
        let run = SimRun { t: out.t, x: out.x, steps: out.steps, solver: out.solver, dt: out.dt, tstop: cfg.tstop, traces };
        if let Some(m) = self.mat.as_mut() {
            publish(m, &run);
        }
        Ok(run)
    }

    /// Run command `id` with JSON `params`. A panic drops the loaded model
    /// (the linked workspace is rolled back) and returns an error — the
    /// session stays usable.
    pub fn execute(&mut self, id: &str, params: Json) -> Result<Json, EngineError> {
        let spec = find_command(id).ok_or_else(|| EngineError::UnknownCommand(id.to_string()))?;
        let mat_snapshot = self.mat.clone();
        match catch_unwind(AssertUnwindSafe(|| (spec.run)(self, &params))) {
            Ok(result) => result,
            Err(_) => {
                self.doc = None;
                self.model = None;
                self.mat = mat_snapshot;
                Err(EngineError::Panic(id.to_string()))
            }
        }
    }

    /// Every registered command.
    pub fn commands(&self) -> &'static [CommandSpec] {
        command_specs()
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

/// Pull every sink trace out of a finished model. `to_workspace` blocks key
/// their trace by `var`; scopes by block path.
fn collect_traces(model: &mut Model) -> Vec<NamedTrace> {
    let mut traces = Vec::new();
    for name in model.block_names().to_vec() {
        if let Some(tw) = model.block_mut::<simblocks::ToWorkspace>(&name) {
            let trace = std::mem::replace(&mut tw.trace, Trace { t: Vec::new(), y: Vec::new() });
            traces.push(NamedTrace { name: tw.var.clone(), to_workspace: true, trace });
        } else if let Some(sc) = model.block_mut::<simblocks::Scope>(&name) {
            let trace = std::mem::replace(&mut sc.trace, Trace { t: Vec::new(), y: Vec::new() });
            traces.push(NamedTrace { name, to_workspace: false, trace });
        }
    }
    traces
}

/// `run.t`/`trace.y` → MatCraft values: `t` is an N×1 column, `y` an N×W
/// matrix (`y[k][i]` sample-major → column-major elements).
fn t_value(t: &[f64]) -> Value {
    Value::Mat(Matrix::col_vector(t.to_vec()))
}

fn y_value(tr: &Trace) -> Value {
    let n = tr.y.len();
    let w = tr.y.first().map(|r| r.len()).unwrap_or(0);
    let mut data = Vec::with_capacity(n.saturating_mul(w));
    for i in 0..w {
        for row in &tr.y {
            data.push(row.get(i).copied().unwrap_or(0.0));
        }
    }
    match Matrix::from_col_major(n, w, data) {
        Ok(m) => Value::Mat(m),
        Err(_) => Value::Mat(Matrix::zeros(0, 0)),
    }
}

/// A trace name usable as a MatCraft field/variable: alnum + `_`, never
/// starting with a digit (nested paths like `sub/scope` fold to
/// `sub_scope`).
fn field_name(s: &str) -> Option<String> {
    let mut out: String = s.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Write a finished run into a MatCraft workspace — the To Workspace
/// contract shared by this engine's `sim.run` and MatCraft's `sim.run`:
///
/// * `out` — a struct: `out.t` is the N×1 time vector, `out.y` the first
///   logged trace, and `out.<name>` each trace's N×W values (so
///   `out.theta(1:10)` indexes straight into the data).
/// * `<var>` — each `to_workspace` block's `{t, y}` struct, bound under its
///   `var` name (MATLAB-style To Workspace semantics).
pub fn publish(lang: &mut matlang::Session, run: &SimRun) {
    let mut out_map: BTreeMap<String, Value> = BTreeMap::new();
    out_map.insert("t".to_string(), t_value(&run.t));
    if let Some(first) = run.traces.first() {
        out_map.insert("y".to_string(), y_value(&first.trace));
    }
    for tr in &run.traces {
        if tr.to_workspace {
            let mut rec: BTreeMap<String, Value> = BTreeMap::new();
            rec.insert("t".to_string(), t_value(&tr.trace.t));
            rec.insert("y".to_string(), y_value(&tr.trace));
            lang.set(tr.name.clone(), Value::Struct(matlang::interp::StructArray::from_map(rec)));
        }
        if let Some(name) = field_name(&tr.name) {
            out_map.insert(name, y_value(&tr.trace));
        }
    }
    lang.set("out", Value::Struct(matlang::interp::StructArray::from_map(out_map)));
}

/// The `sim.run` reply: `{ok, solver, dt, tstop, steps, t, out}` where
/// `out` maps each trace name to `{t, y}` (`y` is sample-major rows).
pub fn run_json(run: &SimRun) -> Json {
    let out: serde_json::Map<String, Json> =
        run.traces.iter().map(|tr| (tr.name.clone(), serde_json::json!({"t": tr.trace.t, "y": tr.trace.y}))).collect();
    serde_json::json!({
        "ok": true,
        "solver": run.solver.name(),
        "dt": run.dt,
        "tstop": run.tstop,
        "steps": run.steps,
        "t": run.t,
        "out": Json::Object(out),
    })
}

/// The `model.check` reply.
pub fn check_json(info: &CheckInfo) -> Json {
    serde_json::json!({
        "ok": true,
        "blocks": info.blocks,
        "wires": info.wires,
        "states": info.states,
        "steps": info.steps,
        "solver": {"solver": info.solver.solver.name(), "dt": info.solver.dt, "tstop": info.solver.tstop},
    })
}

/// The `block.list` reply — the `simblocks::Factory` catalog.
pub fn catalog_json() -> Json {
    let blocks: Vec<Json> = simblocks::Factory::catalog().iter().map(|b| serde_json::json!({"type": b.type_name, "label": b.label})).collect();
    serde_json::json!({"blocks": blocks})
}
