//! Building a [`GraphTemplate`].
//!
//! This is the surface the scripting language will eventually drive, so it is
//! worth it being pleasant in Rust first: everything above it has to survive
//! the songs before Lua is allowed anywhere near it.
//!
//! Every method returns a [`Source`], so a chain reads roughly as the notation
//! does. The exception is [`GraphBuilder::pan`], which returns two — it is the
//! one node here with two outputs, and pretending otherwise would hide the
//! channel from the type that carries it.

use crate::control::ControlId;
use crate::routing::BusId;
use crate::template::{
    Adsr, BreakpointHorizon, Curve, DelayRange, GraphSend, GraphTemplate, Implicit, InitExpr,
    InitScalar, InitScalarError, Input, Node, NodeId, Op, ParamId, ParamSpec, ShapeKind, Source,
    TemplateError,
};
use apteronotus_pattern::SrcSpan;

#[derive(Default)]
pub struct GraphBuilder {
    template: GraphTemplate,
    src: Option<SrcSpan>,
}

/// Construction-time topology, stability and allocation data for one FDN.
///
/// Decay time remains a live signal input to [`GraphBuilder::fdn`]. These
/// fields describe the delay matrix itself and its publication ceiling.
#[derive(Clone, PartialEq, Debug)]
pub struct FdnConfig {
    pub delays: Vec<f64>,
    pub damping: f64,
    pub modulation_rate: f64,
    pub modulation_depth: f64,
    pub max_t60: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Bounds {
    pub min: f64,
    pub max: f64,
}

impl Bounds {
    fn new(min: f64, max: f64) -> Option<Bounds> {
        (min.is_finite() && max.is_finite() && min <= max).then_some(Bounds { min, max })
    }

    fn add(self, other: Bounds) -> Option<Bounds> {
        Bounds::new(self.min + other.min, self.max + other.max)
    }

    fn sub(self, other: Bounds) -> Option<Bounds> {
        Bounds::new(self.min - other.max, self.max - other.min)
    }

    fn mul(self, other: Bounds) -> Option<Bounds> {
        let products = [
            self.min * other.min,
            self.min * other.max,
            self.max * other.min,
            self.max * other.max,
        ];
        let min = products.iter().copied().fold(f64::INFINITY, f64::min);
        let max = products.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Bounds::new(min, max)
    }

    fn div(self, other: Bounds) -> Option<Bounds> {
        if other.min <= 0.0 && other.max >= 0.0 {
            return None;
        }
        let quotients = [
            self.min / other.min,
            self.min / other.max,
            self.max / other.min,
            self.max / other.max,
        ];
        let min = quotients.iter().copied().fold(f64::INFINITY, f64::min);
        let max = quotients.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        Bounds::new(min, max)
    }

    fn neg(self) -> Option<Bounds> {
        Bounds::new(-self.max, -self.min)
    }
}

impl GraphBuilder {
    pub fn new() -> GraphBuilder {
        GraphBuilder::default()
    }

    pub fn with_inputs(channels: usize) -> GraphBuilder {
        let mut builder = GraphBuilder::default();
        builder.template.inputs = channels;
        builder
    }

    pub fn input(&self, channel: usize) -> Source {
        Source::Input(channel)
    }

    /// Publish a persistent program signal into a shared control while
    /// retaining it in the graph's dataflow.
    pub fn write_control(&mut self, signal: Source, target: ControlId) -> Source {
        self.node(
            Op::ControlWrite { target },
            [Input {
                source: signal,
                src: self.src,
            }],
        )
    }

    /// Keep a control writer alive briefly beyond the note gate so a
    /// gate-shaped signal can publish its terminating zero.
    pub fn write_control_with_release(
        &mut self,
        signal: Source,
        target: ControlId,
        release_seconds: f64,
    ) -> Source {
        let source = self.write_control(signal, target);
        self.set_tail(source, release_seconds);
        source
    }

    /// Attribute every node built from here on to `src`.
    ///
    /// Set as the builder walks the source, so a graph node knows which
    /// expression produced it. This is what lets a diagnostic point at a line
    /// rather than at a node index, and it is the graph-side half of the
    /// property the pattern AST already has.
    pub fn at(&mut self, src: SrcSpan) -> &mut GraphBuilder {
        self.src = Some(src);
        self
    }

