//! The simulation driver: fixed-step `euler` and `rk4` over a compiled model.
//!
//! Per major step at time `t` (documented solver semantics):
//! 1. `outputs` pass in execution order,
//! 2. `update` on discrete blocks whose sample time hits `t`, then a second
//!    `outputs` pass so the new held values propagate (this gives a
//!    zero-order hold its true `y(t)=u(kTs)` shape rather than a one-step
//!    delay),
//! 3. `log` on every sink,
//! 4. `derivatives` + one solver step on the flat state vector.

use crate::limits;
use crate::{CompiledModel, Model, SigVal, SimError};

/// Fixed-step solver selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Solver {
    /// Forward Euler, first order.
    Euler,
    /// Classical fixed-step Runge–Kutta 4.
    Rk4,
}

impl Solver {
    /// Solver name → variant; `None` on unknown names.
    pub fn parse(s: &str) -> Option<Solver> {
        match s {
            "euler" => Some(Solver::Euler),
            "rk4" => Some(Solver::Rk4),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Solver::Euler => "euler",
            Solver::Rk4 => "rk4",
        }
    }
}

/// Run parameters: fixed step `dt` over `[0, tstop]`.
#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    pub solver: Solver,
    pub dt: f64,
    pub tstop: f64,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig { solver: Solver::Rk4, dt: 0.001, tstop: 10.0 }
    }
}

/// What a finished run hands back; logged signals live inside the sink
/// blocks (`model.block::<Scope>("name")` after `Sim::into_model`).
#[derive(Debug)]
pub struct SimOut {
    /// Sampled time points (`tstop/dt + 1`).
    pub t: Vec<f64>,
    /// Final flat continuous state.
    pub x: Vec<f64>,
    pub steps: u64,
    pub solver: Solver,
    pub dt: f64,
}

/// Borrow the signals driving block `b`'s inputs from the output buffers.
fn inputs_of<'a>(comp: &'a CompiledModel, bufs: &'a [Vec<SigVal>], b: usize) -> Vec<&'a SigVal> {
    comp.inputs
        .get(b)
        .map(|list| list.iter().filter_map(|src| src.and_then(|(sb, sp)| bufs.get(sb).and_then(|v| v.get(sp)))).collect())
        .unwrap_or_default()
}

/// A compiled, ready-to-run simulation.
pub struct Sim {
    model: Model,
    comp: CompiledModel,
    /// Per-block output-port buffers, indexed `[block][port]`.
    out_bufs: Vec<Vec<SigVal>>,
    /// Flat continuous state.
    x: Vec<f64>,
    /// `(block, steps-per-sample-hit)` for discrete blocks.
    rates: Vec<(usize, u64)>,
    cfg: SimConfig,
}

impl Sim {
    /// Compile `model` and validate `cfg` — all failures surface here,
    /// before `t = 0`.
    pub fn new(mut model: Model, cfg: SimConfig) -> Result<Sim, SimError> {
        if !(cfg.dt.is_finite() && cfg.dt > 0.0) {
            return Err(SimError::Msg("solver dt must be positive".into()));
        }
        if !(cfg.tstop.is_finite() && cfg.tstop >= 0.0) {
            return Err(SimError::Msg("solver tstop must be >= 0".into()));
        }
        let steps = (cfg.tstop / cfg.dt).round() as u64;
        if steps > limits::MAX_STEPS {
            return Err(SimError::TooLarge(format!("{} steps (max {})", steps, limits::MAX_STEPS)));
        }
        let comp = model.compile()?;
        // Each discrete sample time must land on the step grid.
        let mut rates = Vec::with_capacity(comp.discrete.len());
        for &(b, ts) in &comp.discrete {
            let r = ts / cfg.dt;
            let r_int = r.round();
            if r_int < 1.0 || (r - r_int).abs() > 1e-6 {
                let name = model.block_names().get(b).cloned().unwrap_or_default();
                return Err(SimError::BadParam { block: name, msg: format!("sample_time {ts} is not an integer multiple of dt {}", cfg.dt) });
            }
            rates.push((b, r_int as u64));
        }
        let out_bufs = comp.make_buffers();
        let x = comp.x0.clone();
        Ok(Sim { model, comp, out_bufs, x, rates, cfg })
    }

    /// The model being executed (block graph + live state).
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Consume the sim and return the model (states are left as-is).
    pub fn into_model(self) -> Model {
        self.model
    }

    /// Current flat continuous state.
    pub fn state(&self) -> &[f64] {
        &self.x
    }

    /// Continuous state count of block `i` (0 if out of range).
    fn n_states_of(&self, i: usize) -> usize {
        self.model.blocks_ref(i).map(|b| b.n_states()).unwrap_or(0)
    }

