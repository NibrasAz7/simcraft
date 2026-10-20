//! Continuous-time blocks: Integrator, Derivative, TransferFcn,
//! PidController, TransportDelay.
//!
//! Integrator, TransferFcn and PidController own continuous states (the
//! solver integrates them); Derivative is a feedthrough difference
//! approximation; TransportDelay is stateful but continuous-time (its
//! state is an input history, not part of the solver's `x` vector).

use simcore::{Block, Params, PortReq, SampleTime, SigMeta, SigVal, SimError};

use crate::{block_any, params};

/// `integrator {x0}` — `y = x`, `x' = u`. `x0` may be a scalar (broadcast to
/// the input width) or a vector fixing the width.
pub struct Integrator {
    x0: Vec<f64>,
    reqs: [PortReq; 1],
}

impl Integrator {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["x0"])?;
        Ok(Integrator { x0: params::vec(p, "x0", vec![0.0])?, reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for Integrator {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w = ins.first().copied().flatten().map(|m| m.width).unwrap_or(self.x0.len());
        if self.x0.len() == 1 && w > 1 {
            self.x0 = vec![self.x0.first().copied().unwrap_or(0.0); w];
        }
        if self.x0.len() != w {
            return Err(SimError::Msg(format!("integrator x0 has {} elements, input width {w}", self.x0.len())));
        }
        Ok(SigMeta::real(w))
    }
    fn outputs(&mut self, _t: f64, x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::Real(x.to_vec());
        }
        Ok(())
    }
    fn derivatives(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], dx: &mut [f64]) -> Result<(), SimError> {
        let uu = simcore::in_real(u, 0)?;
        for (d, v) in dx.iter_mut().zip(uu.iter()) {
            *d = *v;
        }
        Ok(())
    }
    fn n_states(&self) -> usize {
        self.x0.len()
    }
    fn get_x(&self, x: &mut [f64]) -> Result<(), SimError> {
        x.clone_from_slice(&self.x0[..x.len().min(self.x0.len())]);
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        false
    }
    block_any!();
}

/// `derivative {}` — `y ≈ du/dt` via a backward difference on the input.
/// Feedthrough: the output depends on the input at this instant.
pub struct Derivative {
    prev_u: Vec<f64>,
    prev_t: Option<f64>,
    reqs: [PortReq; 1],
}

impl Derivative {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &[])?;
        Ok(Derivative { prev_u: Vec::new(), prev_t: None, reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for Derivative {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let m = ins.first().copied().flatten().unwrap_or(SigMeta::real(1));
        self.prev_u = vec![0.0; m.width];
        Ok(m)
    }
    fn outputs(&mut self, t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let uu = simcore::in_real(u, 0)?.to_vec();
        // First call anchors the difference (output 0) — no history yet.
        let dt = self.prev_t.map(|pt| t - pt).unwrap_or(0.0);
        let out: Vec<f64> = if dt > 0.0 { uu.iter().zip(self.prev_u.iter()).map(|(a, b)| (a - b) / dt).collect() } else { vec![0.0; uu.len()] };
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::Real(out);
        }
        self.prev_u = uu;
        self.prev_t = Some(t);
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `transfer_fcn {num, den}` — SISO transfer function `num(s)/den(s)` in
/// controllable-canonical form. `num`/`den` are descending-power
/// coefficients; `den` must be nonempty with a nonzero leading coefficient.
pub struct TransferFcn {
    num: Vec<f64>,
    den: Vec<f64>,
    reqs: [PortReq; 1],
}

impl TransferFcn {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["num", "den"])?;
        let mut num = params::vec(p, "num", vec![1.0])?;
        let den = params::req_vec(p, "den")?;
        let a0 = den.first().copied().unwrap_or(0.0);
        if a0 == 0.0 {
            return Err(SimError::Msg("transfer_fcn: leading denominator coefficient must be nonzero".into()));
        }
        // Normalize so den[0] = 1 (controllable-canonical form assumes it).
        let den: Vec<f64> = den.iter().map(|a| a / a0).collect();
        for c in num.iter_mut() {
            *c /= a0;
        }
        Ok(TransferFcn { num, den, reqs: [PortReq::fixed(1)] })
    }

    /// States = order of the denominator (den.len() - 1).
    fn order(&self) -> usize {
        self.den.len().saturating_sub(1)
    }
}

