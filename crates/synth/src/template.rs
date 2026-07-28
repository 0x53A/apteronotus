//! `GraphTemplate` — a staged voice, as data.
//!
//! This is the whole of step 4 in the architecture notes, and it has exactly
//! one invariant, in force from the first line: **no `Box<dyn AudioUnit>`,
//! fundsp node, host-language closure or backend pointer may sit inside it.**
//! Everything here is plain data, comparable, cloneable and inspectable. What
//! that buys is not serialisation — that is deferred until it earns its place —
//! but the things a template must support *now*: validating a voice before it
//! is published, estimating what it costs, attaching source spans to graph
//! nodes so a diagnostic can point at the offending line, and replacing a
//! program without the old one leaving anything behind.
//!
//! ## The symbolic rule
//!
//! `graph = function(n)` runs **once per edit, not once per note**. While it
//! runs, `n.hz`, `n.velocity` and every declared parameter are *symbolic*: they
//! are [`Source::Param`], and any arithmetic on them becomes graph nodes. So
//! `n.hz * (1 + n.bend * decay(ms(45)))` is a multiply node and an add node,
//! staged once, and the note's actual numbers are bound per onset by
//! [`crate::lower`].
//!
//! The consequence worth stating: an ordinary `if` on a note value cannot work,
//! because at build time there is no value to branch on. All the songs stage
//! cleanly under this rule — none of them contains one.
//!
//! ## Sources live on the node, not in an edge list
//!
//! Every input port has exactly one source, so the source belongs next to the
//! port rather than in a side table where a missing or duplicated edge would be
//! representable. Fan-out is many nodes naming one port; fan-in is a `Mix`.
//! This is also the exact shape fundsp's `Net::set_source(node, channel, …)`
//! wants, so lowering is a walk rather than a translation.

use crate::control::ControlId;
use crate::routing::BusId;
use apteronotus_pattern::SrcSpan;

/// Index of a node within one [`GraphTemplate`]. Meaningless anywhere else.
pub type NodeId = usize;

/// The five things every voice is handed whether it declares them or not.
///
/// Session 3 fixed this list rather than letting it accrete: `n.pan` was used
/// in three songs and declared in none. Each has a documented default so a
/// score that never sets one still plays.
///
/// Note what is *not* here. A voice's identity is two separate things — a
/// derived `event_seed`, which anything reproducible must use, and a runtime
/// `voice_handle`, which addresses a sounding voice and which nothing may ever
/// seed from. Neither is a graph input: the seed is consumed when the voice is
/// built, not sampled by a running node.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Implicit {
    /// Pitch in hertz. Default 440.
    Hz,
    /// 0…1. Default 1.
    Velocity,
    /// The note's length in **seconds**, known at instantiation because the
    /// sequencer scheduled it. This is what lets an ADSR be keyed to the note
    /// instead of waiting on a gate signal.
    Duration,
    /// -1 left, +1 right. Default 0.
    Pan,
}

impl Implicit {
    pub const ALL: [Implicit; 4] = [
        Implicit::Hz,
        Implicit::Velocity,
        Implicit::Duration,
        Implicit::Pan,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Implicit::Hz => "hz",
            Implicit::Velocity => "velocity",
            Implicit::Duration => "duration",
            Implicit::Pan => "pan",
        }
    }

    pub fn default_value(self) -> f64 {
        match self {
            Implicit::Hz => 440.0,
            Implicit::Velocity => 1.0,
            Implicit::Duration => 1.0,
            Implicit::Pan => 0.0,
        }
    }
}

/// Which note input a [`Source::Param`] refers to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ParamId {
    Implicit(Implicit),
    /// Index into [`GraphTemplate::params`]. An implementation detail, so
    /// 0-based; user-visible ordinals are 1-based and are somebody else's
    /// problem.
    Declared(usize),
}

