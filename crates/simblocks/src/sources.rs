//! Source blocks: Constant, Step, Sine, Ramp, Clock. No inputs, one output.

use simcore::{Block, Params, PortReq, SigMeta, SigVal, SimError};

use crate::{block_any, params};

/// `constant {value}` — emits `value` (scalar or vector) every step.
pub struct Constant {
    v: Vec<f64>,
    reqs: [PortReq; 0],
}

impl Constant {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["value"])?;
        let v = params::vec(p, "value", vec![1.0])?;
        if v.is_empty() {
            return Err(SimError::Msg("constant `value` must not be empty".into()));
        }
        Ok(Constant { v, reqs: [] })
    }
}

impl Block for Constant {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _port: usize, _ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(self.v.len()))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(SigVal::Real(out)) = y.get_mut(0) {
            out.clone_from(&self.v);
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `step {time, before, after}` — `before` for `t < time`, else `after`.
pub struct Step {
    t0: f64,
    before: f64,
    after: f64,
}

impl Step {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["time", "before", "after"])?;
        Ok(Step { t0: params::num(p, "time", 0.0)?, before: params::num(p, "before", 0.0)?, after: params::num(p, "after", 1.0)? })
    }
}

impl Block for Step {
    fn inputs(&self) -> &[PortReq] {
        &[]
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, _i: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(1))
    }
    fn outputs(&mut self, t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::scalar(if t >= self.t0 { self.after } else { self.before });
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `sine {amplitude, frequency, phase, bias}` — `amp·sin(freq·t + phase) + bias`,
/// `frequency` in rad/s.
pub struct Sine {
    amp: f64,
    freq: f64,
    phase: f64,
    bias: f64,
}

impl Sine {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["amplitude", "frequency", "phase", "bias"])?;
        Ok(Sine {
            amp: params::num(p, "amplitude", 1.0)?,
            freq: params::num(p, "frequency", 1.0)?,
            phase: params::num(p, "phase", 0.0)?,
            bias: params::num(p, "bias", 0.0)?,
        })
    }
}

impl Block for Sine {
    fn inputs(&self) -> &[PortReq] {
        &[]
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, _i: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(1))
    }
    fn outputs(&mut self, t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::scalar(self.amp * (self.freq * t + self.phase).sin() + self.bias);
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `ramp {slope, start_time, initial}` — `initial` before `start_time`,
/// then `initial + slope·(t - start_time)`.
pub struct Ramp {
    slope: f64,
    t0: f64,
    init: f64,
}

impl Ramp {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["slope", "start_time", "initial"])?;
        Ok(Ramp { slope: params::num(p, "slope", 1.0)?, t0: params::num(p, "start_time", 0.0)?, init: params::num(p, "initial", 0.0)? })
    }
}

impl Block for Ramp {
    fn inputs(&self) -> &[PortReq] {
        &[]
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, _i: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(1))
    }
    fn outputs(&mut self, t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let v = if t >= self.t0 { self.init + self.slope * (t - self.t0) } else { self.init };
        if let Some(s) = y.get_mut(0) {
            *s = SigVal::scalar(v);
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `clock {}` — emits the simulation time.
pub struct Clock;

impl Clock {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &[])?;
        Ok(Clock)
    }
}

impl Block for Clock {
    fn inputs(&self) -> &[PortReq] {
        &[]
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, _i: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(1))
    }
    fn outputs(&mut self, t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::scalar(t);
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn pnum(k: &str, v: f64) -> Params {
        let mut p = BTreeMap::new();
        p.insert(k.to_string(), simcore::Param::Num(v));
        p
    }

    #[test]
    fn sine_evaluates() {
        let mut p = pnum("frequency", 2.0);
        p.insert("amplitude".into(), simcore::Param::Num(3.0));
        let mut s = Sine::new(&p).unwrap();
        let mut y = vec![SigVal::scalar(0.0)];
        s.outputs(0.25 * std::f64::consts::PI / 2.0, &[], &[], &mut y).unwrap(); // freq*t = pi/4*? no: 2*0.3927=0.7854 → sin=0.707
        let v = y[0].as_real().unwrap()[0];
        assert!((v - 3.0 * (0.25 * std::f64::consts::PI).sin()).abs() < 1e-12);
    }

    #[test]
    fn step_switches_at_time() {
        let mut p = pnum("time", 0.5);
        p.insert("after".into(), simcore::Param::Num(4.0));
        let mut s = Step::new(&p).unwrap();
        let mut y = vec![SigVal::scalar(0.0)];
        s.outputs(0.4, &[], &[], &mut y).unwrap();
        assert_eq!(y[0].as_real().unwrap()[0], 0.0);
        s.outputs(0.5, &[], &[], &mut y).unwrap();
        assert_eq!(y[0].as_real().unwrap()[0], 4.0);
    }

    #[test]
    fn unknown_param_rejected() {
        let p = pnum("frequeny", 1.0); // typo
        assert!(Sine::new(&p).is_err());
    }
}