impl Block for TransferFcn {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, _ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        if self.num.len() > self.den.len() {
            return Err(SimError::Msg(format!("transfer_fcn is improper: num order {} > den order {}", self.num.len() - 1, self.den.len() - 1)));
        }
        Ok(SigMeta::real(1))
    }
    fn outputs(&mut self, _t: f64, x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        // Controllable canonical: with states xi and input u, define
        //   w = u - a1·x1 - a2·x2 - ...   (den[1..] are the a_i)
        //   y = b0·w + b1·x1 + b2·x2 + ...
        // where b is `num` right-aligned into a length-(n+1) vector:
        // b[i] = num[i - pad], pad = den.len() - num.len(). b0 only exists
        // when the orders match (pad == 0).
        let n = self.order();
        let uu = simcore::in_real(u, 0)?.first().copied().unwrap_or(0.0);
        let mut w = uu;
        for (i, xi) in x.iter().take(n).enumerate() {
            w -= self.den.get(i + 1).copied().unwrap_or(0.0) * xi;
        }
        let pad = self.den.len() - self.num.len();
        let mut yv = if pad == 0 { self.num.first().copied().unwrap_or(0.0) * w } else { 0.0 };
        for (k, xi) in x.iter().take(n).enumerate() {
            let i = k + 1;
            if i >= pad {
                yv += self.num.get(i - pad).copied().unwrap_or(0.0) * xi;
            }
        }
        if let Some(v) = y.get_mut(0) {
            *v = SigVal::scalar(yv);
        }
        Ok(())
    }
    fn derivatives(&mut self, _t: f64, x: &[f64], u: &[&SigVal], dx: &mut [f64]) -> Result<(), SimError> {
        // x1' = w = u - a1·x1 - ...; xi' = x_{i-1} for i >= 2.
        let n = self.order();
        let uu = simcore::in_real(u, 0)?.first().copied().unwrap_or(0.0);
        let mut w = uu;
        for (i, xi) in x.iter().take(n).enumerate() {
            w -= self.den.get(i + 1).copied().unwrap_or(0.0) * xi;
        }
        if let Some(d0) = dx.get_mut(0) {
            *d0 = w;
        }
        for i in 1..n {
            if let (Some(d), Some(xv)) = (dx.get_mut(i), x.get(i - 1)) {
                *d = *xv;
            }
        }
        Ok(())
    }
    fn n_states(&self) -> usize {
        self.order()
    }
    fn get_x(&self, x: &mut [f64]) -> Result<(), SimError> {
        for v in x.iter_mut() {
            *v = 0.0;
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        // Feedthrough only when num and den share the same order (b0 ≠ 0).
        self.num.len() == self.den.len()
    }
    fn sample_time(&self) -> SampleTime {
        SampleTime::Continuous
    }
    block_any!();
}

/// `pid {kp, ki, kd, n}` — parallel PID controller in the continuous form
/// `Kp + Ki/s + Kd·N·s/(s+N)`. `n` is the derivative filter coefficient
/// (default 100); set `kd = 0` for a PI controller. Two continuous states:
/// the error integral `xi` and the filtered-derivative tracker `xd`:
///
/// ```text
/// xi' = e              xd' = n·(e − xd)
/// y   = kp·e + ki·xi + kd·n·(e − xd)
/// ```
///
/// SISO: input and output are scalars.
pub struct PidController {
    kp: f64,
    ki: f64,
    kd: f64,
    n: f64,
    reqs: [PortReq; 1],
}

