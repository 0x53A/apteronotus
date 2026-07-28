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
    Adsr, Curve, DelayRange, GraphSend, GraphTemplate, Implicit, Input, Node, NodeId, Op, ParamId,
    ParamSpec, ShapeKind, Source, TemplateError,
};
use apteronotus_pattern::SrcSpan;

#[derive(Default)]
pub struct GraphBuilder {
    template: GraphTemplate,
    src: Option<SrcSpan>,
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

    pub fn saw(&mut self, hz: impl Into<Source>) -> Source {
        let hz = self.arg(hz);
        self.node(Op::Saw, [hz])
    }

    pub fn pulse(&mut self, hz: impl Into<Source>, duty: impl Into<Source>) -> Source {
        let (hz, duty) = (self.arg(hz), self.arg(duty));
        self.node(Op::Pulse, [hz, duty])
    }

    pub fn noise(&mut self) -> Source {
        self.node(Op::Noise, [])
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
    ) -> Source {
        let (min, max) = (self.arg(min), self.arg(max));
        self.node(Op::InitRandom { stream }, [min, max])
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

    pub fn shape(&mut self, audio: Source, kind: ShapeKind, amount: f64) -> Source {
        let audio = self.arg(audio);
        self.node(Op::Shape { kind, amount }, [audio])
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

    pub fn neg(&mut self, a: Source) -> Source {
        let a = self.arg(a);
        self.node(Op::Neg, [a])
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

    pub fn curve(&mut self, curve: Curve) -> Source {
        self.node(Op::Curve(curve), [])
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

/// The implicit note inputs, as sources, for readability at a call site.
pub mod n {
    use super::{Implicit, ParamId, Source};

    pub const HZ: Source = Source::Param(ParamId::Implicit(Implicit::Hz));
    pub const VELOCITY: Source = Source::Param(ParamId::Implicit(Implicit::Velocity));
    pub const DURATION: Source = Source::Param(ParamId::Implicit(Implicit::Duration));
    pub const PAN: Source = Source::Param(ParamId::Implicit(Implicit::Pan));
}