    /// Evaluate all outputs at `t` against `self.x` in execution order.
    fn eval_outputs(&mut self, t: f64) -> Result<(), SimError> {
        for &b in &self.comp.order {
            // Take this block's out buffers so the input borrows below are
            // clearly disjoint; swap them back once `outputs` fills them.
            let mut dst = match self.out_bufs.get_mut(b) {
                Some(v) => std::mem::take(v),
                None => continue,
            };
            let ins = inputs_of(&self.comp, &self.out_bufs, b);
            let (off, n) = (self.comp.offsets.get(b).copied().unwrap_or(0), self.n_states_of(b));
            let xs: &[f64] = self.x.get(off..off + n).unwrap_or(&[]);
            self.model.blocks_mut(b)?.outputs(t, xs, &ins, &mut dst)?;
            if let Some(slot) = self.out_bufs.get_mut(b) {
                *slot = dst;
            }
        }
        Ok(())
    }

    /// Evaluate `dx/dt = f(t, x)` into `dx` (zeroed first). Runs `outputs`
    /// against the stage state, leaving `self.x` untouched.
    fn eval_derivatives(&mut self, t: f64, x: &[f64], dx: &mut [f64]) -> Result<(), SimError> {
        let saved = std::mem::replace(&mut self.x, x.to_vec());
        let result = (|| {
            self.eval_outputs(t)?;
            for v in dx.iter_mut() {
                *v = 0.0;
            }
            for i in 0..self.model.len() {
                let n = self.n_states_of(i);
                if n == 0 {
                    continue;
                }
                let off = self.comp.offsets.get(i).copied().unwrap_or(0);
                let ins = inputs_of(&self.comp, &self.out_bufs, i);
                let xs: &[f64] = self.x.get(off..off + n).unwrap_or(&[]);
                if let Some(dxs) = dx.get_mut(off..off + n) {
                    self.model.blocks_mut(i)?.derivatives(t, xs, &ins, dxs)?;
                }
            }
            Ok(())
        })();
        self.x = saved;
        result
    }