    pub fn anywhere(&mut self) -> &mut GraphBuilder {
        self.src = None;
        self
    }

    /// Declare a note parameter and get the symbolic source that reads it.
    pub fn param(&mut self, spec: ParamSpec) -> Source {
        self.template.params.push(spec);
        Source::Param(ParamId::Declared(self.template.params.len() - 1))
    }

    /// Look a declared parameter up by name, for a setter that has to find the
    /// port it writes.
    pub fn param_index(&self, name: &str) -> Option<usize> {
        self.template.params.iter().position(|p| p.name == name)
    }

    /// An argument carrying the builder's current span.
    ///
    /// The sugar below routes every operand through here, so an argument's
    /// span is whatever was current when it was written. A caller that knows
    /// better — a parser walking one call's operands, each with its own byte
    /// range — passes [`Input::at`] instead.
    pub fn arg(&self, source: impl Into<Source>) -> Input {
        Input {
            source: source.into(),
            src: self.src,
        }
    }

    /// Add a node. Returns channel 0; use [`Source::port`] for the rest.
    pub fn node(&mut self, op: Op, inputs: impl IntoIterator<Item = Input>) -> Source {
        let id = self.template.nodes.len();
        self.template.nodes.push(Node {
            op,
            inputs: inputs.into_iter().collect(),
            tail: 0.0,
            src: self.src,
        });
        Source::port(id, 0)
    }

    /// Attach conservative lifetime metadata to the node producing `source`.
    ///
    /// Kept crate-private because ordinary graph authors should not guess
    /// tails. Stdlib compositions such as `ring` use it when their musical
    /// parameter already states the lifetime explicitly.
    pub(crate) fn set_tail(&mut self, source: Source, seconds: f64) {
        let node = self
            .node_id(source)
            .expect("tail metadata can only be attached to a node output");
        self.template.nodes[node].tail = if seconds.is_finite() && seconds >= 0.0 {
            self.template.nodes[node].tail.max(seconds)
        } else {
            // Preserve the bad value so validation can turn it into a
            // diagnostic instead of silently laundering it into zero.
            seconds
        };
    }

    /// Conservative construction-time bounds for a scalar expression.
    ///
    /// Declared parameters are safe to bound because note binding clamps them
    /// to their [`ParamSpec`] ranges. Arithmetic nodes propagate intervals.
    /// Oscillators, filters, curves and implicit note inputs deliberately
    /// return `None`: sampling a runtime signal to decide lifetime would cross
    /// the rate boundary this metadata exists to protect.
    pub(crate) fn bounds(&self, source: Source) -> Option<Bounds> {
        match source {
            Source::Const(value) => Bounds::new(value, value),
            Source::Param(ParamId::Declared(index)) => {
                let spec = self.template.params.get(index)?;
                Bounds::new(spec.min, spec.max)
            }
            Source::Param(ParamId::Implicit(_)) => None,
            Source::Control(_) => None,
            Source::Input(_) => None,
            Source::ExternalAudio { .. } => None,
            Source::Port { node, channel: 0 } => {
                let node = self.template.nodes.get(node)?;
                let input = |index: usize| self.bounds(node.inputs.get(index)?.source);
                match node.op {
                    Op::Add => input(0)?.add(input(1)?),
                    Op::Sub => input(0)?.sub(input(1)?),
                    Op::Mul => input(0)?.mul(input(1)?),
                    Op::Div => input(0)?.div(input(1)?),
                    Op::Neg => input(0)?.neg(),
                    _ => None,
                }
            }
            Source::Port { .. } => None,
        }
    }

    pub fn node_id(&self, source: Source) -> Option<NodeId> {
        match source {
            Source::Port { node, .. } => Some(node),
            _ => None,
        }
    }

    /// Reference a writable scalar owned by the program control arena.
    pub fn control(&self, id: ControlId) -> Source {
        Source::Control(id)
    }

    // ------------------------------------------------------------ generators