/// A declared voice parameter — `params = { ring = { 0.02, 2.0, 0.18, "s" } }`.
///
/// Range and unit are carried because `params` being data is the entire payoff:
/// `cutoff(…)` exists as a pattern setter *because* the voice declared
/// `cutoff`, and a UI knob knows its travel without anyone writing one.
#[derive(Clone, PartialEq, Debug)]
pub struct ParamSpec {
    pub name: String,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub unit: Option<String>,
}

impl ParamSpec {
    pub fn new(name: &str, min: f64, max: f64, default: f64) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            default,
            min,
            max,
            unit: None,
        }
    }

    pub fn with_unit(mut self, unit: &str) -> ParamSpec {
        self.unit = Some(unit.to_string());
        self
    }

    pub fn clamp(&self, x: f64) -> f64 {
        x.clamp(self.min, self.max)
    }
}

/// Where an input port gets its signal.
///
/// A scalar is a legal source everywhere a signal is, and lowering lifts it to
/// a constant node. That is the rule that makes this a rack rather than a
/// preset browser: bind `lowpass()` with cutoff and Q as *inputs*, never
/// `lowpass_hz()`, and then any parameter of anything accepts any signal, with
/// a plain number as the degenerate case.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Source {
    Const(f64),
    /// A symbolic note input, resolved per onset.
    Param(ParamId),
    /// A writable scalar owned by the program control arena.
    Control(ControlId),
    /// One channel supplied by the host of this graph instance.
    Input(usize),
    /// Channel `channel` of node `node`.
    Port {
        node: NodeId,
        channel: u32,
    },
}

impl Source {
    pub const fn hz() -> Source {
        Source::Param(ParamId::Implicit(Implicit::Hz))
    }

    pub const fn velocity() -> Source {
        Source::Param(ParamId::Implicit(Implicit::Velocity))
    }

    pub const fn duration() -> Source {
        Source::Param(ParamId::Implicit(Implicit::Duration))
    }

    pub const fn pan() -> Source {
        Source::Param(ParamId::Implicit(Implicit::Pan))
    }

    pub const fn port(node: NodeId, channel: u32) -> Source {
        Source::Port { node, channel }
    }
}

impl From<f64> for Source {
    fn from(x: f64) -> Source {
        Source::Const(x)
    }
}

/// Waveshaper transfer curves, named rather than given as a function, because a
/// host-language closure may never reach a graph.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ShapeKind {
    Tanh,
    Atan,
    Softsign,
    Clip,
    /// Quantise to `amount` levels.
    Crush,
}

/// Allocation bounds for one interpolating delay line.
///
/// Delay time is a signal, so the graph cannot discover its extrema by
/// sampling it. The author supplies this range at construction time instead;
/// the backend allocates once from `max_seconds` and clamps modulation to the
/// range while rendering. Keeping the allocation bound in the data-only IR is
/// what lets publication budgets reject an extravagant graph before it reaches
/// the audio thread.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DelayRange {
    min_seconds: f64,
    max_seconds: f64,
}

impl DelayRange {
    pub fn new(min_seconds: f64, max_seconds: f64) -> Result<DelayRange, DelayRangeError> {
        if !min_seconds.is_finite() || !max_seconds.is_finite() {
            return Err(DelayRangeError::NonFinite);
        }
        if min_seconds < 0.0 {
            return Err(DelayRangeError::Negative { min_seconds });
        }
        if min_seconds > max_seconds {
            return Err(DelayRangeError::Reversed {
                min_seconds,
                max_seconds,
            });
        }
        Ok(DelayRange {
            min_seconds,
            max_seconds,
        })
    }

    pub fn fixed(seconds: f64) -> Result<DelayRange, DelayRangeError> {
        DelayRange::new(seconds, seconds)
    }

    pub fn min_seconds(self) -> f64 {
        self.min_seconds
    }

    pub fn max_seconds(self) -> f64 {
        self.max_seconds
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DelayRangeError {
    NonFinite,
    Negative { min_seconds: f64 },
    Reversed { min_seconds: f64, max_seconds: f64 },
}

impl core::fmt::Display for DelayRangeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DelayRangeError::NonFinite => write!(f, "delay bounds must be finite"),
            DelayRangeError::Negative { min_seconds } => {
                write!(f, "delay minimum cannot be negative: {min_seconds}")
            }
            DelayRangeError::Reversed {
                min_seconds,
                max_seconds,
            } => write!(
                f,
                "delay minimum {min_seconds} exceeds maximum {max_seconds}"
            ),
        }
    }
}

