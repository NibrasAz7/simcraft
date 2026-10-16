//! The model graph: blocks keyed by hierarchical path, wires between ports,
//! and `compile` — signal propagation, port checking, execution order and
//! algebraic-loop detection, all before `t = 0`.

use std::collections::HashMap;

use crate::limits;
use crate::{Block, SampleTime, SigMeta, SigVal, SimError};

/// A port-to-port connection. Port indices are 0-based internally
/// (`.simcraft` files number ports from 1, converted on load).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    pub src_block: usize,
    pub src_port: usize,
    pub dst_block: usize,
    pub dst_port: usize,
}

/// A model: named blocks plus wires. Build it programmatically or via
/// `simmodel::Document::to_model`; then [`Model::compile`] validates it and
/// [`crate::Sim`] runs it.
pub struct Model {
    names: Vec<String>,
    index: HashMap<String, usize>,
    blocks: Vec<Box<dyn Block>>,
    wires: Vec<Wire>,
}

impl Model {
    pub fn new() -> Self {
        Model { names: Vec::new(), index: HashMap::new(), blocks: Vec::new(), wires: Vec::new() }
    }

    /// Add a block at hierarchical path `path` (`top/subsys/blk`). Returns
    /// its index.
    pub fn add_block(&mut self, path: &str, block: Box<dyn Block>) -> Result<usize, SimError> {
        if self.blocks.len() >= limits::MAX_BLOCKS {
            return Err(SimError::TooLarge(format!("more than {} blocks", limits::MAX_BLOCKS)));
        }
        if path.is_empty() || path.split('/').any(|seg| seg.is_empty()) {
            return Err(SimError::Msg(format!("invalid block path `{path}`")));
        }
        if self.index.contains_key(path) {
            return Err(SimError::DuplicateBlock(path.to_string()));
        }
        let i = self.blocks.len();
        self.blocks.push(block);
        self.names.push(path.to_string());
        self.index.insert(path.to_string(), i);
        Ok(i)
    }

    /// Connect `src` (block index, 0-based out port) to `dst`.
    pub fn add_wire(&mut self, src_block: usize, src_port: usize, dst_block: usize, dst_port: usize) -> Result<(), SimError> {
        if self.wires.len() >= limits::MAX_WIRES {
            return Err(SimError::TooLarge(format!("more than {} wires", limits::MAX_WIRES)));
        }
        let sb = self.names.get(src_block).map(String::as_str).unwrap_or("?");
        let db = self.names.get(dst_block).map(String::as_str).unwrap_or("?");
        let Some(sb_) = self.blocks.get(src_block) else {
            return Err(SimError::BadWire(format!("source block index {src_block} out of range")));
        };
        if src_port >= sb_.n_outputs() {
            return Err(SimError::BadWire(format!("`{sb}` has no output port {}", src_port + 1)));
        }
        let Some(db_) = self.blocks.get(dst_block) else {
            return Err(SimError::BadWire(format!("destination block index {dst_block} out of range")));
        };
        if dst_port >= db_.inputs().len() {
            return Err(SimError::BadWire(format!("`{db}` has no input port {}", dst_port + 1)));
        }
        self.wires.push(Wire { src_block, src_port, dst_block, dst_port });
        Ok(())
    }