    pub fn sine(&mut self, hz: impl Into<Source>) -> Source {
        let hz = self.arg(hz);
        self.node(Op::Sine, [hz])
    }

    /// Staged harmonic amplitudes (fundamental first), at most 32, absolute
    /// sum <= 1. Frequency is modulatable; the backend suppresses ultrasonic
    /// partials at the actual device rate. Graph validation checks the table.
    pub fn harmonics(&mut self, hz: impl Into<Source>, amplitudes: Vec<f64>) -> Source {
        let hz = self.arg(hz);
        self.node(Op::Harmonics { amplitudes }, [hz])
    }

    /// Experimental flue waveguide. A retained patch may reopen the wind on
    /// the same vibrating pipe. In a note voice, envelope the PRESSURE input.
    pub fn flue_pipe(
        &mut self,
        hz: impl Into<Source>,
        pressure: impl Into<Source>,
        turbulence: impl Into<Source>,
        min_hz: f64,
    ) -> Source {
        let inputs = [self.arg(hz), self.arg(pressure), self.arg(turbulence)];
        self.node(Op::FluePipe { min_hz }, inputs)
    }

    pub fn cosine(&mut self, hz: impl Into<Source>) -> Source {
        let hz = self.arg(hz);
        self.node(Op::Cosine, [hz])
    }

    pub fn saw(&mut self, hz: impl Into<Source>) -> Source {
        let hz = self.arg(hz);
        self.node(Op::Saw, [hz])
    }

    pub fn pulse(&mut self, hz: impl Into<Source>, duty: impl Into<Source>) -> Source {
        let (hz, duty) = (self.arg(hz), self.arg(duty));
        self.node(Op::Pulse, [hz, duty])
    }

    /// Band-limited triangle; frequency remains an ordinary modulatable input.
    pub fn triangle(&mut self, hz: impl Into<Source>) -> Source {
        let hz = self.arg(hz);
        self.node(Op::Triangle, [hz])
    }

    pub fn noise(&mut self) -> Source {
        self.node(Op::Noise, [])
    }

    pub fn pink(&mut self) -> Source {
        self.node(Op::Pink, [])
    }

    pub fn impulse(&mut self) -> Source {
        self.node(Op::Impulse, [])
    }

    /// One reproducible per-voice scalar in `[min, max)`.
    pub fn init_random(
        &mut self,
        stream: u64,
        min: impl Into<Source>,
        max: impl Into<Source>,
    ) -> Result<Source, InitScalarError> {
        let min = self.init_expr(min.into()).ok_or(InitScalarError::Dynamic)?;
        let max = self.init_expr(max.into()).ok_or(InitScalarError::Dynamic)?;
        Ok(self.node(Op::InitRandom { stream, min, max }, []))
    }

    /// Retained, re-excitable string with dynamic pitch and damping.
    pub fn string_resonator(
        &mut self,
        excitation: Source,
        hz: impl Into<Source>,
        mute: impl Into<Source>,
        min_hz: f64,
        decay: f64,
    ) -> Source {
        let inputs = [self.arg(excitation), self.arg(hz), self.arg(mute)];
        self.node(Op::StringResonator { min_hz, decay }, inputs)
    }

    pub fn pluck(
        &mut self,
        excitation: Source,
        frequency: impl Into<Source>,
        gain_per_second: f64,
        damping: impl Into<Source>,
    ) -> Result<Source, InitScalarError> {
        let frequency = InitScalar::from_source(frequency.into())?;
        let damping = InitScalar::from_source(damping.into())?;
        let excitation = self.arg(excitation);
        Ok(self.node(
            Op::Pluck {
                frequency,
                gain_per_second,
                damping,
                // One hertz is a deliberately generous publication bound.
                // Instantiation rejects lower frequencies before fundsp
                // allocates its delay line.
                max_delay_seconds: 1.0,
            },
            [excitation],
        ))
    }

