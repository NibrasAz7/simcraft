//! Discrete-time blocks: UnitDelay and ZeroOrderHold. Both hold state that
//! advances on `sample_time` hits (`update`), never continuously.

use simcore::{Block, Params, PortReq, SampleTime, SigMeta, SigVal, SimError};

use crate::{block_any, params};

/// `unit_delay {sample_time, x0}` — `y(k) = u(k-1)`. No feedthrough: the
/// output at sample `k` is the held value, not the current input.
pub struct UnitDelay {
    ts: f64,
    held: Vec<f64>,
    reqs: [PortReq; 1],
}

impl UnitDelay {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["sample_time", "x0"])?;
        let ts = params::num(p, "sample_time", 1.0)?;
        if !(ts.is_finite() && ts > 0.0) {
            return Err(SimError::Msg("unit_delay: sample_time must be positive".into()));
        }
        Ok(UnitDelay { ts, held: params::vec(p, "x0", vec![0.0])?, reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for UnitDelay {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w = ins.first().copied().flatten().map(|m| m.width).unwrap_or(self.held.len());
        if self.held.len() == 1 && w > 1 {
            self.held = vec![self.held.first().copied().unwrap_or(0.0); w];
        }
        if self.held.len() != w {
            return Err(SimError::Msg(format!("unit_delay x0 has {} elements, input width {w}", self.held.len())));
        }
        Ok(SigMeta::real(w))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::Real(self.held.clone());
        }
        Ok(())
    }
    fn update(&mut self, _t: f64, u: &[&SigVal]) -> Result<(), SimError> {
        self.held = simcore::in_real(u, 0)?.to_vec();
        Ok(())
    }
    fn sample_time(&self) -> SampleTime {
        SampleTime::Discrete(self.ts)
    }
    fn has_feedthrough(&self) -> bool {
        false
    }
    block_any!();
}

/// `zero_order_hold {sample_time}` — samples `u` every `Ts`, holds between
/// hits. Functionally a UnitDelay with `x0 = u(0)` semantics, kept separate
/// because model authors reach for it by name.
pub struct ZeroOrderHold {
    ts: f64,
    held: Vec<f64>,
    reqs: [PortReq; 1],
}

impl ZeroOrderHold {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["sample_time"])?;
        let ts = params::num(p, "sample_time", 1.0)?;
        if !(ts.is_finite() && ts > 0.0) {
            return Err(SimError::Msg("zero_order_hold: sample_time must be positive".into()));
        }
        Ok(ZeroOrderHold { ts, held: vec![0.0], reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for ZeroOrderHold {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let m = ins.first().copied().flatten().unwrap_or(SigMeta::real(1));
        self.held = vec![0.0; m.width];
        Ok(m)
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::Real(self.held.clone());
        }
        Ok(())
    }
    fn update(&mut self, _t: f64, u: &[&SigVal]) -> Result<(), SimError> {
        self.held = simcore::in_real(u, 0)?.to_vec();
        Ok(())
    }
    fn sample_time(&self) -> SampleTime {
        SampleTime::Discrete(self.ts)
    }
    fn has_feedthrough(&self) -> bool {
        false
    }
    block_any!();
}
