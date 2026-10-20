//! The simulation driver: fixed-step `euler`/`rk4` and adaptive `rk45`
//! (Dormand–Prince) over a compiled model.
//!
//! Per major step at time `t` (documented solver semantics):
//! 1. `outputs` pass in execution order,
//! 2. `update` on discrete blocks whose sample time hits `t`, then a second
//!    `outputs` pass so the new held values propagate (this gives a
//!    zero-order hold its true `y(t)=u(kTs)` shape rather than a one-step
//!    delay),
//! 3. `log` on every sink,
//! 4. `derivatives` + one solver step on the flat state vector.
//!
//! `rk45` is the classic Dormand–Prince 5(4) pair (Dormand & Prince, 1980;
//! Hairer, Nørsett & Wanner, *Solving Ordinary Differential Equations I*,
//! §II.4): the local error is the RMS norm of `x5 − x4` scaled by
//! `atol + rtol·max(|x|, |x5|)`, and the step controller is the standard
//! `h·clamp(0.9·err^(−1/5), 0.2, 10)`. Trial steps are clamped to land
//! exactly on discrete sample hits and on `tstop`.

use crate::limits;
use crate::{CompiledModel, Model, SigVal, SimError};

/// Solver selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Solver {
    /// Forward Euler, first order, fixed step `dt`.
    Euler,
    /// Classical fixed-step Runge–Kutta 4.
    Rk4,
    /// Adaptive Dormand–Prince 5(4). `dt` is the initial step; `rtol`/`atol`
    /// drive error control and `min_step`/`max_step` bound the step.
    Rk45,
}

impl Solver {
    /// Every selectable solver (help text, error messages, UI dropdowns).
    pub const ALL: &'static [Solver] = &[Solver::Euler, Solver::Rk4, Solver::Rk45];

    /// Solver name → variant; `None` on unknown names.
    pub fn parse(s: &str) -> Option<Solver> {
        match s {
            "euler" => Some(Solver::Euler),
            "rk4" => Some(Solver::Rk4),
            "rk45" | "dopri5" | "ode45" => Some(Solver::Rk45),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Solver::Euler => "euler",
            Solver::Rk4 => "rk4",
            Solver::Rk45 => "rk45",
        }
    }
}

/// Run parameters over `[0, tstop]`.
///
/// `rtol`, `atol`, `min_step` and `max_step` apply to `rk45` only. A
/// non-positive `min_step` selects the automatic floor (a few ulps of
/// `tstop`); a non-positive `max_step` selects `tstop/50`.
#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    pub solver: Solver,
    /// Step size for `euler`/`rk4`; initial step for `rk45`.
    pub dt: f64,
    pub tstop: f64,
    /// Relative tolerance (`rk45`).
    pub rtol: f64,
    /// Absolute tolerance (`rk45`).
    pub atol: f64,
    /// Smallest step `rk45` may take before erroring (0 = automatic).
    pub min_step: f64,
    /// Largest step `rk45` may take (0 = `tstop/50`).
    pub max_step: f64,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig { solver: Solver::Rk4, dt: 0.001, tstop: 10.0, rtol: 1e-3, atol: 1e-6, min_step: 0.0, max_step: 0.0 }
    }
}

