//! Sink blocks: no output ports, so `compile` flags them `loggable` and
//! `log(t, u)` runs once per major step. Both record `(t, input)` traces;
//! the engine layer renders a Scope as a matplot figure and writes a
//! ToWorkspace trace into the linked MatCraft session.

use simcore::{Block, Params, PortReq, SigMeta, SigVal, SimError};

use crate::{block_any, params};

/// One logged trace: sample times plus per-element value history.
#[derive(Debug, Clone)]
pub struct Trace {
    pub t: Vec<f64>,
    /// `y[k][i]` — element `i` of the input vector at sample `k`.
    pub y: Vec<Vec<f64>>,
}

impl Trace {
    fn new() -> Self {
        Trace { t: Vec::new(), y: Vec::new() }
    }

    fn push(&mut self, t: f64, v: &[f64]) {
        if self.t.len() < simcore::limits::MAX_LOG_SAMPLES {
            self.t.push(t);
            self.y.push(v.to_vec());
        }
    }
}

/// `scope {}` — records its input every major step. One input port.
pub struct Scope {
    pub trace: Trace,
    reqs: [PortReq; 1],
}

impl Scope {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &[])?;
        Ok(Scope { trace: Trace::new(), reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for Scope {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        0
    }
    fn resolve_out(&mut self, _p: usize, _ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Err(SimError::Msg("scope has no output ports".into()))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], _u: &[&SigVal], _y: &mut Vec<SigVal>) -> Result<(), SimError> {
        Ok(())
    }
    fn log(&mut self, t: f64, u: &[&SigVal]) -> Result<(), SimError> {
        self.trace.push(t, simcore::in_real(u, 0)?);
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        false
    }
    block_any!();
}

/// `to_workspace {var}` — records its input; the host layer writes the
/// `(t, y)` pair into the MatCraft variable `var` after the run.
pub struct ToWorkspace {
    pub var: String,
    pub trace: Trace,
    reqs: [PortReq; 1],
}

impl ToWorkspace {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["var"])?;
        let var = params::string(p, "var", "simout")?;
        if var.is_empty() || !var.chars().all(|c| c.is_alphanumeric() || c == '_') || var.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Err(SimError::Msg(format!("to_workspace: invalid variable name `{var}`")));
        }
        Ok(ToWorkspace { var, trace: Trace::new(), reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for ToWorkspace {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        0
    }
    fn resolve_out(&mut self, _p: usize, _ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Err(SimError::Msg("to_workspace has no output ports".into()))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], _u: &[&SigVal], _y: &mut Vec<SigVal>) -> Result<(), SimError> {
        Ok(())
    }
    fn log(&mut self, t: f64, u: &[&SigVal]) -> Result<(), SimError> {
        self.trace.push(t, simcore::in_real(u, 0)?);
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        false
    }
    block_any!();
}