    /// Look up a block's index by path.
    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.index.get(path).copied()
    }

    /// Block paths in insertion order.
    pub fn block_names(&self) -> &[String] {
        &self.names
    }

    /// All wires.
    pub fn wires(&self) -> &[Wire] {
        &self.wires
    }

    /// Number of blocks.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// True when the model has no blocks.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Shared access to block `i` as a trait object.
    pub fn blocks_ref(&self, i: usize) -> Option<&dyn Block> {
        self.blocks.get(i).map(|b| b.as_ref())
    }

    /// Mutable access to block `i` as a trait object.
    pub fn blocks_mut(&mut self, i: usize) -> Result<&mut dyn Block, SimError> {
        match self.blocks.get_mut(i) {
            Some(b) => Ok(b.as_mut()),
            None => Err(SimError::Msg(format!("block index {i} out of range"))),
        }
    }

    /// Typed access to a block by path — e.g. `model.block::<Scope>("scope1")`
    /// to read logged data after a run.
    pub fn block<T: 'static>(&self, path: &str) -> Option<&T> {
        self.index.get(path).and_then(|&i| self.blocks.get(i)).and_then(|b| b.as_any().downcast_ref::<T>())
    }

    /// Mutable typed access to a block by path.
    pub fn block_mut<T: 'static>(&mut self, path: &str) -> Option<&mut T> {
        match self.index.get(path) {
            Some(&i) => self.blocks.get_mut(i).and_then(|b| b.as_any_mut().downcast_mut::<T>()),
            None => None,
        }
    }

    /// Validate and freeze the model for execution.
    ///
    /// Steps: every input port exactly one driver → propagate widths/dtypes
    /// in full topological order → derive the execution order from the
    /// feedthrough subgraph (a cycle there is an algebraic loop) → lay out
    /// the flat continuous-state vector and discrete sample rates.
    ///
    /// Needs `&mut self` because `resolve_out` may adjust block internals
    /// (e.g. broadcast a scalar initial condition to the input width).
    pub fn compile(&mut self) -> Result<CompiledModel, SimError> {
        let n = self.blocks.len();

        // --- input binding: exactly one driver per input port ---
        // inputs[b][p] = Some((src_block, src_port))
        let mut inputs: Vec<Vec<Option<(usize, usize)>>> = Vec::with_capacity(n);
        let mut problems: Vec<String> = Vec::new();
        for b in self.blocks.iter() {
            inputs.push(vec![None; b.inputs().len()]);
        }
        for w in &self.wires {
            match inputs.get_mut(w.dst_block).and_then(|v| v.get_mut(w.dst_port)) {
                Some(slot @ None) => *slot = Some((w.src_block, w.src_port)),
                Some(Some(_)) => {
                    problems.push(SimError::MultipleDrivers { block: self.name(w.dst_block), port: w.dst_port + 1 }.to_string());
                }
                None => {}
            }
        }
        // Every declared input port must be driven.
        for i in 0..self.blocks.len() {
            for (p, slot) in inputs.get(i).map(|v| v.as_slice()).unwrap_or(&[]).iter().enumerate() {
                if slot.is_none() {
                    problems.push(SimError::UnconnectedInput { block: self.name(i), port: p + 1 }.to_string());
                }
            }
        }
        if !problems.is_empty() {
            return Err(SimError::Msg(problems.join("; ")));
        }

        // --- execution order: topological over feedthrough edges only ---
        // Computed before signal propagation because real models DO contain
        // wire cycles — feedback through a non-feedthrough block (integrator,
        // unit delay) is the whole point. Only a feedthrough cycle is an
        // algebraic loop.
        let feed = |dst: usize| self.blocks.get(dst).map(|b| b.has_feedthrough()).unwrap_or(false);
        let exec_order = match topo_sort(n, &self.wires, feed) {
            Some(o) => o,
            None => {
                let cycle = find_cycle(n, &self.wires, feed);
                let names: Vec<String> = cycle.iter().map(|&i| self.name(i)).collect();
                return Err(SimError::AlgebraicLoop(names));
            }
        };

        // --- signal propagation: fixpoint over the feedthrough order ---
        // Inputs driven by a not-yet-resolved source arrive as `None`; blocks
        // resolve their outputs from parameters in that case (e.g. an
        // integrator's width comes from `x0`). Iterate until stable.
        let mut metas: Vec<Vec<Option<SigMeta>>> = self.blocks.iter().map(|b| vec![None; b.n_outputs()]).collect();
        let mut last_err: Option<SimError> = None;
        for _round in 0..=n {
            let mut changed = false;
            let mut round_err = None;
            for &b in &exec_order {
                let ins: Vec<Option<SigMeta>> = match inputs.get(b) {
                    Some(v) => v.iter().map(|src| src.and_then(|(sb, sp)| metas.get(sb).and_then(|m| m.get(sp)).copied().flatten())).collect(),
                    None => Vec::new(),
                };
                for port in 0..self.blocks[b].n_outputs() {
                    match self.blocks[b].resolve_out(port, &ins) {
                        Ok(meta) => {
                            if meta.width > limits::MAX_SIGNAL_WIDTH {
                                round_err = Some(SimError::TooLarge(format!("`{}` signal width {}", self.name(b), meta.width)));
                                continue;
                            }
                            let slot = metas.get_mut(b).and_then(|m| m.get_mut(port));
                            if let Some(s) = slot {
                                if *s != Some(meta) {
                                    *s = Some(meta);
                                    changed = true;
                                }
                            }
                        }
                        Err(e) => {
                            round_err = Some(SimError::Msg(format!("`{}` output {}: {e}", self.name(b), port + 1)));
                        }
                    }
                }
            }
            last_err = round_err;
            if !changed {
                break;
            }
        }
        if let Some(e) = last_err {
            return Err(e);
        }

        // --- port checking with all metas resolved ---
        for (b, ins) in inputs.iter().enumerate() {
            for (p, req) in self.blocks[b].inputs().iter().enumerate() {
                let meta = ins.get(p).copied().flatten().and_then(|(sb, sp)| metas.get(sb).and_then(|m| m.get(sp)).copied().flatten());
                if let (Some(meta), Some((sb, sp))) = (meta, ins.get(p).copied().flatten()) {
                    if !req.accepts(meta) {
                        return Err(SimError::PortMismatch {
                            src: format!("{}/{}", self.name(sb), sp + 1),
                            dst: format!("{}/{}", self.name(b), p + 1),
                            reason: format!("port wants {}, got {}×{:?}", req.describe(), meta.width, meta.dtype),
                        });
                    }
                }
            }
        }

        // --- continuous state layout ---
        let mut offsets = Vec::with_capacity(n);
        let mut n_states = 0usize;
        for b in self.blocks.iter() {
            offsets.push(n_states);
            n_states = n_states.saturating_add(b.n_states());
        }
        if n_states > limits::MAX_STATES {
            return Err(SimError::TooLarge(format!("{n_states} continuous states (max {})", limits::MAX_STATES)));
        }

        // --- discrete sample rates (validated against dt by Sim) ---
        let mut discrete = Vec::new();
        for (i, b) in self.blocks.iter().enumerate() {
            if let SampleTime::Discrete(ts) = b.sample_time() {
                if !(ts.is_finite() && ts > 0.0) {
                    return Err(SimError::BadParam { block: self.name(i), msg: "discrete sample_time must be positive".into() });
                }
                if b.has_feedthrough() {
                    return Err(SimError::BadParam { block: self.name(i), msg: "discrete blocks must not have direct feedthrough".into() });
                }
                discrete.push((i, ts));
            }
        }

        // Initial state vector.
        let mut x0 = vec![0.0; n_states];
        for (i, b) in self.blocks.iter().enumerate() {
            let off = offsets.get(i).copied().unwrap_or(0);
            let nst = b.n_states();
            if let Some(slice) = x0.get_mut(off..off + nst) {
                b.get_x(slice)?;
            }
        }

        // Every port must have resolved; `None` here means a block failed to
        // resolve without reporting (defensive — resolve_out errs first).
        let metas: Vec<Vec<SigMeta>> = metas.into_iter().map(|ports| ports.into_iter().map(|m| m.unwrap_or(SigMeta::real(1))).collect()).collect();

        Ok(CompiledModel {
            order: exec_order,
            inputs,
            metas,
            offsets,
            x0,
            discrete,
            loggable: (0..n).filter(|&i| self.blocks[i].n_outputs() == 0).collect(),
        })
    }

    fn name(&self, i: usize) -> String {
        self.names.get(i).cloned().unwrap_or_else(|| format!("#{i}"))
    }
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

