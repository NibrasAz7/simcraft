//! simmodel — the `.simcraft` document format (L2).
//!
//! A `.simcraft` file is diff-friendly JSON:
//! ```json
//! {
//!   "format": "simcraft",
//!   "version": 1,
//!   "solver": {"solver": "rk4", "dt": 0.001, "tstop": 10.0},
//!   "blocks": [
//!     {"path": "src", "type": "sine", "params": {"frequency": 2.0}},
//!     {"path": "out", "type": "scope"}
//!   ],
//!   "wires": [
//!     {"from": "src/1", "to": "out/1"}
//!   ]
//! }
//! ```
//! Ports are 1-based in the document (converted to 0-based internally).
//! The `simcraft` format tag and `version` are checked on load; unknown
//! top-level keys are ignored so newer files degrade gracefully.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use simcore::{BlockFactory, Model, Param, Params, Sim, SimConfig, SimError, Solver};
use std::collections::BTreeMap;

/// A parsed `.simcraft` document. Blocks/wires are kept as the raw records
/// until [`Document::to_model`] builds them through a `BlockFactory`.
pub struct Document {
    pub solver: SimConfig,
    blocks: Vec<BlockRec>,
    wires: Vec<WireRec>,
}

struct BlockRec {
    path: String,
    type_name: String,
    params: Params,
}

struct WireRec {
    src: String,
    dst: String,
}

fn msg(s: impl Into<String>) -> SimError {
    SimError::Msg(s.into())
}

fn json_to_param(v: &serde_json::Value) -> Result<Param, SimError> {
    match v {
        serde_json::Value::Number(n) => Ok(Param::Num(n.as_f64().ok_or_else(|| msg("param number out of range"))?)),
        serde_json::Value::String(s) => Ok(Param::Str(s.clone())),
        serde_json::Value::Bool(b) => Ok(Param::Bool(*b)),
        serde_json::Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for e in a {
                out.push(e.as_f64().ok_or_else(|| msg("param arrays must be numbers"))?);
            }
            Ok(Param::Vec(out))
        }
        _ => Err(msg("params must be numbers, arrays, strings or bools")),
    }
}

/// `path/port` (1-based port) → `(path, port0)`.
fn split_endpoint(s: &str) -> Result<(String, usize), SimError> {
    let (p, port) = s.rsplit_once('/').ok_or_else(|| msg(format!("wire endpoint `{s}` needs a /port suffix")))?;
    let n: usize = port.parse().map_err(|_| msg(format!("wire endpoint `{s}` has a non-numeric port")))?;
    if n == 0 {
        return Err(msg(format!("wire endpoint `{s}`: ports are 1-based")));
    }
    Ok((p.to_string(), n - 1))
}

