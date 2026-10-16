//! Routing blocks: Mux concatenates N inputs into one vector port; Demux
//! splits one vector into N output ports.

use simcore::{Block, Params, PortReq, SigMeta, SigVal, SimError};

use crate::{block_any, params};

/// `mux {inputs}` — N real inputs → one vector output of the summed widths.
pub struct Mux {
    reqs: Vec<PortReq>,
}

impl Mux {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["inputs"])?;
        let n = params::count(p, "inputs", 2)?;
        Ok(Mux { reqs: vec![PortReq::REAL_ANY; n] })
    }
}

impl Block for Mux {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        1
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w: usize = ins.iter().filter_map(|m| *m).map(|m| m.width).sum();
        Ok(SigMeta::real(w))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let mut v = Vec::new();
        for i in 0..self.reqs.len() {
            v.extend_from_slice(simcore::in_real(u, i)?);
        }
        if let Some(slot) = y.get_mut(0) {
            *slot = SigVal::Real(v);
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}

/// `demux {outputs}` — one vector input → N scalar-slice outputs. The input
/// width must equal `outputs` for now (equal split of a vector is ambiguous
/// otherwise); wider splits come with the `widths` param later.
pub struct Demux {
    n: usize,
    reqs: [PortReq; 1],
}

impl Demux {
    pub fn new(p: &Params) -> Result<Self, SimError> {
        params::check_keys(p, &["outputs"])?;
        let n = params::count(p, "outputs", 2)?;
        Ok(Demux { n, reqs: [PortReq::REAL_ANY] })
    }
}

impl Block for Demux {
    fn inputs(&self) -> &[PortReq] {
        &self.reqs
    }
    fn n_outputs(&self) -> usize {
        self.n
    }
    fn resolve_out(&mut self, _p: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
        let w = ins.first().copied().flatten().map(|m| m.width).unwrap_or(0);
        if w != self.n {
            return Err(SimError::Msg(format!("demux splits a vector of width {w} into {} scalar ports — widths must match", self.n)));
        }
        Ok(SigMeta::real(1))
    }
    fn outputs(&mut self, _t: f64, _x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
        let v = simcore::in_real(u, 0)?;
        for i in 0..self.n {
            if let (Some(slot), Some(x)) = (y.get_mut(i), v.get(i)) {
                *slot = SigVal::scalar(*x);
            }
        }
        Ok(())
    }
    fn has_feedthrough(&self) -> bool {
        true
    }
    block_any!();
}
