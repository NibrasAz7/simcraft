//! The `Block` trait — one node of a model graph — and the parameter type
//! factories receive from `simmodel` documents.

use std::any::Any;
use std::collections::BTreeMap;

use crate::{PortReq, SigMeta, SigVal, SimError};

/// How a block advances in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SampleTime {
    /// Evaluated every solver step; may own continuous states.
    Continuous,
    /// Discrete block with fixed sample time `Ts` (seconds); `update` fires
    /// on sample hits. Requires `has_feedthrough() == false` so a sample hit
    /// never disturbs the algebraic order mid-step.
    Discrete(f64),
}

/// A block parameter value, converted from the JSON document by `simmodel`.
/// Numbers, number arrays, strings and bools only — anything richer is a
/// model error.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Num(f64),
    Vec(Vec<f64>),
    Str(String),
    Bool(bool),
}

impl Param {
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Param::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_vec(&self) -> Option<&[f64]> {
        match self {
            Param::Vec(v) => Some(v),
            // A scalar is a length-1 vector for params like `x0`/`gain`.
            Param::Num(n) => Some(std::slice::from_ref(n)),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Param::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Param::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Named parameter map handed to a [`BlockFactory`].
pub type Params = BTreeMap<String, Param>;

/// One node of a model graph. Blocks own their internal state; continuous
/// states are extracted to a flat `x` vector the solver owns, via
/// [`Block::n_states`]/[`Block::get_x`]/[`Block::set_x`].
///
/// Lifetimes inside a step (documented order, fixed-step solver):
/// `outputs` → `update` (on discrete sample hits) → re-`outputs` →
/// sink `log` → `derivatives` + integration.
pub trait Block {
    /// Input port requirements; `len()` is the input port count.
    fn inputs(&self) -> &[PortReq];

    /// Number of output ports.
    fn n_outputs(&self) -> usize;

    /// Resolve output `port`'s signature from the resolved inputs
    /// (`None` = unconnected). Called once at model compile; may adjust
    /// internal sizing (e.g. an Integrator broadcasting a scalar `x0`).
    fn resolve_out(&mut self, port: usize, ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError>;

    /// Write this block's outputs at `(t, x)` given inputs `u`.
    /// `y` arrives pre-sized to `n_outputs` with correctly-typed buffers;
    /// overwrite the elements.
    fn outputs(&mut self, t: f64, x: &[f64], u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError>;

    /// Write continuous-state derivatives into `dx` (same slice layout as
    /// `x`). `dx` is zeroed before the call; the default is a no-op for
    /// stateless blocks.
    fn derivatives(&mut self, _t: f64, _x: &[f64], _u: &[&SigVal], _dx: &mut [f64]) -> Result<(), SimError> {
        Ok(())
    }

    /// Advance discrete states on a sample hit; inputs are the signals
    /// already evaluated at this step's time. Default: no discrete state.
    fn update(&mut self, _t: f64, _u: &[&SigVal]) -> Result<(), SimError> {
        Ok(())
    }

    /// Sink hook called once per major step, after the post-update output
    /// pass. Scopes and To Workspace blocks record here (never inside
    /// `outputs`, which also runs inside solver stages).
    fn log(&mut self, _t: f64, _u: &[&SigVal]) -> Result<(), SimError> {
        Ok(())
    }

    /// Number of continuous states contributed to the flat `x` vector.
    fn n_states(&self) -> usize {
        0
    }

    /// Copy this block's continuous states into `x` (len `n_states`).
    fn get_x(&self, _x: &mut [f64]) -> Result<(), SimError> {
        Ok(())
    }

    /// Load continuous states back from `x` (len `n_states`).
    fn set_x(&mut self, _x: &[f64]) -> Result<(), SimError> {
        Ok(())
    }

    /// This block's sample time.
    fn sample_time(&self) -> SampleTime {
        SampleTime::Continuous
    }

    /// True if any output depends on the inputs at the same instant
    /// (direct feedthrough). Drives execution order and algebraic-loop
    /// detection, so it must be honest — false positives report phantom
    /// loops, false negatives produce stale-data steps.
    fn has_feedthrough(&self) -> bool;

    /// Downcast support so hosts can reach sink data (e.g. a Scope's log)
    /// after a run.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Builds blocks from the document's `{type, params}` records. Implemented
/// by `simblocks::Factory`; keeping the trait here lets `simmodel` build
/// models without depending on the block library (same layer).
pub trait BlockFactory {
    /// Instantiate one block, or report an unknown type / bad params.
    fn make(&self, type_name: &str, params: &Params, path: &str) -> Result<Box<dyn Block>, SimError>;

    /// Every type name `make` accepts — for `model.check` and `block.list`.
    fn known_types(&self) -> Vec<&'static str>;
}