    /// Smooth a live signal, or construct a previous-note pitch glide when
    /// the input is exactly the implicit `n.hz` source.
    ///
    /// A fresh per-note voice sees `n.hz` as a constant, so an ordinary
    /// follower would have no transition to observe. The specialized IR node
    /// makes that score/instantiation boundary explicit while leaving every
    /// other input as a normal live follower.
    pub fn slew(
        &mut self,
        input: Source,
        response_time: impl Into<Source>,
    ) -> Result<Source, InitScalarError> {
        let response_time = InitScalar::from_source(response_time.into())?;
        if input == Source::Param(ParamId::Implicit(Implicit::Hz)) {
            Ok(self.node(
                Op::Portamento {
                    target: InitScalar::Param(ParamId::Implicit(Implicit::Hz)),
                    response_time,
                },
                [],
            ))
        } else {
            let input = self.arg(input);
            Ok(self.node(
                Op::Slew {
                    response_time,
                    initial: 0.0,
                },
                [input],
            ))
        }
    }

    pub fn slew_from(
        &mut self,
        input: Source,
        response_time: impl Into<Source>,
        initial: f64,
    ) -> Result<Source, InitScalarError> {
        let response_time = InitScalar::from_source(response_time.into())?;
        let input = self.arg(input);
        Ok(self.node(
            Op::Slew {
                response_time,
                initial,
            },
            [input],
        ))
    }

    pub fn gate_env(
        &mut self,
        gate: Source,
        attack: f64,
        decay: f64,
        sustain: f64,
        release: f64,
    ) -> Source {
        self.node(
            Op::GateEnv {
                attack,
                decay,
                sustain,
                release,
            },
            [self.arg(gate)],
        )
    }

    // --------------------------------------------------------------- filters

    pub fn lowpass(
        &mut self,
        audio: Source,
        cutoff: impl Into<Source>,
        q: impl Into<Source>,
    ) -> Source {
        let a = self.filter_args(audio, cutoff, q);
        self.node(Op::Lowpass, a)
    }

    pub fn highpass(
        &mut self,
        audio: Source,
        cutoff: impl Into<Source>,
        q: impl Into<Source>,
    ) -> Source {
        let a = self.filter_args(audio, cutoff, q);
        self.node(Op::Highpass, a)
    }

    pub fn bandpass(
        &mut self,
        audio: Source,
        cutoff: impl Into<Source>,
        q: impl Into<Source>,
    ) -> Source {
        let a = self.filter_args(audio, cutoff, q);
        self.node(Op::Bandpass, a)
    }

    pub fn peak(
        &mut self,
        audio: Source,
        cutoff: impl Into<Source>,
        q: impl Into<Source>,
    ) -> Source {
        let a = self.filter_args(audio, cutoff, q);
        self.node(Op::Peak, a)
    }

    pub fn moog(
        &mut self,
        audio: Source,
        cutoff: impl Into<Source>,
        q: impl Into<Source>,
    ) -> Source {
        let a = self.filter_args(audio, cutoff, q);
        self.node(Op::Moog, a)
    }

    fn filter_args(
        &self,
        audio: Source,
        cutoff: impl Into<Source>,
        q: impl Into<Source>,
    ) -> [Input; 3] {
        [self.arg(audio), self.arg(cutoff), self.arg(q)]
    }

    // --------------------------------------------------------------- shaping

    pub fn shape(&mut self, audio: Source, kind: ShapeKind, amount: impl Into<Source>) -> Source {
        let (audio, amount) = (self.arg(audio), self.arg(amount));
        self.node(Op::Shape { kind }, [audio, amount])
    }

    pub fn dcblock(&mut self, audio: Source) -> Source {
        let audio = self.arg(audio);
        self.node(Op::DcBlock, [audio])
    }

    // --------------------------------------------------------------- memory

    /// Delay `audio` by the signal `seconds`.
    ///
    /// `range` is construction-time allocation metadata, not a second source
    /// of truth for the audible delay. The runtime signal is clamped to it.
    pub fn delay(
        &mut self,
        audio: Source,
        seconds: impl Into<Source>,
        range: DelayRange,
    ) -> Source {
        let (audio, seconds) = (self.arg(audio), self.arg(seconds));
        self.node(Op::Delay(range), [audio, seconds])
    }

