//! `Factory` — builds blocks from `.simcraft` `{type, params}` records.
//! `simmodel` stays block-library-agnostic by going through this trait.

use simcore::{Block, BlockFactory, Params, SimError};

use crate::{
    Abs, Clock, Constant, DeadZone, Demux, Derivative, Gain, Integrator, LogicalOperator, Mux, PidController, Product, Ramp, Relational, Saturation,
    Scope, Sine, Step, Sum, Switch, ToWorkspace, TransferFcn, TransportDelay, UnitDelay, ZeroOrderHold,
};

/// One catalog row: document type name + what the palette shows.
pub struct BlockInfo {
    pub type_name: &'static str,
    pub label: &'static str,
}

/// The built-in block library.
pub struct Factory;

const CATALOG: &[BlockInfo] = &[
    BlockInfo { type_name: "constant", label: "Constant" },
    BlockInfo { type_name: "step", label: "Step" },
    BlockInfo { type_name: "sine", label: "Sine Wave" },
    BlockInfo { type_name: "ramp", label: "Ramp" },
    BlockInfo { type_name: "clock", label: "Clock" },
    BlockInfo { type_name: "gain", label: "Gain" },
    BlockInfo { type_name: "sum", label: "Sum" },
    BlockInfo { type_name: "product", label: "Product" },
    BlockInfo { type_name: "abs", label: "Abs" },
    BlockInfo { type_name: "saturation", label: "Saturation" },
    BlockInfo { type_name: "dead_zone", label: "Dead Zone" },
    BlockInfo { type_name: "switch", label: "Switch" },
    BlockInfo { type_name: "relational", label: "Relational Operator" },
    BlockInfo { type_name: "logical_operator", label: "Logical Operator" },
    BlockInfo { type_name: "integrator", label: "Integrator" },
    BlockInfo { type_name: "derivative", label: "Derivative" },
    BlockInfo { type_name: "transfer_fcn", label: "Transfer Fcn" },
    BlockInfo { type_name: "pid", label: "PID Controller" },
    BlockInfo { type_name: "transport_delay", label: "Transport Delay" },
    BlockInfo { type_name: "unit_delay", label: "Unit Delay" },
    BlockInfo { type_name: "zero_order_hold", label: "Zero-Order Hold" },
    BlockInfo { type_name: "mux", label: "Mux" },
    BlockInfo { type_name: "demux", label: "Demux" },
    BlockInfo { type_name: "scope", label: "Scope" },
    BlockInfo { type_name: "to_workspace", label: "To Workspace" },
];

impl Factory {
    /// The palette/catalog — drives `block.list` and any UI palette.
    pub fn catalog() -> &'static [BlockInfo] {
        CATALOG
    }
}

impl BlockFactory for Factory {
    fn make(&self, type_name: &str, params: &Params, path: &str) -> Result<Box<dyn Block>, SimError> {
        let err = |e: SimError| SimError::Msg(format!("block `{path}` ({type_name}): {e}"));
        let b: Box<dyn Block> = match type_name {
            "constant" => Box::new(Constant::new(params).map_err(err)?),
            "step" => Box::new(Step::new(params).map_err(err)?),
            "sine" => Box::new(Sine::new(params).map_err(err)?),
            "ramp" => Box::new(Ramp::new(params).map_err(err)?),
            "clock" => Box::new(Clock::new(params).map_err(err)?),
            "gain" => Box::new(Gain::new(params).map_err(err)?),
            "sum" => Box::new(Sum::new(params).map_err(err)?),
            "product" => Box::new(Product::new(params).map_err(err)?),
            "abs" => Box::new(Abs::new(params).map_err(err)?),
            "saturation" => Box::new(Saturation::new(params).map_err(err)?),
            "dead_zone" => Box::new(DeadZone::new(params).map_err(err)?),
            "switch" => Box::new(Switch::new(params).map_err(err)?),
            "relational" => Box::new(Relational::new(params).map_err(err)?),
            "logical_operator" => Box::new(LogicalOperator::new(params).map_err(err)?),
            "integrator" => Box::new(Integrator::new(params).map_err(err)?),
            "derivative" => Box::new(Derivative::new(params).map_err(err)?),
            "transfer_fcn" => Box::new(TransferFcn::new(params).map_err(err)?),
            "pid" => Box::new(PidController::new(params).map_err(err)?),
            "transport_delay" => Box::new(TransportDelay::new(params).map_err(err)?),
            "unit_delay" => Box::new(UnitDelay::new(params).map_err(err)?),
            "zero_order_hold" => Box::new(ZeroOrderHold::new(params).map_err(err)?),
            "mux" => Box::new(Mux::new(params).map_err(err)?),
            "demux" => Box::new(Demux::new(params).map_err(err)?),
            "scope" => Box::new(Scope::new(params).map_err(err)?),
            "to_workspace" => Box::new(ToWorkspace::new(params).map_err(err)?),
            other => {
                return Err(SimError::Msg(format!(
                    "unknown block type `{other}` for `{path}` (have: {})",
                    CATALOG.iter().map(|i| i.type_name).collect::<Vec<_>>().join(", ")
                )));
            }
        };
        Ok(b)
    }

    fn known_types(&self) -> Vec<&'static str> {
        CATALOG.iter().map(|i| i.type_name).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn catalog_types_all_build() {
        let f = Factory;
        let p = BTreeMap::new();
        for t in f.known_types() {
            // Every block must build with defaults — no required params
            // except transfer_fcn.den, which needs an explicit value.
            let mut pp = p.clone();
            if t == "transfer_fcn" {
                pp.insert("den".to_string(), simcore::Param::Vec(vec![1.0, 1.0]));
            }
            assert!(f.make(t, &pp, "test/b").is_ok(), "{t} failed to build with defaults");
        }
    }

    #[test]
    fn unknown_type_lists_catalog() {
        let f = Factory;
        match f.make("nonsense", &BTreeMap::new(), "m/b") {
            Err(SimError::Msg(m)) => assert!(m.contains("unknown block type") && m.contains("integrator")),
            Err(e) => panic!("expected Msg error, got {e:?}"),
            Ok(_) => panic!("expected error, got a block"),
        }
    }
}
