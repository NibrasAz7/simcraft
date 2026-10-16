//! simblocks — the SimCraft M0 block library (L2).
//!
//! Every block implements `simcore::Block`; [`Factory`] instantiates them
//! from `.simcraft` `{type, params}` records (see `simmodel`). All
//! implementations are original — block names and documented semantics only.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod continuous;
mod discrete;
mod factory;
mod math;
mod params;
mod routing;
mod sinks;
mod sources;

pub use continuous::{Derivative, Integrator, TransferFcn};
pub use discrete::{UnitDelay, ZeroOrderHold};
pub use factory::{BlockInfo, Factory};
pub use math::{Abs, Gain, Product, Saturation, Sum, Switch};
pub use routing::{Demux, Mux};
pub use sinks::{Scope, ToWorkspace, Trace};
pub use sources::{Clock, Constant, Ramp, Sine, Step};

/// `as_any`/`as_any_mut` boilerplate for `Block` impls.
macro_rules! block_any {
    () => {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}
pub(crate) use block_any;
