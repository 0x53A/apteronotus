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
use crate::input::AudioInputId;
use crate::note::{Note, ParamValue, ParamValueError};
use crate::routing::BusId;
pub use apteronotus_pattern::{Basis, Curve, CurveClock, CurveTerm};
use apteronotus_pattern::{CurveActivity, SrcSpan};

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
    /// Maximum onset-relative horizon accepted from a `NoteSeconds` event
    /// curve. `None` accepts scalars and `NotePhase` curves only.
    pub max_curve_seconds: Option<f64>,
}

/// One program control sampled at a realtime onset and bound into the new
/// voice as an init-rate parameter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InitControlBinding {
    pub param: ParamId,
    pub control: crate::ControlId,
}

/// Duration of the host-visible pulse emitted by [`Op::OnsetDetector`].
///
/// The detector's `hold_seconds` is a refractory interval, not this pulse
/// length. Keeping the two names and lifetimes separate prevents host polling
/// policy from silently changing retrigger behavior.
pub const ONSET_PULSE_SECONDS: f64 = 0.030;

impl ParamSpec {
    pub fn new(name: &str, min: f64, max: f64, default: f64) -> ParamSpec {
        ParamSpec {
            name: name.to_string(),
            default,
            min,
            max,
            unit: None,
            max_curve_seconds: None,
        }
    }

    pub fn with_unit(mut self, unit: &str) -> ParamSpec {
        self.unit = Some(unit.to_string());
        self
    }

