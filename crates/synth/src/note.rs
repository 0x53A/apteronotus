//! What a scheduled onset hands to a template.
//!
//! A voice is a *function from note to graph*, not a graph with knobs. `n.hz`
//! is baked in when the unit is instantiated, and that is what gives real
//! polyphony. A curve-valued parameter remains a compiled note-clock signal;
//! no language callback survives into the voice.

use crate::template::{GraphTemplate, Implicit, ParamId, ParamSpec};
use apteronotus_pattern::{Curve, CurveActivity, CurveClock, CurveError, CurveRange, RangeProof};

#[derive(Clone, PartialEq, Debug)]
pub enum ParamValue {
    Number(f64),
    Curve(Curve),
}

impl From<f64> for ParamValue {
    fn from(value: f64) -> Self {
        ParamValue::Number(value)
    }
}

impl From<Curve> for ParamValue {
    fn from(value: Curve) -> Self {
        ParamValue::Curve(value)
    }
}

/// The concrete values one onset supplies.
#[derive(Clone, PartialEq, Debug)]
pub struct Note {
    pub hz: f64,
    pub velocity: f64,
    /// Length in seconds. Known here because the sequencer scheduled it, which
    /// is what lets envelopes be note-clock functions rather than gate
    /// followers.
    pub duration: f64,
    pub pan: f64,
    /// Derived from event provenance — never a counter.
    pub seed: u64,
    implicit_curves: [Option<Curve>; 4],
    /// Declared parameters by index, `None` falling back to the declaration's
    /// default.
    declared: Vec<Option<ParamValue>>,
}

impl Note {
    pub fn new(hz: f64) -> Note {
        Note {
            hz,
            velocity: Implicit::Velocity.default_value(),
            duration: Implicit::Duration.default_value(),
            pan: Implicit::Pan.default_value(),
            seed: 0,
            implicit_curves: core::array::from_fn(|_| None),
            declared: Vec::new(),
        }
    }

    pub fn velocity(mut self, velocity: f64) -> Note {
        self.velocity = velocity;
        self.implicit_curves[Implicit::Velocity.index()] = None;
        self
    }

    pub fn duration(mut self, seconds: f64) -> Note {
        self.duration = seconds;
        self.implicit_curves[Implicit::Duration.index()] = None;
        self
    }

    pub fn pan(mut self, pan: f64) -> Note {
        self.pan = pan;
        self.implicit_curves[Implicit::Pan.index()] = None;
        self
    }

    pub fn seed(mut self, seed: u64) -> Note {
        self.seed = seed;
        self
    }

    pub fn bind(mut self, id: ParamId, value: impl Into<ParamValue>) -> Note {
        let value = value.into();
        match (id, value) {
            (ParamId::Implicit(implicit), ParamValue::Number(value)) => {
                self.set_implicit_number(implicit, value);
                self.implicit_curves[implicit.index()] = None;
            }
            (ParamId::Implicit(implicit), ParamValue::Curve(curve)) => {
                self.implicit_curves[implicit.index()] = Some(curve);
            }
            (ParamId::Declared(index), value) => {
                self.ensure_declared(index);
                self.declared[index] = Some(value);
            }
        }
        self
    }

    /// Set a declared scalar parameter by index.
    pub fn set(self, index: usize, value: f64) -> Note {
        self.set_value(index, ParamValue::Number(value))
    }

    pub fn set_value(mut self, index: usize, value: ParamValue) -> Note {
        self.ensure_declared(index);
        self.declared[index] = Some(value);
        self
    }

    /// Resolve and validate one symbolic parameter.
    ///
    /// Existing scalar behavior remains: declared values clamp to their
    /// published range. Curves are never silently clamped; their whole
    /// reachable range must be proven safe.
    pub fn value(
        &self,
        id: ParamId,
        template: &GraphTemplate,
    ) -> Result<ParamValue, ParamValueError> {
        match id {
            ParamId::Implicit(implicit) => {
                if let Some(curve) = &self.implicit_curves[implicit.index()] {
                    validate_curve(id, curve, implicit.spec())?;
                    Ok(ParamValue::Curve(curve.clone()))
                } else {
                    Ok(ParamValue::Number(self.implicit_number(implicit)))
                }
            }
            ParamId::Declared(index) => {
                let spec = template
                    .params
                    .get(index)
                    .ok_or(ParamValueError::Undeclared(index))?;
                match self.declared.get(index).and_then(Option::as_ref) {
                    Some(ParamValue::Number(value)) => Ok(ParamValue::Number(spec.clamp(*value))),
                    Some(ParamValue::Curve(curve)) => {
                        validate_curve(id, curve, spec.into())?;
                        Ok(ParamValue::Curve(curve.clone()))
                    }
                    None => Ok(ParamValue::Number(spec.default)),
                }
            }
        }
    }

    fn ensure_declared(&mut self, index: usize) {
        if self.declared.len() <= index {
            self.declared.resize(index + 1, None);
        }
    }

    fn set_implicit_number(&mut self, implicit: Implicit, value: f64) {
        match implicit {
            Implicit::Hz => self.hz = value,
            Implicit::Velocity => self.velocity = value,
            Implicit::Duration => self.duration = value,
            Implicit::Pan => self.pan = value,
        }
    }