impl core::error::Error for DelayRangeError {}

/// A term of an automation curve.
///
/// Curves are a **basis sum**, `Σ cᵢ·basisᵢ(t − Tᵢ)`, not a breakpoint list.
/// The reason is closure under the operations a composer actually performs:
/// curves add, scale and shift, so superposition works, where two breakpoint
/// lists cannot be added without merging their time grids. An intro gate is
/// literally `step(0) − step(T_end)`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Basis {
    /// 0 before `delay`, 1 after.
    Step,
    /// 0, then rises linearly to 1 over `length`, then holds.
    Ramp,
    /// 1 at `delay`, decaying exponentially with time constant `length`,
    /// scaled so it reaches ~5% at `length` (the t60-ish convention the songs
    /// assume when they write `decay(ms(26))`).
    Decay,
    /// One period of a unipolar sine of period `length`, continuing to
    /// oscillate.
    Sine,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CurveTerm {
    pub basis: Basis,
    /// Multiplier `cᵢ`. Negative is how a gate closes.
    pub coefficient: f64,
    /// `Tᵢ`, in seconds on this curve's clock.
    pub delay: f64,
    /// Time constant / period / rise time, in seconds. Ignored by `Step`.
    pub length: f64,
}

/// A sum of basis terms, evaluated at control rate.
///
/// The clock is **not** stored here, and that is deliberate: a curve has three
/// placements — inside a voice (t = note onset), on a pattern (sampled once per
/// onset, t = transport) and on a bus (persistent, continuous) — and placement
/// stays syntactically visible rather than inferred. Only the first exists in
/// this crate today, so everything here reads a note clock; the other two
/// arrive with buses.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Curve {
    pub terms: Vec<CurveTerm>,
    pub offset: f64,
}

impl Curve {
    pub fn constant(x: f64) -> Curve {
        Curve {
            terms: Vec::new(),
            offset: x,
        }
    }

    pub fn term(mut self, basis: Basis, coefficient: f64, delay: f64, length: f64) -> Curve {
        self.terms.push(CurveTerm {
            basis,
            coefficient,
            delay,
            length,
        });
        self
    }

    /// `decay(t)` — the shape `poles.eod` fires into everything.
    pub fn decay(length: f64) -> Curve {
        Curve::default().term(Basis::Decay, 1.0, 0.0, length)
    }

    /// `window(a, b)` — on at `a`, off at `b`. Two steps, which is the whole
    /// argument for the basis form in one line.
    pub fn window(a: f64, b: f64) -> Curve {
        Curve::default()
            .term(Basis::Step, 1.0, a, 0.0)
            .term(Basis::Step, -1.0, b, 0.0)
    }

    /// Value at `t` seconds on this curve's clock.
    ///
    /// Pure in `t`, which is the point: an LFO or an envelope derived from a
    /// clock has no phase to migrate across a live edit.
    pub fn at(&self, t: f64) -> f64 {
        let mut sum = self.offset;
        for term in &self.terms {
            let u = t - term.delay;
            if u < 0.0 {
                continue;
            }
            let v = match term.basis {
                Basis::Step => 1.0,
                Basis::Ramp => {
                    if term.length <= 0.0 {
                        1.0
                    } else {
                        (u / term.length).min(1.0)
                    }
                }
                Basis::Decay => {
                    if term.length <= 0.0 {
                        0.0
                    } else {
                        // 3 time constants to ~5%, matching t_se ≈ 3/(ζω₀).
                        (-3.0 * u / term.length).exp()
                    }
                }
                Basis::Sine => {
                    if term.length <= 0.0 {
                        0.0
                    } else {
                        0.5 - 0.5 * (core::f64::consts::TAU * u / term.length).cos()
                    }
                }
            };
            sum += term.coefficient * v;
        }
        sum
    }
}