    pub fn with_curve_horizon(mut self, seconds: f64) -> ParamSpec {
        self.max_curve_seconds = Some(seconds);
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
    /// One channel of a program-scope optional host input.
    ExternalAudio {
        input: AudioInputId,
        channel: usize,
    },
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

/// A scalar resolved once when a voice is instantiated.
///
/// Stateful algorithms whose allocation depends on a parameter cannot accept
/// arbitrary audio-rate modulation. Constants and note parameters make that
/// rate boundary explicit in the data-only graph.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InitScalar {
    Const(f64),
    Param(ParamId),
}

/// A scalar expression resolved once when a voice is instantiated.
///
/// Unlike [`InitScalar`], this retains the small arithmetic tree needed by
/// symbolic breakpoint times such as `n.duration + 2.4`. It is deliberately
/// not a general graph: only scalar arithmetic over constants and note
/// parameters is representable.
#[derive(Clone, PartialEq, Debug)]
pub enum InitExpr {
    Const(f64),
    Param(ParamId),
    Add(Box<InitExpr>, Box<InitExpr>),
    Sub(Box<InitExpr>, Box<InitExpr>),
    Mul(Box<InitExpr>, Box<InitExpr>),
    Div(Box<InitExpr>, Box<InitExpr>),
    Neg(Box<InitExpr>),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BreakpointHorizon {
    Absolute(f64),
    GateTail(f64),
    GateBounded,
}

impl InitScalar {
    pub fn from_source(source: Source) -> Result<InitScalar, InitScalarError> {
        match source {
            Source::Const(value) => Ok(InitScalar::Const(value)),
            Source::Param(id) => Ok(InitScalar::Param(id)),
            Source::Control(_)
            | Source::Input(_)
            | Source::ExternalAudio { .. }
            | Source::Port { .. } => Err(InitScalarError::Dynamic),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitScalarError {
    Dynamic,
}

impl core::fmt::Display for InitScalarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "value must be constant or bound once per note")
    }
}

impl core::error::Error for InitScalarError {}

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
/// One left-closed slot in a compiled transport sequence.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TransportSlot {
    pub begin_seconds: f64,
    pub end_seconds: f64,
    pub value: f64,
}

impl TransportSlot {
    fn valid(&self) -> bool {
        self.begin_seconds.is_finite()
            && self.end_seconds.is_finite()
            && self.value.is_finite()
            && self.begin_seconds >= 0.0
            && self.end_seconds > self.begin_seconds
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Op {
    // ------------------------------------------------------------ generators
    /// (hz) → audio
    Sine,
    /// (hz) → audio, with a quarter-cycle initial phase
    Cosine,
    /// (hz) → audio. Band-limited.
    Saw,
    /// (hz, duty) → audio
    Pulse,
    /// () → audio. White.
    Noise,
    /// () → audio. Pink, with approximately 3 dB/octave spectral falloff.
    Pink,
    /// () → audio. A unit sample, once, at onset. δ, and the reason percussion
    /// in `poles.eod` is pole placement: the impulse response of a resonant
    /// second-order section is a struck bar.
    Impulse,
    /// () → one deterministic scalar per voice in the init-rate range.
    InitRandom {
        stream: u64,
        min: InitExpr,
        max: InitExpr,
    },
    /// (excitation) → Karplus–Strong string. Pitch and damping are init-rate
    /// values because they determine the internal delay-line state.
    Pluck {
        frequency: InitScalar,
        gain_per_second: f64,
        damping: InitScalar,
        max_delay_seconds: f64,
    },

    // --------------------------------------------------------------- filters
    /// (audio, cutoff, q) → audio
    Lowpass,
    Highpass,
    Bandpass,
    Peak,
    /// (audio, cutoff, q) → audio. The ladder.
    Moog,

    // -------------------------------------------------------------- shaping
    /// (audio) → audio
    Shape {
        kind: ShapeKind,
    },
    /// (audio) → audio
    DcBlock,

    // --------------------------------------------------------------- memory
    /// (audio, delay_seconds) → audio. Cubic-interpolating, with allocation
    /// bounds fixed when the graph is staged.
    Delay(DelayRange),
    /// (left, right) → (wet_left, wet_right). Stateful stereo FDN.
    Reverb {
        room_size: f64,
        time: f64,
        damping: f64,
    },
    /// (left, right) → limited stereo, with lookahead equal to `attack`.
    Limiter {
        attack: f64,
        release: f64,
    },
    /// (audio) → chorused audio, including the dry signal.
    Chorus {
        seed: u64,
        separation: f64,
        variation: f64,
        frequency: f64,
    },
    /// (audio) → dry plus a bounded delay/optional-lowpass feedback loop.
    FeedbackDelay {
        delay_seconds: f64,
        cutoff_q: Option<(f64, f64)>,
        amount: f64,
    },
    /// (audio) → Schroeder allpass diffusion stage.
    AllpassDelay {
        seconds: f64,
        gain: f64,
    },
    /// (audio, t60_seconds) → wet audio. The matrix is a normalized
    /// power-of-two Hadamard transform and each line owns a bounded,
    /// optionally modulated delay buffer.
    Fdn {
        delays: Vec<f64>,
        damping: f64,
        modulation_rate: f64,
        modulation_depth: f64,
        max_t60: f64,
    },
    /// (audio) → unipolar amplitude control.
    EnvelopeFollower {
        attack: f64,
        release: f64,
    },
    /// (audio) → monophonic frequency control in Hz.
    PitchTracker {
        min_hz: f64,
        max_hz: f64,
        default_hz: f64,
        hold_seconds: f64,
    },
    /// (audio) → a short unipolar pulse on a bounded transient threshold
    /// crossing. The pulse is published to the host as an external event
    /// source; it is not queryable pattern structure.
    OnsetDetector {
        floor: f64,
        hold_seconds: f64,
    },
    /// () → a repeating scalar compiled from one bounded transport-pattern
    /// period. This is persistent-clock data, never note-clock automation.
    TransportSequence {
        period_seconds: f64,
        slots: Vec<TransportSlot>,
    },
    /// (left, right) → mid/side-scaled stereo. `amount = 0` is mono and one
    /// preserves the input width.
    Width,
    /// (signal) → smoothed signal. The response time is resolved once per
    /// voice; the signal itself remains live.
    Slew {
        response_time: InitScalar,
        initial: f64,
    },
    /// (gate) → persistent attack/decay/sustain/release envelope.
    GateEnv {
        attack: f64,
        decay: f64,
        sustain: f64,
        release: f64,
    },
    /// () → onset-relative pitch glide from the preceding track event to the
    /// current note. Kept distinct from `Slew`: a per-note constant `n.hz`
    /// has no edge for an ordinary follower to smooth.
    Portamento {
        target: InitScalar,
        response_time: InitScalar,
    },
    /// Runtime-inserted lifecycle gate for a finite persistent run.
    ///
    /// It is placed before nonterminating activity sources, not at the graph
    /// output, so downstream state receives silence at the boundary and may
    /// drain its declared response tail.
    RunGate {
        active_seconds: f64,
        fade_seconds: f64,
    },
    /// (signal) → signal while publishing the current sample to one
    /// program-scope shared control.
    ///
    /// This is emitted only by the language/runtime's persistent signal
    /// arena. Passing the signal through keeps it in the ordinary graph
    /// dependency order; the arena multiplies its output by zero before
    /// routing so the writer itself is inaudible.
    ControlWrite {
        target: crate::ControlId,
    },

    // ------------------------------------------------------------ arithmetic
    /// (a, b) → a + b. Mix.
    Add,
    /// (a, b) → a − b
    Sub,
    /// (a, b) → a × b. A VCA when one side is an envelope.
    Mul,
    /// (a, b) → a ÷ b
    Div,
    /// (base, exponent) → base raised to exponent
    Pow,
    /// (a) → −a
    Neg,
    /// (hz) → fractional MIDI. Used at the typed boundary between a scheduled
    /// pitch event and a persistent note control.
    HzToMidi,
    /// (a) → a constrained to a fixed finite range
    Clamp {
        min: f64,
        max: f64,
    },

    // ------------------------------------------------------------- envelopes
    /// () → control. Reads the note clock.
    Adsr(Adsr),
    /// (seconds) → control. Exponential note-clock decay with a
    /// construction-time retention bound.
    Decay {
        max_seconds: f64,
    },
    /// (begin_seconds, end_seconds) → control. A bounded note-clock gate.
    Window {
        max_seconds: f64,
    },
    /// () → control. Reads the note clock. See [`Curve`] on placement.
    Curve(Curve),
    /// A piecewise-linear note-clock curve whose breakpoint times are
    /// instantiation-rate scalar expressions.
    BreakpointCurve {
        times: Vec<InitExpr>,
        values: Vec<f64>,
        horizon: BreakpointHorizon,
    },

    // ---------------------------------------------------------------- output
    /// (audio, position) → (left, right). The one node in this set with two
    /// outputs, which is why sources carry a channel.
    Pan,
}

impl Op {
    pub fn inputs(&self) -> usize {
        match self {
            Op::Noise
            | Op::Pink
            | Op::Impulse
            | Op::InitRandom { .. }
            | Op::Adsr(_)
            | Op::Curve(_)
            | Op::BreakpointCurve { .. }
            | Op::TransportSequence { .. }
            | Op::Portamento { .. }
            | Op::RunGate { .. } => 0,
            Op::Sine
            | Op::Cosine
            | Op::Saw
            | Op::Pluck { .. }
            | Op::DcBlock
            | Op::Neg
            | Op::HzToMidi
            | Op::Clamp { .. }
            | Op::Decay { .. }
            | Op::Chorus { .. }
            | Op::FeedbackDelay { .. }
            | Op::AllpassDelay { .. }
            | Op::EnvelopeFollower { .. }
            | Op::PitchTracker { .. }
            | Op::OnsetDetector { .. }
            | Op::Slew { .. }
            | Op::GateEnv { .. }
            | Op::ControlWrite { .. } => 1,
            Op::Pulse
            | Op::Delay(_)
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Pow
            | Op::Shape { .. }
            | Op::Window { .. }
            | Op::Pan
            | Op::Fdn { .. } => 2,
            Op::Width => 3,
            Op::Reverb { .. } | Op::Limiter { .. } => 2,
            Op::Lowpass | Op::Highpass | Op::Bandpass | Op::Peak | Op::Moog => 3,
        }
    }

    pub fn outputs(&self) -> usize {
        match self {
            Op::Pan | Op::Width | Op::Reverb { .. } | Op::Limiter { .. } => 2,
            _ => 1,
        }
    }

    /// Whether input `port` carries activity whose lifetime reaches the output.
    ///
    /// Filter frequency and Q inputs steer activity but do not create it, so a
    /// long cutoff curve must not retain a voice. Arithmetic conservatively
    /// treats both inputs as activity-bearing. In particular `Mul` cannot use
    /// the tempting minimum rule: `audio * dc(0.5)` would otherwise inherit
    /// the constant's zero metadata and truncate every voice. Over-retaining a
    /// delayed signal multiplied by a short envelope is the safe direction.
    pub fn activity_input(&self, port: usize) -> bool {
        match self {
            Op::Lowpass | Op::Highpass | Op::Bandpass | Op::Peak | Op::Moog => port == 0,
            Op::Shape { .. }
            | Op::DcBlock
            | Op::Delay(_)
            | Op::Neg
            | Op::HzToMidi
            | Op::Clamp { .. }
            | Op::Chorus { .. }
            | Op::FeedbackDelay { .. }
            | Op::AllpassDelay { .. }
            | Op::Fdn { .. }
            | Op::EnvelopeFollower { .. }
            | Op::PitchTracker { .. }
            | Op::OnsetDetector { .. }
            | Op::Width
            | Op::Slew { .. }
            | Op::GateEnv { .. }
            | Op::ControlWrite { .. }
            | Op::Pluck { .. }
            | Op::Pan => port == 0,
            Op::Reverb { .. } | Op::Limiter { .. } => port < 2,
            Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Pow => port < 2,
            Op::Sine
            | Op::Cosine
            | Op::Saw
            | Op::Pulse
            | Op::Noise
            | Op::Pink
            | Op::Impulse
            | Op::InitRandom { .. }
            | Op::Adsr(_)
            | Op::Decay { .. }
            | Op::Window { .. }
            | Op::Curve(_)
            | Op::BreakpointCurve { .. }
            | Op::TransportSequence { .. }
            | Op::Portamento { .. }
            | Op::RunGate { .. } => false,
        }
    }

    /// Stateful response after the activity driving this node ends.
    fn response_tail(&self) -> f64 {
        match self {
            Op::Delay(range) => range.max_seconds(),
            Op::Reverb { time, .. } => *time,
            Op::Limiter { attack, .. } => *attack,
            Op::Chorus {
                separation,
                variation,
                ..
            } => separation * 4.0 + variation,
            Op::FeedbackDelay {
                delay_seconds,
                amount,
                ..
            } => {
                let repeats = (0.05_f64.ln() / amount.abs().ln()).ceil().max(1.0);
                delay_seconds * repeats
            }
            Op::AllpassDelay { seconds, gain } => {
                if *gain == 0.0 {
                    *seconds
                } else {
                    seconds * (0.001_f64.ln() / gain.abs().ln()).ceil().max(1.0)
                }
            }
            Op::Fdn { max_t60, .. } => *max_t60,
            Op::EnvelopeFollower { release, .. } => *release,
            Op::OnsetDetector { .. } => ONSET_PULSE_SECONDS,
            Op::Pluck {
                gain_per_second, ..
            } => (0.05_f64.ln() / gain_per_second.ln()).max(0.0),
            Op::GateEnv { release, .. } => *release,
            _ => 0.0,
        }
    }

    /// Warm-up window requested when replacing a retained analyser.
    ///
    /// This is not an audible voice tail. It describes recent-input state that
    /// should be rebuilt before an incompatible persistent analyser becomes
    /// authoritative. Exponential followers have infinite mathematical
    /// support, so their authored response time is the explicit practical
    /// convention rather than a claim of exact finite memory.
    pub fn warmup_seconds(&self) -> f64 {
        match self {
            Op::EnvelopeFollower { attack, release } => attack.max(*release),
            Op::PitchTracker { hold_seconds, .. } => *hold_seconds,
            Op::OnsetDetector { hold_seconds, .. } => hold_seconds.max(ONSET_PULSE_SECONDS),
            _ => 0.0,
        }
    }

    /// Whether this node begins nonterminating audio activity that a finite
    /// patch lifecycle must stop before downstream state can drain.
    pub(crate) fn begins_audio_activity(&self) -> bool {
        matches!(
            self,
            Op::Sine | Op::Cosine | Op::Saw | Op::Pulse | Op::Noise | Op::Pink
        )
    }
}

/// The two independent ways a graph may remain active past onset.
///
/// `gate_tail` is relative to scheduled release. `absolute_horizon` is an
/// onset-relative time supplied by a finite note-clock source. Keeping them
/// separate prevents a response tail on one parallel branch from being added
/// to a horizon on another.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Lifetime {
    pub gate_tail: f64,
    pub absolute_horizon: f64,
}

impl Lifetime {
    pub fn end_after_onset(self, gate: f64) -> f64 {
        (gate + self.gate_tail).max(self.absolute_horizon)
    }