    pub fn reverb(
        &mut self,
        left: Source,
        right: Source,
        room_size: f64,
        time: f64,
        damping: f64,
    ) -> (Source, Source) {
        let inputs = [self.arg(left), self.arg(right)];
        let source = self.node(
            Op::Reverb {
                room_size,
                time,
                damping,
            },
            inputs,
        );
        let node = self.node_id(source).expect("reverb is a node");
        (Source::port(node, 0), Source::port(node, 1))
    }

    pub fn limiter(
        &mut self,
        left: Source,
        right: Source,
        attack: f64,
        release: f64,
    ) -> (Source, Source) {
        let inputs = [self.arg(left), self.arg(right)];
        let source = self.node(Op::Limiter { attack, release }, inputs);
        let node = self.node_id(source).expect("limiter is a node");
        (Source::port(node, 0), Source::port(node, 1))
    }

    pub fn chorus(
        &mut self,
        audio: Source,
        seed: u64,
        separation: f64,
        variation: f64,
        frequency: f64,
    ) -> Source {
        let audio = self.arg(audio);
        self.node(
            Op::Chorus {
                seed,
                separation,
                variation,
                frequency,
            },
            [audio],
        )
    }

    pub fn feedback_delay(
        &mut self,
        audio: Source,
        delay_seconds: f64,
        cutoff_q: Option<(f64, f64)>,
        amount: f64,
    ) -> Source {
        let audio = self.arg(audio);
        self.node(
            Op::FeedbackDelay {
                delay_seconds,
                cutoff_q,
                amount,
            },
            [audio],
        )
    }

    pub fn allpass_delay(&mut self, audio: Source, seconds: f64, gain: f64) -> Source {
        self.node(Op::AllpassDelay { seconds, gain }, [self.arg(audio)])
    }

    pub fn fdn(&mut self, audio: Source, t60: Source, config: FdnConfig) -> Source {
        let audio = self.arg(audio);
        let t60 = self.arg(t60);
        self.node(
            Op::Fdn {
                delays: config.delays,
                damping: config.damping,
                modulation_rate: config.modulation_rate,
                modulation_depth: config.modulation_depth,
                max_t60: config.max_t60,
            },
            [audio, t60],
        )
    }

    pub fn envelope_follower(&mut self, audio: Source, attack: f64, release: f64) -> Source {
        let audio = self.arg(audio);
        self.node(Op::EnvelopeFollower { attack, release }, [audio])
    }

    pub fn pitch_tracker(
        &mut self,
        audio: Source,
        min_hz: f64,
        max_hz: f64,
        default_hz: f64,
        hold_seconds: f64,
    ) -> Source {
        let audio = self.arg(audio);
        self.node(
            Op::PitchTracker {
                min_hz,
                max_hz,
                default_hz,
                hold_seconds,
            },
            [audio],
        )
    }

    pub fn onset_detector(&mut self, audio: Source, floor: f64, hold_seconds: f64) -> Source {
        let audio = self.arg(audio);
        self.node(
            Op::OnsetDetector {
                floor,
                hold_seconds,
            },
            [audio],
        )
    }

    pub fn transport_sequence(
        &mut self,
        period_seconds: f64,
        slots: Vec<crate::TransportSlot>,
    ) -> Source {
        self.node(
            Op::TransportSequence {
                period_seconds,
                slots,
            },
            [],
        )
    }

    pub fn width(&mut self, left: Source, right: Source, amount: Source) -> [Source; 2] {
        let source = self.node(
            Op::Width,
            [self.arg(left), self.arg(right), self.arg(amount)],
        );
        let node = self.node_id(source).expect("width is a node");
        [
            Source::Port { node, channel: 0 },
            Source::Port { node, channel: 1 },
        ]
    }

    // ------------------------------------------------------------ arithmetic

    pub fn add(&mut self, a: impl Into<Source>, b: impl Into<Source>) -> Source {
        let (a, b) = (self.arg(a), self.arg(b));
        self.node(Op::Add, [a, b])
    }

    pub fn sub(&mut self, a: impl Into<Source>, b: impl Into<Source>) -> Source {
        let (a, b) = (self.arg(a), self.arg(b));
        self.node(Op::Sub, [a, b])
    }