/// An attack-decay-sustain-release envelope keyed to a **known** note length.
///
/// fundsp has `adsr_live`, which waits on a gate input, because it was written
/// for a MIDI keyboard where nobody knows when the key comes up. A
/// *sequencer-instantiated* voice does know: its length was decided when it was
/// scheduled. So this is a note-clock function rather than a gate follower, and
/// it is one of the four things `CLAUDE.md` records as missing from fundsp.
///
/// That scope is a real limit, not a formality. A live note has no known
/// duration, so it needs either an explicit gate input or the persistent
/// `patch` lifetime; this envelope is for scheduled voices only, and a voice
/// scheduled on this envelope must be given `duration + release` of sequencer
/// time or the release is cut off by the very thing that made it computable.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Adsr {
    pub attack: f64,
    pub decay: f64,
    pub sustain: f64,
    pub release: f64,
}

impl Adsr {
    pub fn new(attack: f64, decay: f64, sustain: f64, release: f64) -> Adsr {
        Adsr {
            attack,
            decay,
            sustain,
            release,
        }
    }

    /// Value at `t` seconds after onset, for a note held `gate` seconds.
    ///
    /// The release begins at `gate` regardless of where the attack and decay
    /// had got to, so a note shorter than its own attack still releases from
    /// wherever it reached rather than jumping.
    pub fn at(&self, t: f64, gate: f64) -> f64 {
        let held = |u: f64| -> f64 {
            if u < self.attack {
                if self.attack <= 0.0 {
                    1.0
                } else {
                    u / self.attack
                }
            } else if u < self.attack + self.decay {
                if self.decay <= 0.0 {
                    self.sustain
                } else {
                    1.0 + (self.sustain - 1.0) * (u - self.attack) / self.decay
                }
            } else {
                self.sustain
            }
        };
        if t < gate {
            held(t)
        } else if self.release <= 0.0 || t >= gate + self.release {
            0.0
        } else {
            let level = held(gate);
            level * (1.0 - (t - gate) / self.release)
        }
    }

    /// How long after onset the envelope is certainly silent. The scheduler
    /// needs this to know when the voice may be freed.
    pub fn tail(&self) -> f64 {
        self.release
    }
}

/// A primitive. Anything expressible by composition is *not* here — it belongs
/// in a scripting-language stdlib shipped as readable source, which is what
/// keeps this list from becoming the ceiling. `ring`, `supersaw`, `formant` and
/// the whole reverb network are stdlib; only the irreducibly stateful pieces
/// and the ones needing a per-sample feedback path are primitives.
#[derive(Clone, PartialEq, Debug)]
pub enum Op {
    // ------------------------------------------------------------ generators
    /// (hz) → audio
    Sine,
    /// (hz) → audio. Band-limited.
    Saw,
    /// (hz, duty) → audio
    Pulse,
    /// () → audio. White.
    Noise,
    /// () → audio. A unit sample, once, at onset. δ, and the reason percussion
    /// in `poles.eod` is pole placement: the impulse response of a resonant
    /// second-order section is a struck bar.
    Impulse,
    /// (min, max) → one deterministic scalar per voice.
    InitRandom {
        stream: u64,
    },

    // --------------------------------------------------------------- filters
    /// (audio, cutoff, q) → audio
    Lowpass,
    Highpass,
    Bandpass,
    /// (audio, cutoff, q) → audio. The ladder.
    Moog,

    // -------------------------------------------------------------- shaping
    /// (audio) → audio
    Shape {
        kind: ShapeKind,
        amount: f64,
    },
    /// (audio) → audio
    DcBlock,

    // --------------------------------------------------------------- memory
    /// (audio, delay_seconds) → audio. Cubic-interpolating, with allocation
    /// bounds fixed when the graph is staged.
    Delay(DelayRange),

