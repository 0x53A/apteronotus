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

use crate::template::{
    Adsr, Curve, GraphTemplate, Implicit, Input, Node, NodeId, Op, ParamId, ParamSpec, ShapeKind,
    Source, TemplateError,
};
use apteronotus_pattern::SrcSpan;

#[derive(Default)]
pub struct GraphBuilder {
    template: GraphTemplate,
    src: Option<SrcSpan>,
}

impl GraphBuilder {
    pub fn new() -> GraphBuilder {
        GraphBuilder::default()
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
            src: self.src,
        });
        Source::port(id, 0)
    }

    pub fn node_id(&self, source: Source) -> Option<NodeId> {
        match source {
            Source::Port { node, .. } => Some(node),
            _ => None,
        }
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