impl PidController {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["kp", "ki", "kd", "n"])?;
        let kp = params::num(p, "kp", 1.0)?;
        let ki = params::num(p, "ki", 0.0)?;
        let kd = params::num(p, "kd", 0.0)?;
        let n = params::num(p, "n", 100.0)?;
        if kd != 0.0 && !(n.is_finite() && n > 0.0) {
            return Err(SimError::Msg("pid: derivative filter `n` must be positive when `kd` is nonzero".into()));
        }
        Ok(PidController { kp, ki, kd, n: if n.is_finite() && n > 0.0 { n } else { 100.0 }, reqs: [PortReq::fixed(1)] })
    }
}

impl Block for PidController {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, _ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(1))
    }
    fn outputs(&mut self, _t: f64, x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let e = simcore::in_real(u, 0)?.first().copied().unwrap_or(0.0);
        let xi = x.first().copied().unwrap_or(0.0);
        let xd = x.get(1).copied().unwrap_or(0.0);
        let v = self.kp * e + self.ki * xi + self.kd * self.n * (e - xd);
        if let Some(s) = y.get_mut(0) {
            *s = SigVal::scalar(v);
        }
        Ok(())
    }
    fn derivatives(&mut self, _t: f64, x: &[f64], u: &[&SigVal], dx: &mut [f64]) -> Result<(), SimError> {
        let e = simcore::in_real(u, 0)?.first().copied().unwrap_or(0.0);
        if let Some(d) = dx.get_mut(0) {
            *d = e;
        }
        if let Some(d) = dx.get_mut(1) {
            *d = self.n * (e - x.get(1).copied().unwrap_or(0.0));
        }
        Ok(())
    }
    fn n_states(&self) -> usize {
        2
    }
    fn get_x(&self, x: &mut [f64]) -> Result<(), SimError> {
        for v in x.iter_mut() {
            *v = 0.0;
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        // y depends on e directly through the P and D terms.
        self.kp != 0.0 || self.kd != 0.0
    }
    fn sample_time(&self) -> SampleTime {
        SampleTime::Continuous
    }
    block_any!();
}

/// `transport_delay {delay, initial_output}` — `y(t) = u(t − delay)`,
/// linearly interpolated between recorded input samples. Before the first
/// sample older than `t − delay` exists, emits `initial_output`.
///
/// The input history is recorded inside `outputs` (which also runs inside
/// solver stage evaluations — stage times are what make the interpolation
/// accurate for variable-step solvers); entries at or beyond the current
/// time are rewritten so a rejected `rk45` step never leaves stale future
/// samples. `delay` must be positive.
pub struct TransportDelay {
    delay: f64,
    init: Vec<f64>,
    /// `(t, u)` input history, strictly increasing in `t`.
    hist: Vec<(f64, Vec<f64>)>,
    reqs: [PortReq; 1],
}

impl TransportDelay {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["delay", "initial_output"])?;
        let delay = params::num(p, "delay", 1.0)?;
        if !(delay.is_finite() && delay > 0.0) {
            return Err(SimError::Msg("transport_delay `delay` must be positive".into()));
        }
        let init = params::vec(p, "initial_output", vec![0.0])?;
        if init.is_empty() {
            return Err(SimError::Msg("transport_delay `initial_output` must not be empty".into()));
        }
        Ok(TransportDelay { delay, init, hist: Vec::new(), reqs: [PortReq::REAL_ANY] })
    }

    /// `u(t − delay)` from the recorded history, linearly interpolated.
    fn delayed(&self, td: f64) -> Vec<f64> {
        // Index of the first entry strictly after `td`; `i == 0` means the
        // delay reaches back past the first sample → initial_output.
        let i = self.hist.partition_point(|e| e.0 <= td);
        match (i, self.hist.get(i)) {
            (0, _) => self.init.clone(),
            (_, None) => self.hist.last().map(|e| e.1.clone()).unwrap_or_else(|| self.init.clone()),
            (_, Some((t1, u1))) => {
                let (t0, u0) = &self.hist[i - 1];
                let span = t1 - t0;
                let f = if span > 0.0 { ((td - t0) / span).clamp(0.0, 1.0) } else { 1.0 };
                u0.iter().enumerate().map(|(k, a)| a + f * (u1.get(k).copied().unwrap_or(*a) - a)).collect()
            }
        }
    }
}