    // ------------------------------------------------------------ arithmetic
    /// (a, b) → a + b. Mix.
    Add,
    /// (a, b) → a − b
    Sub,
    /// (a, b) → a × b. A VCA when one side is an envelope.
    Mul,
    /// (a, b) → a ÷ b
    Div,
    /// (a) → −a
    Neg,

    // ------------------------------------------------------------- envelopes
    /// () → control. Reads the note clock.
    Adsr(Adsr),
    /// () → control. Reads the note clock. See [`Curve`] on placement.
    Curve(Curve),

    // ---------------------------------------------------------------- output
    /// (audio, position) → (left, right). The one node in this set with two
    /// outputs, which is why sources carry a channel.
    Pan,
}

impl Op {
    pub fn inputs(&self) -> usize {
        match self {
            Op::Noise | Op::Impulse | Op::Adsr(_) | Op::Curve(_) => 0,
            Op::Sine | Op::Saw | Op::Shape { .. } | Op::DcBlock | Op::Neg => 1,
            Op::Pulse
            | Op::InitRandom { .. }
            | Op::Delay(_)
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Pan => 2,
            Op::Lowpass | Op::Highpass | Op::Bandpass | Op::Moog => 3,
        }
    }

    pub fn outputs(&self) -> usize {
        match self {
            Op::Pan => 2,
            _ => 1,
        }
    }

    /// How long this node keeps producing sound after its input stops, in
    /// seconds. Summed along a path it tells the scheduler how long to let a
    /// voice ring before releasing it — a bell with a 2.6 s decay must not be
    /// cut off at its note length.
    pub fn tail(&self) -> f64 {
        match self {
            Op::Delay(range) => range.max_seconds(),
            Op::Adsr(a) => a.tail(),
            Op::Curve(c) => c
                .terms
                .iter()
                .map(|t| t.delay + t.length)
                .fold(0.0, f64::max),
            _ => 0.0,
        }
    }
}

/// One input port, with the span of the expression that filled it.
///
/// The span is per *argument*, not per node, and that is worth the extra field
/// now rather than later: a connection or rate error is almost always about one
/// operand, and a node-level span can only highlight the whole call. The same
/// argument that made the pattern an AST rather than a tree of closures applies
/// one level down.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Input {
    pub source: Source,
    pub src: Option<SrcSpan>,
}

impl Input {
    pub const fn new(source: Source) -> Input {
        Input { source, src: None }
    }

    pub const fn at(source: Source, src: SrcSpan) -> Input {
        Input {
            source,
            src: Some(src),
        }
    }
}

impl From<Source> for Input {
    fn from(source: Source) -> Input {
        Input::new(source)
    }
}

impl From<f64> for Input {
    fn from(x: f64) -> Input {
        Input::new(Source::Const(x))
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Node {
    pub op: Op,
    /// One entry per input port, in the order [`Op`] documents.
    pub inputs: Vec<Input>,
    /// Conservative time this node can keep producing audible output after its
    /// inputs stop, in seconds.
    ///
    /// Most primitives are memoryless and leave this at zero. A readable
    /// stdlib composition may know more than its final primitive does:
    /// `ring(hz, decay)` is a band-pass plus a derived Q, and annotates that
    /// band-pass with `decay` without turning `ring` into a Rust DSP primitive.
    pub tail: f64,
    /// Where in the source text this node was written. Survives into
    /// diagnostics and, later, into the editor's highlighting — the same reason
    /// the pattern is an AST rather than a tree of closures.
    pub src: Option<SrcSpan>,
}

/// A graph-local tap routed to a program bus.
///
/// These outputs may name any internal graph source. A score-level send cannot:
/// it copies the finished [`GraphTemplate::outputs`] and lives in
/// [`crate::routing::EventRouting`] instead.
#[derive(Clone, PartialEq, Debug)]
pub struct GraphSend {
    pub bus: BusId,
    pub outputs: Vec<Input>,
    pub src: Option<SrcSpan>,
}

/// A staged voice: everything except the note's actual numbers.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct GraphTemplate {
    /// Host-provided audio-rate input channels.
    pub inputs: usize,
    pub nodes: Vec<Node>,
    /// Declared note parameters, indexed by [`ParamId::Declared`].
    pub params: Vec<ParamSpec>,
    /// One source per output channel. Two, for a stereo voice.
    pub outputs: Vec<Source>,
    /// Graph-local taps. Their bus handles are resolved against a
    /// program-wide [`crate::routing::BusLayout`] during routed lowering.
    pub sends: Vec<GraphSend>,
}

impl GraphTemplate {
    pub fn channels(&self) -> usize {
        self.outputs.len()
    }

