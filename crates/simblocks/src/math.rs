//! Math blocks: Gain, Sum, Product, Abs, Saturation, Switch, DeadZone —
//! plus the comparison blocks Relational and LogicalOperator, which emit
//! `Bool` signals (e.g. to drive a Switch control port).

use simcore::{in_real, Block, Params, PortReq, SigMeta, SigVal, SimError};

use crate::{block_any, params};

/// `gain {gain}` — scalar or per-element vector gain; a scalar broadcasts
/// over a vector input.
pub struct Gain {
    k: Vec<f64>,
    reqs: [PortReq; 1],
}

impl Gain {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["gain"])?;
        let k = params::vec(p, "gain", vec![1.0])?;
        if k.is_empty() {
            return Err(SimError::Msg("gain `gain` must not be empty".into()));
        }
        Ok(Gain { k, reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for Gain {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let m = ins.first().copied().flatten().unwrap_or(SigMeta::real(1));
        if self.k.len() != 1 && self.k.len() != m.width {
            return Err(SimError::Msg(format!("gain length {} doesn't match input width {}", self.k.len(), m.width)));
        }
        Ok(m)
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let uu = in_real(u, 0)?;
        if let Some(SigVal::Real(v)) = y.get_mut(0) {
            for (i, o) in v.iter_mut().enumerate() {
                let k = self.k.get(i).copied().or_else(|| self.k.first().copied()).unwrap_or(1.0);
                *o = k * uu.get(i).copied().unwrap_or(0.0);
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `sum {signs}` — `"++-"`: one input per char, `+` adds and `-` subtracts.
/// All inputs must share one width; the output inherits it.
pub struct Sum {
    signs: Vec<bool>, // true = '+'
    reqs: Vec<PortReq>,
}

impl Sum {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["signs"])?;
        let s = params::string(p, "signs", "++")?;
        let mut signs = Vec::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '+' => signs.push(true),
                '-' => signs.push(false),
                _ => return Err(SimError::Msg(format!("sum `signs` may only contain '+'/'-', found '{c}'"))),
            }
        }
        if signs.is_empty() {
            return Err(SimError::Msg("sum `signs` must not be empty".into()));
        }
        let n = signs.len();
        Ok(Sum { signs, reqs: vec![PortReq::REAL_ANY; n] })
    }
}

/// Resolve a common width for equal-width input lists (`sum`, `product`).
fn common_width(ins: &[Option<SigMeta>], what: &str) -> Result<usize, SimError> {
    let mut w = None;
    for m in ins.iter().flatten() {
        match w {
            None => w = Some(m.width),
            Some(prev) if prev != m.width => return Err(SimError::Msg(format!("{what} inputs must share one width (saw {prev} and {})", m.width))),
            _ => {}
        }
    }
    Ok(w.unwrap_or(1))
}

impl Block for Sum {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(common_width(ins, "sum")?))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(SigVal::Real(v)) = y.get_mut(0) {
            for (i, o) in v.iter_mut().enumerate() {
                let mut acc = 0.0;
                for (p, &plus) in self.signs.iter().enumerate() {
                    let e = in_real(u, p)?.get(i).copied().unwrap_or(0.0);
                    if plus {
                        acc += e;
                    } else {
                        acc -= e;
                    }
                }
                *o = acc;
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `product {ops}` — `"*/"`: `*` multiplies, `/` divides into the running
/// product. All inputs share one width.
pub struct Product {
    divs: Vec<bool>, // true = divide
    reqs: Vec<PortReq>,
}

impl Product {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["ops"])?;
        let s = params::string(p, "ops", "**")?;
        let mut divs = Vec::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '*' => divs.push(false),
                '/' => divs.push(true),
                _ => return Err(SimError::Msg(format!("product `ops` may only contain '*'/'/', found '{c}'"))),
            }
        }
        if divs.is_empty() {
            return Err(SimError::Msg("product `ops` must not be empty".into()));
        }
        let n = divs.len();
        Ok(Product { divs, reqs: vec![PortReq::REAL_ANY; n] })
    }
}