    fn implicit_number(&self, implicit: Implicit) -> f64 {
        match implicit {
            Implicit::Hz => self.hz,
            Implicit::Velocity => self.velocity,
            Implicit::Duration => self.duration,
            Implicit::Pan => self.pan,
        }
    }
}

fn validate_curve(
    id: ParamId,
    curve: &Curve,
    spec: ParamSpecRef<'_>,
) -> Result<(), ParamValueError> {
    if id == ParamId::Implicit(Implicit::Duration) {
        return Err(ParamValueError::DurationCurve);
    }
    match curve
        .prove_range(spec.min, spec.max)
        .map_err(ParamValueError::InvalidCurve)?
    {
        RangeProof::Safe { .. } => {}
        RangeProof::Unsafe { proven } => {
            return Err(ParamValueError::OutOfRange {
                id,
                allowed: CurveRange {
                    min: spec.min,
                    max: spec.max,
                },
                proven,
            });
        }
        RangeProof::Inconclusive { enclosure } => {
            return Err(ParamValueError::RangeInconclusive {
                id,
                allowed: CurveRange {
                    min: spec.min,
                    max: spec.max,
                },
                enclosure,
            });
        }
    }

    if curve.clock == CurveClock::NoteSeconds
        && let CurveActivity::Finite(horizon) =
            curve.activity().map_err(ParamValueError::InvalidCurve)?
    {
        let limit = spec
            .max_curve_seconds
            .ok_or(ParamValueError::CurveHorizonNotDeclared { id })?;
        if horizon > limit {
            return Err(ParamValueError::CurveHorizonExceeded {
                id,
                found: horizon,
                limit,
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ParamSpecRef<'a> {
    min: f64,
    max: f64,
    max_curve_seconds: Option<f64>,
    _name: Option<&'a str>,
}

impl<'a> From<&'a ParamSpec> for ParamSpecRef<'a> {
    fn from(spec: &'a ParamSpec) -> Self {
        ParamSpecRef {
            min: spec.min,
            max: spec.max,
            max_curve_seconds: spec.max_curve_seconds,
            _name: Some(&spec.name),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum ParamValueError {
    Undeclared(usize),
    DurationCurve,
    InvalidCurve(CurveError),
    OutOfRange {
        id: ParamId,
        allowed: CurveRange,
        proven: CurveRange,
    },
    RangeInconclusive {
        id: ParamId,
        allowed: CurveRange,
        enclosure: CurveRange,
    },
    CurveHorizonNotDeclared {
        id: ParamId,
    },
    CurveHorizonExceeded {
        id: ParamId,
        found: f64,
        limit: f64,
    },
}

impl core::fmt::Display for ParamValueError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ParamValueError::Undeclared(index) => {
                write!(f, "parameter {index} was never declared")
            }
            ParamValueError::DurationCurve => {
                write!(
                    f,
                    "duration is a known scalar; a live gate is a separate signal"
                )
            }
            ParamValueError::InvalidCurve(source) => source.fmt(f),
            ParamValueError::OutOfRange {
                id,
                allowed,
                proven,
            } => write!(
                f,
                "{id:?} curve reaches {}..{}, outside {}..{}",
                proven.min, proven.max, allowed.min, allowed.max
            ),
            ParamValueError::RangeInconclusive {
                id,
                allowed,
                enclosure,
            } => write!(
                f,
                "cannot prove {id:?} curve stays inside {}..{} (sound enclosure {}..{}); restructure the curve into separately bounded terms",
                allowed.min, allowed.max, enclosure.min, enclosure.max
            ),
            ParamValueError::CurveHorizonNotDeclared { id } => write!(
                f,
                "{id:?} does not declare a maximum NoteSeconds curve horizon"
            ),
            ParamValueError::CurveHorizonExceeded { id, found, limit } => write!(
                f,
                "{id:?} curve horizon is {found} seconds, above its declared maximum of {limit}"
            ),
        }
    }
}

impl core::error::Error for ParamValueError {}

impl Implicit {
    pub(crate) const fn index(self) -> usize {
        match self {
            Implicit::Hz => 0,
            Implicit::Velocity => 1,
            Implicit::Duration => 2,
            Implicit::Pan => 3,
        }
    }

    fn spec(self) -> ParamSpecRef<'static> {
        match self {
            Implicit::Hz => ParamSpecRef {
                min: f64::MIN_POSITIVE,
                max: f64::MAX,
                max_curve_seconds: None,
                _name: Some("hz"),
            },
            Implicit::Velocity => ParamSpecRef {
                min: 0.0,
                max: 1.0,
                max_curve_seconds: None,
                _name: Some("velocity"),
            },
            Implicit::Duration => ParamSpecRef {
                min: f64::MIN_POSITIVE,
                max: f64::MAX,
                max_curve_seconds: None,
                _name: Some("duration"),
            },
            Implicit::Pan => ParamSpecRef {
                min: -1.0,
                max: 1.0,
                max_curve_seconds: None,
                _name: Some("pan"),
            },
        }
    }
}