impl Document {
    /// Parse a `.simcraft` JSON document. Everything untrusted is checked
    /// here; building still happens in [`Document::to_model`].
    pub fn parse(text: &str) -> Result<Document, SimError> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| msg(format!("invalid JSON: {e}")))?;
        let obj = v.as_object().ok_or_else(|| msg("model must be a JSON object"))?;
        match obj.get("format").and_then(|f| f.as_str()) {
            Some("simcraft") => {}
            Some(other) => return Err(msg(format!("not a simcraft model (format: {other})"))),
            None => return Err(msg("missing \"format\": \"simcraft\"")),
        }
        match obj.get("version").and_then(|f| f.as_u64()) {
            None | Some(1) => {}
            Some(n) => return Err(msg(format!("unsupported simcraft version {n}"))),
        }

        let mut solver = SimConfig::default();
        if let Some(s) = obj.get("solver").and_then(|s| s.as_object()) {
            if let Some(name) = s.get("solver").and_then(|x| x.as_str()) {
                solver.solver = Solver::parse(name).ok_or_else(|| msg(format!("unknown solver `{name}` (have: euler, rk4)")))?;
            }
            if let Some(dt) = s.get("dt").and_then(|x| x.as_f64()) {
                solver.dt = dt;
            }
            if let Some(t) = s.get("tstop").and_then(|x| x.as_f64()) {
                solver.tstop = t;
            }
        }

        let mut blocks = Vec::new();
        for b in obj.get("blocks").and_then(|b| b.as_array()).cloned().unwrap_or_default() {
            let bo = b.as_object().ok_or_else(|| msg("each block must be an object"))?;
            let path = bo.get("path").and_then(|x| x.as_str()).ok_or_else(|| msg("block missing \"path\""))?.to_string();
            let type_name = bo.get("type").and_then(|x| x.as_str()).ok_or_else(|| msg(format!("block `{path}` missing \"type\"")))?.to_string();
            let mut params = BTreeMap::new();
            if let Some(po) = bo.get("params").and_then(|x| x.as_object()) {
                for (k, pv) in po {
                    params.insert(k.clone(), json_to_param(pv)?);
                }
            }
            blocks.push(BlockRec { path, type_name, params });
        }

        let mut wires = Vec::new();
        for w in obj.get("wires").and_then(|w| w.as_array()).cloned().unwrap_or_default() {
            let wo = w.as_object().ok_or_else(|| msg("each wire must be an object"))?;
            let src = wo.get("from").and_then(|x| x.as_str()).ok_or_else(|| msg("wire missing \"from\""))?.to_string();
            let dst = wo.get("to").and_then(|x| x.as_str()).ok_or_else(|| msg("wire missing \"to\""))?.to_string();
            wires.push(WireRec { src, dst });
        }

        Ok(Document { solver, blocks, wires })
    }

    /// Load from a file path.
    pub fn load(path: &std::path::Path) -> Result<Document, SimError> {
        let text = std::fs::read_to_string(path).map_err(|e| msg(format!("cannot read {}: {e}", path.display())))?;
        Document::parse(&text)
    }

    /// Instantiate every block through `factory` and wire the graph.
    /// Validation continues in `Model::compile` / `Sim::new`.
    pub fn to_model(&self, factory: &dyn BlockFactory) -> Result<Model, SimError> {
        let mut m = Model::new();
        for b in &self.blocks {
            let block = factory.make(&b.type_name, &b.params, &b.path)?;
            m.add_block(&b.path, block)?;
        }
        for w in &self.wires {
            let (sp, sport) = split_endpoint(&w.src)?;
            let (dp, dport) = split_endpoint(&w.dst)?;
            let sb = m.index_of(&sp).ok_or_else(|| SimError::BadWire(format!("wire from `{sp}`: no such block")))?;
            let db = m.index_of(&dp).ok_or_else(|| SimError::BadWire(format!("wire to `{dp}`: no such block")))?;
            m.add_wire(sb, sport, db, dport)?;
        }
        Ok(m)
    }

    /// One-shot: parse → build → ready-to-run [`Sim`].
    pub fn to_sim(&self, factory: &dyn BlockFactory) -> Result<Sim, SimError> {
        Sim::new(self.to_model(factory)?, self.solver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"{
        "format": "simcraft", "version": 1,
        "solver": {"solver": "euler", "dt": 0.01, "tstop": 1.0},
        "blocks": [
            {"path": "src", "type": "constant", "params": {"value": 2.0}},
            {"path": "out", "type": "scope"}
        ],
        "wires": [{"from": "src/1", "to": "out/1"}]
    }"#;

    #[test]
    fn parses_and_builds() {
        let doc = Document::parse(DOC).unwrap();
        assert_eq!(doc.solver.solver, Solver::Euler);
        let m = doc.to_model(&simblocks::Factory).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.wires().len(), 1);
    }

    #[test]
    fn rejects_wrong_format_and_bad_ports() {
        assert!(Document::parse(r#"{"format":"other"}"#).is_err());
        let bad = DOC.replace(r#""to": "out/1""#, r#""to": "out/0""#);
        let doc = Document::parse(&bad).unwrap();
        assert!(doc.to_model(&simblocks::Factory).is_err());
    }

    #[test]
    fn unknown_block_type_errors() {
        let bad = DOC.replace(r#""type": "constant""#, r#""type": "flux_capacitor""#);
        let doc = Document::parse(&bad).unwrap();
        match doc.to_model(&simblocks::Factory) {
            Err(SimError::Msg(m)) => assert!(m.contains("flux_capacitor")),
            Err(e) => panic!("expected Msg error, got {e:?}"),
            Ok(_) => panic!("expected error, got a model"),
        }
    }
}