    pub fn mul(&mut self, a: impl Into<Source>, b: impl Into<Source>) -> Source {
        let (a, b) = (self.arg(a), self.arg(b));
        self.node(Op::Mul, [a, b])
    }

    pub fn div(&mut self, a: impl Into<Source>, b: impl Into<Source>) -> Source {
        let (a, b) = (self.arg(a), self.arg(b));
        self.node(Op::Div, [a, b])
    }

    pub fn pow(&mut self, base: impl Into<Source>, exponent: impl Into<Source>) -> Source {
        let (base, exponent) = (self.arg(base), self.arg(exponent));
        self.node(Op::Pow, [base, exponent])
    }

    pub fn neg(&mut self, a: Source) -> Source {
        let a = self.arg(a);
        self.node(Op::Neg, [a])
    }

    pub fn hz_to_midi(&mut self, hz: impl Into<Source>) -> Source {
        let hz = self.arg(hz);
        self.node(Op::HzToMidi, [hz])
    }

    pub fn clamp(&mut self, value: impl Into<Source>, min: f64, max: f64) -> Source {
        let value = self.arg(value);
        self.node(Op::Clamp { min, max }, [value])
    }

    /// Sum a collection. This is `mix(...)`, and it exists as its own call
    /// because accumulating onto `zero()` is an arity bug waiting to happen —
    /// `poles.eod` had it, since `zero()` takes no input and `ring()` takes one.
    pub fn mix(&mut self, sources: impl IntoIterator<Item = Source>) -> Source {
        let mut it = sources.into_iter();
        let Some(first) = it.next() else {
            return Source::Const(0.0);
        };
        it.fold(first, |acc, s| self.add(acc, s))
    }

    // ------------------------------------------------------------- envelopes

    pub fn adsr(&mut self, adsr: Adsr) -> Source {
        self.node(Op::Adsr(adsr), [])
    }

    pub fn decay(&mut self, seconds: Source) -> Result<Source, DecayError> {
        let bounds = self.bounds(seconds).ok_or(DecayError::Unbounded)?;
        if bounds.min <= 0.0 {
            return Err(DecayError::NonPositive {
                minimum: bounds.min,
            });
        }
        let seconds = self.arg(seconds);
        Ok(self.node(
            Op::Decay {
                max_seconds: bounds.max,
            },
            [seconds],
        ))
    }

    pub fn window(
        &mut self,
        begin_seconds: Source,
        end_seconds: Source,
    ) -> Result<Source, WindowError> {
        let begin = self
            .bounds(begin_seconds)
            .ok_or(WindowError::UnboundedBegin)?;
        if end_seconds == n::DURATION {
            if begin.min < 0.0 {
                return Err(WindowError::NegativeBegin { minimum: begin.min });
            }
            // The note duration is not publication-time bounded, but it is
            // the gate itself. This window can therefore never extend the
            // voice beyond its already-authoritative gate. A short note whose
            // duration precedes `begin` simply produces an empty window.
            return Ok(self.node(
                Op::Window { max_seconds: 0.0 },
                [self.arg(begin_seconds), self.arg(end_seconds)],
            ));
        }
        let end = self.bounds(end_seconds).ok_or(WindowError::UnboundedEnd)?;
        if begin.min < 0.0 {
            return Err(WindowError::NegativeBegin { minimum: begin.min });
        }
        if begin.max >= end.min {
            return Err(WindowError::Unordered {
                latest_begin: begin.max,
                earliest_end: end.min,
            });
        }
        let inputs = [self.arg(begin_seconds), self.arg(end_seconds)];
        Ok(self.node(
            Op::Window {
                max_seconds: end.max,
            },
            inputs,
        ))
    }

    pub fn curve(&mut self, curve: Curve) -> Source {
        self.node(Op::Curve(curve), [])
    }