impl Block for Product {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(SigMeta::real(common_width(ins, "product")?))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(SigVal::Real(v)) = y.get_mut(0) {
            for (i, o) in v.iter_mut().enumerate() {
                let mut acc = 1.0;
                for (p, &div) in self.divs.iter().enumerate() {
                    let e = in_real(u, p)?.get(i).copied().unwrap_or(0.0);
                    if div {
                        acc /= e;
                    } else {
                        acc *= e;
                    }
                }
                *o = acc;
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `abs {}` — elementwise absolute value.
pub struct Abs {
    reqs: [PortReq; 1],
}

impl Abs {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &[])?;
        Ok(Abs { reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for Abs {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(ins.first().copied().flatten().unwrap_or(SigMeta::real(1)))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let uu = in_real(u, 0)?;
        if let Some(SigVal::Real(v)) = y.get_mut(0) {
            for (o, i) in v.iter_mut().zip(uu.iter()) {
                *o = i.abs();
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `saturation {lower, upper}` — elementwise clamp.
pub struct Saturation {
    lo: f64,
    hi: f64,
    reqs: [PortReq; 1],
}

impl Saturation {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["lower", "upper"])?;
        let lo = params::num(p, "lower", -0.5)?;
        let hi = params::num(p, "upper", 0.5)?;
        if lo > hi {
            return Err(SimError::Msg("saturation `lower` must be <= `upper`".into()));
        }
        Ok(Saturation { lo, hi, reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for Saturation {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(ins.first().copied().flatten().unwrap_or(SigMeta::real(1)))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let uu = in_real(u, 0)?;
        if let Some(SigVal::Real(v)) = y.get_mut(0) {
            for (o, i) in v.iter_mut().zip(uu.iter()) {
                *o = i.clamp(self.lo, self.hi);
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `switch {threshold, criteria}` — three inputs; the control `u2` selects
/// `u1` when it passes `criteria` (`"u2>=threshold"` default,
/// `"u2>threshold"`, `"u2~=0"`), else `u3`. A scalar `u2` selects the whole
/// vector; a vector `u2` selects elementwise. `u2` may be real or bool.
#[derive(Debug, Clone, Copy)]
enum Criteria {
    Gte,
    Gt,
    Neq,
}

pub struct Switch {
    threshold: f64,
    crit: Criteria,
    reqs: [PortReq; 3],
}

impl Switch {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["threshold", "criteria"])?;
        let crit = match params::string(p, "criteria", "u2>=threshold")?.as_str() {
            "u2>=threshold" => Criteria::Gte,
            "u2>threshold" => Criteria::Gt,
            "u2~=0" => Criteria::Neq,
            other => return Err(SimError::Msg(format!("switch `criteria` `{other}` unknown (have: u2>=threshold, u2>threshold, u2~=0)"))),
        };
        Ok(Switch { threshold: params::num(p, "threshold", 0.0)?, crit, reqs: [PortReq::REAL_ANY, PortReq::ANY, PortReq::REAL_ANY] })
    }

    fn pass(&self, c: f64) -> bool {
        match self.crit {
            Criteria::Gte => c >= self.threshold,
            Criteria::Gt => c > self.threshold,
            Criteria::Neq => c != 0.0,
        }
    }
}

impl Block for Switch {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w1 = ins.first().copied().flatten().map(|m| m.width).unwrap_or(1);
        let w3 = ins.get(2).copied().flatten().map(|m| m.width).unwrap_or(w1);
        if w1 != w3 {
            return Err(SimError::Msg(format!("switch data inputs must share one width (saw {w1} and {w3})")));
        }
        if let Some(c) = ins.get(1).copied().flatten() {
            if c.width != 1 && c.width != w1 {
                return Err(SimError::Msg(format!("switch control width {} must be 1 or {}", c.width, w1)));
            }
        }
        Ok(SigMeta::real(w1))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let u1 = in_real(u, 0)?;
        let u3 = in_real(u, 2)?;
        if let Some(SigVal::Real(v)) = y.get_mut(0) {
            for (i, o) in v.iter_mut().enumerate() {
                let c = match u.get(1).copied() {
                    Some(SigVal::Bool(b)) => b.get(if b.len() == 1 { 0 } else { i }).copied().unwrap_or(false),
                    Some(SigVal::Real(r)) => self.pass(r.get(if r.len() == 1 { 0 } else { i }).copied().unwrap_or(0.0)),
                    None => false,
                };
                *o = if c { u1.get(i).copied().unwrap_or(0.0) } else { u3.get(i).copied().unwrap_or(0.0) };
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `dead_zone {lower, upper}` — zero inside `[lower, upper]`; outside, the
/// amount by which the input passes the bound (`u−upper` above,
/// `u−lower` below). Elementwise.
pub struct DeadZone {
    lo: f64,
    hi: f64,
    reqs: [PortReq; 1],
}

impl DeadZone {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["lower", "upper"])?;
        let lo = params::num(p, "lower", -0.5)?;
        let hi = params::num(p, "upper", 0.5)?;
        if lo > hi {
            return Err(SimError::Msg("dead_zone `lower` must be <= `upper`".into()));
        }
        Ok(DeadZone { lo, hi, reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for DeadZone {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        Ok(ins.first().copied().flatten().unwrap_or(SigMeta::real(1)))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let uu = in_real(u, 0)?;
        if let Some(SigVal::Real(v)) = y.get_mut(0) {
            for (o, i) in v.iter_mut().zip(uu.iter()) {
                *o = if *i > self.hi {
                    i - self.hi
                } else if *i < self.lo {
                    i - self.lo
                } else {
                    0.0
                };
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `relational {op}` — two inputs, elementwise comparison, bool output.
/// `op` is one of `==`, `~=`, `<`, `<=`, `>`, `>=` (default `<=`). A
/// width-1 input broadcasts against a wider other input.
pub struct Relational {
    op: RelOp,
    reqs: [PortReq; 2],
}

/// Comparison operators for [`Relational`].
#[derive(Debug, Clone, Copy)]
enum RelOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl RelOp {
    fn eval(self, a: f64, b: f64) -> bool {
        match self {
            RelOp::Eq => a == b,
            RelOp::Ne => a != b,
            RelOp::Lt => a < b,
            RelOp::Le => a <= b,
            RelOp::Gt => a > b,
            RelOp::Ge => a >= b,
        }
    }
}

impl Relational {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["op"])?;
        let op = match params::string(p, "op", "<=")?.as_str() {
            "==" => RelOp::Eq,
            "~=" => RelOp::Ne,
            "<" => RelOp::Lt,
            "<=" => RelOp::Le,
            ">" => RelOp::Gt,
            ">=" => RelOp::Ge,
            other => return Err(SimError::Msg(format!("relational `op` `{other}` unknown (have: ==, ~=, <, <=, >, >=)"))),
        };
        Ok(Relational { op, reqs: [PortReq::REAL_ANY, PortReq::REAL_ANY] })
    }
}

/// Element `i` of an input that may be width-1 broadcast or full width.
fn elem_or_first(v: &[f64], i: usize) -> f64 {
    v.get(if v.len() == 1 { 0 } else { i }).copied().unwrap_or(0.0)
}

/// `true` value of an input element: bool as-is, real as `!= 0`.
fn truthy(s: Option<&SigVal>, i: usize) -> bool {
    match s {
        Some(SigVal::Bool(v)) => v.get(if v.len() == 1 { 0 } else { i }).copied().unwrap_or(false),
        Some(SigVal::Real(v)) => elem_or_first(v, i) != 0.0,
        None => false,
    }
}

/// Common width for blocks that broadcast width-1 inputs (`relational`,
/// `logical_operator`); `None` widths count as scalar.
fn broadcast_width(ins: &[Option<SigMeta>], what: &str) -> Result<usize, SimError> {
    let mut w = 1usize;
    for m in ins.iter().flatten() {
        if m.width != 1 {
            if w != 1 && w != m.width {
                return Err(SimError::Msg(format!("{what} input widths must match or be 1 (saw {w} and {})", m.width)));
            }
            w = m.width;
        }
    }
    Ok(w)
}

impl Block for Relational {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w = broadcast_width(ins, "relational")?;
        Ok(SigMeta { width: w, dtype: simcore::DType::Bool })
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let a = in_real(u, 0)?;
        let b = in_real(u, 1)?;
        if let Some(SigVal::Bool(v)) = y.get_mut(0) {
            for (i, o) in v.iter_mut().enumerate() {
                *o = self.op.eval(elem_or_first(a, i), elem_or_first(b, i));
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `logical_operator {op, inputs}` — elementwise boolean combine.
/// `op` is `and`/`or`/`xor`/`nand`/`nor` (default `and`, `inputs`
/// ports, default 2) or `not` (exactly 1 port). Inputs may be bool or
/// real (nonzero is true); width-1 inputs broadcast.
pub struct LogicalOperator {
    op: LogOp,
    reqs: Vec<PortReq>,
}

#[derive(Debug, Clone, Copy)]
enum LogOp {
    And,
    Or,
    Xor,
    Nand,
    Nor,
    Not,
}

impl LogicalOperator {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["op", "inputs"])?;
        let op = match params::string(p, "op", "and")?.as_str() {
            "and" => LogOp::And,
            "or" => LogOp::Or,
            "xor" => LogOp::Xor,
            "nand" => LogOp::Nand,
            "nor" => LogOp::Nor,
            "not" => LogOp::Not,
            other => {
                return Err(SimError::Msg(format!("logical_operator `op` `{other}` unknown (have: and, or, xor, nand, nor, not)")));
            }
        };
        let n = params::count(p, "inputs", 2)?;
        let n = match op {
            LogOp::Not => {
                if n != 1 {
                    return Err(SimError::Msg("logical_operator `not` takes exactly 1 input".into()));
                }
                1
            }
            _ => n,
        };
        Ok(LogicalOperator { op, reqs: vec![PortReq::ANY; n] })
    }
}

impl Block for LogicalOperator {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w = broadcast_width(ins, "logical_operator")?;
        Ok(SigMeta { width: w, dtype: simcore::DType::Bool })
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        if let Some(SigVal::Bool(v)) = y.get_mut(0) {
            for (i, o) in v.iter_mut().enumerate() {
                *o = match self.op {
                    LogOp::Not => !truthy(u.first().copied(), i),
                    LogOp::And => u.iter().all(|s| truthy(Some(*s), i)),
                    LogOp::Or => u.iter().any(|s| truthy(Some(*s), i)),
                    LogOp::Xor => u.iter().filter(|s| truthy(Some(*s), i)).count() % 2 == 1,
                    LogOp::Nand => !u.iter().all(|s| truthy(Some(*s), i)),
                    LogOp::Nor => !u.iter().any(|s| truthy(Some(*s), i)),
                };
            }
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

    fn pstr(k: &str, v: &str) -> Params {
        let mut p = BTreeMap::new();
        p.insert(k.to_string(), simcore::Param::Str(v.to_string()));
        p
    }

    fn run1(b: &mut dyn Block, ins: &[SigVal]) -> Vec<f64> {
        let refs: Vec<&SigVal> = ins.iter().collect();
        let meta: Vec<Option<SigMeta>> = ins.iter().map(|s| Some(s.meta())).collect();
        b.resolve_out(0, &meta).unwrap();
        let mut y = vec![SigVal::scalar(0.0); b.n_outputs()];
        // size the buffer to the resolved width
        let m = ins.first().map(|s| s.meta()).unwrap_or(SigMeta::real(1));
        y[0] = SigVal::zeros(m);
        b.outputs(0.0, &[], &refs, &mut y).unwrap();
        y[0].as_real().unwrap().to_vec()
    }

    #[test]
    fn gain_scalar_and_vector() {
        let mut g = Gain::new(&BTreeMap::from([("gain".to_string(), simcore::Param::Num(3.0))])).unwrap();
        assert_eq!(run1(&mut g, &[SigVal::Real(vec![1.0, -2.0])]), vec![3.0, -6.0]);
    }

    #[test]
    fn sum_signs() {
        let mut s = Sum::new(&pstr("signs", "+-")).unwrap();
        let out = run1(&mut s, &[SigVal::scalar(5.0), SigVal::scalar(2.0)]);
        assert_eq!(out, vec![3.0]);
    }

    #[test]
    fn product_ops() {
        let mut pr = Product::new(&pstr("ops", "*/")).unwrap();
        let out = run1(&mut pr, &[SigVal::scalar(6.0), SigVal::scalar(2.0)]);
        assert_eq!(out, vec![3.0]);
    }

    #[test]
    fn saturation_clamps() {
        let mut p = BTreeMap::new();
        p.insert("lower".to_string(), simcore::Param::Num(-1.0));
        p.insert("upper".to_string(), simcore::Param::Num(1.0));
        let mut s = Saturation::new(&p).unwrap();
        assert_eq!(run1(&mut s, &[SigVal::Real(vec![-5.0, 0.3, 9.0])]), vec![-1.0, 0.3, 1.0]);
    }

    #[test]
    fn switch_selects() {
        let mut s = Switch::new(&BTreeMap::new()).unwrap();
        let ins = [SigVal::scalar(10.0), SigVal::scalar(1.0), SigVal::scalar(-7.0)];
        assert_eq!(run1(&mut s, &ins), vec![10.0]); // u2=1 >= 0 → u1
        let ins = [SigVal::scalar(10.0), SigVal::scalar(-1.0), SigVal::scalar(-7.0)];
        assert_eq!(run1(&mut s, &ins), vec![-7.0]); // u2=-1 < 0 → u3
    }

    fn run_bool(b: &mut dyn Block, ins: &[SigVal]) -> Vec<bool> {
        let refs: Vec<&SigVal> = ins.iter().collect();
        let meta: Vec<Option<SigMeta>> = ins.iter().map(|s| Some(s.meta())).collect();
        let m = b.resolve_out(0, &meta).unwrap();
        let mut y = vec![SigVal::zeros(m)];
        b.outputs(0.0, &[], &refs, &mut y).unwrap();
        y[0].as_bool().unwrap().to_vec()
    }

    #[test]
    fn dead_zone_kills_inner_band() {
        let mut p = BTreeMap::new();
        p.insert("lower".to_string(), simcore::Param::Num(-0.5));
        p.insert("upper".to_string(), simcore::Param::Num(0.5));
        let mut d = DeadZone::new(&p).unwrap();
        let got = run1(&mut d, &[SigVal::Real(vec![-2.0, -0.3, 0.0, 0.7, 3.0])]);
        for (g, want) in got.iter().zip([-1.5, 0.0, 0.0, 0.2, 2.5]) {
            assert!((g - want).abs() < 1e-12, "{got:?}");
        }
    }

    #[test]
    fn relational_compares_with_broadcast() {
        let mut p = BTreeMap::new();
        p.insert("op".to_string(), simcore::Param::Str(">".into()));
        let mut r = Relational::new(&p).unwrap();
        // vector vs scalar broadcast
        let ins = [SigVal::Real(vec![1.0, 5.0, -2.0]), SigVal::scalar(3.0)];
        assert_eq!(run_bool(&mut r, &ins), vec![false, true, false]);
        let mut r = Relational::new(&BTreeMap::new()).unwrap(); // default <=
        let ins = [SigVal::scalar(2.0), SigVal::scalar(2.0)];
        assert_eq!(run_bool(&mut r, &ins), vec![true]);
    }

    #[test]
    fn logical_combines_bool_and_real() {
        let mut p = BTreeMap::new();
        p.insert("op".to_string(), simcore::Param::Str("and".into()));
        p.insert("inputs".to_string(), simcore::Param::Num(2.0));
        let mut l = LogicalOperator::new(&p).unwrap();
        // A bool input feeds straight from a relational block's output.
        let ins = [SigVal::Bool(vec![true, false]), SigVal::Real(vec![1.0, 1.0])];
        assert_eq!(run_bool(&mut l, &ins), vec![true, false]);
        let mut p = BTreeMap::new();
        p.insert("op".to_string(), simcore::Param::Str("not".into()));
        p.insert("inputs".to_string(), simcore::Param::Num(1.0));
        let mut l = LogicalOperator::new(&p).unwrap();
        assert_eq!(run_bool(&mut l, &[SigVal::Bool(vec![true, false])]), vec![false, true]);
    }

    #[test]
    fn relational_rejects_bad_op_and_mismatched_widths() {
        let mut p = BTreeMap::new();
        p.insert("op".to_string(), simcore::Param::Str("??".into()));
        assert!(Relational::new(&p).is_err());
        let mut r = Relational::new(&BTreeMap::new()).unwrap();
        let metas = [Some(SigMeta::real(2)), Some(SigMeta::real(3))];
        assert!(r.resolve_out(0, &metas).is_err());
        let ok = [Some(SigMeta::real(3)), Some(SigMeta::real(1))];
        assert_eq!(r.resolve_out(0, &ok).unwrap(), SigMeta { width: 3, dtype: simcore::DType::Bool });
    }
}