    pub fn cost(&self) -> GraphCost {
        GraphCost {
            nodes: self.nodes.len(),
            connections: self
                .nodes
                .iter()
                .map(|node| node.inputs.len())
                .sum::<usize>()
                + self.outputs.len()
                + self
                    .sends
                    .iter()
                    .map(|send| send.outputs.len())
                    .sum::<usize>(),
            input_channels: self.inputs,
            output_channels: self.outputs.len()
                + self
                    .sends
                    .iter()
                    .map(|send| send.outputs.len())
                    .sum::<usize>(),
            declared_parameters: self.params.len(),
            delay_buffer_seconds: self
                .nodes
                .iter()
                .map(|node| match node.op {
                    Op::Delay(range) => range.max_seconds(),
                    _ => 0.0,
                })
                .sum(),
            tail_seconds: self.tail(),
        }
    }

    /// Validate caller-selected publication limits.
    ///
    /// The engine does not smuggle policy into the IR by choosing one universal
    /// ceiling. Desktop, browser and embedded hosts supply their own budgets,
    /// but all enforce them over the same deterministic estimate.
    pub fn validate_limits(&self, limits: GraphLimits) -> Result<GraphCost, GraphLimitError> {
        let cost = self.cost();
        if cost.nodes > limits.nodes {
            return Err(GraphLimitError::Nodes {
                found: cost.nodes,
                limit: limits.nodes,
            });
        }
        if cost.connections > limits.connections {
            return Err(GraphLimitError::Connections {
                found: cost.connections,
                limit: limits.connections,
            });
        }
        if cost.input_channels > limits.input_channels {
            return Err(GraphLimitError::InputChannels {
                found: cost.input_channels,
                limit: limits.input_channels,
            });
        }
        if cost.output_channels > limits.output_channels {
            return Err(GraphLimitError::OutputChannels {
                found: cost.output_channels,
                limit: limits.output_channels,
            });
        }
        if cost.delay_buffer_seconds > limits.delay_buffer_seconds {
            return Err(GraphLimitError::DelayBuffer {
                found: cost.delay_buffer_seconds,
                limit: limits.delay_buffer_seconds,
            });
        }
        if cost.tail_seconds > limits.tail_seconds {
            return Err(GraphLimitError::Tail {
                found: cost.tail_seconds,
                limit: limits.tail_seconds,
            });
        }
        Ok(cost)
    }

    /// The longest tail on any path to an output, in seconds.
    ///
    /// Accumulated **along paths**, not maximised over nodes. A one-second
    /// delay feeding a five-second reverb needs six, and a per-node maximum
    /// would say five and cut the tail off. That is the failure worth guarding
    /// against: holding a dead voice an extra second is inaudible, truncating a
    /// bell is not.
    ///
    /// Valid only on a template that passes [`validate`](Self::validate),
    /// which is what guarantees nodes reference only earlier ones and so lets
    /// one forward pass find the longest path. It is still an upper bound
    /// rather than an exact figure — series composition of tails is additive
    /// only in the worst case — and it will need explicit per-op composition
    /// rules once feedback exists, since a feedback path has no finite longest
    /// path at all.
    pub fn tail(&self) -> f64 {
        let mut through = vec![0.0f64; self.nodes.len()];
        for (id, node) in self.nodes.iter().enumerate() {
            let upstream = node
                .inputs
                .iter()
                .map(|input| match input.source {
                    Source::Port { node, .. } => through.get(node).copied().unwrap_or(0.0),
                    _ => 0.0,
                })
                .fold(0.0, f64::max);
            through[id] = upstream + node.tail.max(node.op.tail());
        }
        self.outputs
            .iter()
            .chain(
                self.sends
                    .iter()
                    .flat_map(|send| send.outputs.iter().map(|input| &input.source)),
            )
            .map(|source| match source {
                Source::Port { node, .. } => through.get(*node).copied().unwrap_or(0.0),
                _ => 0.0,
            })
            .fold(0.0, f64::max)
    }