/// The frozen, validated form of a [`Model`]; what [`crate::Sim`] executes.
#[derive(Debug)]
pub struct CompiledModel {
    /// Evaluation order for `outputs` (feedthrough topo order).
    pub order: Vec<usize>,
    /// `inputs[b][p] = (src_block, src_port)` driver of each input port.
    pub inputs: Vec<Vec<Option<(usize, usize)>>>,
    /// Resolved signature of every output port.
    pub metas: Vec<Vec<SigMeta>>,
    /// `offsets[b]` = start of block b's slice in the flat `x` vector.
    pub offsets: Vec<usize>,
    /// Initial continuous state.
    pub x0: Vec<f64>,
    /// `(block index, sample time)` for each discrete block.
    pub discrete: Vec<(usize, f64)>,
    /// Blocks with no output ports — sinks that get the per-step `log` call.
    pub loggable: Vec<usize>,
}

impl CompiledModel {
    /// Fresh output-port buffers, correctly typed and sized.
    pub fn make_buffers(&self) -> Vec<Vec<SigVal>> {
        self.metas.iter().map(|ports| ports.iter().map(|m| SigVal::zeros(*m)).collect()).collect()
    }
}

/// Kahn topological sort over the wire graph, keeping only edges whose
/// destination satisfies `keep_edge(dst)`. Returns `None` on a cycle.
fn topo_sort(n: usize, wires: &[Wire], keep_edge: impl Fn(usize) -> bool) -> Option<Vec<usize>> {
    let mut indeg = vec![0usize; n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for w in wires {
        if w.src_block >= n || w.dst_block >= n || !keep_edge(w.dst_block) {
            continue;
        }
        if let Some(d) = indeg.get_mut(w.dst_block) {
            *d += 1;
        }
        if let Some(s) = succs.get_mut(w.src_block) {
            s.push(w.dst_block);
        }
    }
    let mut queue: Vec<usize> = (0..n).filter(|&i| indeg.get(i).copied().unwrap_or(0) == 0).collect();
    let mut order = Vec::with_capacity(n);
    let mut head = 0;
    while let Some(&b) = queue.get(head) {
        head += 1;
        order.push(b);
        for &s in succs.get(b).map(|v| v.as_slice()).unwrap_or(&[]) {
            if let Some(d) = indeg.get_mut(s) {
                *d -= 1;
                if *d == 0 {
                    queue.push(s);
                }
            }
        }
    }
    if order.len() == n {
        Some(order)
    } else {
        None
    }
}

/// Extract one cycle from the feedthrough subgraph for the error message.
fn find_cycle(n: usize, wires: &[Wire], keep_edge: impl Fn(usize) -> bool) -> Vec<usize> {
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for w in wires {
        if w.src_block < n && w.dst_block < n && keep_edge(w.dst_block) {
            if let Some(s) = succs.get_mut(w.src_block) {
                s.push(w.dst_block);
            }
        }
    }
    // Iterative DFS with an explicit stack of (node, child-iter-index).
    let mut color = vec![0u8; n]; // 0 white, 1 gray, 2 black
    let mut parent = vec![usize::MAX; n];
    for start in 0..n {
        if color.get(start).copied().unwrap_or(2) != 0 {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        if let Some(c) = color.get_mut(start) {
            *c = 1;
        }
        while let Some(&(node, ci)) = stack.last() {
            let next = succs.get(node).and_then(|s| s.get(ci)).copied();
            match next {
                Some(m) => {
                    if let Some(top) = stack.last_mut() {
                        top.1 += 1;
                    }
                    match color.get(m).copied().unwrap_or(2) {
                        0 => {
                            if let Some(c) = color.get_mut(m) {
                                *c = 1;
                            }
                            if let Some(p) = parent.get_mut(m) {
                                *p = node;
                            }
                            stack.push((m, 0));
                        }
                        1 => {
                            // Back edge node→m: walk parents to reconstruct.
                            let mut cycle = vec![m];
                            let mut cur = node;
                            while cur != m && cur != usize::MAX {
                                cycle.push(cur);
                                cur = parent.get(cur).copied().unwrap_or(usize::MAX);
                            }
                            cycle.reverse();
                            cycle.push(m);
                            return cycle;
                        }
                        _ => {}
                    }
                }
                None => {
                    if let Some(c) = color.get_mut(node) {
                        *c = 2;
                    }
                    stack.pop();
                }
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::SampleTime;
    use crate::PortReq;
    use std::any::Any;

    /// Minimal feedthrough passthrough for graph tests: y = u (+ bias).
    struct Wire1 {
        reqs: Vec<PortReq>,
        feed: bool,
    }
    impl Wire1 {
        fn feed() -> Self {
            Wire1 { reqs: vec![PortReq::REAL_ANY], feed: true }
        }
        fn nofeed() -> Self {
            Wire1 { reqs: vec![PortReq::REAL_ANY], feed: false }
        }
    }
    impl Block for Wire1 {
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
            if let Some(v) = y.get_mut(0) {
                *v = u.first().copied().cloned().unwrap_or(SigVal::scalar(0.0));
            }
            Ok(())
        }
        fn has_feedthrough(&self) -> bool {
            self.feed
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    struct Src;
    impl Block for Src {
        fn inputs(&self) -> &[PortReq] {
            &[]
        }
        fn n_outputs(&self) -> usize {
            1
        }
        fn resolve_out(&mut self, _p: usize, _ins: &[Option<SigMeta>]) -> Result<SigMeta, SimError> {
            Ok(SigMeta::real(1))
        }
        fn outputs(&mut self, _t: f64, _x: &[f64], _u: &[&SigVal], y: &mut Vec<SigVal>) -> Result<(), SimError> {
            if let Some(v) = y.get_mut(0) {
                *v = SigVal::scalar(1.0);
            }
            Ok(())
        }
        fn has_feedthrough(&self) -> bool {
            true
        }
        fn sample_time(&self) -> SampleTime {
            SampleTime::Continuous
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn topo_order_respects_wires() {
        let mut m = Model::new();
        let g = m.add_block("g", Box::new(Wire1::feed())).unwrap();
        let s = m.add_block("s", Box::new(Src)).unwrap();
        m.add_wire(s, 0, g, 0).unwrap();
        let c = m.compile().unwrap();
        let pos = |i| c.order.iter().position(|&b| b == i).unwrap();
        assert!(pos(s) < pos(g));
    }

    #[test]
    fn algebraic_loop_detected() {
        let mut m = Model::new();
        let a = m.add_block("a", Box::new(Wire1::feed())).unwrap();
        let b = m.add_block("b", Box::new(Wire1::feed())).unwrap();
        m.add_wire(a, 0, b, 0).unwrap();
        m.add_wire(b, 0, a, 0).unwrap();
        match m.compile() {
            Err(SimError::AlgebraicLoop(names)) => {
                assert!(names.contains(&"a".to_string()) && names.contains(&"b".to_string()));
            }
            other => panic!("expected AlgebraicLoop, got {other:?}"),
        }
    }

    #[test]
    fn feedback_through_nonfeedthrough_is_fine() {
        // gain -> integrator -> back to gain: the classic feedback loop must
        // NOT be flagged, because the integrator has no feedthrough.
        let mut m = Model::new();
        let a = m.add_block("integ", Box::new(Wire1::nofeed())).unwrap();
        let b = m.add_block("gain", Box::new(Wire1::feed())).unwrap();
        m.add_wire(a, 0, b, 0).unwrap();
        m.add_wire(b, 0, a, 0).unwrap();
        assert!(m.compile().is_ok());
    }

    #[test]
    fn unconnected_input_reported() {
        let mut m = Model::new();
        m.add_block("g", Box::new(Wire1::feed())).unwrap();
        let e = m.compile().unwrap_err().to_string();
        assert!(e.contains("unconnected"), "{e}");
    }

    #[test]
    fn multiple_drivers_reported() {
        let mut m = Model::new();
        let s1 = m.add_block("s1", Box::new(Src)).unwrap();
        let s2 = m.add_block("s2", Box::new(Src)).unwrap();
        let g = m.add_block("g", Box::new(Wire1::feed())).unwrap();
        m.add_wire(s1, 0, g, 0).unwrap();
        m.add_wire(s2, 0, g, 0).unwrap();
        assert!(m.compile().unwrap_err().to_string().contains("more than one wire"));
    }

    #[test]
    fn bad_wire_ports_rejected() {
        let mut m = Model::new();
        let s = m.add_block("s", Box::new(Src)).unwrap();
        let g = m.add_block("g", Box::new(Wire1::feed())).unwrap();
        assert!(m.add_wire(s, 3, g, 0).is_err());
        assert!(m.add_wire(s, 0, g, 4).is_err());
        assert!(m.add_wire(99, 0, g, 0).is_err());
    }
}