impl Block for TransportDelay {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w = ins.first().copied().flatten().map(|m| m.width).unwrap_or(self.init.len());
        if self.init.len() == 1 && w > 1 {
            self.init = vec![self.init.first().copied().unwrap_or(0.0); w];
        }
        if self.init.len() != w {
            return Err(SimError::Msg(format!("transport_delay initial_output has {} elements, input width {w}", self.init.len())));
        }
        Ok(SigMeta::real(w))
    }
    fn outputs(&mut self, t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let uu = simcore::in_real(u, 0)?.to_vec();
        // Record (t, u): rewrite any entries at or after t so solver stage
        // re-evals and rejected steps don't leave stale future samples.
        while self.hist.last().is_some_and(|e| e.0 >= t) {
            self.hist.pop();
        }
        if self.hist.len() >= simcore::limits::MAX_LOG_SAMPLES {
            return Err(SimError::TooLarge(format!("transport_delay history exceeds {} samples", simcore::limits::MAX_LOG_SAMPLES)));
        }
        self.hist.push((t, uu));
        // Drop entries strictly below the interpolation window, keeping
        // the last one at-or-before `t − delay` as the lower bracket.
        let td = t - self.delay;
        let keep_from = self.hist.partition_point(|e| e.0 <= td).saturating_sub(1);
        if keep_from > 0 {
            self.hist.drain(0..keep_from);
        }
        let v = self.delayed(td);
        if let Some(s) = y.get_mut(0) {
            *s = SigVal::Real(v);
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        // The current input only enters the history — the output reads
        // samples at `t − delay`, strictly in the past.
        false
    }
    block_any!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tf(num: &[f64], den: &[f64]) -> TransferFcn {
        let mut p = BTreeMap::new();
        p.insert("num".to_string(), simcore::Param::Vec(num.to_vec()));
        p.insert("den".to_string(), simcore::Param::Vec(den.to_vec()));
        TransferFcn::new(&p).unwrap()
    }

    #[test]
    fn integrator_broadcasts_x0() {
        let mut p = BTreeMap::new();
        p.insert("x0".to_string(), simcore::Param::Num(2.0));
        let mut b = Integrator::new(&p).unwrap();
        let m = b.resolve_out(0, &[Some(SigMeta::real(3))]).unwrap();
        assert_eq!(m.width, 3);
        assert_eq!(b.n_states(), 3);
    }

    #[test]
    fn first_order_lag_dc_gain() {
        // 1/(s+1): DC gain should be 1 → at steady state y = u.
        let mut b = tf(&[1.0], &[1.0, 1.0]);
        let m = b.resolve_out(0, &[Some(SigMeta::real(1))]).unwrap();
        assert_eq!(m.width, 1);
        // x = 5 means w = u - x; set u so w balances: for u=5, x=5 → w=0,
        // y = x (b1·x1 with num=[0,1] after padding) = 5.
        let u = SigVal::scalar(5.0);
        let mut y = vec![SigVal::scalar(0.0)];
        b.outputs(0.0, &[5.0], &[&u], &mut y).unwrap();
        assert_eq!(y[0].as_real().unwrap()[0], 5.0);
    }

    fn pid(kp: f64, ki: f64, kd: f64) -> PidController {
        let mut p = BTreeMap::new();
        p.insert("kp".into(), simcore::Param::Num(kp));
        p.insert("ki".into(), simcore::Param::Num(ki));
        p.insert("kd".into(), simcore::Param::Num(kd));
        PidController::new(&p).unwrap()
    }

    #[test]
    fn pid_p_i_d_terms() {
        // P only: y = kp·e, feedthrough on.
        let mut b = pid(2.0, 0.0, 0.0);
        let u = SigVal::scalar(3.0);
        let mut y = vec![SigVal::scalar(0.0)];
        b.outputs(0.0, &[0.0, 0.0], &[&u], &mut y).unwrap();
        assert_eq!(y[0].as_real().unwrap()[0], 6.0);
        assert!(b.has_feedthrough());
        // PI: y = kp·e + ki·xi; dx = [e, n·(e−xd)].
        let mut b = pid(1.0, 4.0, 0.0);
        b.outputs(0.0, &[2.0, 0.0], &[&u], &mut y).unwrap();
        assert_eq!(y[0].as_real().unwrap()[0], 3.0 + 8.0);
        let mut dx = [0.0; 2];
        b.derivatives(0.0, &[2.0, 0.0], &[&u], &mut dx).unwrap();
        assert_eq!(dx[0], 3.0); // xi' = e
        assert_eq!(dx[1], 100.0 * 3.0); // xd' = n·e (default n=100)
    }

    #[test]
    fn pid_full_form_output() {
        // kp=1, ki=0.5, kd=2, n=100; e=1, xi=4, xd=0.2:
        // y = 1 + 0.5·4 + 2·100·(1−0.2) = 1 + 2 + 160 = 163.
        let mut p = BTreeMap::new();
        p.insert("kp".into(), simcore::Param::Num(1.0));
        p.insert("ki".into(), simcore::Param::Num(0.5));
        p.insert("kd".into(), simcore::Param::Num(2.0));
        let mut b = PidController::new(&p).unwrap();
        let u = SigVal::scalar(1.0);
        let mut y = vec![SigVal::scalar(0.0)];
        b.outputs(0.0, &[4.0, 0.2], &[&u], &mut y).unwrap();
        assert!((y[0].as_real().unwrap()[0] - 163.0).abs() < 1e-9);
    }

    #[test]
    fn transport_delay_outputs_past_input() {
        let mut p = BTreeMap::new();
        p.insert("delay".into(), simcore::Param::Num(1.0));
        let mut b = TransportDelay::new(&p).unwrap();
        let mut y = vec![SigVal::scalar(0.0)];
        // Before the history reaches back past `delay`: initial output.
        let u0 = SigVal::scalar(0.0);
        b.outputs(0.5, &[], &[&u0], &mut y).unwrap();
        assert_eq!(y[0].as_real().unwrap()[0], 0.0);
        // u(t) = t ramp: (1.0, 1.0), (2.0, 2.0).
        for (t, v) in [(1.0, 1.0), (2.0, 2.0)] {
            let u = SigVal::scalar(v);
            b.outputs(t, &[], &[&u], &mut y).unwrap();
        }
        // y(2.0) = u(1.0) = 1.0 exactly (a recorded sample exists there).
        assert_eq!(y[0].as_real().unwrap()[0], 1.0);
        // y(2.5): td=1.5 → interp between (1,1) and (2,2) → 1.5.
        let u = SigVal::scalar(2.5);
        b.outputs(2.5, &[], &[&u], &mut y).unwrap();
        assert!((y[0].as_real().unwrap()[0] - 1.5).abs() < 1e-12);
        assert!(!b.has_feedthrough());
    }

    #[test]
    fn transport_delay_rejects_bad_params() {
        let mut p = BTreeMap::new();
        p.insert("delay".into(), simcore::Param::Num(0.0));
        assert!(TransportDelay::new(&p).is_err());
        let mut p = BTreeMap::new();
        p.insert("n".into(), simcore::Param::Num(0.0));
        p.insert("kd".into(), simcore::Param::Num(1.0));
        assert!(PidController::new(&p).is_err());
    }
}
