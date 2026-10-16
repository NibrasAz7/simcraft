//! simcore — the SimCraft kernel (L1).
//!
//! A [`Model`] is a directed graph of [`Block`]s wired port-to-port. Ports are
//! typed ([`SigMeta`]: element width × [`DType`]); widths and dtypes propagate
//! at compile time so mismatches fail before `t = 0`, never mid-run.
//! [`Sim`] drives the compiled model with a fixed-step solver (`euler`,
//! `rk4`), evaluating outputs in topological order each step, firing discrete
//! `update`s on sample hits, and logging sinks once per major step.
//!
//! Dependency rule: `simcore` may depend on `matcore`/`matlang` but never on
//! simblocks, simmodel, or any UI crate.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod block;
mod model;
mod signal;
mod sim;

pub use block::{Block, BlockFactory, Param, Params, SampleTime};
pub use model::{CompiledModel, Model, Wire};
pub use signal::{in_real, DType, DTypeReq, PortReq, SigMeta, SigVal, WidthReq};
pub use sim::{Sim, SimConfig, SimOut, Solver};

use std::fmt;

/// Every failure the kernel can report. Anything untrusted (model files,
/// parameters, solver settings) surfaces here, never as a panic.
#[derive(Debug)]
pub enum SimError {
    /// A feedthrough cycle makes the step's output order unsolvable.
    AlgebraicLoop(Vec<String>),
    /// Two blocks share a hierarchical path.
    DuplicateBlock(String),
    /// A wire references a block or port that does not exist.
    BadWire(String),
    /// An input port has no driver.
    UnconnectedInput { block: String, port: usize },
    /// A port is driven by more than one wire.
    MultipleDrivers { block: String, port: usize },
    /// Source signal doesn't satisfy the destination port requirement.
    PortMismatch { src: String, dst: String, reason: String },
    /// A block rejects its parameters or resolved port signature.
    BadParam { block: String, msg: String },
    /// An input-derived size exceeded a safety cap.
    TooLarge(String),
    /// Anything else.
    Msg(String),
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimError::AlgebraicLoop(names) => write!(f, "algebraic loop through: {}", names.join(" -> ")),
            SimError::DuplicateBlock(n) => write!(f, "duplicate block path `{n}`"),
            SimError::BadWire(m) => write!(f, "bad wire: {m}"),
            SimError::UnconnectedInput { block, port } => write!(f, "block `{block}` input port {port} is unconnected"),
            SimError::MultipleDrivers { block, port } => write!(f, "block `{block}` input port {port} is driven by more than one wire"),
            SimError::PortMismatch { src, dst, reason } => write!(f, "signal mismatch {src} -> {dst}: {reason}"),
            SimError::BadParam { block, msg } => write!(f, "block `{block}`: {msg}"),
            SimError::TooLarge(m) => write!(f, "size limit exceeded: {m}"),
            SimError::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SimError {}

/// Caps on input-derived sizes (untrusted model files).
pub mod limits {
    /// Max blocks in one model.
    pub const MAX_BLOCKS: usize = 100_000;
    /// Max wires in one model.
    pub const MAX_WIRES: usize = 500_000;
    /// Max flat continuous states.
    pub const MAX_STATES: usize = 1_000_000;
    /// Max elements of one signal.
    pub const MAX_SIGNAL_WIDTH: usize = 1_000_000;
    /// Max fixed-step iterations in one `sim.run`.
    pub const MAX_STEPS: u64 = 50_000_000;
    /// Max samples one sink will record.
    pub const MAX_LOG_SAMPLES: usize = 2_000_000;
}