    /// Check every invariant the lowering relies on.
    ///
    /// Run before publication, never on the audio thread. This is where the
    /// "unbounded work happens before publication" rule cashes out: evaluation
    /// is a hotkey, so there is real time to do this, and a failure leaves the
    /// previous program playing.
    pub fn validate(&self) -> Result<(), TemplateError> {
        if self.outputs.is_empty() {
            return Err(TemplateError::NoOutputs);
        }
        for (param, spec) in self.params.iter().enumerate() {
            if !spec.min.is_finite()
                || !spec.max.is_finite()
                || !spec.default.is_finite()
                || spec.min > spec.max
                || !(spec.min..=spec.max).contains(&spec.default)
            {
                return Err(TemplateError::InvalidParamRange { param });
            }
        }
        for (id, node) in self.nodes.iter().enumerate() {
            if !node.tail.is_finite() || node.tail < 0.0 {
                return Err(TemplateError::InvalidTail {
                    node: id,
                    seconds: node.tail,
                });
            }
            if node.inputs.len() != node.op.inputs() {
                return Err(TemplateError::Arity {
                    node: id,
                    expected: node.op.inputs(),
                    found: node.inputs.len(),
                });
            }
            if matches!(node.op, Op::InitRandom { .. }) {
                for (port, input) in node.inputs.iter().enumerate() {
                    if matches!(
                        input.source,
                        Source::Port { .. } | Source::Control(_) | Source::Input(_)
                    ) {
                        return Err(TemplateError::DynamicInitRandom { node: id, port });
                    }
                }
            }
            for (port, input) in node.inputs.iter().enumerate() {
                self.check_source(input.source, Some((id, port)))?;
            }
        }
        for (channel, source) in self.outputs.iter().enumerate() {
            self.check_source(*source, None).map_err(|e| match e {
                TemplateError::DanglingPort { .. } => TemplateError::BadOutput { channel },
                other => other,
            })?;
        }
        for (send_index, send) in self.sends.iter().enumerate() {
            if send.outputs.is_empty() {
                return Err(TemplateError::EmptySend { send: send_index });
            }
            for input in &send.outputs {
                self.check_source(input.source, None)?;
            }
        }
        Ok(())
    }

