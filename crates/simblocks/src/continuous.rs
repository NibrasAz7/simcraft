//! Continuous-time blocks: Integrator, Derivative, TransferFcn.
//!
//! Integrator and TransferFcn own continuous states (the solver integrates
//! them); Derivative is a feedthrough difference approximation.

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
}