/// What a finished run hands back; logged signals live inside the sink
/// blocks (`model.block::<Scope>("name")` after `Sim::into_model`).
#[derive(Debug)]
pub struct SimOut {
    /// Sampled time points — uniform for fixed-step solvers, the accepted
    /// step times for `rk45`.
    pub t: Vec<f64>,
    /// Final flat continuous state.
    pub x: Vec<f64>,
    /// Accepted major steps.
    pub steps: u64,
    /// Rejected trial steps (`rk45` only; always 0 for fixed-step solvers).
    pub rejected: u64,
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

/// A pending discrete sample hit for the variable-step driver: `next` is
/// the next `k·ts` not yet consumed (starts at 0 — discrete blocks fire at
/// `t = 0`, matching the fixed-step scheduler).
struct Hit {
    block: usize,
    ts: f64,
    next: f64,
}

/// A compiled, ready-to-run simulation. Drives itself one major step at a
/// time via [`Sim::step`]; [`Sim::run`] is `step()` to `tstop`.
pub struct Sim {
    model: Model,
    comp: CompiledModel,
    /// Per-block output-port buffers, indexed `[block][port]`.
    out_bufs: Vec<Vec<SigVal>>,
    /// Flat continuous state.
    x: Vec<f64>,
    /// `(block, steps-per-sample-hit)` — fixed-step discrete schedule.
    rates: Vec<(usize, u64)>,
    /// Discrete sample-hit tracking — `rk45` only.
    hits: Vec<Hit>,
    cfg: SimConfig,
    /// Current simulation time.
    t: f64,
    /// Fixed-step counter (which grid point was last observed).
    step_idx: u64,
    /// `rk45` controller's proposed next step.
    h: f64,
    accepted: u64,
    rejected: u64,
    finished: bool,
    t_log: Vec<f64>,
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
        let comp = model.compile()?;
        let mut rates = Vec::new();
        let mut hits = Vec::new();
        match cfg.solver {
            Solver::Euler | Solver::Rk4 => {
                let steps = (cfg.tstop / cfg.dt).round() as u64;
                if steps > limits::MAX_STEPS {
                    return Err(SimError::TooLarge(format!("{} steps (max {})", steps, limits::MAX_STEPS)));
                }
                // Each discrete sample time must land on the step grid.
                rates.reserve(comp.discrete.len());
                for &(b, ts) in &comp.discrete {
                    let r = ts / cfg.dt;
                    let r_int = r.round();
                    if r_int < 1.0 || (r - r_int).abs() > 1e-6 {
                        let name = model.block_names().get(b).cloned().unwrap_or_default();
                        return Err(SimError::BadParam { block: name, msg: format!("sample_time {ts} is not an integer multiple of dt {}", cfg.dt) });
                    }
                    rates.push((b, r_int as u64));
                }
            }
            Solver::Rk45 => {
                if !(cfg.rtol.is_finite() && cfg.rtol > 0.0) {
                    return Err(SimError::Msg("solver rtol must be positive".into()));
                }
                if !(cfg.atol.is_finite() && cfg.atol >= 0.0) {
                    return Err(SimError::Msg("solver atol must be >= 0".into()));
                }
                if !(cfg.min_step.is_finite() && cfg.min_step >= 0.0) {
                    return Err(SimError::Msg("solver min_step must be >= 0".into()));
                }
                if !cfg.max_step.is_finite() {
                    return Err(SimError::Msg("solver max_step must be finite".into()));
                }
                // Variable step: sample times needn't lie on a grid — the
                // solver lands on each hit exactly instead.
                hits = comp.discrete.iter().map(|&(b, ts)| Hit { block: b, ts, next: 0.0 }).collect();
            }
        }
        let out_bufs = comp.make_buffers();
        let x = comp.x0.clone();
        let h = cfg.dt;
        Ok(Sim { model, comp, out_bufs, x, rates, hits, cfg, t: 0.0, step_idx: 0, h, accepted: 0, rejected: 0, finished: false, t_log: Vec::new() })
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

    /// Current simulation time.
    pub fn time(&self) -> f64 {
        self.t
    }

    /// Accepted steps so far.
    pub fn accepted(&self) -> u64 {
        self.accepted
    }

    /// Rejected trial steps so far (`rk45` only).
    pub fn rejected(&self) -> u64 {
        self.rejected
    }