    fn parallel(self, other: Lifetime) -> Lifetime {
        Lifetime {
            gate_tail: self.gate_tail.max(other.gate_tail),
            absolute_horizon: self.absolute_horizon.max(other.absolute_horizon),
        }
    }

    fn through_response(self, seconds: f64) -> Lifetime {
        Lifetime {
            gate_tail: self.gate_tail + seconds,
            absolute_horizon: if self.absolute_horizon > 0.0 {
                self.absolute_horizon + seconds
            } else {
                0.0
            },
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

    /// Whether this staged graph reads a particular per-note parameter.
    ///
    /// The language/runtime boundary uses this to distinguish fixed-frequency
    /// trigger voices from pitched voices without teaching the pattern crate
    /// any musical domain knowledge.
    pub fn uses_param(&self, target: ParamId) -> bool {
        let source_uses = |source: Source| matches!(source, Source::Param(id) if id == target);
        self.nodes
            .iter()
            .flat_map(|node| node.inputs.iter().map(|input| input.source))
            .chain(self.outputs.iter().copied())
            .chain(
                self.sends
                    .iter()
                    .flat_map(|send| send.outputs.iter().map(|input| input.source)),
            )
            .any(source_uses)
            || self.nodes.iter().any(|node| match node.op {
                Op::Pluck {
                    frequency, damping, ..
                } => {
                    matches!(frequency, InitScalar::Param(id) if id == target)
                        || matches!(damping, InitScalar::Param(id) if id == target)
                }
                Op::InitRandom {
                    ref min, ref max, ..
                } => init_expr_uses_param(min, target) || init_expr_uses_param(max, target),
                Op::BreakpointCurve { ref times, .. } => {
                    times.iter().any(|time| init_expr_uses_param(time, target))
                }
                Op::Slew { response_time, .. } => {
                    matches!(response_time, InitScalar::Param(id) if id == target)
                }
                Op::Portamento {
                    target: glide_target,
                    response_time,
                } => {
                    matches!(glide_target, InitScalar::Param(id) if id == target)
                        || matches!(response_time, InitScalar::Param(id) if id == target)
                }
                _ => false,
            })
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
            data_entries: self
                .nodes
                .iter()
                .map(|node| match &node.op {
                    Op::Curve(curve) => curve.terms.len(),
                    Op::BreakpointCurve { times, values, .. } => times.len() + values.len(),
                    Op::TransportSequence { slots, .. } => slots.len(),
                    Op::Fdn { delays, .. } => delays.len(),
                    _ => 0,
                })
                .sum(),
            delay_buffer_seconds: self
                .nodes
                .iter()
                .map(|node| match node.op {
                    Op::Delay(range) => range.max_seconds(),
                    // fundsp's stereo reverb owns 32 delay lines whose
                    // maximum propagation time is bounded by room diameter
                    // over the speed of sound.
                    Op::Reverb { room_size, .. } => 32.0 * room_size / 343.0,
                    Op::Limiter { attack, .. } => 2.0 * attack,
                    Op::Chorus {
                        separation,
                        variation,
                        ..
                    } => separation * 10.0 + variation * 5.0,
                    Op::FeedbackDelay { delay_seconds, .. } => delay_seconds,
                    Op::AllpassDelay { seconds, .. } => seconds,
                    Op::Fdn {
                        ref delays,
                        modulation_depth,
                        ..
                    } => delays.iter().sum::<f64>() + delays.len() as f64 * modulation_depth,
                    Op::Pluck {
                        max_delay_seconds, ..
                    } => max_delay_seconds,
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
        if cost.data_entries > limits.data_entries {
            return Err(GraphLimitError::DataEntries {
                found: cost.data_entries,
                limit: limits.data_entries,
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

    /// Gate-relative response and onset-relative activity reaching an output.
    ///
    /// Response tails add in series. At a parallel join the two components are
    /// maximised independently, so a delayed gate-bound branch cannot lend its
    /// delay to an unrelated long envelope. Feedback will require an explicit
    /// lifetime rule before it can enter this acyclic representation.
    pub fn lifetime(&self) -> Lifetime {
        self.lifetime_impl(None)
            .expect("static template lifetime does not bind parameter values")
    }

    /// Lifetime for one concrete event, including curve-valued parameters.
    pub fn lifetime_for(&self, note: &Note) -> Result<Lifetime, ParamValueError> {
        self.lifetime_impl(Some(note))
    }

    fn lifetime_impl(&self, note: Option<&Note>) -> Result<Lifetime, ParamValueError> {
        let mut through = vec![Lifetime::default(); self.nodes.len()];
        for (id, node) in self.nodes.iter().enumerate() {
            let upstream = node
                .inputs
                .iter()
                .enumerate()
                .filter(|(port, _)| node.op.activity_input(*port))
                .map(|(_, input)| match input.source {
                    Source::Port { node, .. } => Ok(through.get(node).copied().unwrap_or_default()),
                    source => self.source_lifetime(source, note),
                })
                .try_fold(Lifetime::default(), |lifetime, source| {
                    source.map(|source| lifetime.parallel(source))
                })?;
            let intrinsic = match &node.op {
                Op::Adsr(adsr) => Lifetime {
                    gate_tail: adsr.tail(),
                    absolute_horizon: 0.0,
                },
                Op::Decay { max_seconds } => Lifetime {
                    gate_tail: 0.0,
                    absolute_horizon: *max_seconds,
                },
                Op::Window { max_seconds } => Lifetime {
                    gate_tail: 0.0,
                    absolute_horizon: *max_seconds,
                },
                Op::Curve(curve) => match curve.activity() {
                    Ok(CurveActivity::Finite(horizon))
                        if curve.clock == CurveClock::NoteSeconds =>
                    {
                        Lifetime {
                            gate_tail: 0.0,
                            absolute_horizon: horizon,
                        }
                    }
                    Ok(CurveActivity::Finite(_)) => Lifetime {
                        gate_tail: 0.0,
                        absolute_horizon: 0.0,
                    },
                    Ok(CurveActivity::GateBounded) | Err(_) => Lifetime::default(),
                },
                Op::BreakpointCurve { horizon, .. } => match horizon {
                    BreakpointHorizon::Absolute(seconds) => Lifetime {
                        gate_tail: 0.0,
                        absolute_horizon: *seconds,
                    },
                    BreakpointHorizon::GateTail(seconds) => Lifetime {
                        gate_tail: *seconds,
                        absolute_horizon: 0.0,
                    },
                    BreakpointHorizon::GateBounded => Lifetime::default(),
                },
                Op::RunGate { active_seconds, .. } => Lifetime {
                    gate_tail: 0.0,
                    absolute_horizon: *active_seconds,
                },
                _ => upstream,
            };
            through[id] = intrinsic.through_response(node.tail.max(node.op.response_tail()));
        }
        self.outputs
            .iter()
            .chain(
                self.sends
                    .iter()
                    .flat_map(|send| send.outputs.iter().map(|input| &input.source)),
            )
            .map(|source| match source {
                Source::Port { node, .. } => Ok(through.get(*node).copied().unwrap_or_default()),
                source => self.source_lifetime(*source, note),
            })
            .try_fold(Lifetime::default(), |lifetime, source| {
                source.map(|source| lifetime.parallel(source))
            })
    }

    fn source_lifetime(
        &self,
        source: Source,
        note: Option<&Note>,
    ) -> Result<Lifetime, ParamValueError> {
        let Source::Param(id) = source else {
            return Ok(Lifetime::default());
        };
        let Some(note) = note else {
            let absolute_horizon = match id {
                ParamId::Declared(index) => self
                    .params
                    .get(index)
                    .and_then(|spec| spec.max_curve_seconds)
                    .unwrap_or(0.0),
                ParamId::Implicit(_) => 0.0,
            };
            return Ok(Lifetime {
                gate_tail: 0.0,
                absolute_horizon,
            });
        };
        match note.value(id, self)? {
            ParamValue::Curve(curve) => match curve
                .activity()
                .map_err(ParamValueError::InvalidCurve)?
            {
                CurveActivity::Finite(horizon) if curve.clock == CurveClock::NoteSeconds => {
                    Ok(Lifetime {
                        gate_tail: 0.0,
                        absolute_horizon: horizon,
                    })
                }
                CurveActivity::Finite(_) | CurveActivity::GateBounded => Ok(Lifetime::default()),
            },
            ParamValue::Number(_) => Ok(Lifetime::default()),
        }
    }

    /// Compatibility view used by publication budgets that do not know a
    /// particular note gate. It is the larger lifetime component, not a claim
    /// that both components compose as one additive tail.
    pub fn tail(&self) -> f64 {
        let lifetime = self.lifetime();
        lifetime.gate_tail.max(lifetime.absolute_horizon)
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
            if spec.name.is_empty()
                || spec.name == apteronotus_pattern::PRIMARY_FIELD
                || Implicit::ALL
                    .iter()
                    .any(|implicit| implicit.name() == spec.name)
            {
                return Err(TemplateError::InvalidParamName { param });
            }
            if self.params[..param]
                .iter()
                .any(|earlier| earlier.name == spec.name)
            {
                return Err(TemplateError::DuplicateParamName { param });
            }
            if !spec.min.is_finite()
                || !spec.max.is_finite()
                || !spec.default.is_finite()
                || spec.min > spec.max
                || !(spec.min..=spec.max).contains(&spec.default)
                || spec
                    .max_curve_seconds
                    .is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0)
            {
                return Err(TemplateError::InvalidParamRange { param });
            }
        }
        for (id, node) in self.nodes.iter().enumerate() {
            if let Op::Curve(curve) = &node.op {
                curve
                    .validate()
                    .map_err(|source| TemplateError::InvalidCurve { node: id, source })?;
            }
            let valid_effect = match node.op {
                Op::Reverb {
                    room_size,
                    time,
                    damping,
                } => {
                    room_size.is_finite()
                        && room_size > 0.0
                        && time.is_finite()
                        && time >= 0.0
                        && damping.is_finite()
                        && (0.0..=1.0).contains(&damping)
                }
                Op::Limiter { attack, release } => {
                    attack.is_finite() && attack >= 0.0 && release.is_finite() && release >= 0.0
                }
                Op::Clamp { min, max } => min.is_finite() && max.is_finite() && min <= max,
                Op::Decay { max_seconds } => max_seconds.is_finite() && max_seconds > 0.0,
                Op::Window { max_seconds } => max_seconds.is_finite() && max_seconds >= 0.0,
                Op::BreakpointCurve {
                    ref times,
                    ref values,
                    horizon,
                } => {
                    !times.is_empty()
                        && times.len() == values.len()
                        && values.iter().all(|value| value.is_finite())
                        && times.iter().all(|time| valid_init_expr(time, &self.params))
                        && match horizon {
                            BreakpointHorizon::Absolute(seconds)
                            | BreakpointHorizon::GateTail(seconds) => {
                                seconds.is_finite() && seconds >= 0.0
                            }
                            BreakpointHorizon::GateBounded => true,
                        }
                }
                Op::Chorus {
                    separation,
                    variation,
                    frequency,
                    ..
                } => {
                    separation.is_finite()
                        && separation >= 0.0
                        && variation.is_finite()
                        && variation >= 0.0
                        && frequency.is_finite()
                        && frequency > 0.0
                }
                Op::FeedbackDelay {
                    delay_seconds,
                    cutoff_q,
                    amount,
                } => {
                    delay_seconds.is_finite()
                        && delay_seconds > 0.0
                        && amount.is_finite()
                        && amount.abs() > 0.0
                        && amount.abs() < 1.0
                        && cutoff_q.is_none_or(|(cutoff, q)| {
                            cutoff.is_finite() && cutoff > 0.0 && q.is_finite() && q > 0.0
                        })
                }
                Op::AllpassDelay { seconds, gain } => {
                    seconds.is_finite() && seconds > 0.0 && gain.is_finite() && gain.abs() < 1.0
                }
                Op::Fdn {
                    ref delays,
                    damping,
                    modulation_rate,
                    modulation_depth,
                    max_t60,
                } => {
                    !delays.is_empty()
                        && delays.len().is_power_of_two()
                        && delays.len() <= 32
                        && delays.iter().all(|delay| {
                            delay.is_finite() && *delay > modulation_depth && *delay > 0.0
                        })
                        && damping.is_finite()
                        && (0.0..1.0).contains(&damping)
                        && modulation_rate.is_finite()
                        && modulation_rate >= 0.0
                        && modulation_depth.is_finite()
                        && modulation_depth >= 0.0
                        && max_t60.is_finite()
                        && max_t60 > 0.0
                }
                Op::EnvelopeFollower { attack, release } => {
                    attack.is_finite() && attack >= 0.0 && release.is_finite() && release >= 0.0
                }
                Op::PitchTracker {
                    min_hz,
                    max_hz,
                    default_hz,
                    hold_seconds,
                } => {
                    min_hz.is_finite()
                        && min_hz > 0.0
                        && max_hz.is_finite()
                        && max_hz >= min_hz
                        && default_hz.is_finite()
                        && (min_hz..=max_hz).contains(&default_hz)
                        && hold_seconds.is_finite()
                        && hold_seconds >= 0.0
                }
                Op::OnsetDetector {
                    floor,
                    hold_seconds,
                } => {
                    floor.is_finite()
                        && floor > 0.0
                        && hold_seconds.is_finite()
                        && hold_seconds >= 0.0
                }
                Op::TransportSequence {
                    period_seconds,
                    ref slots,
                } => {
                    period_seconds.is_finite()
                        && period_seconds > 0.0
                        && !slots.is_empty()
                        && slots.iter().all(TransportSlot::valid)
                        && slots
                            .windows(2)
                            .all(|pair| pair[0].end_seconds == pair[1].begin_seconds)
                        && slots[0].begin_seconds == 0.0
                        && slots
                            .last()
                            .is_some_and(|slot| slot.end_seconds == period_seconds)
                }
                Op::Width => true,
                Op::Pluck {
                    frequency,
                    gain_per_second,
                    damping,
                    max_delay_seconds,
                } => {
                    init_scalar_valid(frequency, &self.params)
                        && init_scalar_valid(damping, &self.params)
                        && gain_per_second.is_finite()
                        && gain_per_second > 0.0
                        && gain_per_second < 1.0
                        && max_delay_seconds.is_finite()
                        && max_delay_seconds > 0.0
                }
                Op::Slew {
                    response_time,
                    initial,
                } => init_scalar_valid(response_time, &self.params) && initial.is_finite(),
                Op::GateEnv {
                    attack,
                    decay,
                    sustain,
                    release,
                } => {
                    attack.is_finite()
                        && attack >= 0.0
                        && decay.is_finite()
                        && decay >= 0.0
                        && sustain.is_finite()
                        && (0.0..=1.0).contains(&sustain)
                        && release.is_finite()
                        && release >= 0.0
                }
                Op::Portamento {
                    target,
                    response_time,
                } => {
                    init_scalar_valid(target, &self.params)
                        && init_scalar_valid(response_time, &self.params)
                }
                Op::RunGate {
                    active_seconds,
                    fade_seconds,
                } => {
                    active_seconds.is_finite()
                        && active_seconds > 0.0
                        && fade_seconds.is_finite()
                        && fade_seconds >= 0.0
                        && fade_seconds * 2.0 <= active_seconds
                }
                _ => true,
            };
            if !valid_effect {
                return Err(TemplateError::InvalidEffect { node: id });
            }
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
            Source::ExternalAudio { .. } => Ok(()),
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

fn init_scalar_valid(value: InitScalar, params: &[ParamSpec]) -> bool {
    match value {
        InitScalar::Const(value) => value.is_finite(),
        InitScalar::Param(ParamId::Implicit(_)) => true,
        InitScalar::Param(ParamId::Declared(index)) => index < params.len(),
    }
}

fn valid_init_expr(value: &InitExpr, params: &[ParamSpec]) -> bool {
    match value {
        InitExpr::Const(value) => value.is_finite(),
        InitExpr::Param(ParamId::Implicit(_)) => true,
        InitExpr::Param(ParamId::Declared(index)) => *index < params.len(),
        InitExpr::Add(left, right)
        | InitExpr::Sub(left, right)
        | InitExpr::Mul(left, right)
        | InitExpr::Div(left, right) => {
            valid_init_expr(left, params) && valid_init_expr(right, params)
        }
        InitExpr::Neg(inner) => valid_init_expr(inner, params),
    }
}

fn init_expr_uses_param(value: &InitExpr, target: ParamId) -> bool {
    match value {
        InitExpr::Const(_) => false,
        InitExpr::Param(id) => *id == target,
        InitExpr::Add(left, right)
        | InitExpr::Sub(left, right)
        | InitExpr::Mul(left, right)
        | InitExpr::Div(left, right) => {
            init_expr_uses_param(left, target) || init_expr_uses_param(right, target)
        }
        InitExpr::Neg(inner) => init_expr_uses_param(inner, target),
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
    InvalidParamName {
        param: usize,
    },
    DuplicateParamName {
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
    InvalidCurve {
        node: NodeId,
        source: apteronotus_pattern::CurveError,
    },
    InvalidEffect {
        node: NodeId,
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
            TemplateError::InvalidParamName { param } => {
                write!(f, "parameter {param} has an empty or reserved name")
            }
            TemplateError::DuplicateParamName { param } => {
                write!(f, "parameter {param} duplicates an earlier name")
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
            TemplateError::InvalidCurve { node, source } => {
                write!(f, "node {node} has an invalid curve: {source}")
            }
            TemplateError::InvalidEffect { node } => {
                write!(f, "node {node} has invalid effect parameters")
            }
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
    /// Flat data stored by bounded table-driven nodes such as transport
    /// sequences, curves, breakpoint envelopes, and FDN delay layouts.
    pub data_entries: usize,
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
    pub data_entries: usize,
    pub delay_buffer_seconds: f64,
    pub tail_seconds: f64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GraphLimitError {
    Nodes { found: usize, limit: usize },
    Connections { found: usize, limit: usize },
    InputChannels { found: usize, limit: usize },
    OutputChannels { found: usize, limit: usize },
    DataEntries { found: usize, limit: usize },
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
            GraphLimitError::DataEntries { found, limit } => write!(
                f,
                "graph stores {found} table entries, more than the limit of {limit}"
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