    /// Run from `t = 0` to `tstop`; returns time history and final state.
    /// Takes `&mut self` so sink traces remain reachable via
    /// [`Sim::model`]/[`Sim::into_model`] after the run.
    pub fn run(&mut self) -> Result<SimOut, SimError> {
        let h = self.cfg.dt;
        let steps = (self.cfg.tstop / h).round() as u64;
        // Capped already by MAX_STEPS in `Sim::new`.
        let mut t_log: Vec<f64> = Vec::with_capacity(steps.saturating_add(1).min(limits::MAX_LOG_SAMPLES as u64) as usize);

        let mut t = 0.0;
        for step in 0..=steps {
            // 1) outputs at (t, x)
            self.eval_outputs(t)?;
            // 2) discrete sample hits: update, then re-propagate outputs so
            //    held values take effect at this instant.
            let mut fired = false;
            for &(b, rate) in &self.rates {
                if rate > 0 && step % rate == 0 {
                    let ins = inputs_of(&self.comp, &self.out_bufs, b);
                    self.model.blocks_mut(b)?.update(t, &ins)?;
                    fired = true;
                }
            }
            if fired {
                self.eval_outputs(t)?;
            }
            // 3) sink logging — once per major step.
            for i in 0..self.model.len() {
                if self.comp.loggable.contains(&i) {
                    let ins = inputs_of(&self.comp, &self.out_bufs, i);
                    self.model.blocks_mut(i)?.log(t, &ins)?;
                }
            }
            t_log.push(t);
            if step == steps {
                break;
            }
            // 4) integrate the continuous states.
            match self.cfg.solver {
                Solver::Euler => {
                    let n = self.x.len();
                    let mut dx = vec![0.0; n];
                    let x = self.x.clone();
                    self.eval_derivatives(t, &x, &mut dx)?;
                    for (xi, d) in self.x.iter_mut().zip(dx.iter()) {
                        *xi += h * d;
                    }
                }
                Solver::Rk4 => {
                    let n = self.x.len();
                    let x0 = self.x.clone();
                    let mut k1 = vec![0.0; n];
                    let mut k2 = vec![0.0; n];
                    let mut k3 = vec![0.0; n];
                    let mut k4 = vec![0.0; n];
                    self.eval_derivatives(t, &x0, &mut k1)?;
                    let xs: Vec<f64> = x0.iter().zip(&k1).map(|(a, b)| a + 0.5 * h * b).collect();
                    self.eval_derivatives(t + 0.5 * h, &xs, &mut k2)?;
                    let xs: Vec<f64> = x0.iter().zip(&k2).map(|(a, b)| a + 0.5 * h * b).collect();
                    self.eval_derivatives(t + 0.5 * h, &xs, &mut k3)?;
                    let xs: Vec<f64> = x0.iter().zip(&k3).map(|(a, b)| a + h * b).collect();
                    self.eval_derivatives(t + h, &xs, &mut k4)?;
                    for (xi, a, b, c, d) in self.x.iter_mut().zip(&k1).zip(&k2).zip(&k3).zip(&k4).map(|((((xi, a), b), c), d)| (xi, a, b, c, d)) {
                        *xi += h / 6.0 * (a + 2.0 * b + 2.0 * c + d);
                    }
                }
            }
            t += h;
        }
        Ok(SimOut { t: t_log, x: self.x.clone(), steps, solver: self.cfg.solver, dt: h })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::SampleTime;
    use crate::{PortReq, SigMeta};
    use std::any::Any;

    /// dx = u (a raw integrator), one continuous state.
    struct Integrator;
    impl crate::Block for Integrator {
        fn inputs(&self) -> &[PortReq] {
            &[PortReq::REAL_ANY]
        }
        fn n_outputs(&self) -> usize {
            1
        }
        fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
            Ok(ins.first().copied().flatten().unwrap_or(SigMeta::real(1)))
        }
        fn outputs(&mut self, _t: f64, x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
            if let Some(v) = y.get_mut(0) {
                *v = SigVal::scalar(x.first().copied().unwrap_or(0.0));
            }
            Ok(())
        }
        fn derivatives(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], dx: &mut [f64]) -> Result<(), SimError> {
            if let Some(d) = dx.get_mut(0) {
                *d = u.first().and_then(|s| s.as_real().ok()).and_then(|v| v.first()).copied().unwrap_or(0.0);
            }
            Ok(())
        }
        fn n_states(&self) -> usize {
            1
        }
        fn get_x(&self, x: &mut [f64]) -> Result<(), SimError> {
            if let Some(v) = x.get_mut(0) {
                *v = 1.0; // x0 = 1 for tests
            }
            Ok(())
        }
        fn set_x(&mut self, _x: &[f64]) -> Result<(), SimError> {
            Ok(())
        }
        fn sample_time(&self) -> SampleTime {
            SampleTime::Continuous
        }
        fn has_feedthrough(&self) -> bool {
            false
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// y = k*u — feedthrough gain.
    struct Gain(f64);
    impl crate::Block for Gain {
        fn inputs(&self) -> &[PortReq] {
            &[PortReq::REAL_ANY]
        }
        fn n_outputs(&self) -> usize {
            1
        }
        fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
            Ok(ins.first().copied().flatten().unwrap_or(SigMeta::real(1)))
        }
        fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
            let uu = crate::in_real(u, 0)?;
            if let Some(SigVal::Real(v)) = y.get_mut(0) {
                for (o, i) in v.iter_mut().zip(uu.iter()) {
                    *o = self.0 * i;
                }
            }
            Ok(())
        }
        fn has_feedthrough(&self) -> bool {
            true
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    /// Model: integrator looped through gain -1 → x' = -x, x0 = 1.
    fn decay_model() -> Model {
        let mut m = Model::new();
        let integ = m.add_block("i", Box::new(Integrator)).unwrap();
        let g = m.add_block("g", Box::new(Gain(-1.0))).unwrap();
        m.add_wire(integ, 0, g, 0).unwrap();
        m.add_wire(g, 0, integ, 0).unwrap();
        m
    }

    #[test]
    fn rk4_integrates_decay() {
        // x' = -x, x0=1 → x(t) = e^-t. RK4 global error at h=0.1 is ~3e-7.
        let mut sim = Sim::new(decay_model(), SimConfig { solver: Solver::Rk4, dt: 0.1, tstop: 1.0 }).unwrap();
        let out = sim.run().unwrap();
        let exact = (-1.0f64).exp();
        let got = out.x.first().copied().unwrap_or(0.0);
        assert!((got - exact).abs() < 1e-6, "rk4: {got} vs {exact}");
    }

    #[test]
    fn euler_integrates_decay_coarsely() {
        let mut sim = Sim::new(decay_model(), SimConfig { solver: Solver::Euler, dt: 0.001, tstop: 1.0 }).unwrap();
        let out = sim.run().unwrap();
        let exact = (-1.0f64).exp();
        let got = out.x.first().copied().unwrap_or(0.0);
        assert!((got - exact).abs() < 1e-3, "euler: {got} vs {exact}");
        assert_eq!(out.t.len(), 1001);
    }

    #[test]
    fn bad_solver_settings_rejected() {
        assert!(Sim::new(decay_model(), SimConfig { solver: Solver::Rk4, dt: 0.0, tstop: 1.0 }).is_err());
        assert!(Sim::new(decay_model(), SimConfig { solver: Solver::Rk4, dt: 0.1, tstop: -1.0 }).is_err());
    }
}