    fn check_source(
        &self,
        source: Source,
        at: Option<(NodeId, usize)>,
    ) -> Result<(), TemplateError> {
        match source {
            Source::Const(x) => {
                if x.is_finite() {
                    Ok(())
                } else {
                    Err(TemplateError::NonFiniteConstant)
                }
            }
            Source::Param(ParamId::Declared(i)) if i >= self.params.len() => {
                Err(TemplateError::UndeclaredParam(i))
            }
            Source::Param(_) => Ok(()),
            Source::Control(_) => Ok(()),
            Source::Input(channel) => {
                if channel < self.inputs {
                    Ok(())
                } else {
                    Err(TemplateError::BadInput { channel })
                }
            }
            Source::Port { node, channel } => {
                let target = self
                    .nodes
                    .get(node)
                    .ok_or(TemplateError::DanglingPort { node, channel })?;
                if channel as usize >= target.op.outputs() {
                    return Err(TemplateError::DanglingPort { node, channel });
                }
                // Forward references are how a cycle would be written. A
                // Delay node owns audio history, but merely putting one in an
                // otherwise arbitrary cycle does not define feedback order or
                // gain. Require a DAG in build order until feedback has its
                // own explicit representation.
                if let Some((id, port)) = at
                    && node >= id
                {
                    return Err(TemplateError::Cycle { node: id, port });
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum TemplateError {
    NoOutputs,
    Arity {
        node: NodeId,
        expected: usize,
        found: usize,
    },
    DanglingPort {
        node: NodeId,
        channel: u32,
    },
    BadOutput {
        channel: usize,
    },
    Cycle {
        node: NodeId,
        port: usize,
    },
    UndeclaredParam(usize),
    InvalidParamRange {
        param: usize,
    },
    NonFiniteConstant,
    InvalidTail {
        node: NodeId,
        seconds: f64,
    },
    EmptySend {
        send: usize,
    },
    BadInput {
        channel: usize,
    },
    DynamicInitRandom {
        node: NodeId,
        port: usize,
    },
}

impl core::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TemplateError::NoOutputs => write!(f, "graph has no outputs"),
            TemplateError::Arity {
                node,
                expected,
                found,
            } => write!(f, "node {node} takes {expected} inputs, got {found}"),
            TemplateError::DanglingPort { node, channel } => {
                write!(f, "no such port: node {node} channel {channel}")
            }
            TemplateError::BadOutput { channel } => write!(f, "output {channel} has no source"),
            TemplateError::Cycle { node, port } => {
                write!(
                    f,
                    "node {node} input {port} refers forward; graphs are acyclic"
                )
            }
            TemplateError::UndeclaredParam(i) => write!(f, "parameter {i} was never declared"),
            TemplateError::InvalidParamRange { param } => {
                write!(f, "parameter {param} has an invalid range or default")
            }
            TemplateError::NonFiniteConstant => write!(f, "constant is not finite"),
            TemplateError::InvalidTail { node, seconds } => {
                write!(f, "node {node} has invalid tail {seconds} seconds")
            }
            TemplateError::EmptySend { send } => {
                write!(f, "graph send {send} has no output channels")
            }
            TemplateError::BadInput { channel } => {
                write!(f, "graph has no input channel {channel}")
            }
            TemplateError::DynamicInitRandom { node, port } => write!(
                f,
                "node {node} init-random input {port} is not fixed at voice instantiation"
            ),
        }
    }
}

impl core::error::Error for TemplateError {}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GraphCost {
    pub nodes: usize,
    pub connections: usize,
    pub input_channels: usize,
    /// Main outputs plus graph-send stem channels.
    pub output_channels: usize,
    pub declared_parameters: usize,
    /// Sum of maximum delay-line lengths. At a known sample rate this converts
    /// directly to the dominant state-memory allocation.
    pub delay_buffer_seconds: f64,
    pub tail_seconds: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GraphLimits {
    pub nodes: usize,
    pub connections: usize,
    pub input_channels: usize,
    pub output_channels: usize,
    pub delay_buffer_seconds: f64,
    pub tail_seconds: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GraphLimitError {
    Nodes { found: usize, limit: usize },
    Connections { found: usize, limit: usize },
    InputChannels { found: usize, limit: usize },
    OutputChannels { found: usize, limit: usize },
    DelayBuffer { found: f64, limit: f64 },
    Tail { found: f64, limit: f64 },
}

impl core::fmt::Display for GraphLimitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GraphLimitError::Nodes { found, limit } => {
                write!(f, "graph has {found} nodes, more than the limit of {limit}")
            }
            GraphLimitError::Connections { found, limit } => write!(
                f,
                "graph has {found} connections, more than the limit of {limit}"
            ),
            GraphLimitError::InputChannels { found, limit } => write!(
                f,
                "graph has {found} input channels, more than the limit of {limit}"
            ),
            GraphLimitError::OutputChannels { found, limit } => write!(
                f,
                "graph has {found} output channels, more than the limit of {limit}"
            ),
            GraphLimitError::DelayBuffer { found, limit } => write!(
                f,
                "graph allocates {found} delay-line seconds, more than the limit of {limit}"
            ),
            GraphLimitError::Tail { found, limit } => write!(
                f,
                "graph has a {found}-second tail, more than the limit of {limit}"
            ),
        }
    }
}

impl core::error::Error for GraphLimitError {}