    pub fn breakpoint_curve(
        &mut self,
        points: &[(Source, f64)],
    ) -> Result<Source, BreakpointCurveError> {
        if points.is_empty() {
            return Err(BreakpointCurveError::Empty);
        }
        if points.iter().any(|(_, value)| !value.is_finite()) {
            return Err(BreakpointCurveError::NonFiniteValue);
        }
        let times = points
            .iter()
            .map(|(time, _)| self.init_expr(*time))
            .collect::<Option<Vec<_>>>()
            .ok_or(BreakpointCurveError::DynamicTime)?;
        let values = points.iter().map(|(_, value)| *value).collect::<Vec<_>>();
        let terminal_scale = values.iter().map(|value| value.abs()).sum::<f64>().max(1.0);
        let horizon = if values.last().copied().unwrap_or_default().abs()
            > f64::EPSILON * (values.len() + 1) as f64 * terminal_scale
        {
            BreakpointHorizon::GateBounded
        } else {
            let (duration, offset) = affine_duration(times.last().expect("points are nonempty"))
                .ok_or(BreakpointCurveError::UnboundedHorizon)?;
            if duration == 0.0 && offset >= 0.0 {
                BreakpointHorizon::Absolute(offset)
            } else if duration == 1.0 && offset >= 0.0 {
                BreakpointHorizon::GateTail(offset)
            } else {
                return Err(BreakpointCurveError::UnboundedHorizon);
            }
        };
        Ok(self.node(
            Op::BreakpointCurve {
                times,
                values,
                horizon,
            },
            [],
        ))
    }

    fn init_expr(&self, source: Source) -> Option<InitExpr> {
        match source {
            Source::Const(value) => Some(InitExpr::Const(value)),
            Source::Param(id) => Some(InitExpr::Param(id)),
            Source::Port { node, channel: 0 } => {
                let node = self.template.nodes.get(node)?;
                let input = |index: usize| self.init_expr(node.inputs.get(index)?.source);
                match node.op {
                    Op::Add => Some(InitExpr::Add(Box::new(input(0)?), Box::new(input(1)?))),
                    Op::Sub => Some(InitExpr::Sub(Box::new(input(0)?), Box::new(input(1)?))),
                    Op::Mul => Some(InitExpr::Mul(Box::new(input(0)?), Box::new(input(1)?))),
                    Op::Div => Some(InitExpr::Div(Box::new(input(0)?), Box::new(input(1)?))),
                    Op::Neg => Some(InitExpr::Neg(Box::new(input(0)?))),
                    _ => None,
                }
            }
            Source::Control(_)
            | Source::Input(_)
            | Source::ExternalAudio { .. }
            | Source::Port { .. } => None,
        }
    }

    // ---------------------------------------------------------------- output

    /// Tap graph-local channels and route them to `bus`.
    ///
    /// `level` is a signal, not merely a scalar, so an instrument may automate
    /// its own send without asking the score layer to address an internal wire.
    /// The bus channel count is checked later against the program's
    /// [`crate::routing::BusLayout`].
    pub fn send(&mut self, bus: BusId, channels: &[Source], level: impl Into<Source>) {
        let level = level.into();
        let outputs = channels
            .iter()
            .copied()
            .map(|channel| {
                let scaled = self.mul(channel, level);
                self.arg(scaled)
            })
            .collect();
        self.template.sends.push(GraphSend {
            bus,
            outputs,
            src: self.src,
        });
    }

    /// Equal-power pan. Returns `(left, right)`.
    pub fn pan(&mut self, audio: Source, position: impl Into<Source>) -> (Source, Source) {
        let (audio, position) = (self.arg(audio), self.arg(position));
        let s = self.node(Op::Pan, [audio, position]);
        let node = self.node_id(s).expect("pan is a node");
        (Source::port(node, 0), Source::port(node, 1))
    }

    /// Finish as a stereo voice, panning `audio` by the implicit `n.pan`.
    ///
    /// The common ending, and the reason `pan` is in the implicit contract:
    /// three of the four songs pattern it and none of them declare it.
    pub fn out_panned(&mut self, audio: Source) -> Result<GraphTemplate, TemplateError> {
        let (l, r) = self.pan(audio, Source::pan());
        self.out(&[l, r])
    }

    pub fn out_mono(&mut self, audio: Source) -> Result<GraphTemplate, TemplateError> {
        self.out(&[audio])
    }