    /// True once `tstop` has been reached and logged.
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Sample times logged so far (uniform for fixed-step, adaptive for
    /// `rk45`). Capped at [`limits::MAX_LOG_SAMPLES`].
    pub fn t_log(&self) -> &[f64] {
        &self.t_log
    }

    /// Comparison epsilon for sample hits and `tstop` — a few ulps of the
    /// run length (steps land on hits exactly; this absorbs the rounding).
    fn time_tol(&self) -> f64 {
        64.0 * f64::EPSILON * self.cfg.tstop.abs().max(1.0)
    }

    /// Effective smallest `rk45` step: `cfg.min_step` or an ulp floor.
    fn hmin(&self) -> f64 {
        if self.cfg.min_step > 0.0 {
            self.cfg.min_step
        } else {
            self.time_tol().max(f64::MIN_POSITIVE)
        }
    }

    /// Effective largest `rk45` step: `cfg.max_step` or `tstop/50`.
    fn hmax(&self) -> f64 {
        if self.cfg.max_step > 0.0 {
            self.cfg.max_step
        } else {
            (self.cfg.tstop / 50.0).max(self.cfg.dt)
        }
    }

    /// The per-major-step observation at `t`: outputs pass, `update` on
    /// the due discrete blocks, a second outputs pass so the new held
    /// values propagate, then `log` on every sink.
    fn observe(&mut self, t: f64, due: &[usize]) -> Result<(), SimError> {
        self.eval_outputs(t)?;
        let mut fired = false;
        for &b in due {
            let ins = inputs_of(&self.comp, &self.out_bufs, b);
            self.model.blocks_mut(b)?.update(t, &ins)?;
            fired = true;
        }
        if fired {
            self.eval_outputs(t)?;
        }
        for i in 0..self.model.len() {
            if self.comp.loggable.contains(&i) {
                let ins = inputs_of(&self.comp, &self.out_bufs, i);
                self.model.blocks_mut(i)?.log(t, &ins)?;
            }
        }
        Ok(())
    }

    /// One major step: observe at the current `t` (outputs, due discrete
    /// updates, sink logs), then advance the solver. Returns `Ok(true)`
    /// while the run may continue, `Ok(false)` once `tstop` is logged.
    /// The first call observes `t = 0`.
    pub fn step(&mut self) -> Result<bool, SimError> {
        if self.finished {
            return Ok(false);
        }
        let t = self.t;
        let mut due: Vec<usize> = Vec::new();
        match self.cfg.solver {
            Solver::Euler | Solver::Rk4 => {
                for &(b, rate) in &self.rates {
                    if rate > 0 && self.step_idx.is_multiple_of(rate) {
                        due.push(b);
                    }
                }
            }
            Solver::Rk45 => {
                let ttol = self.time_tol();
                for hit in &mut self.hits {
                    if hit.next - t <= ttol {
                        due.push(hit.block);
                        while hit.next - t <= ttol {
                            hit.next += hit.ts;
                        }
                    }
                }
            }
        }
        self.observe(t, &due)?;
        if self.t_log.len() < limits::MAX_LOG_SAMPLES {
            self.t_log.push(t);
        }
        match self.cfg.solver {
            Solver::Euler | Solver::Rk4 => {
                let steps = (self.cfg.tstop / self.cfg.dt).round() as u64;
                if self.step_idx >= steps {
                    self.finished = true;
                    return Ok(false);
                }
                self.integrate_fixed(t)?;
                self.step_idx += 1;
                self.t += self.cfg.dt;
            }
            Solver::Rk45 => {
                if self.t >= self.cfg.tstop - self.time_tol() {
                    self.finished = true;
                    return Ok(false);
                }
                self.integrate_rk45()?;
            }
        }
        self.accepted += 1;
        if self.accepted.saturating_add(self.rejected) > limits::MAX_STEPS {
            return Err(SimError::TooLarge(format!("more than {} solver steps", limits::MAX_STEPS)));
        }
        Ok(true)
    }

    /// One `euler`/`rk4` integration step of size `dt` from `t`.
    fn integrate_fixed(&mut self, t: f64) -> Result<(), SimError> {
        let h = self.cfg.dt;
        let n = self.x.len();
        match self.cfg.solver {
            Solver::Euler => {
                let x = self.x.clone();
                let mut dx = vec![0.0; n];
                self.eval_derivatives(t, &x, &mut dx)?;
                for (xi, d) in self.x.iter_mut().zip(dx.iter()) {
                    *xi += h * d;
                }
            }
            Solver::Rk4 => {
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
            Solver::Rk45 => {}
        }
        Ok(())
    }

    /// One accepted `rk45` step from `self.t`: retries with a shrinking
    /// `h` until the scaled local error meets `rtol`/`atol`. Errors when
    /// even the minimum step can't.
    fn integrate_rk45(&mut self) -> Result<(), SimError> {
        let (hmin, hmax) = (self.hmin(), self.hmax());
        let t = self.t;
        // Trial steps land exactly on the next pending sample hit or tstop.
        let mut bound = self.cfg.tstop - t;
        for hit in &self.hits {
            let d = hit.next - t;
            if d > 0.0 && d < bound {
                bound = d;
            }
        }
        loop {
            let h_t = self.h.min(hmax).min(bound);
            if h_t <= 0.0 {
                return Err(SimError::Msg(format!("rk45 step collapsed at t={t}")));
            }
            let (x_new, err) = self.rk45_attempt(t, h_t)?;
            // Controller: h · clamp(0.9·err^(−1/5), 0.2, 10); NaN errors
            // shrink hard rather than panic.
            let grow = if err.is_finite() && err > 0.0 {
                (0.9 * err.powf(-0.2)).clamp(0.2, 10.0)
            } else if err.is_finite() {
                10.0
            } else {
                0.2
            };
            if err <= 1.0 {
                self.x = x_new;
                // Snap to the boundary the step was clamped to.
                self.t = if h_t >= self.cfg.tstop - t { self.cfg.tstop } else { t + h_t };
                self.h = (h_t * grow).clamp(hmin, hmax);
                return Ok(());
            }
            self.rejected += 1;
            if h_t <= hmin {
                return Err(SimError::Msg(format!(
                    "rk45 cannot meet rtol={} atol={} at t={t} (error {:.3}); minimum step {} reached",
                    self.cfg.rtol, self.cfg.atol, err, hmin
                )));
            }
            self.h = (h_t * grow.min(1.0)).max(hmin);
            if self.accepted.saturating_add(self.rejected) >= limits::MAX_STEPS {
                return Err(SimError::TooLarge(format!("more than {} solver steps", limits::MAX_STEPS)));
            }
        }
    }

    /// One Dormand–Prince 5(4) trial step of size `h` from `(t, x)`:
    /// returns `(x5, scaled_err)` where `scaled_err` is the RMS of
    /// `(x5 − x4) / (atol + rtol·max(|x|, |x5|))` — accept when `<= 1`.
    fn rk45_attempt(&mut self, t: f64, h: f64) -> Result<(Vec<f64>, f64), SimError> {
        // Dormand–Prince tableau (1980) — the classic published
        // coefficients, e.g. Hairer/Nørsett/Wanner §II.5 table 5.2.
        const A21: f64 = 1.0 / 5.0;
        const A31: f64 = 3.0 / 40.0;
        const A32: f64 = 9.0 / 40.0;
        const A41: f64 = 44.0 / 45.0;
        const A42: f64 = -56.0 / 15.0;
        const A43: f64 = 32.0 / 9.0;
        const A51: f64 = 19372.0 / 6561.0;
        const A52: f64 = -25360.0 / 2187.0;
        const A53: f64 = 64448.0 / 6561.0;
        const A54: f64 = -212.0 / 729.0;
        const A61: f64 = 9017.0 / 3168.0;
        const A62: f64 = -355.0 / 33.0;
        const A63: f64 = 46732.0 / 5247.0;
        const A64: f64 = 49.0 / 176.0;
        const A65: f64 = -5103.0 / 18656.0;
        const B1: f64 = 35.0 / 384.0;
        const B3: f64 = 500.0 / 1113.0;
        const B4: f64 = 125.0 / 192.0;
        const B5: f64 = -2187.0 / 6784.0;
        const B6: f64 = 11.0 / 84.0;
        // x5 − x4 directly in b-weight differences (b − b̂).
        const E1: f64 = 71.0 / 57600.0;
        const E3: f64 = -71.0 / 16695.0;
        const E4: f64 = 71.0 / 1920.0;
        const E5: f64 = -17253.0 / 339200.0;
        const E6: f64 = 22.0 / 525.0;
        const E7: f64 = -1.0 / 40.0;

        let n = self.x.len();
        let x0 = self.x.clone();
        let mut k1 = vec![0.0; n];
        let mut k2 = vec![0.0; n];
        let mut k3 = vec![0.0; n];
        let mut k4 = vec![0.0; n];
        let mut k5 = vec![0.0; n];
        let mut k6 = vec![0.0; n];
        let mut k7 = vec![0.0; n];
        self.eval_derivatives(t, &x0, &mut k1)?;
        let xs = lincomb(&x0, h, &[(A21, &k1)]);
        self.eval_derivatives(t + h / 5.0, &xs, &mut k2)?;
        let xs = lincomb(&x0, h, &[(A31, &k1), (A32, &k2)]);
        self.eval_derivatives(t + 0.3 * h, &xs, &mut k3)?;
        let xs = lincomb(&x0, h, &[(A41, &k1), (A42, &k2), (A43, &k3)]);
        self.eval_derivatives(t + 0.8 * h, &xs, &mut k4)?;
        let xs = lincomb(&x0, h, &[(A51, &k1), (A52, &k2), (A53, &k3), (A54, &k4)]);
        self.eval_derivatives(t + h * 8.0 / 9.0, &xs, &mut k5)?;
        let xs = lincomb(&x0, h, &[(A61, &k1), (A62, &k2), (A63, &k3), (A64, &k4), (A65, &k5)]);
        self.eval_derivatives(t + h, &xs, &mut k6)?;
        let x5 = lincomb(&x0, h, &[(B1, &k1), (B3, &k3), (B4, &k4), (B5, &k5), (B6, &k6)]);
        self.eval_derivatives(t + h, &x5, &mut k7)?;

        let mut acc = 0.0;
        for i in 0..n {
            let e = h * (E1 * k1[i] + E3 * k3[i] + E4 * k4[i] + E5 * k5[i] + E6 * k6[i] + E7 * k7[i]);
            let sc = self.cfg.atol + self.cfg.rtol * x0[i].abs().max(x5[i].abs());
            let q = e / sc;
            acc += q * q;
        }
        let err = if n == 0 { 0.0 } else { (acc / n as f64).sqrt() };
        Ok((x5, err))
    }

    /// Run to `tstop`; returns time history and final state. Equivalent to
    /// calling [`Sim::step`] until it reports done. Takes `&mut self` so
    /// sink traces remain reachable via [`Sim::model`]/[`Sim::into_model`]
    /// after the run.
    pub fn run(&mut self) -> Result<SimOut, SimError> {
        while self.step()? {}
        Ok(SimOut {
            t: std::mem::take(&mut self.t_log),
            x: self.x.clone(),
            steps: self.accepted,
            rejected: self.rejected,
            solver: self.cfg.solver,
            dt: self.cfg.dt,
        })
    }
}

/// `x0 + h·(a1·k1 + a2·k2 + …)` — the RK stage state.
fn lincomb(x0: &[f64], h: f64, terms: &[(f64, &[f64])]) -> Vec<f64> {
    let mut xs = x0.to_vec();
    for &(a, k) in terms {
        for (xi, &kv) in xs.iter_mut().zip(k.iter()) {
            *xi += h * a * kv;
        }
    }
    xs
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
        let mut sim = Sim::new(decay_model(), SimConfig { solver: Solver::Rk4, dt: 0.1, tstop: 1.0, ..SimConfig::default() }).unwrap();
        let out = sim.run().unwrap();
        let exact = (-1.0f64).exp();
        let got = out.x.first().copied().unwrap_or(0.0);
        assert!((got - exact).abs() < 1e-6, "rk4: {got} vs {exact}");
    }

    #[test]
    fn euler_integrates_decay_coarsely() {
        let mut sim = Sim::new(decay_model(), SimConfig { solver: Solver::Euler, dt: 0.001, tstop: 1.0, ..SimConfig::default() }).unwrap();
        let out = sim.run().unwrap();
        let exact = (-1.0f64).exp();
        let got = out.x.first().copied().unwrap_or(0.0);
        assert!((got - exact).abs() < 1e-3, "euler: {got} vs {exact}");
        assert_eq!(out.t.len(), 1001);
    }

    #[test]
    fn rk45_integrates_decay_adaptively() {
        // rtol=1e-8: far fewer steps than euler needs for the same
        // accuracy, and the global error tracks the tolerance.
        let cfg = SimConfig { solver: Solver::Rk45, dt: 0.01, tstop: 5.0, rtol: 1e-8, atol: 1e-10, ..SimConfig::default() };
        let mut sim = Sim::new(decay_model(), cfg).unwrap();
        let out = sim.run().unwrap();
        let exact = (-5.0f64).exp();
        let got = out.x.first().copied().unwrap_or(0.0);
        assert!((got - exact).abs() < 1e-7, "rk45: {got} vs {exact}");
        assert!(out.steps < 200, "adaptive run took {} steps", out.steps);
        assert_eq!(out.t.len() as u64, out.steps + 1);
        // Last logged time lands exactly on tstop.
        assert_eq!(out.t.last().copied(), Some(5.0));
    }

    #[test]
    fn rk45_step_by_step_matches_run() {
        // `step()` is the same driver `run()` loops over.
        let cfg = SimConfig { solver: Solver::Rk45, dt: 0.05, tstop: 1.0, rtol: 1e-6, atol: 1e-8, ..SimConfig::default() };
        let mut sim = Sim::new(decay_model(), cfg).unwrap();
        let mut n = 0;
        while sim.step().unwrap() {
            n += 1;
        }
        assert!(n > 0 && sim.finished());
        assert!((sim.time() - 1.0).abs() < 1e-12);
        let exact = (-1.0f64).exp();
        assert!((sim.state().first().copied().unwrap_or(0.0) - exact).abs() < 1e-5);
        // Calling step after finish is a no-op.
        assert!(!sim.step().unwrap());
    }

    #[test]
    fn bad_solver_settings_rejected() {
        assert!(Sim::new(decay_model(), SimConfig { solver: Solver::Rk4, dt: 0.0, tstop: 1.0, ..SimConfig::default() }).is_err());
        assert!(Sim::new(decay_model(), SimConfig { solver: Solver::Rk4, dt: 0.1, tstop: -1.0, ..SimConfig::default() }).is_err());
        assert!(Sim::new(decay_model(), SimConfig { solver: Solver::Rk45, dt: 0.1, tstop: 1.0, rtol: 0.0, ..SimConfig::default() }).is_err());
        assert!(Sim::new(decay_model(), SimConfig { solver: Solver::Rk45, dt: 0.1, tstop: 1.0, atol: -1.0, ..SimConfig::default() }).is_err());
        assert!(Sim::new(decay_model(), SimConfig { solver: Solver::Rk45, dt: 0.1, tstop: 1.0, min_step: -0.1, ..SimConfig::default() }).is_err());
    }
}
