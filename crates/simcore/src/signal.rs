//! Signal typing: element dtype, port width, and runtime values.

use crate::SimError;

/// Element type of a signal. `Real` covers scalar/vector `f64` data;
/// `Bool` carries control/logic signals (e.g. a Switch control port).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    Real,
    Bool,
}

/// The resolved signature of one port: element count × dtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigMeta {
    pub width: usize,
    pub dtype: DType,
}

impl SigMeta {
    pub fn real(width: usize) -> Self {
        SigMeta { width, dtype: DType::Real }
    }
}

/// What an input port accepts for width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthReq {
    /// Any width.
    Any,
    /// Exactly this many elements.
    Fixed(usize),
}

/// What an input port accepts for dtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DTypeReq {
    Real,
    Bool,
    Any,
}

/// Requirement declared by one input port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortReq {
    pub width: WidthReq,
    pub dtype: DTypeReq,
}

impl PortReq {
    /// Scalar/vector real input of any width — the common case.
    pub const REAL_ANY: PortReq = PortReq { width: WidthReq::Any, dtype: DTypeReq::Real };
    /// Control input accepting real or bool of any width.
    pub const ANY: PortReq = PortReq { width: WidthReq::Any, dtype: DTypeReq::Any };

    pub fn fixed(width: usize) -> Self {
        PortReq { width: WidthReq::Fixed(width), dtype: DTypeReq::Real }
    }

    /// True if a resolved source signal can drive this port.
    pub fn accepts(&self, meta: SigMeta) -> bool {
        let w = match self.width {
            WidthReq::Any => true,
            WidthReq::Fixed(n) => meta.width == n,
        };
        let d = match self.dtype {
            DTypeReq::Any => true,
            DTypeReq::Real => meta.dtype == DType::Real,
            DTypeReq::Bool => meta.dtype == DType::Bool,
        };
        w && d
    }

    /// Human-readable expectation for error messages.
    pub fn describe(&self) -> String {
        let w = match self.width {
            WidthReq::Any => "any width".to_string(),
            WidthReq::Fixed(n) => format!("width {n}"),
        };
        let d = match self.dtype {
            DTypeReq::Real => "real",
            DTypeReq::Bool => "bool",
            DTypeReq::Any => "any dtype",
        };
        format!("{w} {d}")
    }
}

/// A runtime signal value on one output port.
#[derive(Debug, Clone, PartialEq)]
pub enum SigVal {
    Real(Vec<f64>),
    Bool(Vec<bool>),
}

impl SigVal {
    /// Real scalar.
    pub fn scalar(v: f64) -> Self {
        SigVal::Real(vec![v])
    }

    pub fn width(&self) -> usize {
        match self {
            SigVal::Real(v) => v.len(),
            SigVal::Bool(v) => v.len(),
        }
    }

    pub fn dtype(&self) -> DType {
        match self {
            SigVal::Real(_) => DType::Real,
            SigVal::Bool(_) => DType::Bool,
        }
    }

    pub fn meta(&self) -> SigMeta {
        SigMeta { width: self.width(), dtype: self.dtype() }
    }

    /// A zero value matching `meta` — used to initialise port buffers.
    pub fn zeros(meta: SigMeta) -> Self {
        match meta.dtype {
            DType::Real => SigVal::Real(vec![0.0; meta.width]),
            DType::Bool => SigVal::Bool(vec![false; meta.width]),
        }
    }

    /// Borrow the elements of a real signal.
    pub fn as_real(&self) -> Result<&[f64], SimError> {
        match self {
            SigVal::Real(v) => Ok(v),
            SigVal::Bool(_) => Err(SimError::Msg("expected a real signal, got bool".into())),
        }
    }

    /// Borrow the elements of a bool signal.
    pub fn as_bool(&self) -> Result<&[bool], SimError> {
        match self {
            SigVal::Bool(v) => Ok(v),
            SigVal::Real(_) => Err(SimError::Msg("expected a bool signal, got real".into())),
        }
    }
}

/// Fetch input `i` as real elements — the boilerplate every math block shares.
pub fn in_real<'a>(u: &[&'a SigVal], i: usize) -> Result<&'a [f64], SimError> {
    match u.get(i) {
        Some(s) => s.as_real(),
        None => Err(SimError::Msg(format!("missing input port {}", i + 1))),
    }
}