    /// Finish, validating. A template that does not validate never exists.
    pub fn out(&mut self, channels: &[Source]) -> Result<GraphTemplate, TemplateError> {
        let mut template = core::mem::take(&mut self.template);
        template.outputs = channels.to_vec();
        template.validate()?;
        Ok(template)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DecayError {
    Unbounded,
    NonPositive { minimum: f64 },
}

impl core::fmt::Display for DecayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecayError::Unbounded => write!(
                f,
                "decay length must be a bounded scalar expression, not an audio signal"
            ),
            DecayError::NonPositive { minimum } => {
                write!(f, "decay length can reach non-positive value {minimum}")
            }
        }
    }
}

impl core::error::Error for DecayError {}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WindowError {
    UnboundedBegin,
    UnboundedEnd,
    NegativeBegin {
        minimum: f64,
    },
    Unordered {
        latest_begin: f64,
        earliest_end: f64,
    },
}

impl core::fmt::Display for WindowError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WindowError::UnboundedBegin | WindowError::UnboundedEnd => write!(
                f,
                "window bounds must be bounded scalar expressions, not audio signals"
            ),
            WindowError::NegativeBegin { minimum } => {
                write!(f, "window begin can be negative: {minimum}")
            }
            WindowError::Unordered {
                latest_begin,
                earliest_end,
            } => write!(
                f,
                "window begin can reach {latest_begin}, not before end {earliest_end}"
            ),
        }
    }
}

impl core::error::Error for WindowError {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BreakpointCurveError {
    Empty,
    NonFiniteValue,
    DynamicTime,
    UnboundedHorizon,
}

impl core::fmt::Display for BreakpointCurveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BreakpointCurveError::Empty => {
                write!(f, "breakpoint curve requires at least one point")
            }
            BreakpointCurveError::NonFiniteValue => {
                write!(f, "breakpoint curve values must be finite")
            }
            BreakpointCurveError::DynamicTime => write!(
                f,
                "breakpoint times must be constants or note-parameter scalar arithmetic"
            ),
            BreakpointCurveError::UnboundedHorizon => write!(
                f,
                "a zero-settling breakpoint curve must end at fixed seconds or n.duration plus a non-negative offset"
            ),
        }
    }
}

impl core::error::Error for BreakpointCurveError {}

fn affine_duration(expression: &InitExpr) -> Option<(f64, f64)> {
    match expression {
        InitExpr::Const(value) => Some((0.0, *value)),
        InitExpr::Param(ParamId::Implicit(Implicit::Duration)) => Some((1.0, 0.0)),
        InitExpr::Param(_) => None,
        InitExpr::Add(left, right) => {
            let (la, lb) = affine_duration(left)?;
            let (ra, rb) = affine_duration(right)?;
            Some((la + ra, lb + rb))
        }
        InitExpr::Sub(left, right) => {
            let (la, lb) = affine_duration(left)?;
            let (ra, rb) = affine_duration(right)?;
            Some((la - ra, lb - rb))
        }
        InitExpr::Mul(left, right) => {
            let (la, lb) = affine_duration(left)?;
            let (ra, rb) = affine_duration(right)?;
            (la == 0.0 || ra == 0.0).then_some((la * rb + ra * lb, lb * rb))
        }
        InitExpr::Div(left, right) => {
            let (la, lb) = affine_duration(left)?;
            let (ra, rb) = affine_duration(right)?;
            (ra == 0.0 && rb != 0.0).then_some((la / rb, lb / rb))
        }
        InitExpr::Neg(inner) => {
            let (a, b) = affine_duration(inner)?;
            Some((-a, -b))
        }
    }
}

/// The implicit note inputs, as sources, for readability at a call site.
pub mod n {
    use super::{Implicit, ParamId, Source};

    pub const HZ: Source = Source::Param(ParamId::Implicit(Implicit::Hz));
    pub const VELOCITY: Source = Source::Param(ParamId::Implicit(Implicit::Velocity));
    pub const DURATION: Source = Source::Param(ParamId::Implicit(Implicit::Duration));
    pub const PAN: Source = Source::Param(ParamId::Implicit(Implicit::Pan));
}
