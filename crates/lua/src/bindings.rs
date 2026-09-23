use crate::{
    ExternalControlBinding, ExternalDegrade, Limits, PatchControl, PatchControlKind, Program,
    Track, VoiceId,
};
use apteronotus_music::{Chord, Key, Pitch, VoicingShape};
use apteronotus_pattern::{
    ArpMode, Basis, ControlValue, Curve, CurveClock, Frac, GroupNode, Pattern, PatternMathOp,
    RangeProof, Signal as PatternSignal, Span, SrcSpan, Timeline, TimelineEvent, TimelineId,
    Value as PatternValue, mini,
};
use apteronotus_synth::{
    Adsr, AudioInputId, AudioInputSpec, BusId, ControlId, ControlSpec, DelayRange, EventRouting,
    FdnConfig, GraphBuilder, Implicit, InitControlBinding, Note, ParamId, ParamSpec, ParamValue,
    PatchTemplate, ShapeKind, Source, TransportSlot, n, stdlib,
};
use apteronotus_transport::{TempoMap, TempoPoint};
use piccolo::{
    Callback, CallbackReturn, Context, Error, IntoValue, MetaMethod, Table, UserData, Value,
};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

const INTERNAL_CONTROL_PREFIX: &str = "__apteronotus.";

pub(crate) const PRELUDE: &str = r#"
-- Piccolo intentionally ships a very small table library. These are ordinary
-- Lua so they exercise the same fuel budget as user code and remain identical
-- on native and wasm.
function table.insert(t, position, value)
  if value == nil then
    value = position
    position = #t + 1
  end
  if position < 1 or position > #t + 1 then
    error("position out of bounds")
  end
  for i = #t, position, -1 do
    t[i + 1] = t[i]
  end
  t[position] = value
end

function table.remove(t, position)
  local length = #t
  position = position or length
  if position < 1 or position > length then
    return nil
  end
  local value = t[position]
  for i = position, length - 1 do
    t[i] = t[i + 1]
  end
  t[length] = nil
  return value
end

function table.concat(t, separator, first, last)
  separator = separator or ""
  first = first or 1
  last = last or #t
  local result = ""
  for i = first, last do
    if i > first then result = result .. separator end
    result = result .. t[i]
  end
  return result
end

function table.sort(t, before)
  before = before or function(a, b) return a < b end
  -- Insertion sort is compact and deterministic. Evaluation fuel bounds its
  -- quadratic worst case, and authored topology tables are normally tiny.
  for i = 2, #t do
    local value = t[i]
    local j = i
    while j > 1 and before(value, t[j - 1]) do
      t[j] = t[j - 1]
      j = j - 1
    end
    t[j] = value
  end
end

-- Unit-preserving graph/pattern helpers. Operator metamethods decide whether
-- these build symbolic graph arithmetic or ordinary numbers.
function exp2(x)
  return 2 ^ x
end

function semitones(x)
  return exp2(x / 12)
end

function note_hz(note)
  return 440 * exp2((note - 69) / 12)
end

function ringmod(modulator, amount)
  return mul(1 + amount * (modulator - 1))
end

function tremolo(rate, depth)
  return mul(1 - depth + depth * sine(rate))
end

local function checked_table(value, name)
  if type(value) ~= "table" then
    error(name .. " expects a table")
  end
  return value
end

function voice(spec)
  checked_table(spec, "voice")
  if type(spec.graph) ~= "function" then
    error("voice.graph must be a function")
  end
  local note = __begin_voice(spec.params)
  local ok, output = pcall(spec.graph, note)
  if not ok then
    __abort_voice()
    error(output)
  end
  return __finish_voice(output)
end

function patch(spec)
  checked_table(spec, "patch")
  if type(spec.graph) ~= "function" then
    error("patch.graph must be a function")
  end
  local controls = __begin_patch(spec.inputs, spec.params, spec.controls)
  local ok, output = pcall(spec.graph, controls)
  if not ok then
    __abort_voice()
    error(output)
  end
  return __finish_patch(output)
end
"#;

pub(crate) struct BuildState {
    pub program: Program,
    pub active: Option<ActiveGraph>,
    generation: u64,
    graph_nodes: usize,
    pattern_nodes: usize,
    bus_count: usize,
    tempo_declared: bool,
    timeline_count: u64,
    pending_sends: Vec<PendingSend>,
    pending_master: Option<Processor>,
    shared_graph: GraphBuilder,
    shared_writes: Vec<Source>,
    shared_signal_count: usize,
    limits: Limits,
}

pub(crate) struct ActiveGraph {
    graph: GraphBuilder,
    generation: u64,
    lifetime: ActiveLifetime,
    input_channels: usize,
    patch_controls: Vec<PatchControl>,
}

#[derive(Clone, Copy, Debug)]
enum ActiveLifetime {
    Voice,
    Patch,
}

#[derive(Clone, Debug)]
struct LuaSource {
    generation: u64,
    channels: Vec<Source>,
}

/// An ordered port frame built with `|`.
///
/// This is deliberately distinct from a multichannel signal: a stereo signal
/// is one value with two output channels, while `(audio | cutoff | q)` is
/// three inputs waiting to be connected to a processor.
#[derive(Clone, Debug)]
struct LuaBundle {
    generation: u64,
    inputs: Vec<Source>,
}

/// Evaluation-only patch topology. Applying one of these immediately emits
/// ordinary data-only `GraphTemplate` nodes; the value itself never survives
/// evaluation.
#[derive(Clone, Debug)]
struct LuaProcessor {
    generation: u64,
    processor: Processor,
}

#[derive(Clone, Debug)]
enum Processor {
    Sine,
    Cosine,
    Saw,
    Triangle,
    Pulse,
    Pluck {
        frequency: Source,
        gain_per_second: f64,
        damping: Source,
    },
    Filter {
        kind: FilterKind,
        cutoff_q: Option<(Source, Source)>,
    },
    Shape {
        kind: ShapeKind,
        amount: Source,
    },
    DcBlock,
    Delay {
        seconds: Source,
        range: DelayRange,
    },
    /// A fixed Schroeder all-pass delay used to assemble diffusion networks.
    AllpassDelay {
        seconds: f64,
        gain: f64,
    },
    Fdn {
        delays: Vec<f64>,
        damping: f64,
        modulation_rate: f64,
        modulation_depth: f64,
        t60: ProcessorScalar,
    },
    EnvelopeFollower {
        attack: f64,
        release: f64,
    },
    PitchTracker {
        min_hz: f64,
        max_hz: f64,
        default_hz: f64,
        hold_seconds: f64,
    },
    OnsetDetector {
        floor: f64,
        hold_seconds: f64,
    },
    Width {
        amount: Source,
    },
    Reverb {
        room_size: f64,
        time: f64,
        damping: f64,
    },
    Limiter {
        attack: f64,
        release: f64,
    },
    Chorus {
        seed: u64,
        separation: f64,
        variation: f64,
        frequency: f64,
    },
    /// Construction-only marker. `pipe` consumes it together with the
    /// preceding supported loop body, so no ambiguous cyclic graph reaches
    /// the data-only synth IR.
    Feedback {
        amount: f64,
    },
    FeedbackDelay {
        delay_seconds: f64,
        cutoff_q: Option<(f64, f64)>,
        amount: f64,
    },
    Slew {
        response_time: Source,
    },
    Mul(Source),
    Ring {
        hz: Source,
        decay: Source,
    },
    Pan(Source),
    Send {
        bus: BusId,
        channels: usize,
        level: Source,
    },
    Chain(Box<Processor>, Box<Processor>),
    Stack(Box<Processor>, Box<Processor>),
    Sum(Box<Processor>, Box<Processor>),
    Product(Box<Processor>, Box<Processor>),
    Scale(Box<Processor>, Source),
    Branch(Box<Processor>, Box<Processor>),
}

#[derive(Clone, Debug)]
enum ProcessorScalar {
    Bound { source: Source, max: f64 },
    SendParam(String),
}

#[derive(Clone, Debug)]
struct PendingSend {
    bus: BusId,
    processor: Processor,
    level: f64,
}

#[derive(Clone, Copy, Debug)]
enum FilterKind {
    Lowpass,
    Highpass,
    Bandpass,
    Peak,
    Moog,
}

#[derive(Clone, Debug)]
struct LuaPattern {
    pattern: Pattern,
    controls: Vec<PatternControl>,
    routing: EventRouting,
    /// A realtime trigger cannot be queried through the pure pattern AST.
    /// Its fallback pattern is silence; this marker keeps the live identity
    /// owned for the host's minimum-latency trigger path.
    external_trigger: Option<LuaExternalTrigger>,
    /// Absolute note duration awaiting placement through a TempoMap.
    ///
    /// Cycle/beats holds live directly in `Pattern::Hold`. Seconds cannot:
    /// the dependency-free pattern crate deliberately has no transport clock.
    hold_seconds: Option<f64>,
}

#[derive(Clone, Debug)]
struct LuaExternalTrigger {
    control: ControlId,
    degrades: Vec<ExternalDegrade>,
}

/// A literal music-theory value that exists only during evaluation.
///
/// It becomes an ordinary grouped numeric pattern when `voicing(...)` is
/// applied. Patterned chord symbols need a distinct query-time design and are
/// intentionally not accepted by this construction-time builder.
#[derive(Clone, Debug)]
struct LuaChord {
    chord: Chord,
    anchor: Option<Pitch>,
    call_site: u64,
}

#[derive(Clone, Copy, Debug)]
struct LuaAnchor(Pitch);

#[derive(Clone, Copy, Debug)]
struct LuaVoicing(VoicingShape);

/// One finite-placement request held only while evaluating `timeline { ... }`.
#[derive(Clone, Debug)]
struct LuaPlacement {
    start: Frac,
    capture_cycles: Frac,
    pattern: LuaPattern,
}

#[derive(Clone, Copy, Debug)]
struct LuaSpan(Span);

/// An evaluation-only structural pattern transformation.
///
/// This is data, not a Lua callback: applying it immediately builds ordinary
/// pattern AST nodes, so no host-language closure reaches pattern queries.
#[derive(Clone, Debug)]
enum PatternTransform {
    Fast(Frac),
    Slow(Frac),
    Shift(Frac),
    Rev,
    Degrade {
        amount: f64,
        seed: u64,
    },
    Segment(i64),
    Range {
        min: f64,
        max: f64,
    },
    Every {
        cycles: i64,
        transform: Box<PatternTransform>,
    },
    Off {
        by: Frac,
        transform: Box<PatternTransform>,
    },
    Sometimes {
        amount: f64,
        seed: u64,
        transform: Box<PatternTransform>,
    },
    Ply {
        counts: Vec<u32>,
        seed: u64,
    },
    Arp {
        mode: ArpMode,
        spacing: Option<Frac>,
    },
    MergeControl {
        pattern: Pattern,
        controls: Vec<PatternControl>,
    },
    GroupPrimary,
    PrimaryAdd(f64),
}

impl PatternTransform {
    fn apply(&self, pattern: Pattern) -> Result<Pattern, String> {
        Ok(match self {
            Self::Fast(factor) => pattern.fast(*factor),
            Self::Slow(factor) => pattern.slow(*factor),
            Self::Shift(by) => pattern.late(*by),
            Self::Rev => pattern.rev(),
            Self::Degrade { amount, seed } => pattern.degrade_by(*amount, *seed),
            Self::Segment(steps) => pattern.segment(*steps),
            Self::Range { min, max } => pattern.range(*min, *max),
            Self::Every { cycles, transform } => {
                let transformed = transform.apply(pattern.clone())?;
                Pattern::When {
                    modulo: *cycles,
                    offset: 0,
                    then: Box::new(transformed),
                    otherwise: Box::new(pattern),
                }
            }
            Self::Off { by, transform } => {
                let transformed = transform.apply(pattern.clone())?.late(*by);
                Pattern::stack(vec![pattern, transformed])
            }
            Self::Sometimes {
                amount,
                seed,
                transform,
            } => {
                let untouched = pattern.clone().degrade_by(*amount, *seed);
                let touched = transform.apply(pattern.undegrade_by(*amount, *seed))?;
                Pattern::stack(vec![untouched, touched])
            }
            Self::Ply { counts, seed } => pattern.ply(counts.clone(), *seed),
            Self::Arp { mode, spacing } => match spacing {
                Some(spacing) => pattern.arp_spaced(*mode, *spacing),
                None => pattern.arp(*mode),
            },
            Self::MergeControl {
                pattern: controls, ..
            } => pattern
                .merge(controls.clone())
                .map_err(|error| error.to_string())?,
            Self::GroupPrimary => pattern.group_primary(),
            Self::PrimaryAdd(amount) => pattern
                .add_to_primary(*amount)
                .map_err(|error| error.to_string())?,
        })
    }

    fn append_controls(&self, output: &mut Vec<PatternControl>) {
        match self {
            PatternTransform::Every { transform, .. }
            | PatternTransform::Off { transform, .. }
            | PatternTransform::Sometimes { transform, .. } => {
                transform.append_controls(output);
            }
            PatternTransform::MergeControl { controls, .. } => {
                output.extend(controls.clone());
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
struct LuaPatternTransform(PatternTransform);

#[derive(Clone, Copy, Debug)]
struct LuaHold(LuaDuration);

#[derive(Clone, Debug)]
struct LuaEventSend(EventRouting);

#[derive(Clone, Debug)]
struct PatternControl {
    name: String,
    /// `None` means a pattern-rate value whose exact range is known only at
    /// its sampled onset. The name is still checked against the target voice.
    value: Option<ControlValue>,
    /// A program-scope signal that must remain live instead of being sampled
    /// into an init-rate event value.
    live: Option<ControlId>,
    /// A program control sampled only when a realtime trigger arrives.
    onset: Option<ControlId>,
    /// Unnamed numeric/curve pattern sampled at an arrived external onset.
    sample: Option<Pattern>,
}

#[derive(Clone, Debug)]
struct LuaCurve(Curve);

#[derive(Clone, Copy, Debug)]
struct LuaPhase(f64);

#[derive(Clone, Copy, PartialEq, Debug)]
enum TimeUnit {
    Seconds,
    Cycles,
    Beats {
        beats_per_cycle: f64,
        seconds_per_beat: Option<f64>,
    },
}

#[derive(Clone, Copy, Debug)]
struct LuaDuration {
    value: f64,
    unit: TimeUnit,
}

#[derive(Clone, Copy, Debug)]
struct LuaVoice(usize);

#[derive(Clone, Copy, Debug)]
struct LuaPatch(usize);

#[derive(Clone, Copy, Debug)]
struct LuaControl(ControlId);

#[derive(Clone, Debug)]
struct LuaParamRef(String);

#[derive(Clone, Debug)]
struct LuaPatchControlSpec {
    min: f64,
    max: f64,
    default: f64,
    unit: Option<String>,
    kind: PatchControlKind,
}

#[derive(Clone, Copy, Debug)]
struct LuaAudioInput {
    id: AudioInputId,
    channels: usize,
}

/// One program-scope derived signal.
///
/// `source` belongs to the single persistent signal arena. `control` is its
/// fan-out boundary into independently instantiated voice and patch graphs.
#[derive(Clone, Copy, Debug)]
struct LuaSharedSignal {
    source: Source,
    control: ControlId,
    min: f64,
    max: f64,
    default: f64,
}

#[derive(Clone, Copy, Debug)]
struct LuaAtOnset(LuaSharedSignal);

#[derive(Clone, Copy, Debug)]
struct LuaBus {
    id: BusId,
    channels: usize,
}

/// Capability token accepted by `init_rand`, not a graph-rate source.
///
/// Making this userdata instead of `Source::Param` prevents authored
/// arithmetic from confusing deterministic event identity with a numeric voice
/// handle or live signal.
#[derive(Clone, Copy, Debug)]
struct LuaEventSeed;

impl BuildState {
    pub fn new(limits: Limits) -> Self {
        Self {
            program: Program::default(),
            active: None,
            generation: 0,
            graph_nodes: 0,
            pattern_nodes: 0,
            bus_count: 0,
            tempo_declared: false,
            timeline_count: 0,
            pending_sends: Vec::new(),
            pending_master: None,
            shared_graph: GraphBuilder::new(),
            shared_writes: Vec::new(),
            shared_signal_count: 0,
            limits,
        }
    }

    fn active_mut<'gc>(&mut self, ctx: Context<'gc>) -> Result<&mut ActiveGraph, Error<'gc>> {
        self.active
            .as_mut()
            .ok_or_else(|| binding_error(ctx, "graph primitive used outside voice.graph"))
    }

    fn spend_node<'gc>(&mut self, ctx: Context<'gc>) -> Result<(), Error<'gc>> {
        if self.graph_nodes >= self.limits.graph_nodes {
            return Err(binding_error(
                ctx,
                format!("graph node limit of {} exceeded", self.limits.graph_nodes),
            ));
        }
        self.graph_nodes += 1;
        Ok(())
    }

    fn spend_pattern<'gc>(
        &mut self,
        ctx: Context<'gc>,
        pattern: &Pattern,
    ) -> Result<(), Error<'gc>> {
        pattern
            .validate_value_limits(self.limits.pattern_values)
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        self.pattern_nodes = self.pattern_nodes.saturating_add(pattern_nodes(pattern));
        if self.pattern_nodes > self.limits.pattern_nodes {
            return Err(binding_error(
                ctx,
                format!(
                    "pattern node limit of {} exceeded",
                    self.limits.pattern_nodes
                ),
            ));
        }
        Ok(())
    }
}

pub(crate) fn install<'gc>(
    ctx: Context<'gc>,
    state: Rc<RefCell<BuildState>>,
) -> Result<(), Error<'gc>> {
    lock_down(ctx)?;

    set_callback(ctx, "__begin_voice", state.clone(), begin_voice)?;
    set_callback(ctx, "__abort_voice", state.clone(), abort_voice)?;
    set_callback(ctx, "__finish_voice", state.clone(), finish_voice)?;
    set_callback(ctx, "__begin_patch", state.clone(), begin_patch)?;
    set_callback(ctx, "__finish_patch", state.clone(), finish_patch)?;
    set_callback(ctx, "control", state.clone(), control)?;
    set_callback(ctx, "note_control", state.clone(), note_control)?;
    set_callback(ctx, "gate_control", state.clone(), gate_control)?;
    set_callback(ctx, "control_input", state.clone(), control_input)?;
    set_callback(ctx, "audio_input", state.clone(), audio_input)?;
    set_callback(ctx, "param", state.clone(), param_ref)?;
    set_callback(ctx, "bus", state.clone(), bus)?;
    set_callback(ctx, "send", state.clone(), send_return)?;
    set_callback(ctx, "run", state.clone(), run)?;
    set_callback(ctx, "master", state.clone(), master)?;
    set_callback(ctx, "secs", state.clone(), seconds)?;
    set_callback(ctx, "ms", state.clone(), milliseconds)?;
    set_callback(ctx, "bars", state.clone(), bars)?;
    set_callback(ctx, "beats", state.clone(), beats)?;
    set_callback(ctx, "tempo", state.clone(), tempo)?;
    set_callback(ctx, "key", state.clone(), key)?;
    set_callback(ctx, "at", state.clone(), at)?;
    set_callback(ctx, "span", state.clone(), finite_span)?;
    set_callback(ctx, "timeline", state.clone(), timeline)?;
    set_callback(ctx, "pattern", state.clone(), pattern)?;
    set_callback(ctx, "__pattern_at", state.clone(), pattern_at)?;
    set_callback(ctx, "play", state.clone(), play)?;
    set_callback(ctx, "__play_at", state.clone(), play_at)?;
    set_callback(ctx, "fast", state.clone(), fast_pattern)?;
    set_callback(ctx, "slow", state.clone(), slow_pattern)?;
    set_callback(ctx, "shift", state.clone(), shift_pattern)?;
    set_callback(ctx, "late", state.clone(), shift_pattern)?;
    set_callback(ctx, "early", state.clone(), early_pattern)?;
    set_callback(ctx, "segment", state.clone(), segment_pattern)?;
    set_callback(ctx, "control_signal", state.clone(), control_signal)?;
    set_callback(ctx, "at_onset", state.clone(), at_onset)?;
    set_callback(ctx, "range", state.clone(), range_pattern)?;
    set_callback(ctx, "degrade", state.clone(), degrade_pattern)?;
    set_callback(ctx, "__degrade_at", state.clone(), degrade_pattern_at)?;
    set_callback(ctx, "every", state.clone(), every_pattern)?;
    set_callback(ctx, "off", state.clone(), off_pattern)?;
    set_callback(ctx, "sometimes", state.clone(), sometimes_pattern)?;
    set_callback(ctx, "__sometimes_at", state.clone(), sometimes_pattern_at)?;
    set_callback(ctx, "ply", state.clone(), ply_pattern)?;
    set_callback(ctx, "__ply_at", state.clone(), ply_pattern_at)?;
    set_callback(ctx, "arp", state.clone(), arp_pattern)?;
    set_callback(ctx, "chord", state.clone(), chord)?;
    set_callback(ctx, "__chord_at", state.clone(), chord_at)?;
    set_callback(ctx, "anchor", state.clone(), anchor)?;
    set_callback(ctx, "voicing", state.clone(), voicing)?;
    set_callback(ctx, "root_notes", state.clone(), root_notes)?;
    set_callback(ctx, "octave", state.clone(), octave)?;
    set_callback(ctx, "hold", state.clone(), hold)?;
    set_callback(ctx, "phase", state.clone(), phase)?;
    set_callback(ctx, "curve", state.clone(), event_curve)?;
    set_callback(ctx, "note", state.clone(), note_pattern)?;
    set_callback(ctx, "velocity", state.clone(), velocity)?;
    set_callback(ctx, "scale", state.clone(), scale)?;
    set_callback(ctx, "perlin", state.clone(), perlin)?;
    set_callback(ctx, "__perlin_at", state.clone(), perlin_at)?;
    set_callback(ctx, "cosine", state.clone(), cosine)?;
    set_callback(ctx, "__cosine_at", state.clone(), cosine_at)?;

    set_callback(ctx, "sine", state.clone(), sine)?;
    set_callback(ctx, "__sine_at", state.clone(), sine_at)?;
    set_callback(ctx, "dc", state.clone(), dc)?;
    set_callback(ctx, "zero", state.clone(), zero)?;
    set_callback(ctx, "saw", state.clone(), saw)?;
    set_callback(ctx, "soft_saw", state.clone(), soft_saw)?;
    set_callback(ctx, "__saw_at", state.clone(), saw_at)?;
    set_callback(ctx, "pulse", state.clone(), pulse)?;
    set_callback(ctx, "triangle", state.clone(), triangle)?;
    set_callback(ctx, "noise", state.clone(), noise)?;
    set_callback(ctx, "pink", state.clone(), pink)?;
    set_callback(ctx, "impulse", state.clone(), impulse)?;
    set_callback(ctx, "init_random", state.clone(), init_random)?;
    set_callback(ctx, "init_rand", state.clone(), init_random)?;
    set_callback(ctx, "rand", state.clone(), rand)?;
    set_callback(ctx, "__rand_at", state.clone(), rand_at)?;
    set_callback(ctx, "lowpass", state.clone(), lowpass)?;
    set_callback(ctx, "highpass", state.clone(), highpass)?;
    set_callback(ctx, "bandpass", state.clone(), bandpass)?;
    set_callback(ctx, "peak", state.clone(), peak)?;
    set_callback(ctx, "moog", state.clone(), moog)?;
    set_callback(ctx, "pluck", state.clone(), pluck)?;
    set_callback(ctx, "shape", state.clone(), shape)?;
    set_callback(ctx, "dcblock", state.clone(), dcblock)?;
    set_callback(ctx, "delay", state.clone(), delay)?;
    set_callback(ctx, "predelay", state.clone(), predelay)?;
    set_callback(ctx, "diffuse", state.clone(), diffuse)?;
    set_callback(ctx, "fdn", state.clone(), fdn)?;
    set_callback(ctx, "envelope_follower", state.clone(), envelope_follower)?;
    set_callback(ctx, "pitch_tracker", state.clone(), pitch_tracker)?;
    set_callback(ctx, "onset_detector", state.clone(), onset_detector)?;
    set_callback(ctx, "width", state.clone(), width)?;
    set_callback(ctx, "reverb", state.clone(), reverb)?;
    set_callback(ctx, "limiter", state.clone(), limiter)?;
    set_callback(ctx, "chorus", state.clone(), chorus)?;
    set_callback(ctx, "ensemble", state.clone(), ensemble)?;
    set_callback(ctx, "feedback", state.clone(), feedback_delay)?;
    set_callback(ctx, "slew", state.clone(), slew)?;
    set_callback(ctx, "gate_env", state.clone(), gate_env)?;
    set_callback(ctx, "string_resonator", state.clone(), string_resonator)?;
    set_callback(ctx, "add", state.clone(), add)?;
    set_callback(ctx, "sub", state.clone(), sub)?;
    set_callback(ctx, "mul", state.clone(), mul)?;
    set_callback(ctx, "div", state.clone(), div)?;
    set_callback(ctx, "clamp", state.clone(), clamp)?;
    set_callback(ctx, "neg", state.clone(), neg)?;
    set_callback(ctx, "mix", state.clone(), mix)?;
    set_callback(ctx, "flue_pipe", state.clone(), flue_pipe)?;
    set_callback(ctx, "harmonics", state.clone(), harmonics)?;
    set_callback(ctx, "adsr", state.clone(), adsr)?;
    set_callback(ctx, "step", state.clone(), step)?;
    set_callback(ctx, "__step_at", state.clone(), step_at)?;
    set_callback(ctx, "ramp", state.clone(), ramp)?;
    set_callback(ctx, "line", state.clone(), line)?;
    set_callback(ctx, "__line_at", state.clone(), line_at)?;
    set_callback(ctx, "decay", state.clone(), decay)?;
    set_callback(ctx, "window", state.clone(), window)?;
    set_callback(ctx, "__window_at", state.clone(), window_at)?;
    set_callback(ctx, "ring", state.clone(), ring)?;
    set_callback(ctx, "pan", state.clone(), pan)?;
    set_callback(ctx, "to", state.clone(), to)?;
    set_callback(ctx, "duck", state.clone(), duck)?;

    let source_meta = Table::new(&ctx);
    set_operator(ctx, source_meta, MetaMethod::Add, state.clone(), add)?;
    set_operator(ctx, source_meta, MetaMethod::Sub, state.clone(), sub)?;
    set_operator(ctx, source_meta, MetaMethod::Mul, state.clone(), mul)?;
    set_operator(ctx, source_meta, MetaMethod::Div, state.clone(), div)?;
    set_operator(ctx, source_meta, MetaMethod::Pow, state.clone(), pow)?;
    set_operator(ctx, source_meta, MetaMethod::Unm, state.clone(), neg)?;
    set_operator(
        ctx,
        source_meta,
        MetaMethod::BOr,
        state.clone(),
        stack_ports,
    )?;
    set_operator(ctx, source_meta, MetaMethod::Shr, state.clone(), pipe)?;
    ctx.set_global("__apteronotus_source_meta", source_meta);

    let bundle_meta = Table::new(&ctx);
    set_operator(
        ctx,
        bundle_meta,
        MetaMethod::BOr,
        state.clone(),
        stack_ports,
    )?;
    set_operator(ctx, bundle_meta, MetaMethod::Shr, state.clone(), pipe)?;
    ctx.set_global("__apteronotus_bundle_meta", bundle_meta);

    let processor_meta = Table::new(&ctx);
    set_operator(
        ctx,
        processor_meta,
        MetaMethod::Add,
        state.clone(),
        processor_add,
    )?;
    set_operator(
        ctx,
        processor_meta,
        MetaMethod::Sub,
        state.clone(),
        processor_sub,
    )?;
    set_operator(
        ctx,
        processor_meta,
        MetaMethod::Mul,
        state.clone(),
        processor_mul,
    )?;
    set_operator(
        ctx,
        processor_meta,
        MetaMethod::BOr,
        state.clone(),
        processor_stack,
    )?;
    set_operator(
        ctx,
        processor_meta,
        MetaMethod::BAnd,
        state.clone(),
        processor_sum,
    )?;
    set_operator(
        ctx,
        processor_meta,
        MetaMethod::BXor,
        state.clone(),
        processor_branch,
    )?;
    set_operator(ctx, processor_meta, MetaMethod::Shr, state.clone(), pipe)?;
    ctx.set_global("__apteronotus_processor_meta", processor_meta);

    let pattern_meta = Table::new(&ctx);
    set_operator(
        ctx,
        pattern_meta,
        MetaMethod::Add,
        state.clone(),
        pattern_add,
    )?;
    set_operator(
        ctx,
        pattern_meta,
        MetaMethod::Sub,
        state.clone(),
        pattern_sub,
    )?;
    set_operator(
        ctx,
        pattern_meta,
        MetaMethod::Mul,
        state.clone(),
        pattern_mul,
    )?;
    set_operator(
        ctx,
        pattern_meta,
        MetaMethod::Div,
        state.clone(),
        pattern_div,
    )?;
    set_operator(
        ctx,
        pattern_meta,
        MetaMethod::Unm,
        state.clone(),
        pattern_neg,
    )?;
    set_operator(
        ctx,
        pattern_meta,
        MetaMethod::Shr,
        state.clone(),
        merge_patterns,
    )?;
    ctx.set_global("__apteronotus_pattern_meta", pattern_meta);

    let chord_meta = Table::new(&ctx);
    set_operator(ctx, chord_meta, MetaMethod::Shr, state.clone(), chord_pipe)?;
    ctx.set_global("__apteronotus_chord_meta", chord_meta);

    let duration_meta = Table::new(&ctx);
    set_operator(
        ctx,
        duration_meta,
        MetaMethod::Add,
        state.clone(),
        duration_add,
    )?;
    set_operator(
        ctx,
        duration_meta,
        MetaMethod::Sub,
        state.clone(),
        duration_sub,
    )?;
    ctx.set_global("__apteronotus_duration_meta", duration_meta);

    ctx.set_global(
        "rev",
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::Rev)),
    );

    let voice_meta = Table::new(&ctx);
    set_operator(
        ctx,
        voice_meta,
        MetaMethod::Index,
        state.clone(),
        voice_index,
    )?;
    ctx.set_global("__apteronotus_voice_meta", voice_meta);

    let patch_meta = Table::new(&ctx);
    set_operator(ctx, patch_meta, MetaMethod::Index, state, patch_index)?;
    ctx.set_global("__apteronotus_patch_meta", patch_meta);

    Ok(())
}

/// Finish program-scope routing after the entire source has declared its buses.
///
/// `send { ... }` returns a bus handle immediately so voice graphs can target
/// it lexically. Its persistent return processor cannot be staged until the
/// flattened bus layout is complete, however. All sends and the optional
/// master are therefore compiled here into one full-layout input patch. Its
/// wet returns are mixed into the main lanes and the consumed send lanes are
/// cleared by routed lowering. Authors never observe or count those flattened
/// lane ordinals.
pub(crate) fn finalize<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
) -> Result<(), Error<'gc>> {
    let mut state = state.borrow_mut();
    finalize_shared_signals(ctx, &mut state)?;
    if state.pending_sends.is_empty() && state.pending_master.is_none() {
        return Ok(());
    }
    if state.active.is_some() {
        return Err(binding_error(
            ctx,
            "evaluation ended while a graph was still being staged",
        ));
    }
    if state.program.patches.len() >= state.limits.patches {
        return Err(binding_error(
            ctx,
            format!("patch limit of {} exceeded", state.limits.patches),
        ));
    }

    let sends = std::mem::take(&mut state.pending_sends);
    let master = state.pending_master.take();
    let main_channels = state.program.buses.main_channels();
    let total_channels = state.program.buses.total_channels();
    state.generation = state.generation.wrapping_add(1);
    let generation = state.generation;
    state.active = Some(ActiveGraph {
        graph: GraphBuilder::with_inputs(total_channels),
        generation,
        lifetime: ActiveLifetime::Patch,
        input_channels: total_channels,
        patch_controls: Vec::new(),
    });
    let mut outputs = (0..total_channels).map(Source::Input).collect::<Vec<_>>();

    for send in sends {
        let range = state
            .program
            .buses
            .bus_range(send.bus)
            .ok_or_else(|| binding_error(ctx, "send bus does not belong to this edit"))?;
        let inputs = range.map(Source::Input).collect::<Vec<_>>();
        let wet = apply_processor(ctx, &mut state, &send.processor, &inputs)?;
        if wet.len() != main_channels {
            return Err(binding_error(
                ctx,
                "send graph output no longer matches the main channel layout",
            ));
        }
        for (channel, wet) in wet.into_iter().enumerate() {
            state.spend_node(ctx)?;
            let scaled = state
                .active_mut(ctx)?
                .graph
                .mul(wet, Source::Const(send.level));
            state.spend_node(ctx)?;
            outputs[channel] = state.active_mut(ctx)?.graph.add(outputs[channel], scaled);
        }
    }

    if let Some(master) = master {
        let mastered = apply_processor(ctx, &mut state, &master, &outputs[..main_channels])?;
        if mastered.len() != main_channels {
            return Err(binding_error(
                ctx,
                "master graph output no longer matches the main channel layout",
            ));
        }
        outputs[..main_channels].copy_from_slice(&mastered);
    }

    let active = state
        .active
        .take()
        .expect("the program-scope return graph was installed above");
    let mut graph = active.graph;
    let graph = graph
        .out(&outputs[..main_channels])
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    graph
        .validate_limits(state.limits.graph_publication)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    let patch = PatchTemplate::new(graph).map_err(|error| binding_error(ctx, error.to_string()))?;
    let id = state.program.patches.len();
    state.program.patches.push(patch);
    state.program.patch_controls.push(Vec::new());
    state.program.runs.push(crate::PatchRun {
        patch: crate::PatchId(id),
        span: None,
        routing: EventRouting::new(),
    });
    Ok(())
}

fn finalize_shared_signals<'gc>(
    ctx: Context<'gc>,
    state: &mut BuildState,
) -> Result<(), Error<'gc>> {
    if state.shared_writes.is_empty() {
        return Ok(());
    }
    if state.program.patches.len() >= state.limits.patches {
        return Err(binding_error(
            ctx,
            format!("patch limit of {} exceeded", state.limits.patches),
        ));
    }

    let writes = std::mem::take(&mut state.shared_writes);
    let mut graph = std::mem::take(&mut state.shared_graph);
    let mut silence = Source::Const(0.0);
    for writer in writes {
        state.spend_node(ctx)?;
        let muted = graph.mul(writer, Source::Const(0.0));
        state.spend_node(ctx)?;
        silence = graph.add(silence, muted);
    }
    let graph = graph
        .out(&[silence])
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    graph
        .validate_limits(state.limits.graph_publication)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    let patch = PatchTemplate::new(graph).map_err(|error| binding_error(ctx, error.to_string()))?;
    let id = crate::PatchId(state.program.patches.len());
    state.program.patches.push(patch);
    state.program.patch_controls.push(Vec::new());
    state.program.runs.insert(
        0,
        crate::PatchRun {
            patch: id,
            span: None,
            routing: EventRouting::new(),
        },
    );
    Ok(())
}

fn lock_down<'gc>(ctx: Context<'gc>) -> Result<(), Error<'gc>> {
    // Stubs make an attempted capability use a diagnostic while preserving
    // the ordinary callable shape expected by code that probes with pcall.
    let unavailable = Callback::from_fn(&ctx, |ctx, _, _| {
        Err("function is not available in the Apteronotus sandbox"
            .into_value(ctx)
            .into())
    });
    ctx.set_global("collectgarbage", unavailable);
    ctx.set_global("print", unavailable);
    if let Value::Table(math) = ctx.get_global_value("math") {
        math.set(ctx, "random", unavailable)?;
        math.set(ctx, "randomseed", unavailable)?;
    }
    Ok(())
}

type BindingFn = for<'gc> fn(
    Context<'gc>,
    &Rc<RefCell<BuildState>>,
    piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>>;

fn set_callback<'gc>(
    ctx: Context<'gc>,
    name: &'static str,
    state: Rc<RefCell<BuildState>>,
    callback: BindingFn,
) -> Result<(), Error<'gc>> {
    ctx.set_global(
        name,
        Callback::from_fn(&ctx, move |ctx, _, stack| callback(ctx, &state, stack)),
    );
    Ok(())
}

fn set_operator<'gc>(
    ctx: Context<'gc>,
    metatable: Table<'gc>,
    method: MetaMethod,
    state: Rc<RefCell<BuildState>>,
    callback: BindingFn,
) -> Result<(), Error<'gc>> {
    metatable.set(
        ctx,
        method,
        Callback::from_fn(&ctx, move |ctx, _, stack| callback(ctx, &state, stack)),
    )?;
    Ok(())
}

fn begin_voice<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let params = stack.get(0);
    let mut state = state.borrow_mut();
    if state.active.is_some() {
        return Err(binding_error(ctx, "cannot stage nested voice graphs"));
    }
    if state.program.voices.len() >= state.limits.voices {
        return Err(binding_error(
            ctx,
            format!("voice limit of {} exceeded", state.limits.voices),
        ));
    }

    state.generation = state.generation.wrapping_add(1);
    let generation = state.generation;
    let mut graph = GraphBuilder::new();
    let note = Table::new(&ctx);
    note.set(ctx, "hz", lua_source(ctx, generation, [n::HZ])?)?;
    note.set(ctx, "velocity", lua_source(ctx, generation, [n::VELOCITY])?)?;
    note.set(ctx, "duration", lua_source(ctx, generation, [n::DURATION])?)?;
    note.set(ctx, "pan", lua_source(ctx, generation, [n::PAN])?)?;
    note.set(ctx, "id", UserData::new_static(&ctx, LuaEventSeed))?;

    if !params.is_nil() {
        let Value::Table(params) = params else {
            return Err(binding_error(ctx, "voice.params must be a table"));
        };
        let mut specs = Vec::new();
        for (key, value) in params {
            let Value::String(name) = key else {
                return Err(binding_error(ctx, "voice parameter names must be strings"));
            };
            let name = name
                .to_str()
                .map_err(|_| binding_error(ctx, "voice parameter names must be UTF-8"))?
                .to_owned();
            specs.push((name, read_param_spec(ctx, value)?));
        }
        // Lua deliberately does not promise map iteration order. Parameter
        // indices do need to be reproducible, so derive them from names.
        specs.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, parsed) in specs {
            let mut spec = ParamSpec::new(&name, parsed.min, parsed.max, parsed.default);
            if let Some(unit) = parsed.unit {
                spec = spec.with_unit(&unit);
            }
            if let Some(seconds) = parsed.max_curve_seconds {
                spec = spec.with_curve_horizon(seconds);
            }
            let source = graph.param(spec);
            note.set(ctx, name, lua_source(ctx, generation, [source])?)?;
        }
    }

    state.active = Some(ActiveGraph {
        graph,
        generation,
        lifetime: ActiveLifetime::Voice,
        input_channels: 0,
        patch_controls: Vec::new(),
    });
    stack.replace(ctx, note);
    Ok(CallbackReturn::Return)
}

fn abort_voice<'gc>(
    _ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    state.borrow_mut().active = None;
    stack.clear();
    Ok(CallbackReturn::Return)
}

fn finish_voice<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let output = stack.get(0);
    let mut state = state.borrow_mut();
    let active = state
        .active
        .take()
        .ok_or_else(|| binding_error(ctx, "voice graph is not active"))?;
    let channels = read_channels(ctx, output, active.generation)?;
    if channels.is_empty() {
        return Err(binding_error(
            ctx,
            "voice graph returned no output channels",
        ));
    }
    let mut graph = active.graph;
    let template = graph
        .out(&channels)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    template
        .validate_limits(state.limits.graph_publication)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    let id = state.program.voices.len();
    state.program.voices.push(template);
    stack.replace(ctx, lua_voice(ctx, id)?);
    Ok(CallbackReturn::Return)
}

fn begin_patch<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let inputs = if stack.get(0).is_nil() {
        0
    } else {
        read_count(ctx, stack.get(0), "patch input channels")?
    };
    let params = stack.get(1);
    let declared_controls = stack.get(2);
    let mut state = state.borrow_mut();
    if state.active.is_some() {
        return Err(binding_error(ctx, "cannot stage nested graphs"));
    }
    if state.program.patches.len() >= state.limits.patches {
        return Err(binding_error(
            ctx,
            format!("patch limit of {} exceeded", state.limits.patches),
        ));
    }
    state.generation = state.generation.wrapping_add(1);
    let generation = state.generation;
    let graph = GraphBuilder::with_inputs(inputs);
    let controls = Table::new(&ctx);
    let mut patch_controls = Vec::new();

    if inputs > 0 {
        let input_values = Table::new(&ctx);
        for channel in 0..inputs {
            input_values.set(
                ctx,
                (channel + 1) as i64,
                lua_source(ctx, generation, [Source::Input(channel)])?,
            )?;
        }
        controls.set(ctx, "inputs", input_values)?;
        controls.set(
            ctx,
            "input",
            lua_source(ctx, generation, (0..inputs).map(Source::Input))?,
        )?;
    }

    if !params.is_nil() {
        let Value::Table(params) = params else {
            return Err(binding_error(ctx, "patch.params must be a table"));
        };
        let mut specs = Vec::new();
        for (key, value) in params {
            let Value::String(name) = key else {
                return Err(binding_error(ctx, "patch parameter names must be strings"));
            };
            let name = name
                .to_str()
                .map_err(|_| binding_error(ctx, "patch parameter names must be UTF-8"))?
                .to_owned();
            specs.push((name, read_param_spec(ctx, value)?));
        }
        specs.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, parsed) in specs {
            if state.program.controls.specs().len() >= state.limits.controls {
                return Err(binding_error(
                    ctx,
                    format!("control limit of {} exceeded", state.limits.controls),
                ));
            }
            // ControlLayout names are unique program-wide while patch
            // parameter names are local. Declaration-order qualification is a
            // provisional display name only; arena identity and cross-edit
            // reconciliation must never derive from it.
            let qualified = format!("patch{}.{}", state.program.patches.len() + 1, name);
            let mut spec = ControlSpec::new(&qualified, parsed.min, parsed.max, parsed.default);
            if let Some(unit) = parsed.unit {
                spec = spec.with_unit(&unit);
            }
            let id = state
                .program
                .controls
                .add(spec)
                .map_err(|error| binding_error(ctx, error.to_string()))?;
            patch_controls.push(PatchControl {
                name: name.clone(),
                id,
                kind: PatchControlKind::Number,
            });
            controls.set(
                ctx,
                name,
                lua_source(ctx, generation, [Source::Control(id)])?,
            )?;
        }
    }

    if !declared_controls.is_nil() {
        let Value::Table(declared_controls) = declared_controls else {
            return Err(binding_error(ctx, "patch.controls must be a table"));
        };
        let mut specs = Vec::new();
        for (key, value) in declared_controls {
            let Value::String(name) = key else {
                return Err(binding_error(ctx, "patch control names must be strings"));
            };
            let name = name
                .to_str()
                .map_err(|_| binding_error(ctx, "patch control names must be UTF-8"))?
                .to_owned();
            let Value::UserData(data) = value else {
                return Err(binding_error(
                    ctx,
                    "patch controls must use control {...}, note_control(...), or gate_control(...)",
                ));
            };
            let spec = data
                .downcast_static::<LuaPatchControlSpec>()
                .map_err(|_| {
                    binding_error(
                        ctx,
                        "patch controls must use control {...}, note_control(...), or gate_control(...)",
                    )
                })?
                .clone();
            specs.push((name, spec));
        }
        specs.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, parsed) in specs {
            if state.program.controls.specs().len() >= state.limits.controls {
                return Err(binding_error(
                    ctx,
                    format!("control limit of {} exceeded", state.limits.controls),
                ));
            }
            let qualified = format!("patch{}.{}", state.program.patches.len() + 1, name);
            let mut spec = ControlSpec::new(&qualified, parsed.min, parsed.max, parsed.default);
            if let Some(unit) = &parsed.unit {
                spec = spec.with_unit(unit);
            }
            let id = state
                .program
                .controls
                .add(spec)
                .map_err(|error| binding_error(ctx, error.to_string()))?;
            patch_controls.push(PatchControl {
                name: name.clone(),
                id,
                kind: parsed.kind,
            });
            controls.set(
                ctx,
                name,
                lua_source(ctx, generation, [Source::Control(id)])?,
            )?;
        }
    }

    state.active = Some(ActiveGraph {
        graph,
        generation,
        lifetime: ActiveLifetime::Patch,
        input_channels: inputs,
        patch_controls,
    });
    stack.replace(ctx, controls);
    Ok(CallbackReturn::Return)
}

fn finish_patch<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let output = stack.get(0);
    let mut state = state.borrow_mut();
    let (generation, inputs, lifetime) = {
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| binding_error(ctx, "patch graph is not active"))?;
        (active.generation, active.input_channels, active.lifetime)
    };
    if !matches!(lifetime, ActiveLifetime::Patch) {
        return Err(binding_error(ctx, "active graph is not a patch"));
    }
    let channels = if is_processor(output) {
        // A processor-returning patch is shorthand for applying that topology
        // to every explicit host input. No hidden input exists for a
        // zero-input patch, so ordinary generator patches return a signal.
        let processor = read_processor(ctx, output, generation)?;
        let sources = (0..inputs).map(Source::Input).collect::<Vec<_>>();
        apply_processor(ctx, &mut state, &processor, &sources)?
    } else {
        read_channels(ctx, output, generation)?
    };
    if channels.is_empty() {
        return Err(binding_error(
            ctx,
            "patch graph returned no output channels",
        ));
    }
    let active = state
        .active
        .take()
        .expect("the active patch was checked above");
    let mut graph = active.graph;
    let graph = graph
        .out(&channels)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    graph
        .validate_limits(state.limits.graph_publication)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    let patch = PatchTemplate::new(graph).map_err(|error| binding_error(ctx, error.to_string()))?;
    let id = state.program.patches.len();
    state.program.patches.push(patch);
    state.program.patch_controls.push(active.patch_controls);
    stack.replace(ctx, lua_patch(ctx, id)?);
    Ok(CallbackReturn::Return)
}

fn control<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    declare_control(ctx, state, &mut stack, false)
}

fn control_input<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    declare_control(ctx, state, &mut stack, true)
}

fn param_ref<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        return Err(binding_error(
            ctx,
            "param(name) is only valid while constructing a program-scope send graph",
        ));
    }
    let name = read_string(ctx, stack.get(0), "send parameter name")?;
    if name.is_empty() {
        return Err(binding_error(ctx, "send parameter name must not be empty"));
    }
    stack.replace(ctx, UserData::new_static(&ctx, LuaParamRef(name)));
    Ok(CallbackReturn::Return)
}

fn declare_control<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    external: bool,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(table) = stack.get(0) else {
        return Err(binding_error(
            ctx,
            if external {
                "control_input expects a specification table"
            } else {
                "control expects a specification table"
            },
        ));
    };
    let Value::Table(range) = table.get_value(ctx, "range") else {
        return Err(binding_error(
            ctx,
            if external {
                "control_input.range must be a two-number table"
            } else {
                "control.range must be a two-number table"
            },
        ));
    };
    let min = read_number(ctx, range.get_value(ctx, 1), "control minimum")?;
    let max = read_number(ctx, range.get_value(ctx, 2), "control maximum")?;
    let default = read_number(ctx, table.get_value(ctx, "default"), "control default")?;
    let unit = table.get_value(ctx, "unit");
    let unit = if unit.is_nil() {
        None
    } else {
        Some(read_string(ctx, unit, "control unit")?)
    };
    let name_value = table.get_value(ctx, "name");
    if name_value.is_nil() && !external {
        stack.replace(
            ctx,
            UserData::new_static(
                &ctx,
                LuaPatchControlSpec {
                    min,
                    max,
                    default,
                    unit,
                    kind: PatchControlKind::Number,
                },
            ),
        );
        return Ok(CallbackReturn::Return);
    }
    let name = read_string(
        ctx,
        name_value,
        if external {
            "control input name"
        } else {
            "control name"
        },
    )?;
    if name.starts_with(INTERNAL_CONTROL_PREFIX) {
        return Err(binding_error(
            ctx,
            format!("control names beginning with {INTERNAL_CONTROL_PREFIX:?} are reserved"),
        ));
    }
    let mut spec = ControlSpec::new(&name, min, max, default);
    if let Some(unit) = &unit {
        spec = spec.with_unit(unit);
    }
    let mut state = state.borrow_mut();
    if state.active.is_some() {
        return Err(binding_error(
            ctx,
            "program controls must be declared outside a graph",
        ));
    }
    if state.program.controls.specs().len() >= state.limits.controls {
        return Err(binding_error(
            ctx,
            format!("control limit of {} exceeded", state.limits.controls),
        ));
    }
    let id = state
        .program
        .controls
        .add(spec)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    if external {
        state.program.control_inputs.push(name);
    }
    stack.replace(ctx, lua_control(ctx, id)?);
    Ok(CallbackReturn::Return)
}

fn note_control<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let note = read_string(ctx, stack.get(0), "note control default")?;
    let default = Pitch::parse(&note)
        .map_err(|error| binding_error(ctx, error.to_string()))?
        .midi();
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatchControlSpec {
                min: -256.0,
                max: 256.0,
                default,
                unit: Some("midi".into()),
                kind: PatchControlKind::Note,
            },
        ),
    );
    Ok(CallbackReturn::Return)
}

fn gate_control<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Boolean(default) = stack.get(0) else {
        return Err(binding_error(ctx, "gate_control expects true or false"));
    };
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatchControlSpec {
                min: 0.0,
                max: 1.0,
                default: if default { 1.0 } else { 0.0 },
                unit: None,
                kind: PatchControlKind::Gate,
            },
        ),
    );
    Ok(CallbackReturn::Return)
}

fn audio_input<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(table) = stack.get(0) else {
        return Err(binding_error(
            ctx,
            "audio_input expects a specification table",
        ));
    };
    let name = read_string(ctx, table.get_value(ctx, "name"), "audio input name")?;
    let channels = read_count(
        ctx,
        table.get_value(ctx, "channels"),
        "audio input channels",
    )?;
    let fallback = read_string(
        ctx,
        table.get_value(ctx, "fallback"),
        "audio input fallback",
    )?;
    if fallback != "silence" {
        return Err(binding_error(
            ctx,
            "audio_input currently supports only fallback = \"silence\"",
        ));
    }

    let mut state = state.borrow_mut();
    if state.active.is_some() {
        return Err(binding_error(
            ctx,
            "audio inputs must be declared outside a graph",
        ));
    }
    if state.program.audio_inputs.specs().len() >= state.limits.audio_inputs {
        return Err(binding_error(
            ctx,
            format!(
                "audio input limit of {} exceeded",
                state.limits.audio_inputs
            ),
        ));
    }
    let id = state
        .program
        .audio_inputs
        .add(AudioInputSpec::silence(&name, channels))
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, lua_audio_input(ctx, id, channels)?);
    Ok(CallbackReturn::Return)
}

fn bus<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let channels = match stack.get(0) {
        Value::Table(table) => read_count(ctx, table.get_value(ctx, "channels"), "bus channels")?,
        value => read_count(ctx, value, "bus channels")?,
    };
    let mut state = state.borrow_mut();
    if state.active.is_some() {
        return Err(binding_error(ctx, "buses must be declared outside a graph"));
    }
    if state.bus_count >= state.limits.buses {
        return Err(binding_error(
            ctx,
            format!("bus limit of {} exceeded", state.limits.buses),
        ));
    }
    let id = state
        .program
        .buses
        .add_bus(channels)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.bus_count += 1;
    stack.replace(ctx, UserData::new_static(&ctx, LuaBus { id, channels }));
    Ok(CallbackReturn::Return)
}

fn send_return<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(spec) = stack.get(0) else {
        return Err(binding_error(ctx, "send expects a specification table"));
    };
    let mut processor = read_processor(ctx, spec.get_value(ctx, "graph"), 0)?;
    let level = read_number(ctx, spec.get_value(ctx, "level"), "send level")?;
    let declared_params = match spec.get_value(ctx, "params") {
        value if value.is_nil() => Vec::new(),
        Value::Table(params) => {
            let mut parsed = Vec::new();
            for (key, value) in params {
                let Value::String(name) = key else {
                    return Err(binding_error(ctx, "send parameter names must be strings"));
                };
                let name = name
                    .to_str()
                    .map_err(|_| binding_error(ctx, "send parameter names must be UTF-8"))?
                    .to_owned();
                parsed.push((name, read_param_spec(ctx, value)?));
            }
            parsed.sort_by(|left, right| left.0.cmp(&right.0));
            parsed
        }
        _ => return Err(binding_error(ctx, "send.params must be a table")),
    };
    let mut state = state.borrow_mut();
    if state.active.is_some() {
        return Err(binding_error(ctx, "sends must be declared outside a graph"));
    }
    if state.bus_count >= state.limits.buses {
        return Err(binding_error(
            ctx,
            format!("bus limit of {} exceeded", state.limits.buses),
        ));
    }
    let mut params = HashMap::new();
    for (name, parsed) in declared_params {
        if state.program.controls.specs().len() >= state.limits.controls {
            return Err(binding_error(
                ctx,
                format!("control limit of {} exceeded", state.limits.controls),
            ));
        }
        let qualified = format!("send{}.{}", state.pending_sends.len() + 1, name);
        let mut control = ControlSpec::new(&qualified, parsed.min, parsed.max, parsed.default);
        if let Some(unit) = parsed.unit {
            control = control.with_unit(&unit);
        }
        let id = state
            .program
            .controls
            .add(control)
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        params.insert(name, (Source::Control(id), parsed.max));
    }
    resolve_send_processor_params(ctx, &mut processor, &params)?;
    let channels = state.program.buses.main_channels();
    let processor = broadcast_mono_processor(processor, channels);
    if processor_inputs(&processor) != channels || processor_outputs(&processor) != channels {
        return Err(binding_error(
            ctx,
            format!("send graph must have {channels} inputs and {channels} outputs"),
        ));
    }
    let bus = state
        .program
        .buses
        .add_bus(channels)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.bus_count += 1;
    state.pending_sends.push(PendingSend {
        bus,
        processor,
        level,
    });
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaBus { id: bus, channels }),
    );
    Ok(CallbackReturn::Return)
}

fn resolve_send_processor_params<'gc>(
    ctx: Context<'gc>,
    processor: &mut Processor,
    params: &HashMap<String, (Source, f64)>,
) -> Result<(), Error<'gc>> {
    match processor {
        Processor::Fdn { t60, .. } => {
            if let ProcessorScalar::SendParam(name) = t60 {
                let (source, max) = params.get(name).copied().ok_or_else(|| {
                    binding_error(
                        ctx,
                        format!("send graph references undeclared parameter {name:?}"),
                    )
                })?;
                *t60 = ProcessorScalar::Bound { source, max };
            }
        }
        Processor::Chain(left, right)
        | Processor::Stack(left, right)
        | Processor::Sum(left, right)
        | Processor::Product(left, right)
        | Processor::Branch(left, right) => {
            resolve_send_processor_params(ctx, left, params)?;
            resolve_send_processor_params(ctx, right, params)?;
        }
        Processor::Scale(inner, _) => resolve_send_processor_params(ctx, inner, params)?,
        _ => {}
    }
    Ok(())
}

fn master<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let processor = read_processor(ctx, stack.get(0), 0)?;
    let channels = state.borrow().program.buses.main_channels();
    if processor_inputs(&processor) != channels || processor_outputs(&processor) != channels {
        return Err(binding_error(
            ctx,
            format!("master graph must have {channels} inputs and {channels} outputs"),
        ));
    }
    let mut state = state.borrow_mut();
    if state.pending_master.is_some() {
        return Err(binding_error(ctx, "master may be declared only once"));
    }
    state.pending_master = Some(processor);
    stack.clear();
    Ok(CallbackReturn::Return)
}

fn run<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(ctx, "run expects a patch handle"));
    };
    let patch = data
        .downcast_static::<LuaPatch>()
        .map_err(|_| binding_error(ctx, "run expects a patch handle"))?
        .0;
    let span = if stack.len() >= 2 {
        let Value::UserData(data) = stack.get(1) else {
            return Err(binding_error(ctx, "run span must come from span(...)"));
        };
        Some(
            data.downcast_static::<LuaSpan>()
                .map_err(|_| binding_error(ctx, "run span must come from span(...)"))?
                .0,
        )
    } else {
        None
    };
    if stack.len() > 2 {
        return Err(binding_error(
            ctx,
            "run expects run(patch) or run(patch, span)",
        ));
    }
    let mut state = state.borrow_mut();
    if patch >= state.program.patches.len() {
        return Err(binding_error(
            ctx,
            "patch handle does not belong to this edit",
        ));
    }
    if span.is_some() && state.program.patches[patch].graph().inputs != 0 {
        return Err(binding_error(
            ctx,
            "a finite run currently requires an autonomous zero-input patch",
        ));
    }
    state.program.runs.push(crate::PatchRun {
        patch: crate::PatchId(patch),
        span,
        routing: EventRouting::new(),
    });
    stack.clear();
    Ok(CallbackReturn::Return)
}

fn seconds<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let value = read_number(ctx, stack.get(0), "seconds")?;
    stack.replace(
        ctx,
        lua_duration(
            ctx,
            LuaDuration {
                value,
                unit: TimeUnit::Seconds,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn duration_add<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    duration_binary(ctx, &mut stack, false)
}

fn duration_sub<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    duration_binary(ctx, &mut stack, true)
}

fn duration_binary<'gc>(
    ctx: Context<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
    subtract: bool,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let read = |value| {
        let Value::UserData(data) = value else {
            return Err(binding_error(
                ctx,
                "duration arithmetic expects two durations",
            ));
        };
        data.downcast_static::<LuaDuration>()
            .copied()
            .map_err(|_| binding_error(ctx, "duration arithmetic expects two durations"))
    };
    let left = read(stack.get(0))?;
    let right = read(stack.get(1))?;
    let combine = |left: f64, right: f64| {
        if subtract { left - right } else { left + right }
    };
    let duration = match (left.unit, right.unit) {
        (TimeUnit::Seconds, TimeUnit::Seconds) => LuaDuration {
            value: combine(left.value, right.value),
            unit: TimeUnit::Seconds,
        },
        (TimeUnit::Seconds, _) | (_, TimeUnit::Seconds) => {
            return Err(binding_error(
                ctx,
                "cannot mix absolute seconds with bars/beats in duration arithmetic",
            ));
        }
        _ => {
            let cycles = |duration: LuaDuration| match duration.unit {
                TimeUnit::Cycles => duration.value,
                TimeUnit::Beats {
                    beats_per_cycle, ..
                } => duration.value / beats_per_cycle,
                TimeUnit::Seconds => unreachable!("seconds were rejected above"),
            };
            LuaDuration {
                value: combine(cycles(left), cycles(right)),
                unit: TimeUnit::Cycles,
            }
        }
    };
    if !duration.value.is_finite() {
        return Err(binding_error(
            ctx,
            "duration arithmetic produced a non-finite value",
        ));
    }
    stack.replace(ctx, lua_duration(ctx, duration)?);
    Ok(CallbackReturn::Return)
}

fn milliseconds<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let value = read_number(ctx, stack.get(0), "milliseconds")? / 1_000.0;
    stack.replace(
        ctx,
        lua_duration(
            ctx,
            LuaDuration {
                value,
                unit: TimeUnit::Seconds,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn bars<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let value = read_number(ctx, stack.get(0), "bars")?;
    stack.replace(
        ctx,
        lua_duration(
            ctx,
            LuaDuration {
                value,
                unit: TimeUnit::Cycles,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn beats<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let value = read_number(ctx, stack.get(0), "beats")?;
    let state = state.borrow();
    if !state.tempo_declared {
        return Err(binding_error(
            ctx,
            "beats(...) requires tempo(...) earlier in the evaluation",
        ));
    }
    let tempo = &state.program.tempo;
    let seconds_per_beat = match tempo.points() {
        [point] if point.over.is_none() => Some(60.0 / point.bpm),
        _ => None,
    };
    stack.replace(
        ctx,
        lua_duration(
            ctx,
            LuaDuration {
                value,
                unit: TimeUnit::Beats {
                    beats_per_cycle: tempo.beats_per_cycle(),
                    seconds_per_beat,
                },
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn tempo<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().tempo_declared {
        return Err(binding_error(
            ctx,
            "tempo may be declared only once per evaluation",
        ));
    }

    let map = match stack.get(0) {
        Value::Table(table) => {
            let beats_per_cycle = optional_number(
                ctx,
                table.get_value(ctx, "beats_per_cycle"),
                4.0,
                "beats per cycle",
            )?;
            let length = usize::try_from(table.length())
                .map_err(|_| binding_error(ctx, "tempo table is too large"))?;
            let limit = state.borrow().limits.tempo_points;
            if length > limit {
                return Err(binding_error(
                    ctx,
                    format!("tempo point limit of {limit} exceeded"),
                ));
            }
            let mut points = Vec::with_capacity(length);
            for index in 1..=length {
                let table_index = i64::try_from(index)
                    .map_err(|_| binding_error(ctx, "tempo table is too large"))?;
                let Value::Table(point) = table.get_value(ctx, table_index) else {
                    return Err(binding_error(
                        ctx,
                        format!("tempo point {index} must be a table"),
                    ));
                };
                let at = read_pattern_time(ctx, point.get_value(ctx, "at"), "tempo point time")?;
                let bpm = read_number(ctx, point.get_value(ctx, "bpm"), "tempo point BPM")?;
                let over = match point.get_value(ctx, "over") {
                    Value::Nil => None,
                    value => Some(read_pattern_time(ctx, value, "tempo ramp duration")?),
                };
                points.push(match over {
                    Some(over) => TempoPoint::ramp(at, bpm, over),
                    None => TempoPoint::step(at, bpm),
                });
            }
            TempoMap::new(beats_per_cycle, points)
        }
        value => TempoMap::constant(read_number(ctx, value, "tempo BPM")?, 4.0),
    }
    .map_err(|error| binding_error(ctx, error.to_string()))?;

    let mut state = state.borrow_mut();
    state.program.tempo = map;
    state.tempo_declared = true;
    stack.clear();
    Ok(CallbackReturn::Return)
}

fn key<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().program.key.is_some() {
        return Err(binding_error(
            ctx,
            "key may be declared only once per evaluation",
        ));
    }
    let tonic = read_string(ctx, stack.get(0), "key tonic")?;
    let mode = read_string(ctx, stack.get(1), "key mode")?;
    let key = Key::parse(&tonic, &mode).map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().program.key = Some(key);
    stack.clear();
    Ok(CallbackReturn::Return)
}

fn hold<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(
            ctx,
            "hold expects an explicit secs(...), ms(...), bars(...), or beats(...) duration",
        ));
    };
    let duration = *data.downcast_static::<LuaDuration>().map_err(|_| {
        binding_error(
            ctx,
            "hold expects an explicit secs(...), ms(...), bars(...), or beats(...) duration",
        )
    })?;
    if !duration.value.is_finite() || duration.value <= 0.0 {
        return Err(binding_error(
            ctx,
            "hold duration must be positive and finite",
        ));
    }
    if let Some(cycles) = cycle_hold_duration(duration) {
        let limit = state.borrow().limits.max_hold_cycles.max(0);
        if cycles > Frac::int(limit) {
            return Err(binding_error(
                ctx,
                format!(
                    "hold duration of {cycles} cycles exceeds the query look-back limit of \
                     {limit} cycles"
                ),
            ));
        }
    }
    stack.replace(ctx, UserData::new_static(&ctx, LuaHold(duration)));
    Ok(CallbackReturn::Return)
}

fn cycle_hold_duration(duration: LuaDuration) -> Option<Frac> {
    match duration.unit {
        TimeUnit::Seconds => None,
        TimeUnit::Cycles => Some(Frac::approx(duration.value, 1_000_000)),
        TimeUnit::Beats {
            beats_per_cycle, ..
        } => Some(Frac::approx(duration.value / beats_per_cycle, 1_000_000)),
    }
}

fn at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if !(2..=3).contains(&stack.len()) {
        return Err(binding_error(
            ctx,
            "at expects a start, a pattern, and an optional typed capture duration",
        ));
    }
    let start = read_placement_time(ctx, state, stack.get(0))?;
    let capture_cycles = if stack.len() == 3 {
        read_capture_duration(ctx, state, start, stack.get(2))?
    } else {
        Frac::ONE
    };
    let limit = state.borrow().limits.max_timeline_capture_cycles.max(0);
    if capture_cycles <= Frac::ZERO || capture_cycles > Frac::int(limit) {
        return Err(binding_error(
            ctx,
            format!("at capture duration must be positive and no longer than {limit} cycles"),
        ));
    }
    let pattern = read_lua_pattern(ctx, stack.get(1))
        .map_err(|_| {
            binding_error(
                ctx,
                "at pattern must come from pattern(), note(), or timeline()",
            )
        })?
        .clone();
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPlacement {
                start,
                capture_cycles,
                pattern,
            },
        ),
    );
    Ok(CallbackReturn::Return)
}

fn finite_span<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.len() != 2 {
        return Err(binding_error(ctx, "span expects a beginning and an end"));
    }
    let begin = read_placement_time(ctx, state, stack.get(0))?;
    let end = read_placement_time(ctx, state, stack.get(1))?;
    if begin >= end {
        return Err(binding_error(
            ctx,
            "span end must be later than its beginning",
        ));
    }
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaSpan(Span::new(begin, end))),
    );
    Ok(CallbackReturn::Return)
}

fn timeline<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(table) = stack.get(0) else {
        return Err(binding_error(
            ctx,
            "timeline expects a table of at() placements",
        ));
    };

    let mut events = Vec::new();
    let mut controls = Vec::new();
    for placement_index in 1..=table.length() {
        let Value::UserData(data) = table.get_value(ctx, placement_index) else {
            return Err(binding_error(
                ctx,
                format!("timeline item {placement_index} must be an at() placement"),
            ));
        };
        let placement = data
            .downcast_static::<LuaPlacement>()
            .map_err(|_| {
                binding_error(
                    ctx,
                    format!("timeline item {placement_index} must be an at() placement"),
                )
            })?
            .clone();
        if !placement.pattern.routing.sends().is_empty() {
            return Err(binding_error(
                ctx,
                "apply score sends after timeline(...), not inside an at(...) placement",
            ));
        }
        if placement.pattern.hold_seconds.is_some() && !state.borrow().tempo_declared {
            return Err(binding_error(
                ctx,
                "hold(secs(...)) inside a timeline requires tempo(...) earlier in the evaluation",
            ));
        }
        controls.extend(placement.pattern.controls);

        let density = placement.pattern.pattern.density();
        if !density.is_finite() || density > mini::limits::EVENTS {
            return Err(binding_error(
                ctx,
                format!(
                    "timeline placement produces about {density:.0} events, more than the \
                     per-cycle limit of {}",
                    mini::limits::EVENTS
                ),
            ));
        }
        // Partial final cycles can contain a full cycle's onset density.
        let estimated = (density * placement.capture_cycles.to_f64().ceil()).ceil() as usize;
        if events.len().saturating_add(estimated) > state.borrow().limits.pattern_nodes {
            return Err(binding_error(
                ctx,
                format!(
                    "timeline capture exceeds the pattern node limit of {}",
                    state.borrow().limits.pattern_nodes
                ),
            ));
        }

        // A placement captures its local window (one cycle by default). Query order is
        // the stable structural order and therefore also defines captured
        // event ordinals; it is not a musical chord-order contract.
        for event in placement
            .pattern
            .pattern
            .onsets(Span::new(Frac::ZERO, placement.capture_cycles))
        {
            if events.len() >= state.borrow().limits.pattern_nodes {
                return Err(binding_error(
                    ctx,
                    "timeline capture exceeds the pattern node limit",
                ));
            }
            let whole = event
                .whole
                .expect("onsets cannot contain continuous signal events");
            let begin = whole.begin.checked_add(placement.start).ok_or_else(|| {
                binding_error(ctx, "timeline onset exceeds the cycle-time representation")
            })?;
            let end = if let Some(seconds) = placement.pattern.hold_seconds {
                let state = state.borrow();
                let begin_seconds = state.program.tempo.cycle_to_seconds(begin);
                state
                    .program
                    .tempo
                    .seconds_to_cycle(begin_seconds + seconds)
                    .map_err(|error| binding_error(ctx, error.to_string()))?
            } else {
                whole.end.checked_add(placement.start).ok_or_else(|| {
                    binding_error(
                        ctx,
                        "timeline release exceeds the cycle-time representation",
                    )
                })?
            };
            let shifted = Span::new(begin, end);
            let ordinal = u64::try_from(events.len())
                .map_err(|_| binding_error(ctx, "timeline event count exceeds u64"))?;
            let mut captured = TimelineEvent::new(shifted, event.value, ordinal);
            if let Some(src) = event.src {
                captured = captured.at(src);
            }
            if let Some(group) = event.group {
                captured = captured.in_group(group);
            }
            events.push(captured);
        }
    }

    if events.is_empty() {
        let pattern = Pattern::Silence;
        state.borrow_mut().spend_pattern(ctx, &pattern)?;
        stack.replace(
            ctx,
            lua_pattern(
                ctx,
                LuaPattern {
                    pattern,
                    controls,
                    routing: EventRouting::new(),
                    external_trigger: None,
                    hold_seconds: None,
                },
            )?,
        );
        return Ok(CallbackReturn::Return);
    }

    let begin = events
        .iter()
        .map(|event| event.whole.begin)
        .min()
        .expect("non-empty events have a first begin");
    let end = events
        .iter()
        .map(|event| event.whole.end)
        .max()
        .expect("non-empty events have a final end");
    let id = {
        let mut state = state.borrow_mut();
        let id = TimelineId::new(state.timeline_count);
        state.timeline_count = state.timeline_count.wrapping_add(1);
        id
    };
    let timeline = Timeline::new(id, Span::new(begin, end), events)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    let pattern = Pattern::timeline(timeline);
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls,
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn pattern<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    parse_pattern(ctx, state, &mut stack, 0, 0, None)
}

fn read_placement_time<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    value: Value<'gc>,
) -> Result<Frac, Error<'gc>> {
    let Value::UserData(data) = value else {
        return Ok(Frac::approx(
            read_number(ctx, value, "timeline placement time")?,
            1_000_000,
        ));
    };
    let Ok(duration) = data.downcast_static::<LuaDuration>() else {
        return Ok(Frac::approx(
            read_number(ctx, value, "timeline placement time")?,
            1_000_000,
        ));
    };
    match duration.unit {
        TimeUnit::Cycles => Ok(Frac::approx(duration.value, 1_000_000)),
        TimeUnit::Beats {
            beats_per_cycle, ..
        } => Ok(Frac::approx(duration.value / beats_per_cycle, 1_000_000)),
        TimeUnit::Seconds => {
            let state = state.borrow();
            if !state.tempo_declared {
                return Err(binding_error(
                    ctx,
                    "at(secs(...), ...) requires tempo(...) earlier in the evaluation",
                ));
            }
            state
                .program
                .tempo
                .seconds_to_cycle(duration.value)
                .map_err(|error| binding_error(ctx, error.to_string()))
        }
    }
}

fn read_capture_duration<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    start: Frac,
    value: Value<'gc>,
) -> Result<Frac, Error<'gc>> {
    let expected = "at capture duration expects bars(...), beats(...), secs(...), or ms(...)";
    let Value::UserData(data) = value else {
        return Err(binding_error(ctx, expected));
    };
    let duration = data
        .downcast_static::<LuaDuration>()
        .map_err(|_| binding_error(ctx, expected))?;
    if !duration.value.is_finite() || duration.value <= 0.0 {
        return Err(binding_error(
            ctx,
            "at capture duration must be positive and finite",
        ));
    }
    if let Some(cycles) = cycle_hold_duration(*duration) {
        return Ok(cycles);
    }
    let state = state.borrow();
    if !state.tempo_declared {
        return Err(binding_error(
            ctx,
            "at capture in seconds requires tempo(...) earlier in the evaluation",
        ));
    }
    let start_seconds = state.program.tempo.cycle_to_seconds(start);
    let end = state
        .program
        .tempo
        .seconds_to_cycle(start_seconds + duration.value)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    end.checked_sub(start).ok_or_else(|| {
        binding_error(
            ctx,
            "at capture duration exceeds the cycle-time representation",
        )
    })
}

fn pattern_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let binding = read_call_site(ctx, stack.get(0))?;
    let base = read_document_base(ctx, stack.get(1))?;
    parse_pattern(ctx, state, &mut stack, 2, binding, base)
}

fn parse_pattern<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    source_index: usize,
    binding: u64,
    base: Option<usize>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let source = read_string(ctx, stack.get(source_index), "pattern source")?;
    let pattern = mini::parse_in(&source, binding, base)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn play<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    play_bound(ctx, state, &mut stack, 0, 0, None)
}

fn play_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let binding = read_call_site(ctx, stack.get(0))?;
    let base = read_document_base(ctx, stack.get(1))?;
    play_bound(ctx, state, &mut stack, 2, binding, base)
}

fn play_bound<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    binding: u64,
    base: Option<usize>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    enum PlayTarget {
        Voice(usize),
        Patch(usize),
    }
    let Value::UserData(target) = stack.get(argument_offset) else {
        return Err(binding_error(
            ctx,
            "play target must be a voice or patch handle",
        ));
    };
    let target = if let Ok(voice) = target.downcast_static::<LuaVoice>() {
        PlayTarget::Voice(voice.0)
    } else if let Ok(patch) = target.downcast_static::<LuaPatch>() {
        PlayTarget::Patch(patch.0)
    } else {
        return Err(binding_error(
            ctx,
            "play target must be a voice or patch handle",
        ));
    };
    let (pattern, routing, controls, external_trigger) = match stack.get(argument_offset + 1) {
        Value::String(source) => {
            let source = source
                .to_str()
                .map_err(|_| binding_error(ctx, "pattern source must be UTF-8"))?;
            (
                mini::parse_in(source, binding, base)
                    .map_err(|error| binding_error(ctx, error.to_string()))?,
                EventRouting::new(),
                Vec::new(),
                None,
            )
        }
        Value::UserData(data) => {
            let pattern = data
                .downcast_static::<LuaPattern>()
                .map_err(|_| binding_error(ctx, "play pattern must come from pattern()"))?;
            if pattern.hold_seconds.is_some() {
                return Err(binding_error(
                    ctx,
                    "hold(secs(...)) must be resolved inside at(...) and timeline {...}",
                ));
            }
            (
                pattern.pattern.clone(),
                pattern.routing.clone(),
                pattern.controls.clone(),
                pattern.external_trigger.clone(),
            )
        }
        _ => return Err(binding_error(ctx, "play expects a pattern or string")),
    };

    let mut state = state.borrow_mut();
    if state.program.tracks.len() >= state.limits.tracks {
        return Err(binding_error(
            ctx,
            format!("track limit of {} exceeded", state.limits.tracks),
        ));
    }
    match target {
        PlayTarget::Voice(voice) => {
            if voice >= state.program.voices.len() {
                return Err(binding_error(
                    ctx,
                    "voice handle does not belong to this edit",
                ));
            }
            let template = &state.program.voices[voice];
            let mut onset_bindings = Vec::new();
            let mut external_controls = Vec::new();
            for control in &controls {
                if let Some(source) = control.onset {
                    let param = if control.name == "hz" {
                        ParamId::Implicit(Implicit::Hz)
                    } else {
                        control_param_id(ctx, template, &control.name)?
                    };
                    onset_bindings.push(InitControlBinding {
                        param,
                        control: source,
                    });
                    continue;
                }
                if control.live.is_some() {
                    return Err(binding_error(
                        ctx,
                        format!(
                            "{} is a live program signal; ordinary polyphonic voice parameters are init-rate",
                            control.name
                        ),
                    ));
                }
                if let Some(value) = &control.value {
                    validate_voice_control(ctx, template, &control.name, value)?;
                } else {
                    control_param_id(ctx, template, &control.name)?;
                }
                if external_trigger.is_some() {
                    let param = control_param_id(ctx, template, &control.name)?;
                    let sample = control.sample.clone().ok_or_else(|| {
                        binding_error(
                            ctx,
                            format!(
                                "{} cannot be sampled at a live onset through this setter",
                                control.name
                            ),
                        )
                    })?;
                    external_controls.push(ExternalControlBinding {
                        param,
                        pattern: sample,
                    });
                }
            }
            state.spend_pattern(ctx, &pattern)?;
            let (external_trigger, external_degrades) = external_trigger
                .map(|external| (Some(external.control), external.degrades))
                .unwrap_or_default();
            state.program.tracks.push(Track {
                voice: VoiceId(voice),
                pattern,
                routing,
                onset_bindings,
                external_controls,
                external_degrades,
                external_trigger,
            });
        }
        PlayTarget::Patch(patch) => {
            if controls.iter().any(|control| control.onset.is_some()) {
                return Err(binding_error(
                    ctx,
                    "persistent patch controls remain live; at_onset is only for triggered voices",
                ));
            }
            if patch >= state.program.patches.len() {
                return Err(binding_error(
                    ctx,
                    "patch handle does not belong to this edit",
                ));
            }
            if state.program.patches[patch].graph().inputs != 0 {
                return Err(binding_error(
                    ctx,
                    "play(patch, pattern) currently requires an autonomous zero-input patch",
                ));
            }
            if routing.duck_control().is_some() {
                return Err(binding_error(
                    ctx,
                    "duck is not yet a persistent-patch routing control",
                ));
            }
            let extent = pattern.finite_extent().ok_or_else(|| {
                binding_error(
                    ctx,
                    "play(patch, pattern) requires finite timeline material so the persistent instance has an explicit lifetime",
                )
            })?;
            if extent.begin >= extent.end {
                return Err(binding_error(
                    ctx,
                    "persistent patch control pattern has an empty extent",
                ));
            }
            let driver = build_patch_control_driver(ctx, &state, patch, &controls)?;
            for _ in 0..driver.nodes.len() {
                state.spend_node(ctx)?;
            }
            driver
                .validate_limits(state.limits.graph_publication)
                .map_err(|error| binding_error(ctx, error.to_string()))?;
            if state.program.voices.len() >= state.limits.voices {
                return Err(binding_error(
                    ctx,
                    format!("voice limit of {} exceeded", state.limits.voices),
                ));
            }
            let driver_id = state.program.voices.len();
            state.program.voices.push(driver);
            state.program.runs.push(crate::PatchRun {
                patch: crate::PatchId(patch),
                span: Some(extent),
                routing,
            });
            state.spend_pattern(ctx, &pattern)?;
            state.program.tracks.push(Track {
                voice: VoiceId(driver_id),
                pattern,
                routing: EventRouting::new(),
                onset_bindings: Vec::new(),
                external_controls: Vec::new(),
                external_degrades: external_trigger
                    .as_ref()
                    .map_or_else(Vec::new, |external| external.degrades.clone()),
                external_trigger: external_trigger.map(|external| external.control),
            });
        }
    }
    stack.clear();
    Ok(CallbackReturn::Return)
}

fn build_patch_control_driver<'gc>(
    ctx: Context<'gc>,
    state: &BuildState,
    patch: usize,
    pattern_controls: &[PatternControl],
) -> Result<apteronotus_synth::GraphTemplate, Error<'gc>> {
    let controls = state
        .program
        .patch_controls
        .get(patch)
        .ok_or_else(|| binding_error(ctx, "patch control layout is missing"))?;
    let mut graph = GraphBuilder::new();
    let mut writers = Vec::with_capacity(controls.len());
    for control in controls {
        let spec = state
            .program
            .controls
            .spec(control.id)
            .ok_or_else(|| binding_error(ctx, "patch control does not belong to this edit"))?;
        let writer = match control.kind {
            PatchControlKind::Note => {
                let midi = graph.hz_to_midi(n::HZ);
                graph.write_control(midi, control.id)
            }
            PatchControlKind::Gate => {
                let gate = graph
                    .window(Source::Const(0.0), n::DURATION)
                    .map_err(|error| binding_error(ctx, error.to_string()))?;
                graph.write_control_with_release(gate, control.id, 0.002)
            }
            PatchControlKind::Number => {
                let mut param = ParamSpec::new(&control.name, spec.min, spec.max, spec.default);
                if let Some(unit) = &spec.unit {
                    param = param.with_unit(unit);
                }
                let sampled = graph.param(param);
                let source = pattern_controls
                    .iter()
                    .find(|candidate| candidate.name == control.name)
                    .and_then(|candidate| candidate.live)
                    .map(Source::Control)
                    .unwrap_or(sampled);
                graph.write_control(source, control.id)
            }
        };
        writers.push(writer);
    }

    let mut sink = Source::Const(0.0);
    for writer in writers {
        let muted = graph.mul(writer, Source::Const(0.0));
        sink = graph.add(sink, muted);
    }
    let outputs = vec![sink; state.program.buses.main_channels()];
    graph
        .out(&outputs)
        .map_err(|error| binding_error(ctx, error.to_string()))
}

fn merge_patterns<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    // Lua asks the right operand's `__shr` metamethod to handle
    // `"c4" >> setter(...)`. Accept the same mini-string shorthand here that
    // `play` accepts; the source pass wraps direct literals in `pattern_at`
    // when it can retain an exact construction-site identity.
    let left = numeric_pattern_operand(ctx, stack.get(0))?;
    let mut external_trigger = left.external_trigger.clone();
    let right = stack.get(1);
    let (pattern, controls, routing, hold_seconds) = if let Value::UserData(data) = right
        && let Ok(transform) = data.downcast_static::<LuaPatternTransform>()
    {
        let mut controls = left.controls.clone();
        transform.0.append_controls(&mut controls);
        let pattern = if let Some(external) = &mut external_trigger {
            match &transform.0 {
                PatternTransform::Degrade { amount, seed } => {
                    external.degrades.push(ExternalDegrade {
                        amount: *amount,
                        seed: *seed,
                    });
                    left.pattern.clone()
                }
                PatternTransform::MergeControl { .. } => left.pattern.clone(),
                _ => {
                    return Err(binding_error(
                        ctx,
                        "this cyclic pattern transform is not defined for a live trigger",
                    ));
                }
            }
        } else {
            transform
                .0
                .apply(left.pattern.clone())
                .map_err(|error| binding_error(ctx, error))?
        };
        (pattern, controls, left.routing.clone(), left.hold_seconds)
    } else if let Value::UserData(data) = right
        && let Ok(hold) = data.downcast_static::<LuaHold>()
    {
        if left.pattern.is_continuous_signal() {
            return Err(binding_error(ctx, "hold requires discrete event structure"));
        }
        if left.hold_seconds.is_some() || left.pattern.has_hold() {
            return Err(binding_error(
                ctx,
                "an event pattern may declare hold only once",
            ));
        }
        let (pattern, hold_seconds) = match cycle_hold_duration(hold.0) {
            Some(duration) => (left.pattern.clone().hold(duration), None),
            None => (left.pattern.clone(), Some(hold.0.value)),
        };
        (
            pattern,
            left.controls.clone(),
            left.routing.clone(),
            hold_seconds,
        )
    } else if let Value::UserData(data) = right
        && let Ok(send) = data.downcast_static::<LuaEventSend>()
    {
        let mut routing = left.routing.clone();
        routing
            .merge(&send.0)
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        (
            left.pattern.clone(),
            left.controls.clone(),
            routing,
            left.hold_seconds,
        )
    } else {
        let right = read_lua_pattern(ctx, right)?;
        if right.external_trigger.is_some() {
            return Err(binding_error(
                ctx,
                "a realtime trigger must provide the left event structure",
            ));
        }
        if right.pattern.has_note_phase_curve()
            && left.hold_seconds.is_none()
            && !left.pattern.has_hold()
        {
            return Err(binding_error(
                ctx,
                "phase-clock curves require an explicit hold(...) on the event pattern",
            ));
        }
        let pattern = left
            .pattern
            .clone()
            .merge(right.pattern.clone())
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        let mut controls = left.controls.clone();
        controls.extend(right.controls.clone());
        let mut routing = left.routing.clone();
        routing
            .merge(&right.routing)
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        (pattern, controls, routing, left.hold_seconds)
    };
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls,
                routing,
                external_trigger,
                hold_seconds,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn pattern_add<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    pattern_math(ctx, state, stack, PatternMathOp::Add)
}

fn pattern_sub<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    pattern_math(ctx, state, stack, PatternMathOp::Sub)
}

fn pattern_mul<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    pattern_math(ctx, state, stack, PatternMathOp::Mul)
}

fn pattern_div<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    pattern_math(ctx, state, stack, PatternMathOp::Div)
}

fn pattern_math<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    op: PatternMathOp,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        return Err(binding_error(
            ctx,
            "pattern-rate arithmetic cannot be used while staging a graph",
        ));
    }
    let left = numeric_pattern_operand(ctx, stack.get(0))?;
    let right = numeric_pattern_operand(ctx, stack.get(1))?;
    if !left.controls.is_empty()
        || !right.controls.is_empty()
        || !left.routing.sends().is_empty()
        || !right.routing.sends().is_empty()
        || left.external_trigger.is_some()
        || right.external_trigger.is_some()
    {
        return Err(binding_error(
            ctx,
            "arithmetic applies before control setters and score sends",
        ));
    }
    let pattern = left
        .pattern
        .math(op, right.pattern)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn pattern_neg<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        return Err(binding_error(
            ctx,
            "pattern-rate arithmetic cannot be used while staging a graph",
        ));
    }
    let value = numeric_pattern_operand(ctx, stack.get(0))?;
    if !value.controls.is_empty()
        || !value.routing.sends().is_empty()
        || value.external_trigger.is_some()
    {
        return Err(binding_error(
            ctx,
            "arithmetic applies before control setters and score sends",
        ));
    }
    let pattern = Pattern::signal(PatternSignal::Constant(-1.0))
        .math(PatternMathOp::Mul, value.pattern)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn numeric_pattern_operand<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<LuaPattern, Error<'gc>> {
    if let Some(number) = value.to_number() {
        if !number.is_finite() {
            return Err(binding_error(
                ctx,
                "pattern arithmetic requires finite numbers",
            ));
        }
        return Ok(LuaPattern {
            pattern: Pattern::signal(PatternSignal::Constant(number)),
            controls: Vec::new(),
            routing: EventRouting::new(),
            external_trigger: None,
            hold_seconds: None,
        });
    }
    match value {
        Value::String(source) => {
            let source = source
                .to_str()
                .map_err(|_| binding_error(ctx, "numeric pattern source must be UTF-8"))?;
            Ok(LuaPattern {
                pattern: mini::parse_in(source, 0, None)
                    .map_err(|error| binding_error(ctx, error.to_string()))?,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            })
        }
        Value::UserData(data) => data.downcast_static::<LuaPattern>().cloned().map_err(|_| {
            binding_error(
                ctx,
                "pattern arithmetic expects a number, mini string, or pattern-rate value",
            )
        }),
        _ => Err(binding_error(
            ctx,
            "pattern arithmetic expects a number, mini string, or pattern-rate value",
        )),
    }
}

fn fast_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let factor = read_pattern_ratio(ctx, stack.get(0), "fast factor")?;
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::Fast(factor))),
    );
    Ok(CallbackReturn::Return)
}

fn slow_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let factor = read_pattern_ratio(ctx, stack.get(0), "slow factor")?;
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::Slow(factor))),
    );
    Ok(CallbackReturn::Return)
}

fn shift_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let by = read_pattern_time(ctx, stack.get(0), "shift amount")?;
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::Shift(by))),
    );
    Ok(CallbackReturn::Return)
}

fn early_pattern<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if let Value::Table(delays) = stack.get(0) {
        if delays.length() == 0 {
            return Err(binding_error(
                ctx,
                "early reflection delays must not be empty",
            ));
        }
        let gain = read_number(ctx, stack.get(1), "early reflection gain")?;
        if !gain.is_finite() || gain < 0.0 {
            return Err(binding_error(
                ctx,
                "early reflection gain must be finite and non-negative",
            ));
        }
        let per_tap = gain / (delays.length() as f64).sqrt();
        let mut processor = None;
        for index in 1..=delays.length() {
            let seconds =
                read_seconds(ctx, delays.get_value(ctx, index), "early reflection delay")?;
            let range = DelayRange::fixed(seconds)
                .map_err(|error| binding_error(ctx, error.to_string()))?;
            let tap = Processor::Scale(
                Box::new(Processor::Delay {
                    seconds: Source::Const(seconds),
                    range,
                }),
                Source::Const(per_tap),
            );
            processor = Some(match processor {
                None => tap,
                Some(current) => Processor::Sum(Box::new(current), Box::new(tap)),
            });
        }
        stack.replace(
            ctx,
            lua_processor(
                ctx,
                processor_generation(state),
                processor.expect("nonempty early-reflection table was checked"),
            )?,
        );
        return Ok(CallbackReturn::Return);
    }
    let by = read_pattern_time(ctx, stack.get(0), "early amount")?;
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::Shift(-by))),
    );
    Ok(CallbackReturn::Return)
}

fn segment_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let steps = read_positive_i64(ctx, stack.get(0), "segment step count")?;
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::Segment(steps))),
    );
    Ok(CallbackReturn::Return)
}

/// Compile one explicitly bounded pattern period into a persistent control
/// source. Pattern querying stays on the evaluation thread; the resulting DSP
/// node owns only a flat slot table and never calls back into the algebra.
fn control_signal<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    {
        let state = state.borrow();
        if !matches!(
            state.active.as_ref().map(|active| active.lifetime),
            Some(ActiveLifetime::Patch)
        ) {
            return Err(binding_error(
                ctx,
                "control_signal is a transport-clock source for persistent patch graphs",
            ));
        }
    }
    let pattern = read_lua_pattern(ctx, stack.get(0))?;
    if !pattern.controls.is_empty()
        || !pattern.routing.sends().is_empty()
        || pattern.external_trigger.is_some()
        || pattern.hold_seconds.is_some()
    {
        return Err(binding_error(
            ctx,
            "control_signal expects a plain numeric pattern before setters, sends, and live triggers",
        ));
    }
    let period = read_pattern_time(ctx, stack.get(1), "control signal repeat period")?;
    if period <= Frac::ZERO {
        return Err(binding_error(
            ctx,
            "control signal repeat period must be positive",
        ));
    }
    let period_limit = state.borrow().limits.max_control_signal_cycles.max(0);
    if period > Frac::int(period_limit) {
        return Err(binding_error(
            ctx,
            format!(
                "control signal repeat period of {} cycles exceeds the compile-window limit of \
                 {period_limit} cycles",
                period.to_f64()
            ),
        ));
    }
    let seconds_per_cycle = {
        let state = state.borrow();
        match state.program.tempo.points() {
            [point] if point.over.is_none() => {
                60.0 * state.program.tempo.beats_per_cycle() / point.bpm
            }
            _ => {
                return Err(binding_error(
                    ctx,
                    "control_signal cannot compile a repeating seconds clock under changing tempo",
                ));
            }
        }
    };
    let span = Span::new(Frac::ZERO, period);
    let mut events = pattern
        .pattern
        .query(span)
        .into_iter()
        .filter_map(|event| {
            let whole = event.whole?;
            let clipped = span.sect(whole)?;
            if clipped.begin >= period {
                return None;
            }
            Some((clipped, event.value))
        })
        .collect::<Vec<_>>();
    let slot_limit = state.borrow().limits.pattern_nodes;
    if events.len() > slot_limit {
        return Err(binding_error(
            ctx,
            format!(
                "control signal has {} slots, more than the pattern limit of {slot_limit}",
                events.len()
            ),
        ));
    }
    events.sort_by_key(|(span, _)| (span.begin, span.end));
    let mut cursor = Frac::ZERO;
    let mut slots = Vec::with_capacity(events.len());
    for (slot, value) in events {
        if slot.begin != cursor {
            return Err(binding_error(
                ctx,
                "control_signal period must be covered exactly once without gaps or overlaps",
            ));
        }
        let Some(value) = value.as_f64() else {
            return Err(binding_error(
                ctx,
                "control_signal accepts only numeric pattern values",
            ));
        };
        if !value.is_finite() {
            return Err(binding_error(ctx, "control_signal values must be finite"));
        }
        slots.push(TransportSlot {
            begin_seconds: slot.begin.to_f64() * seconds_per_cycle,
            end_seconds: slot.end.to_f64() * seconds_per_cycle,
            value,
        });
        cursor = slot.end;
    }
    if cursor != period || slots.is_empty() {
        return Err(binding_error(
            ctx,
            "control_signal period must be covered exactly once without gaps or overlaps",
        ));
    }
    let period_seconds = period.to_f64() * seconds_per_cycle;
    // Ensure the serialized table and period share the exact same terminal
    // float; validation can then use equality without a fuzzy topology rule.
    slots
        .last_mut()
        .expect("nonempty slots were checked above")
        .end_seconds = period_seconds;

    let generation = current_generation(ctx, state)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state
        .active_mut(ctx)?
        .graph
        .transport_sequence(period_seconds, slots);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn at_onset<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        return Err(binding_error(
            ctx,
            "at_onset is an event-context rate boundary, not a graph processor",
        ));
    }
    let operand = shared_signal_operand(ctx, state, stack.get(0))
        .ok_or_else(|| binding_error(ctx, "at_onset expects a program-scope control signal"))??;
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaAtOnset(LuaSharedSignal {
                source: operand.source,
                control: operand.control,
                min: operand.min,
                max: operand.max,
                default: operand.default,
            }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn range_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let min = read_number(ctx, stack.get(0), "range minimum")?;
    let max = read_number(ctx, stack.get(1), "range maximum")?;
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::Range { min, max }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn degrade_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_degrade_transform(ctx, &mut stack, 0, 0)
}

fn degrade_pattern_at<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_degrade_transform(ctx, &mut stack, 1, call_site)
}

fn make_degrade_transform<'gc>(
    ctx: Context<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
    amount_index: usize,
    call_site: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let amount = read_probability(ctx, stack.get(amount_index), "degrade amount")?;
    let seed = source_seed(call_site);
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::Degrade { amount, seed }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn read_probability<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    what: &str,
) -> Result<f64, Error<'gc>> {
    let amount = read_number(ctx, value, what)?;
    if !(0.0..=1.0).contains(&amount) {
        return Err(binding_error(
            ctx,
            format!("{what} must be between zero and one"),
        ));
    }
    Ok(amount)
}

fn source_seed(call_site: u64) -> u64 {
    // The byte offset is already a stable identity within this evaluation.
    // Mix it once so nearby calls do not feed nearby raw seeds to the pattern
    // hash even though its finalizer would also decorrelate them.
    call_site
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_mul(0xbf58_476d_1ce4_e5b9)
}

fn every_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let cycles = read_positive_i64(ctx, stack.get(0), "every cycle count")?;
    let transform = read_pattern_operation(ctx, stack.get(1))?;
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::Every {
                cycles,
                transform: Box::new(transform),
            }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn off_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let by = read_pattern_time(ctx, stack.get(0), "off shift")?;
    let transform = read_pattern_operation(ctx, stack.get(1))?;
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::Off {
                by,
                transform: Box::new(transform),
            }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn sometimes_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_sometimes_transform(ctx, &mut stack, 0, 0)
}

fn sometimes_pattern_at<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_sometimes_transform(ctx, &mut stack, 1, call_site)
}

fn ply_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_ply_transform(ctx, &mut stack, 0, 0)
}

fn ply_pattern_at<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_ply_transform(ctx, &mut stack, 1, call_site)
}

fn arp_pattern<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let mode = match read_string(ctx, stack.get(0), "arp order")?.as_str() {
        "up" => ArpMode::Up,
        "down" => ArpMode::Down,
        "outside-in" => ArpMode::OutsideIn,
        "inside-out" => ArpMode::InsideOut,
        other => {
            return Err(binding_error(
                ctx,
                format!(
                    "unknown arp order {other:?}; expected up, down, outside-in, or inside-out"
                ),
            ));
        }
    };
    let spacing = if stack.len() >= 2 {
        let spacing = read_pattern_time(ctx, stack.get(1), "arp spacing")?;
        if spacing <= Frac::ZERO {
            return Err(binding_error(ctx, "arp spacing must be positive"));
        }
        Some(spacing)
    } else {
        None
    };
    if stack.len() > 2 {
        return Err(binding_error(
            ctx,
            "arp expects an order and optional spacing",
        ));
    }
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::Arp { mode, spacing }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn chord<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_chord(ctx, &mut stack, 0, 0)
}

fn chord_at<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_chord(ctx, &mut stack, 1, call_site)
}

fn make_chord<'gc>(
    ctx: Context<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
    source_index: usize,
    call_site: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let source = read_string(ctx, stack.get(source_index), "chord symbol")?;
    let chord = Chord::parse(&source).map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(
        ctx,
        lua_chord(
            ctx,
            LuaChord {
                chord,
                anchor: None,
                call_site,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn anchor<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let source = read_string(ctx, stack.get(0), "anchor note")?;
    let pitch = Pitch::parse(&source).map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, UserData::new_static(&ctx, LuaAnchor(pitch)));
    Ok(CallbackReturn::Return)
}

fn voicing<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let source = read_string(ctx, stack.get(0), "voicing shape")?;
    let shape =
        VoicingShape::parse(&source).map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, UserData::new_static(&ctx, LuaVoicing(shape)));
    Ok(CallbackReturn::Return)
}

fn chord_pipe<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(left) = stack.get(0) else {
        return Err(binding_error(ctx, "chord operator expects a chord builder"));
    };
    let mut chord = left
        .downcast_static::<LuaChord>()
        .map_err(|_| binding_error(ctx, "chord operator expects a chord builder"))?
        .clone();
    let Value::UserData(right) = stack.get(1) else {
        return Err(binding_error(
            ctx,
            "chord expects anchor(...) followed by voicing(...)",
        ));
    };

    if let Ok(anchor) = right.downcast_static::<LuaAnchor>() {
        if chord.anchor.replace(anchor.0).is_some() {
            return Err(binding_error(ctx, "chord anchor may only be declared once"));
        }
        stack.replace(ctx, lua_chord(ctx, chord)?);
        return Ok(CallbackReturn::Return);
    }

    let shape = right
        .downcast_static::<LuaVoicing>()
        .map_err(|_| binding_error(ctx, "chord expects anchor(...) followed by voicing(...)"))?
        .0;
    let anchor = chord
        .anchor
        .ok_or_else(|| binding_error(ctx, "voicing requires a preceding anchor(...)"))?;
    let src = call_site_span(chord.call_site, "chord");
    let root = chord.chord.root().semitones() as i32;
    let pitches = chord.chord.voice(anchor, shape);
    let primary = pitches
        .iter()
        .position(|pitch| (pitch.midi().round() as i32).rem_euclid(12) == root)
        .map(|index| index as u32);
    let members = pitches
        .into_iter()
        .map(|pitch| Pattern::primary_at(ControlValue::Number(pitch.midi()), src))
        .collect();
    let pattern = Pattern::group_with_primary(
        GroupNode::new(source_seed(chord.call_site)),
        members,
        primary,
    );
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn root_notes<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if !stack.is_empty() {
        return Err(binding_error(ctx, "root_notes does not accept arguments"));
    }
    stack.replace(
        ctx,
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::GroupPrimary)),
    );
    Ok(CallbackReturn::Return)
}

fn octave<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let octaves = read_number(ctx, stack.get(0), "octave shift")?;
    if octaves.fract() != 0.0 {
        return Err(binding_error(ctx, "octave shift must be a whole number"));
    }
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::PrimaryAdd(octaves * 12.0)),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn make_sometimes_transform<'gc>(
    ctx: Context<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    call_site: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let amount = read_probability(ctx, stack.get(argument_offset), "sometimes probability")?;
    let transform = read_pattern_operation(ctx, stack.get(argument_offset + 1))?;
    let seed = source_seed(call_site);
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::Sometimes {
                amount,
                seed,
                transform: Box::new(transform),
            }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn make_ply_transform<'gc>(
    ctx: Context<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    call_site: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let counts = match stack.get(argument_offset) {
        value if value.to_number().is_some() => {
            vec![read_ply_count(ctx, value, "ply count")?]
        }
        Value::String(source) => {
            let source = source
                .to_str()
                .map_err(|_| binding_error(ctx, "ply count choice must be UTF-8"))?;
            let mut counts = Vec::new();
            for choice in source.split('|') {
                let choice = choice.trim();
                if choice.is_empty() {
                    return Err(binding_error(
                        ctx,
                        "ply count choice contains an empty branch",
                    ));
                }
                let count = choice.parse::<u32>().map_err(|_| {
                    binding_error(ctx, format!("invalid ply count choice {choice:?}"))
                })?;
                if count == 0 || i64::from(count) > mini::limits::COUNT {
                    return Err(binding_error(
                        ctx,
                        format!("ply count must be between 1 and {}", mini::limits::COUNT),
                    ));
                }
                counts.push(count);
            }
            if counts.len() > 64 {
                return Err(binding_error(
                    ctx,
                    "ply count choice has more than 64 branches",
                ));
            }
            counts
        }
        _ => {
            return Err(binding_error(
                ctx,
                "ply expects a positive whole count or a `2 | 6` count choice",
            ));
        }
    };
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            LuaPatternTransform(PatternTransform::Ply {
                counts,
                seed: source_seed(call_site),
            }),
        ),
    );
    Ok(CallbackReturn::Return)
}

fn read_ply_count<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    what: &str,
) -> Result<u32, Error<'gc>> {
    let count = read_number(ctx, value, what)?;
    if count.fract() != 0.0 || count < 1.0 || count > mini::limits::COUNT as f64 {
        return Err(binding_error(
            ctx,
            format!(
                "{what} must be a whole number between 1 and {}",
                mini::limits::COUNT
            ),
        ));
    }
    Ok(count as u32)
}

fn voice_index<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(ctx, "parameter setter requires a voice"));
    };
    let voice = data
        .downcast_static::<LuaVoice>()
        .map_err(|_| binding_error(ctx, "parameter setter requires a voice"))?
        .0;
    let name = read_string(ctx, stack.get(1), "voice parameter name")?;
    {
        let state = state.borrow();
        let template = state
            .program
            .voices
            .get(voice)
            .ok_or_else(|| binding_error(ctx, "voice handle does not belong to this edit"))?;
        if name != "hz" {
            control_param_id(ctx, template, &name)?;
        }
    }

    let state = state.clone();
    let callback_name = name.clone();
    let callback = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        if let Value::UserData(data) = stack.get(0)
            && let Ok(sample) = data.downcast_static::<LuaAtOnset>()
        {
            validate_onset_control(ctx, &state, Some(voice), &callback_name, sample.0)?;
            return replace_with_onset_control(
                ctx,
                &state,
                &mut stack,
                &callback_name,
                sample.0,
                callback_name == "hz",
            );
        }
        if callback_name == "hz" {
            return Err(binding_error(
                ctx,
                "hz requires at_onset(...) so literal Hertz is not retuned as MIDI",
            ));
        }
        if let Ok(value) = read_setter_control_value(ctx, stack.get(0)) {
            let borrowed = state.borrow();
            let template =
                borrowed.program.voices.get(voice).ok_or_else(|| {
                    binding_error(ctx, "voice handle does not belong to this edit")
                })?;
            validate_voice_control(ctx, template, &callback_name, &value)?;
            drop(borrowed);
            return replace_with_named_control(ctx, &state, &mut stack, &callback_name, value);
        }
        replace_with_named_pattern_input(ctx, &state, &mut stack, &callback_name)
    });
    stack.replace(ctx, callback);
    Ok(CallbackReturn::Return)
}

fn patch_index<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(ctx, "parameter setter requires a patch"));
    };
    let patch = data
        .downcast_static::<LuaPatch>()
        .map_err(|_| binding_error(ctx, "parameter setter requires a patch"))?
        .0;
    let name = read_string(ctx, stack.get(1), "patch parameter name")?;
    let (control, spec) = {
        let borrowed = state.borrow();
        let control = borrowed
            .program
            .patch_controls
            .get(patch)
            .and_then(|controls| controls.iter().find(|control| control.name == name))
            .cloned()
            .ok_or_else(|| binding_error(ctx, format!("patch has no parameter {name:?}")))?;
        let spec = borrowed
            .program
            .controls
            .spec(control.id)
            .cloned()
            .ok_or_else(|| binding_error(ctx, "patch control does not belong to this edit"))?;
        (control, spec)
    };

    let state = state.clone();
    let callback = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        if control.kind != PatchControlKind::Number {
            return Err(binding_error(
                ctx,
                format!(
                    "patch {} is driven by the event pitch/gate and is not an ordinary numeric setter",
                    control.name
                ),
            ));
        }
        if let Value::UserData(data) = stack.get(0)
            && let Ok(signal) = data.downcast_static::<LuaSharedSignal>()
        {
            if signal.min < spec.min || signal.max > spec.max {
                return Err(binding_error(
                    ctx,
                    format!(
                        "{} live range {}..{} is outside {}..{}",
                        control.name, signal.min, signal.max, spec.min, spec.max
                    ),
                ));
            }
            return replace_with_live_control(ctx, &state, &mut stack, &control.name, *signal);
        }
        if let Ok(value) = read_setter_control_value(ctx, stack.get(0)) {
            validate_patch_control_value(ctx, &control.name, &spec, &value)?;
            return replace_with_named_control(ctx, &state, &mut stack, &control.name, value);
        }
        replace_with_named_pattern_input(ctx, &state, &mut stack, &control.name)
    });
    stack.replace(ctx, callback);
    Ok(CallbackReturn::Return)
}

fn validate_patch_control_value<'gc>(
    ctx: Context<'gc>,
    name: &str,
    spec: &ControlSpec,
    value: &ControlValue,
) -> Result<(), Error<'gc>> {
    match value {
        ControlValue::Number(value) if (spec.min..=spec.max).contains(value) => Ok(()),
        ControlValue::Number(value) => Err(binding_error(
            ctx,
            format!("{name} value {value} is outside {}..{}", spec.min, spec.max),
        )),
        ControlValue::Curve(curve) => match curve
            .prove_range(spec.min, spec.max)
            .map_err(|error| binding_error(ctx, error.to_string()))?
        {
            RangeProof::Safe { .. } => Ok(()),
            RangeProof::Unsafe { proven } => Err(binding_error(
                ctx,
                format!(
                    "{name} curve reaches {}..{}, outside {}..{}",
                    proven.min, proven.max, spec.min, spec.max
                ),
            )),
            RangeProof::Inconclusive { enclosure } => Err(binding_error(
                ctx,
                format!(
                    "{name} curve range {}..{} cannot be proven inside {}..{}; restructure the curve",
                    enclosure.min, enclosure.max, spec.min, spec.max
                ),
            )),
        },
        ControlValue::Text(_) | ControlValue::Bool(_) => Err(binding_error(
            ctx,
            format!("{name} expects a number or curve"),
        )),
    }
}

fn velocity<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if let Value::UserData(data) = stack.get(0)
        && let Ok(sample) = data.downcast_static::<LuaAtOnset>()
    {
        validate_onset_control(ctx, state, None, "velocity", sample.0)?;
        return replace_with_onset_control(ctx, state, &mut stack, "velocity", sample.0, false);
    }
    if let Ok(value) = read_setter_control_value(ctx, stack.get(0)) {
        replace_with_named_control(ctx, state, &mut stack, "velocity", value)
    } else {
        replace_with_named_pattern_input(ctx, state, &mut stack, "velocity")
    }
}

fn note_pattern<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let value = match stack.get(0) {
        Value::String(value) => ControlValue::Text(
            value
                .to_str()
                .map_err(|_| binding_error(ctx, "note name must be UTF-8"))?
                .to_owned(),
        ),
        value if value.to_number().is_some() => {
            ControlValue::Number(read_number(ctx, value, "MIDI note")?)
        }
        _ => {
            return Err(binding_error(
                ctx,
                "note expects a note name or MIDI number",
            ));
        }
    };
    let pattern = Pattern::primary(value);
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn phase<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let value = read_number(ctx, stack.get(0), "note phase")?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(binding_error(ctx, "phase must be finite and within 0..1"));
    }
    stack.replace(ctx, UserData::new_static(&ctx, LuaPhase(value)));
    Ok(CallbackReturn::Return)
}

fn event_curve<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(table) = stack.get(0) else {
        return Err(binding_error(ctx, "curve expects a table"));
    };
    if state.borrow().active.is_some() && table.get_value(ctx, "clock").is_nil() {
        let generation = current_generation(ctx, state)?;
        if seconds_curve_has_symbolic_time(ctx, table) {
            let points = read_symbolic_seconds_breakpoints(ctx, table, generation)?;
            let mut state = state.borrow_mut();
            state.spend_node(ctx)?;
            let source = state
                .active_mut(ctx)?
                .graph
                .breakpoint_curve(&points)
                .map_err(|error| binding_error(ctx, error.to_string()))?;
            stack.replace(ctx, lua_source(ctx, generation, [source])?);
            return Ok(CallbackReturn::Return);
        }
        let curve = read_seconds_breakpoint_curve(ctx, table)?;
        let mut state = state.borrow_mut();
        state.spend_node(ctx)?;
        let source = state.active_mut(ctx)?.graph.curve(curve);
        stack.replace(ctx, lua_source(ctx, generation, [source])?);
        return Ok(CallbackReturn::Return);
    }
    if table.get_value(ctx, "clock").is_nil() {
        let curve = read_phase_breakpoint_curve(ctx, table)?;
        stack.replace(ctx, UserData::new_static(&ctx, LuaCurve(curve)));
        return Ok(CallbackReturn::Return);
    }
    let clock = match read_string(ctx, table.get_value(ctx, "clock"), "curve clock")?.as_str() {
        "note_seconds" => CurveClock::NoteSeconds,
        "note_phase" => CurveClock::NotePhase,
        other => {
            return Err(binding_error(
                ctx,
                format!("unknown curve clock {other:?}; use note_seconds or note_phase"),
            ));
        }
    };
    let offset = optional_number(ctx, table.get_value(ctx, "offset"), 0.0, "curve offset")?;
    let term_table = match table.get_value(ctx, "terms") {
        Value::Nil => table,
        Value::Table(terms) => terms,
        _ => return Err(binding_error(ctx, "curve.terms must be a table")),
    };
    let mut terms = Vec::new();
    for (key, value) in term_table {
        let index = match key {
            Value::Integer(index) if index > 0 => index,
            Value::Number(index) if index > 0.0 && index.fract() == 0.0 => index as i64,
            _ => continue,
        };
        let Value::Table(term) = value else {
            return Err(binding_error(
                ctx,
                format!("curve term {index} must be a table"),
            ));
        };
        terms.push((index, read_curve_term(ctx, term, index)?));
    }
    terms.sort_by_key(|(index, _)| *index);
    let curve = terms
        .into_iter()
        .fold(Curve::new(clock, offset), |curve, (_, term)| {
            curve.term(term.basis, term.coefficient, term.delay, term.length)
        });
    curve
        .validate()
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, UserData::new_static(&ctx, LuaCurve(curve)));
    Ok(CallbackReturn::Return)
}

fn seconds_curve_has_symbolic_time<'gc>(ctx: Context<'gc>, table: Table<'gc>) -> bool {
    (1..=table.length()).any(|index| {
        let Value::Table(point) = table.get_value(ctx, index) else {
            return false;
        };
        let Value::UserData(data) = point.get_value(ctx, 1) else {
            return false;
        };
        data.downcast_static::<LuaSource>().is_ok()
    })
}

fn read_symbolic_seconds_breakpoints<'gc>(
    ctx: Context<'gc>,
    table: Table<'gc>,
    generation: u64,
) -> Result<Vec<(Source, f64)>, Error<'gc>> {
    if table.length() == 0 {
        return Err(binding_error(
            ctx,
            "graph breakpoint curve requires at least one { seconds, value } point",
        ));
    }
    let mut points = Vec::with_capacity(table.length() as usize);
    for index in 1..=table.length() {
        let Value::Table(point) = table.get_value(ctx, index) else {
            return Err(binding_error(
                ctx,
                format!("breakpoint curve point {index} must be a table"),
            ));
        };
        let time = read_mono(ctx, point.get_value(ctx, 1), generation).map_err(|_| {
            binding_error(
                ctx,
                format!(
                    "breakpoint curve point {index} time must be seconds or note-parameter scalar arithmetic"
                ),
            )
        })?;
        let value = read_number(
            ctx,
            point.get_value(ctx, 2),
            &format!("breakpoint curve point {index} value"),
        )?;
        points.push((time, value));
    }
    Ok(points)
}

fn read_seconds_breakpoint_curve<'gc>(
    ctx: Context<'gc>,
    table: Table<'gc>,
) -> Result<Curve, Error<'gc>> {
    let length = table.length();
    if length == 0 {
        return Err(binding_error(
            ctx,
            "graph breakpoint curve requires at least one { seconds, value } point",
        ));
    }
    let mut points = Vec::with_capacity(length as usize);
    for index in 1..=length {
        let Value::Table(point) = table.get_value(ctx, index) else {
            return Err(binding_error(
                ctx,
                format!("breakpoint curve point {index} must be a table"),
            ));
        };
        let time = read_seconds(
            ctx,
            point.get_value(ctx, 1),
            &format!("breakpoint curve point {index} time"),
        )
        .map_err(|_| {
            binding_error(
                ctx,
                format!(
                    "breakpoint curve point {index} time must be fixed seconds; \
                     symbolic breakpoint times are not implemented yet"
                ),
            )
        })?;
        let value = read_number(
            ctx,
            point.get_value(ctx, 2),
            &format!("breakpoint curve point {index} value"),
        )?;
        if time < 0.0 {
            return Err(binding_error(
                ctx,
                format!("breakpoint curve point {index} time cannot be negative"),
            ));
        }
        if let Some((previous, _)) = points.last()
            && time <= *previous
        {
            return Err(binding_error(
                ctx,
                format!("breakpoint curve point {index} must be later than the previous point"),
            ));
        }
        points.push((time, value));
    }

    let mut curve = Curve::new(CurveClock::NoteSeconds, points[0].1);
    for pair in points.windows(2) {
        let (begin, from) = pair[0];
        let (end, to) = pair[1];
        curve = curve.term(Basis::Ramp, to - from, begin, end - begin);
    }
    curve
        .validate()
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    Ok(curve)
}

fn read_phase_breakpoint_curve<'gc>(
    ctx: Context<'gc>,
    table: Table<'gc>,
) -> Result<Curve, Error<'gc>> {
    let length = table.length();
    if length == 0 {
        return Err(binding_error(
            ctx,
            "breakpoint curve requires at least one { phase(...), value } point",
        ));
    }
    let mut points = Vec::with_capacity(length as usize);
    for index in 1..=length {
        let Value::Table(point) = table.get_value(ctx, index) else {
            return Err(binding_error(
                ctx,
                format!("breakpoint curve point {index} must be a table"),
            ));
        };
        let Value::UserData(time) = point.get_value(ctx, 1) else {
            return Err(binding_error(
                ctx,
                format!("breakpoint curve point {index} time must use phase(...)"),
            ));
        };
        let time = time
            .downcast_static::<LuaPhase>()
            .map_err(|_| {
                binding_error(
                    ctx,
                    format!("breakpoint curve point {index} time must use phase(...)"),
                )
            })?
            .0;
        let value = read_number(
            ctx,
            point.get_value(ctx, 2),
            &format!("breakpoint curve point {index} value"),
        )?;
        if let Some((previous, _)) = points.last()
            && time <= *previous
        {
            return Err(binding_error(
                ctx,
                format!("breakpoint curve point {index} must be later than the previous point"),
            ));
        }
        points.push((time, value));
    }

    let mut curve = Curve::new(CurveClock::NotePhase, points[0].1);
    for pair in points.windows(2) {
        let (begin, from) = pair[0];
        let (end, to) = pair[1];
        curve = curve.term(Basis::Ramp, to - from, begin, end - begin);
    }
    curve
        .validate()
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    Ok(curve)
}

fn read_curve_term<'gc>(
    ctx: Context<'gc>,
    term: Table<'gc>,
    index: i64,
) -> Result<apteronotus_pattern::CurveTerm, Error<'gc>> {
    let field = |name, position| {
        let named = term.get_value(ctx, name);
        if named.is_nil() {
            term.get_value(ctx, position)
        } else {
            named
        }
    };
    let basis_name = read_string(ctx, field("basis", 1), "curve basis")?;
    let basis = match basis_name.as_str() {
        "step" => Basis::Step,
        "ramp" | "line" => Basis::Ramp,
        "decay" => Basis::Decay,
        "sine" => Basis::Sine,
        other => {
            return Err(binding_error(
                ctx,
                format!("curve term {index} has unknown basis {other:?}"),
            ));
        }
    };
    Ok(apteronotus_pattern::CurveTerm {
        basis,
        coefficient: optional_number(ctx, field("coefficient", 2), 1.0, "curve coefficient")?,
        delay: optional_number(ctx, field("delay", 3), 0.0, "curve delay")?,
        length: optional_number(ctx, field("length", 4), 0.0, "curve length")?,
    })
}

fn optional_number<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    default: f64,
    what: &str,
) -> Result<f64, Error<'gc>> {
    if value.is_nil() {
        Ok(default)
    } else {
        read_number(ctx, value, what)
    }
}

fn replace_with_named_control<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    name: &str,
    value: ControlValue,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let sample = Pattern::pure(PatternValue::Leaf(value.clone()));
    let pattern = sample
        .clone()
        .named(name)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: vec![PatternControl {
                    name: name.to_owned(),
                    value: Some(value),
                    live: None,
                    onset: None,
                    sample: Some(sample),
                }],
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn replace_with_live_control<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    name: &str,
    signal: LuaSharedSignal,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    // The scalar is only a query-visible placeholder preserving ordinary map
    // structure. The patch driver recognizes `live` and binds the retained
    // control directly, so this value is never sampled into the destination.
    let value = ControlValue::Number(signal.default);
    let pattern = Pattern::pure(PatternValue::Leaf(value.clone()))
        .named(name)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: vec![PatternControl {
                    name: name.to_owned(),
                    value: Some(value),
                    live: Some(signal.control),
                    onset: None,
                    sample: None,
                }],
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn validate_onset_control<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    voice: Option<usize>,
    name: &str,
    signal: LuaSharedSignal,
) -> Result<(), Error<'gc>> {
    let state = state.borrow();
    let source = state
        .program
        .controls
        .spec(signal.control)
        .ok_or_else(|| binding_error(ctx, "onset signal does not belong to this edit"))?;
    let (min, max) = match name {
        "hz" => {
            if source.min <= 0.0 {
                return Err(binding_error(
                    ctx,
                    "onset-sampled Hertz must have a proven positive range",
                ));
            }
            return Ok(());
        }
        "velocity" => (0.0, 1.0),
        name => {
            let voice = voice.ok_or_else(|| {
                binding_error(ctx, format!("{name} requires a voice-scoped setter"))
            })?;
            let template =
                state.program.voices.get(voice).ok_or_else(|| {
                    binding_error(ctx, "voice handle does not belong to this edit")
                })?;
            let ParamId::Declared(index) = control_param_id(ctx, template, name)? else {
                return Err(binding_error(
                    ctx,
                    format!("{name} cannot be sampled from a live onset signal"),
                ));
            };
            let spec = &template.params[index];
            (spec.min, spec.max)
        }
    };
    if source.min < min || source.max > max {
        return Err(binding_error(
            ctx,
            format!(
                "{name} onset signal range {}..{} is outside {min}..{max}",
                source.min, source.max
            ),
        ));
    }
    Ok(())
}

fn replace_with_onset_control<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    name: &str,
    signal: LuaSharedSignal,
    primary_hz: bool,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let pattern = if primary_hz {
        // Hertz is a typed init binding, not a generic field and not a MIDI
        // value. The realtime-trigger fallback is silence, so no query-time
        // primary replacement is required or permitted.
        Pattern::Silence
    } else {
        Pattern::pure(PatternValue::Leaf(ControlValue::Number(signal.default)))
            .named(name)
            .map_err(|error| binding_error(ctx, error.to_string()))?
    };
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: vec![PatternControl {
                    name: name.to_owned(),
                    value: None,
                    live: None,
                    onset: Some(signal.control),
                    sample: None,
                }],
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn replace_with_named_pattern_input<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    name: &str,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let input = numeric_pattern_operand(ctx, stack.get(0))?;
    if !input.controls.is_empty() {
        return Err(binding_error(
            ctx,
            "a control setter cannot wrap another named control pattern",
        ));
    }
    let sample = input.pattern;
    let pattern = sample
        .clone()
        .named(name)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: vec![PatternControl {
                    name: name.to_owned(),
                    value: None,
                    live: None,
                    onset: None,
                    sample: Some(sample),
                }],
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn read_control_value<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<ControlValue, Error<'gc>> {
    if value.to_number().is_some() {
        return read_number(ctx, value, "control value").map(ControlValue::Number);
    }
    match value {
        Value::String(value) => value
            .to_str()
            .map(|value| ControlValue::Text(value.to_owned()))
            .map_err(|_| binding_error(ctx, "control text must be UTF-8")),
        Value::Boolean(value) => Ok(ControlValue::Bool(value)),
        Value::UserData(data) => data
            .downcast_static::<LuaCurve>()
            .map(|curve| ControlValue::Curve(curve.0.clone()))
            .map_err(|_| binding_error(ctx, "control value must be scalar or curve")),
        _ => Err(binding_error(
            ctx,
            "control value must be a number, string, boolean or curve",
        )),
    }
}

fn read_setter_control_value<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<ControlValue, Error<'gc>> {
    if matches!(value, Value::String(_)) {
        return Err(binding_error(
            ctx,
            "setter strings are numeric mini-patterns",
        ));
    }
    read_control_value(ctx, value)
}

fn control_param_id<'gc>(
    ctx: Context<'gc>,
    template: &apteronotus_synth::GraphTemplate,
    name: &str,
) -> Result<ParamId, Error<'gc>> {
    match name {
        "velocity" => Ok(ParamId::Implicit(Implicit::Velocity)),
        "pan" => Ok(ParamId::Implicit(Implicit::Pan)),
        "duration" => Err(binding_error(
            ctx,
            "duration is determined by the event span; a live gate is separate",
        )),
        "hz" => Err(binding_error(
            ctx,
            "hz is reserved until typed frequency controls are implemented",
        )),
        name => template
            .params
            .iter()
            .position(|spec| spec.name == name)
            .map(ParamId::Declared)
            .ok_or_else(|| binding_error(ctx, format!("voice has no parameter named {name:?}"))),
    }
}

fn validate_voice_control<'gc>(
    ctx: Context<'gc>,
    template: &apteronotus_synth::GraphTemplate,
    name: &str,
    value: &ControlValue,
) -> Result<(), Error<'gc>> {
    let id = control_param_id(ctx, template, name)?;
    let param = match value {
        ControlValue::Number(value) => {
            let (min, max) = match id {
                ParamId::Implicit(Implicit::Velocity) => (0.0, 1.0),
                ParamId::Implicit(Implicit::Pan) => (-1.0, 1.0),
                ParamId::Declared(index) => {
                    let spec = &template.params[index];
                    (spec.min, spec.max)
                }
                ParamId::Implicit(Implicit::Hz | Implicit::Duration) => unreachable!(),
            };
            if !(min..=max).contains(value) {
                return Err(binding_error(
                    ctx,
                    format!("{name} value {value} is outside {min}..{max}"),
                ));
            }
            ParamValue::Number(*value)
        }
        ControlValue::Curve(curve) => ParamValue::Curve(curve.clone()),
        ControlValue::Text(_) | ControlValue::Bool(_) => {
            return Err(binding_error(
                ctx,
                format!("{name} expects a number or curve"),
            ));
        }
    };
    Note::new(440.0)
        .duration(1.0)
        .bind(id, param)
        .value(id, template)
        .map(|_| ())
        .map_err(|error| binding_error(ctx, error.to_string()))
}

fn read_lua_pattern<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<&'gc LuaPattern, Error<'gc>> {
    let Value::UserData(data) = value else {
        return Err(binding_error(
            ctx,
            "pattern operator expects a control pattern or pattern transform",
        ));
    };
    data.downcast_static::<LuaPattern>().map_err(|_| {
        binding_error(
            ctx,
            "pattern operator expects a control pattern or pattern transform",
        )
    })
}

fn read_pattern_operation<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<PatternTransform, Error<'gc>> {
    let Value::UserData(data) = value else {
        return Err(binding_error(
            ctx,
            "expected a pattern transform or control setter",
        ));
    };
    if let Ok(transform) = data.downcast_static::<LuaPatternTransform>() {
        return Ok(transform.0.clone());
    }
    let control = data
        .downcast_static::<LuaPattern>()
        .map_err(|_| binding_error(ctx, "expected a pattern transform or control setter"))?;
    if control.controls.is_empty()
        || !control.routing.sends().is_empty()
        || control.routing.duck_control().is_some()
        || control.hold_seconds.is_some()
    {
        return Err(binding_error(
            ctx,
            "nested pattern operation must be a plain control setter",
        ));
    }
    Ok(PatternTransform::MergeControl {
        pattern: control.pattern.clone(),
        controls: control.controls.clone(),
    })
}

fn sine<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_sine(ctx, state, stack, 0, None)
}

fn sine_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_sine(
        ctx,
        state,
        stack,
        1,
        Some(call_site_span(call_site, "sine")),
    )
}

fn make_sine<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        return periodic_pattern(
            ctx,
            state,
            &mut stack,
            PatternSignal::Sine,
            "sine frequency",
            argument_offset,
            src,
        );
    }
    let generation = current_generation(ctx, state)?;
    if stack.len() == argument_offset {
        stack.replace(ctx, lua_processor(ctx, generation, Processor::Sine)?);
        return Ok(CallbackReturn::Return);
    }
    let input = read_mono(ctx, stack.get(argument_offset), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.sine(input);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn saw<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_saw(ctx, state, stack, 0, None)
}

fn zero<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        return Err(binding_error(ctx, "zero is a graph-rate source"));
    }
    if !stack.is_empty() {
        return Err(binding_error(ctx, "zero expects no arguments"));
    }
    let generation = current_generation(ctx, state)?;
    stack.replace(ctx, lua_source(ctx, generation, [Source::Const(0.0)])?);
    Ok(CallbackReturn::Return)
}

fn soft_saw<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let hz = read_mono(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    for _ in 0..5 {
        state.spend_node(ctx)?;
    }
    let graph = &mut state.active_mut(ctx)?.graph;
    let saw = graph.saw(hz);
    let fundamental = graph.sine(hz);
    let saw = graph.mul(saw, 0.72);
    let fundamental = graph.mul(fundamental, 0.28);
    let source = graph.add(saw, fundamental);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn saw_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_saw(ctx, state, stack, 1, Some(call_site_span(call_site, "saw")))
}

fn make_saw<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        return periodic_pattern(
            ctx,
            state,
            &mut stack,
            PatternSignal::Saw,
            "saw frequency",
            argument_offset,
            src,
        );
    }
    let generation = current_generation(ctx, state)?;
    if stack.len() == argument_offset {
        stack.replace(ctx, lua_processor(ctx, generation, Processor::Saw)?);
        return Ok(CallbackReturn::Return);
    }
    let input = read_mono(ctx, stack.get(argument_offset), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.saw(input);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn cosine<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_cosine(ctx, state, stack, 0, None)
}

fn cosine_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_cosine(
        ctx,
        state,
        stack,
        1,
        Some(call_site_span(call_site, "cosine")),
    )
}

fn make_cosine<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        let generation = current_generation(ctx, state)?;
        if stack.len() == argument_offset {
            stack.replace(ctx, lua_processor(ctx, generation, Processor::Cosine)?);
            return Ok(CallbackReturn::Return);
        }
        let input = read_mono(ctx, stack.get(argument_offset), generation)?;
        let mut state = state.borrow_mut();
        state.spend_node(ctx)?;
        let source = state.active_mut(ctx)?.graph.cosine(input);
        stack.replace(ctx, lua_source(ctx, generation, [source])?);
        return Ok(CallbackReturn::Return);
    }
    periodic_pattern(
        ctx,
        state,
        &mut stack,
        PatternSignal::Cosine,
        "cosine frequency",
        argument_offset,
        src,
    )
}

fn perlin<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_perlin(ctx, state, &mut stack, 0, 0, None)
}

fn perlin_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_perlin(
        ctx,
        state,
        &mut stack,
        1,
        source_seed(call_site),
        Some(call_site_span(call_site, "perlin")),
    )
}

fn make_perlin<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    frequency_index: usize,
    seed: u64,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        return Err(binding_error(
            ctx,
            "perlin is a pattern-rate transport signal",
        ));
    }
    let frequency = read_number(ctx, stack.get(frequency_index), "perlin frequency")?;
    periodic_pattern_at(
        ctx,
        state,
        stack,
        PatternSignal::Perlin(seed),
        frequency,
        src,
    )
}

fn periodic_pattern<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    signal: PatternSignal,
    what: &str,
    argument_offset: usize,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let frequency = if stack.len() == argument_offset {
        1.0
    } else {
        read_number(ctx, stack.get(argument_offset), what)?
    };
    periodic_pattern_at(ctx, state, stack, signal, frequency, src)
}

fn periodic_pattern_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    signal: PatternSignal,
    frequency: f64,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if frequency <= 0.0 {
        return Err(binding_error(
            ctx,
            "transport signal frequency must be positive",
        ));
    }
    let pattern = match src {
        Some(src) => Pattern::signal_at(signal, src),
        None => Pattern::signal(signal),
    }
    .fast(Frac::approx(frequency, 1_000_000));
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn scale<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        let min = read_number(ctx, stack.get(0), "scale minimum")?;
        let max = read_number(ctx, stack.get(1), "scale maximum")?;
        if let Some(input) = shared_signal_operand(ctx, state, stack.get(2)) {
            let input = input?;
            let mut state = state.borrow_mut();
            for _ in 0..3 {
                state.spend_node(ctx)?;
            }
            let range = state
                .shared_graph
                .sub(Source::Const(max), Source::Const(min));
            let scaled = state.shared_graph.mul(range, input.source);
            let source = state.shared_graph.add(Source::Const(min), scaled);
            let at_min = min + (max - min) * input.min;
            let at_max = min + (max - min) * input.max;
            let signal = publish_shared_signal(
                ctx,
                &mut state,
                source,
                at_min.min(at_max),
                at_min.max(at_max),
                min + (max - min) * input.default,
            )?;
            stack.replace(ctx, lua_shared_signal(ctx, signal)?);
            return Ok(CallbackReturn::Return);
        }
        let input = numeric_pattern_operand(ctx, stack.get(2))?;
        if !input.controls.is_empty() || !input.routing.sends().is_empty() {
            return Err(binding_error(
                ctx,
                "scale applies before control setters and score sends",
            ));
        }
        let pattern = input.pattern.range(min, max);
        state.borrow_mut().spend_pattern(ctx, &pattern)?;
        stack.replace(
            ctx,
            lua_pattern(
                ctx,
                LuaPattern {
                    pattern,
                    controls: Vec::new(),
                    routing: EventRouting::new(),
                    external_trigger: None,
                    hold_seconds: None,
                },
            )?,
        );
        return Ok(CallbackReturn::Return);
    }

    let generation = current_generation(ctx, state)?;
    let min = read_mono(ctx, stack.get(0), generation)?;
    let max = read_mono(ctx, stack.get(1), generation)?;
    let input = read_mono(ctx, stack.get(2), generation)?;
    let mut state = state.borrow_mut();
    for _ in 0..3 {
        state.spend_node(ctx)?;
    }
    let range = state.active_mut(ctx)?.graph.sub(max, min);
    let scaled = state.active_mut(ctx)?.graph.mul(range, input);
    let source = state.active_mut(ctx)?.graph.add(min, scaled);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

#[derive(Clone, Copy)]
struct SharedOperand {
    source: Source,
    control: ControlId,
    min: f64,
    max: f64,
    default: f64,
}

fn shared_signal_operand<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    value: Value<'gc>,
) -> Option<Result<SharedOperand, Error<'gc>>> {
    let Value::UserData(data) = value else {
        return None;
    };
    if let Ok(signal) = data.downcast_static::<LuaSharedSignal>() {
        return Some(Ok(SharedOperand {
            source: signal.source,
            control: signal.control,
            min: signal.min,
            max: signal.max,
            default: signal.default,
        }));
    }
    let Ok(control) = data.downcast_static::<LuaControl>() else {
        return None;
    };
    let state = state.borrow();
    let Some(spec) = state.program.controls.spec(control.0) else {
        return Some(Err(binding_error(
            ctx,
            "program control does not belong to this edit",
        )));
    };
    Some(Ok(SharedOperand {
        source: Source::Control(control.0),
        control: control.0,
        min: spec.min,
        max: spec.max,
        default: spec.default,
    }))
}

fn dc<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let source = read_mono(ctx, stack.get(0), generation)?;
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn triangle<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    if stack.is_empty() {
        stack.replace(ctx, lua_processor(ctx, generation, Processor::Triangle)?);
        return Ok(CallbackReturn::Return);
    }
    if stack.len() != 1 {
        return Err(binding_error(ctx, "triangle expects one frequency input"));
    }
    let hz = read_mono(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.triangle(hz);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn pulse<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    if stack.is_empty() {
        stack.replace(ctx, lua_processor(ctx, generation, Processor::Pulse)?);
        return Ok(CallbackReturn::Return);
    }
    let hz = read_mono(ctx, stack.get(0), generation)?;
    let duty = read_mono(ctx, stack.get(1), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.pulse(hz, duty);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn flue_pipe<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.len() != 4 {
        return Err(binding_error(
            ctx,
            "flue_pipe expects hz, pressure, turbulence, {min_hz}",
        ));
    }
    let generation = current_generation(ctx, state)?;
    let hz = read_mono(ctx, stack.get(0), generation)?;
    let pressure = read_mono(ctx, stack.get(1), generation)?;
    let turbulence = read_mono(ctx, stack.get(2), generation)?;
    let Value::Table(spec) = stack.get(3) else {
        return Err(binding_error(ctx, "flue_pipe expects a bounds table"));
    };
    let min_hz = read_number(ctx, spec.get_value(ctx, "min_hz"), "flue minimum Hz")?;
    if !min_hz.is_finite() || !(20.0..=1000.0).contains(&min_hz) {
        return Err(binding_error(
            ctx,
            "flue_pipe min_hz must be within 20..1000",
        ));
    }
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let output = state
        .active_mut(ctx)?
        .graph
        .flue_pipe(hz, pressure, turbulence, min_hz);
    stack.replace(ctx, lua_source(ctx, generation, [output])?);
    Ok(CallbackReturn::Return)
}

fn harmonics<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.len() != 2 {
        return Err(binding_error(ctx, "harmonics expects hz, { amplitudes }"));
    }
    let generation = current_generation(ctx, state)?;
    let hz = read_mono(ctx, stack.get(0), generation)?;
    let Value::Table(table) = stack.get(1) else {
        return Err(binding_error(ctx, "harmonics expects an amplitude table"));
    };
    let length = table.length();
    if !(1..=32).contains(&length) {
        return Err(binding_error(ctx, "harmonics expects 1..32 amplitudes"));
    }
    let mut amplitudes = Vec::with_capacity(length as usize);
    for index in 1..=length {
        amplitudes.push(read_number(
            ctx,
            table.get_value(ctx, index),
            "harmonic amplitude",
        )?);
    }
    if amplitudes.iter().any(|x| !x.is_finite())
        || amplitudes.iter().map(|x| x.abs()).sum::<f64>() > 1.0 + 1e-12
    {
        return Err(binding_error(
            ctx,
            "harmonics amplitudes must be finite with absolute sum <= 1",
        ));
    }
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let output = state.active_mut(ctx)?.graph.harmonics(hz, amplitudes);
    stack.replace(ctx, lua_source(ctx, generation, [output])?);
    Ok(CallbackReturn::Return)
}

fn string_resonator<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.len() != 4 {
        return Err(binding_error(
            ctx,
            "string_resonator expects excitation, hz, mute, { min_hz, decay }",
        ));
    }
    let generation = current_generation(ctx, state)?;
    let audio = read_mono(ctx, stack.get(0), generation)?;
    let hz = read_mono(ctx, stack.get(1), generation)?;
    let mute = read_mono(ctx, stack.get(2), generation)?;
    let Value::Table(spec) = stack.get(3) else {
        return Err(binding_error(
            ctx,
            "string_resonator expects a bounds table",
        ));
    };
    let min_hz = read_number(ctx, spec.get_value(ctx, "min_hz"), "string minimum Hz")?;
    let decay = read_seconds(ctx, spec.get_value(ctx, "decay"), "string nominal decay")?;
    if !min_hz.is_finite()
        || !(1.0..=20_000.0).contains(&min_hz)
        || !decay.is_finite()
        || decay <= 0.0
        || decay > 120.0
    {
        return Err(binding_error(
            ctx,
            "string_resonator requires min_hz in 1..20000 and decay in (0, 120] seconds",
        ));
    }
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let output = state
        .active_mut(ctx)?
        .graph
        .string_resonator(audio, hz, mute, min_hz, decay);
    stack.replace(ctx, lua_source(ctx, generation, [output])?);
    Ok(CallbackReturn::Return)
}

fn pluck<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let processor_form = stack.len() == 3;
    let generation = current_generation(ctx, state)?;
    let offset = usize::from(!processor_form);
    let frequency = read_mono(ctx, stack.get(offset), generation)?;
    let gain_per_second = read_number(ctx, stack.get(offset + 1), "pluck gain per second")?;
    let damping = read_mono(ctx, stack.get(offset + 2), generation)?;
    if !gain_per_second.is_finite() || !(0.0..1.0).contains(&gain_per_second) {
        return Err(binding_error(
            ctx,
            "pluck gain per second must be finite and within 0..1",
        ));
    }
    if processor_form {
        stack.replace(
            ctx,
            lua_processor(
                ctx,
                generation,
                Processor::Pluck {
                    frequency,
                    gain_per_second,
                    damping,
                },
            )?,
        );
        return Ok(CallbackReturn::Return);
    }
    let audio = read_mono(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let output = state
        .active_mut(ctx)?
        .graph
        .pluck(audio, frequency, gain_per_second, damping)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, lua_source(ctx, generation, [output])?);
    Ok(CallbackReturn::Return)
}

macro_rules! generator_zero {
    ($name:ident, $method:ident) => {
        fn $name<'gc>(
            ctx: Context<'gc>,
            state: &Rc<RefCell<BuildState>>,
            mut stack: piccolo::Stack<'gc, '_>,
        ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
            let generation = current_generation(ctx, state)?;
            let mut state = state.borrow_mut();
            state.spend_node(ctx)?;
            let source = state.active_mut(ctx)?.graph.$method();
            stack.replace(ctx, lua_source(ctx, generation, [source])?);
            Ok(CallbackReturn::Return)
        }
    };
}

generator_zero!(noise, noise);
generator_zero!(pink, pink);
generator_zero!(impulse, impulse);

fn init_random<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let offset = usize::from(is_event_seed(stack.get(0)));
    let stream = read_stream(ctx, stack.get(offset))?;
    let min = read_mono(ctx, stack.get(offset + 1), generation)?;
    let max = read_mono(ctx, stack.get(offset + 2), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state
        .active_mut(ctx)?
        .graph
        .init_random(stream, min, max)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn rand<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_rand(ctx, state, &mut stack, 0, 0)
}

fn rand_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_rand(ctx, state, &mut stack, 1, source_seed(call_site))
}

fn make_rand<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    offset: usize,
    stream: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        return Err(binding_error(
            ctx,
            "rand(min, max) is an init-rate voice value; transport randomness uses perlin or mini-notation degrade",
        ));
    }
    let generation = current_generation(ctx, state)?;
    let min = read_mono(ctx, stack.get(offset), generation)?;
    let max = read_mono(ctx, stack.get(offset + 1), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state
        .active_mut(ctx)?
        .graph
        .init_random(stream, min, max)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

macro_rules! filter {
    ($name:ident, $method:ident, $kind:ident) => {
        fn $name<'gc>(
            ctx: Context<'gc>,
            state: &Rc<RefCell<BuildState>>,
            mut stack: piccolo::Stack<'gc, '_>,
        ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
            if stack.len() <= 2 {
                let generation = processor_generation(state);
                let cutoff_q = if stack.is_empty() {
                    None
                } else {
                    Some((
                        read_mono(ctx, stack.get(0), generation)?,
                        if stack.len() == 2 {
                            read_mono(ctx, stack.get(1), generation)?
                        } else {
                            // Neutral first-slice default for the partial
                            // filter form. `filter()` itself remains the fully
                            // modulatable audio/cutoff/Q three-port processor.
                            Source::Const(0.707)
                        },
                    ))
                };
                stack.replace(
                    ctx,
                    lua_processor(
                        ctx,
                        generation,
                        Processor::Filter {
                            kind: FilterKind::$kind,
                            cutoff_q,
                        },
                    )?,
                );
                return Ok(CallbackReturn::Return);
            }
            let generation = current_generation(ctx, state)?;
            let audio = read_mono(ctx, stack.get(0), generation)?;
            let cutoff = read_mono(ctx, stack.get(1), generation)?;
            let q = read_mono(ctx, stack.get(2), generation)?;
            let mut state = state.borrow_mut();
            state.spend_node(ctx)?;
            let source = state.active_mut(ctx)?.graph.$method(audio, cutoff, q);
            stack.replace(ctx, lua_source(ctx, generation, [source])?);
            Ok(CallbackReturn::Return)
        }
    };
}

filter!(lowpass, lowpass, Lowpass);
filter!(highpass, highpass, Highpass);
filter!(bandpass, bandpass, Bandpass);
filter!(peak, peak, Peak);
filter!(moog, moog, Moog);

fn shape<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let offset = usize::from(stack.len() >= 3);
    let kind = match read_string(ctx, stack.get(offset), "shape kind")?.as_str() {
        "tanh" => ShapeKind::Tanh,
        "atan" => ShapeKind::Atan,
        "soft" | "softsign" => ShapeKind::Softsign,
        "clip" => ShapeKind::Clip,
        "crush" => ShapeKind::Crush,
        other => return Err(binding_error(ctx, format!("unknown shape kind {other:?}"))),
    };
    let generation = if offset == 0 {
        processor_generation(state)
    } else {
        current_generation(ctx, state)?
    };
    let amount = read_mono(ctx, stack.get(offset + 1), generation)?;
    if offset == 0 {
        stack.replace(
            ctx,
            lua_processor(ctx, generation, Processor::Shape { kind, amount })?,
        );
        return Ok(CallbackReturn::Return);
    }
    let audio = read_mono(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.shape(audio, kind, amount);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn dcblock<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.is_empty() {
        let generation = processor_generation(state);
        stack.replace(ctx, lua_processor(ctx, generation, Processor::DcBlock)?);
        return Ok(CallbackReturn::Return);
    }
    let generation = current_generation(ctx, state)?;
    let audio = read_mono(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.dcblock(audio);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn delay<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let processor_form = stack.len() <= 2;
    let generation = if processor_form {
        processor_generation(state)
    } else {
        current_generation(ctx, state)?
    };
    let seconds_index = usize::from(!processor_form);
    let seconds = read_mono(ctx, stack.get(seconds_index), generation)?;
    let range_value = stack.get(seconds_index + 1);
    let range = read_delay_range(ctx, seconds, range_value)?;
    if processor_form {
        stack.replace(
            ctx,
            lua_processor(ctx, generation, Processor::Delay { seconds, range })?,
        );
        return Ok(CallbackReturn::Return);
    }
    let audio = read_mono(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.delay(audio, seconds, range);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn predelay<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.len() != 1 {
        return Err(binding_error(ctx, "predelay expects one fixed duration"));
    }
    delay(ctx, state, stack)
}

fn diffuse<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let (delays, gains) = match stack.get(0) {
        Value::Table(spec) if matches!(spec.get_value(ctx, "delays"), Value::Table(_)) => {
            let Value::Table(delays) = spec.get_value(ctx, "delays") else {
                unreachable!("the guard above established a delay table");
            };
            let Value::Table(gains) = spec.get_value(ctx, "gains") else {
                return Err(binding_error(ctx, "diffuse.gains must be a table"));
            };
            if delays.length() == 0 || delays.length() != gains.length() {
                return Err(binding_error(
                    ctx,
                    "diffuse delays and gains must have the same nonzero length",
                ));
            }
            let mut delay_values = Vec::with_capacity(delays.length() as usize);
            let mut gain_values = Vec::with_capacity(gains.length() as usize);
            for index in 1..=delays.length() {
                delay_values.push(read_seconds(
                    ctx,
                    delays.get_value(ctx, index),
                    "diffuser delay",
                )?);
                gain_values.push(read_number(
                    ctx,
                    gains.get_value(ctx, index),
                    "diffuser gain",
                )?);
            }
            (delay_values, gain_values)
        }
        Value::Table(delays) => {
            if delays.length() == 0 {
                return Err(binding_error(ctx, "diffuse expects at least one delay"));
            }
            let gain = read_number(ctx, stack.get(1), "diffuser gain")?;
            let mut delay_values = Vec::with_capacity(delays.length() as usize);
            for index in 1..=delays.length() {
                delay_values.push(read_seconds(
                    ctx,
                    delays.get_value(ctx, index),
                    "diffuser delay",
                )?);
            }
            let gain_values = vec![gain; delay_values.len()];
            (delay_values, gain_values)
        }
        _ => {
            return Err(binding_error(
                ctx,
                "diffuse expects either { delays = {...}, gains = {...} } or ({...}, gain)",
            ));
        }
    };

    let mut processor = None;
    for (seconds, gain) in delays.into_iter().zip(gains) {
        if !seconds.is_finite() || seconds <= 0.0 || !gain.is_finite() || gain.abs() >= 1.0 {
            return Err(binding_error(
                ctx,
                "diffuser delays must be positive and finite and gains must have magnitude below 1",
            ));
        }
        let stage = Processor::AllpassDelay { seconds, gain };
        processor = Some(match processor {
            None => stage,
            Some(current) => Processor::Chain(Box::new(current), Box::new(stage)),
        });
    }
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            processor_generation(state),
            processor.expect("the nonempty delay table was checked above"),
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn fdn<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(spec) = stack.get(0) else {
        return Err(binding_error(ctx, "fdn expects a specification table"));
    };
    let matrix = spec.get_value(ctx, "matrix");
    if !matrix.is_nil() && read_string(ctx, matrix, "FDN matrix")? != "hadamard" {
        return Err(binding_error(
            ctx,
            "fdn currently supports only the \"hadamard\" matrix",
        ));
    }

    let delays = match spec.get_value(ctx, "delays") {
        Value::Table(delays) => {
            if delays.length() == 0 {
                return Err(binding_error(ctx, "fdn.delays must not be empty"));
            }
            let mut values = Vec::with_capacity(delays.length() as usize);
            for index in 1..=delays.length() {
                values.push(read_seconds(
                    ctx,
                    delays.get_value(ctx, index),
                    "FDN delay",
                )?);
            }
            values
        }
        value if value.is_nil() => {
            let size = read_count(ctx, spec.get_value(ctx, "size"), "FDN size")?;
            if size == 0 || !size.is_power_of_two() || size > 32 {
                return Err(binding_error(
                    ctx,
                    "FDN size must be a power of two between 1 and 32",
                ));
            }
            // A geometric spread avoids the strongly coincident modes of
            // equal lines while keeping the allocation deterministic.
            let first = 0.0437_f64;
            let last = 0.0893_f64;
            (0..size)
                .map(|index| {
                    if size == 1 {
                        first
                    } else {
                        first * (last / first).powf(index as f64 / (size - 1) as f64)
                    }
                })
                .collect()
        }
        _ => return Err(binding_error(ctx, "fdn.delays must be a table")),
    };
    if !delays.len().is_power_of_two() || delays.len() > 32 {
        return Err(binding_error(
            ctx,
            "FDN delay count must be a power of two no larger than 32",
        ));
    }

    let damping = read_number(ctx, spec.get_value(ctx, "damping"), "FDN damping")?;
    if !(0.0..1.0).contains(&damping) {
        return Err(binding_error(ctx, "FDN damping must lie in 0..1"));
    }
    let (modulation_rate, modulation_depth) = match spec.get_value(ctx, "modulation") {
        Value::Table(modulation) => (
            read_number(
                ctx,
                modulation.get_value(ctx, "rate"),
                "FDN modulation rate",
            )?,
            read_seconds(
                ctx,
                modulation.get_value(ctx, "depth"),
                "FDN modulation depth",
            )?,
        ),
        value if value.is_nil() => (0.0, 0.0),
        _ => {
            return Err(binding_error(
                ctx,
                "fdn.modulation must be a { rate, depth } table",
            ));
        }
    };
    if modulation_rate < 0.0
        || modulation_depth < 0.0
        || delays
            .iter()
            .any(|delay| !delay.is_finite() || *delay <= modulation_depth)
    {
        return Err(binding_error(
            ctx,
            "FDN delays must be positive and longer than the non-negative modulation depth",
        ));
    }

    let decay = {
        let decay = spec.get_value(ctx, "decay");
        if decay.is_nil() {
            spec.get_value(ctx, "t60")
        } else {
            decay
        }
    };
    let t60 = match decay {
        Value::UserData(data) => match data.downcast_static::<LuaParamRef>() {
            Ok(reference) => ProcessorScalar::SendParam(reference.0.clone()),
            Err(_) => {
                let seconds = read_seconds(ctx, decay, "FDN decay time")?;
                ProcessorScalar::Bound {
                    source: Source::Const(seconds),
                    max: seconds,
                }
            }
        },
        _ => {
            let seconds = read_seconds(ctx, decay, "FDN decay time")?;
            ProcessorScalar::Bound {
                source: Source::Const(seconds),
                max: seconds,
            }
        }
    };
    if matches!(t60, ProcessorScalar::Bound { max, .. } if max <= 0.0) {
        return Err(binding_error(ctx, "FDN decay time must be positive"));
    }

    stack.replace(
        ctx,
        lua_processor(
            ctx,
            processor_generation(state),
            Processor::Fdn {
                delays,
                damping,
                modulation_rate,
                modulation_depth,
                t60,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn envelope_follower<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let attack = read_seconds(ctx, stack.get(0), "envelope follower attack")?;
    let release = read_seconds(ctx, stack.get(1), "envelope follower release")?;
    if attack < 0.0 || release < 0.0 {
        return Err(binding_error(
            ctx,
            "envelope follower attack and release must be non-negative",
        ));
    }
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            processor_generation(state),
            Processor::EnvelopeFollower { attack, release },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn pitch_tracker<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(spec) = stack.get(0) else {
        return Err(binding_error(
            ctx,
            "pitch_tracker expects a { min, max, hold } table",
        ));
    };
    let min_hz = read_number(ctx, spec.get_value(ctx, "min"), "pitch tracker minimum")?;
    let max_hz = read_number(ctx, spec.get_value(ctx, "max"), "pitch tracker maximum")?;
    let hold_seconds = read_seconds(ctx, spec.get_value(ctx, "hold"), "pitch tracker hold")?;
    if min_hz <= 0.0 || max_hz < min_hz || hold_seconds < 0.0 {
        return Err(binding_error(
            ctx,
            "pitch tracker requires 0 < min <= max and a non-negative hold",
        ));
    }
    let default_hz = 220.0_f64.clamp(min_hz, max_hz);
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            processor_generation(state),
            Processor::PitchTracker {
                min_hz,
                max_hz,
                default_hz,
                hold_seconds,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn onset_detector<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(spec) = stack.get(0) else {
        return Err(binding_error(
            ctx,
            "onset_detector expects a { floor, hold } table",
        ));
    };
    let floor = read_number(ctx, spec.get_value(ctx, "floor"), "onset detector floor")?;
    let hold_seconds = read_seconds(ctx, spec.get_value(ctx, "hold"), "onset detector hold")?;
    if !floor.is_finite() || floor <= 0.0 || !hold_seconds.is_finite() || hold_seconds < 0.0 {
        return Err(binding_error(
            ctx,
            "onset detector requires a positive finite floor and non-negative hold",
        ));
    }
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            processor_generation(state),
            Processor::OnsetDetector {
                floor,
                hold_seconds,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn width<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = processor_generation(state);
    let amount = read_mono(ctx, stack.get(0), generation)?;
    stack.replace(
        ctx,
        lua_processor(ctx, generation, Processor::Width { amount })?,
    );
    Ok(CallbackReturn::Return)
}

fn reverb<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let room_size = read_number(ctx, stack.get(0), "reverb room size")?;
    let time = read_seconds(ctx, stack.get(1), "reverb time")?;
    let damping = if stack.len() >= 3 {
        read_number(ctx, stack.get(2), "reverb damping")?
    } else {
        0.5
    };
    if room_size <= 0.0 || time < 0.0 || !(0.0..=1.0).contains(&damping) {
        return Err(binding_error(
            ctx,
            "reverb expects a positive room size, non-negative time, and damping in 0..=1",
        ));
    }
    let generation = processor_generation(state);
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            generation,
            Processor::Reverb {
                room_size,
                time,
                damping,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn limiter<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let attack = read_seconds(ctx, stack.get(0), "limiter attack")?;
    let release = read_seconds(ctx, stack.get(1), "limiter release")?;
    if attack <= 0.0 || release <= 0.0 {
        return Err(binding_error(
            ctx,
            "limiter attack and release must be positive",
        ));
    }
    let generation = processor_generation(state);
    stack.replace(
        ctx,
        lua_processor(ctx, generation, Processor::Limiter { attack, release })?,
    );
    Ok(CallbackReturn::Return)
}

fn chorus<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let seed = read_count(ctx, stack.get(0), "chorus seed")? as u64;
    let separation = read_seconds(ctx, stack.get(1), "chorus separation")?;
    let variation = read_seconds(ctx, stack.get(2), "chorus variation")?;
    let frequency = read_number(ctx, stack.get(3), "chorus modulation frequency")?;
    if separation < 0.0 || variation < 0.0 || frequency <= 0.0 {
        return Err(binding_error(
            ctx,
            "chorus delays must be non-negative and frequency must be positive",
        ));
    }
    let generation = processor_generation(state);
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            generation,
            Processor::Chorus {
                seed,
                separation,
                variation,
                frequency,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn ensemble<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(spec) = stack.get(0) else {
        return Err(binding_error(ctx, "ensemble expects a specification table"));
    };
    let Value::Table(delays) = spec.get_value(ctx, "delays") else {
        return Err(binding_error(ctx, "ensemble.delays must be a table"));
    };
    let Value::Table(rates) = spec.get_value(ctx, "rates") else {
        return Err(binding_error(ctx, "ensemble.rates must be a table"));
    };
    if delays.length() == 0 || delays.length() != rates.length() {
        return Err(binding_error(
            ctx,
            "ensemble delays and rates must have the same nonzero length",
        ));
    }
    let depth = read_seconds(ctx, spec.get_value(ctx, "depth"), "ensemble depth")?;
    let mut processor = None;
    for index in 1..=delays.length() {
        let line = Processor::Chorus {
            seed: index as u64,
            separation: read_seconds(ctx, delays.get_value(ctx, index), "ensemble line delay")?,
            variation: depth,
            frequency: read_number(ctx, rates.get_value(ctx, index), "ensemble line rate")?,
        };
        processor = Some(match processor {
            None => line,
            Some(current) => Processor::Sum(Box::new(current), Box::new(line)),
        });
    }
    let processor = Processor::Scale(
        Box::new(processor.expect("nonempty ensemble checked above")),
        Source::Const(1.0 / delays.length() as f64),
    );
    stack.replace(
        ctx,
        lua_processor(ctx, processor_generation(state), processor)?,
    );
    Ok(CallbackReturn::Return)
}

fn slew<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.is_empty() || stack.len() > 2 {
        return Err(binding_error(
            ctx,
            "slew expects slew(response_seconds) or slew(signal, response_seconds)",
        ));
    }
    let generation = if state.borrow().active.is_none() && stack.len() == 1 {
        0
    } else {
        current_generation(ctx, state)?
    };
    if stack.len() == 1 {
        let response_time = read_mono(ctx, stack.get(0), generation)?;
        stack.replace(
            ctx,
            lua_processor(ctx, generation, Processor::Slew { response_time })?,
        );
        return Ok(CallbackReturn::Return);
    }

    let input = read_mono(ctx, stack.get(0), generation)?;
    let response_time = read_mono(ctx, stack.get(1), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let output = state
        .active_mut(ctx)?
        .graph
        .slew(input, response_time)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, lua_source(ctx, generation, [output])?);
    Ok(CallbackReturn::Return)
}

fn gate_env<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let gate = read_mono(ctx, stack.get(0), generation)?;
    let attack = read_seconds(ctx, stack.get(1), "gate envelope attack")?;
    let decay = read_seconds(ctx, stack.get(2), "gate envelope decay")?;
    let sustain = read_number(ctx, stack.get(3), "gate envelope sustain")?;
    let release = read_seconds(ctx, stack.get(4), "gate envelope release")?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state
        .active_mut(ctx)?
        .graph
        .gate_env(gate, attack, decay, sustain, release);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn feedback_delay<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let amount = read_number(ctx, stack.get(0), "feedback amount")?;
    if !amount.is_finite() || amount == 0.0 || amount.abs() >= 1.0 {
        return Err(binding_error(
            ctx,
            "feedback amount must be finite, nonzero, and have magnitude below 1",
        ));
    }
    let generation = processor_generation(state);
    stack.replace(
        ctx,
        lua_processor(ctx, generation, Processor::Feedback { amount })?,
    );
    Ok(CallbackReturn::Return)
}

macro_rules! binary {
    ($name:ident, $method:ident) => {
        fn $name<'gc>(
            ctx: Context<'gc>,
            state: &Rc<RefCell<BuildState>>,
            mut stack: piccolo::Stack<'gc, '_>,
        ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
            let generation = current_generation(ctx, state)?;
            let a = read_arithmetic_channels(ctx, stack.get(0), generation)?;
            let b = read_arithmetic_channels(ctx, stack.get(1), generation)?;
            let (a, b) = align_arithmetic_channels(ctx, a, b)?;
            let mut state = state.borrow_mut();
            let mut outputs = Vec::with_capacity(a.len());
            for (a, b) in a.into_iter().zip(b) {
                state.spend_node(ctx)?;
                outputs.push(state.active_mut(ctx)?.graph.$method(a, b));
            }
            stack.replace(ctx, lua_source(ctx, generation, outputs)?);
            Ok(CallbackReturn::Return)
        }
    };
}

binary!(add, add);
binary!(sub, sub);
binary!(div, div);
binary!(pow, pow);

fn clamp<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let value = read_mono(ctx, stack.get(0), generation)?;
    let min = read_number(ctx, stack.get(1), "clamp minimum")?;
    let max = read_number(ctx, stack.get(2), "clamp maximum")?;
    if min > max {
        return Err(binding_error(
            ctx,
            "clamp minimum cannot exceed its maximum",
        ));
    }
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.clamp(value, min, max);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn mul<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if stack.len() == 1 {
        let generation = processor_generation(state);
        let gain = read_mono(ctx, stack.get(0), generation)?;
        stack.replace(ctx, lua_processor(ctx, generation, Processor::Mul(gain))?);
        return Ok(CallbackReturn::Return);
    }
    let generation = current_generation(ctx, state)?;
    let a = read_arithmetic_channels(ctx, stack.get(0), generation)?;
    let b = read_arithmetic_channels(ctx, stack.get(1), generation)?;
    let (a, b) = align_arithmetic_channels(ctx, a, b)?;
    let mut state = state.borrow_mut();
    let mut outputs = Vec::with_capacity(a.len());
    for (a, b) in a.into_iter().zip(b) {
        state.spend_node(ctx)?;
        outputs.push(state.active_mut(ctx)?.graph.mul(a, b));
    }
    stack.replace(ctx, lua_source(ctx, generation, outputs)?);
    Ok(CallbackReturn::Return)
}

fn neg<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let inputs = read_arithmetic_channels(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    let mut outputs = Vec::with_capacity(inputs.len());
    for input in inputs {
        state.spend_node(ctx)?;
        outputs.push(state.active_mut(ctx)?.graph.neg(input));
    }
    stack.replace(ctx, lua_source(ctx, generation, outputs)?);
    Ok(CallbackReturn::Return)
}

fn mix<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    if let Value::Table(table) = stack.get(0)
        && table.length() > 0
        && is_processor(table.get_value(ctx, 1))
    {
        let mut processors = Vec::new();
        for index in 1..=table.length() {
            processors.push(read_processor(
                ctx,
                table.get_value(ctx, index),
                generation,
            )?);
        }
        let processor = processors
            .into_iter()
            .reduce(|left, right| Processor::Sum(Box::new(left), Box::new(right)))
            .expect("a non-empty processor table has a first element");
        stack.replace(ctx, lua_processor(ctx, generation, processor)?);
        return Ok(CallbackReturn::Return);
    }
    let mut inputs = Vec::new();
    if let Value::Table(table) = stack.get(0) {
        for index in 1..=table.length() {
            inputs.push(read_mono(ctx, table.get_value(ctx, index), generation)?);
        }
    } else {
        for value in &stack {
            inputs.push(read_mono(ctx, value, generation)?);
        }
    }
    let nodes = inputs.len().saturating_sub(1);
    let mut state = state.borrow_mut();
    for _ in 0..nodes {
        state.spend_node(ctx)?;
    }
    let source = state.active_mut(ctx)?.graph.mix(inputs);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn adsr<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let envelope = Adsr::new(
        read_seconds(ctx, stack.get(0), "ADSR attack")?,
        read_seconds(ctx, stack.get(1), "ADSR decay")?,
        read_number(ctx, stack.get(2), "ADSR sustain")?,
        read_seconds(ctx, stack.get(3), "ADSR release")?,
    );
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.adsr(envelope);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn step<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_step(ctx, state, stack, 0, None)
}

fn step_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_step(
        ctx,
        state,
        stack,
        1,
        Some(call_site_span(call_site, "step")),
    )
}

fn make_step<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        let at = if stack.len() == argument_offset {
            Frac::ZERO
        } else {
            read_pattern_time(ctx, stack.get(argument_offset), "transport step time")?
        };
        return transport_signal(ctx, state, &mut stack, PatternSignal::Step { at }, src);
    }
    let delay = if stack.len() == argument_offset {
        0.0
    } else {
        read_seconds(ctx, stack.get(argument_offset), "step delay")?
    };
    curve_node(
        ctx,
        state,
        &mut stack,
        Curve::default().term(Basis::Step, 1.0, delay, 0.0),
    )
}

fn line<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_line(ctx, state, stack, 0, None)
}

fn line_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_line(
        ctx,
        state,
        stack,
        1,
        Some(call_site_span(call_site, "line")),
    )
}

fn make_line<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        let length = read_seconds(ctx, stack.get(argument_offset), "ramp length")?;
        let delay = if stack.len() >= argument_offset + 2 {
            read_seconds(ctx, stack.get(argument_offset + 1), "ramp delay")?
        } else {
            0.0
        };
        return curve_node(
            ctx,
            state,
            &mut stack,
            Curve::default().term(Basis::Ramp, 1.0, delay, length),
        );
    }
    let from = read_number(ctx, stack.get(argument_offset), "line starting value")?;
    let to = read_number(ctx, stack.get(argument_offset + 1), "line ending value")?;
    let length = read_pattern_time(ctx, stack.get(argument_offset + 2), "line duration")?;
    if length <= Frac::ZERO {
        return Err(binding_error(ctx, "line duration must be positive"));
    }
    transport_signal(
        ctx,
        state,
        &mut stack,
        PatternSignal::Line { from, to, length },
        src,
    )
}

fn ramp<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let length = read_seconds(ctx, stack.get(0), "ramp length")?;
    let delay = if stack.len() >= 2 {
        read_seconds(ctx, stack.get(1), "ramp delay")?
    } else {
        0.0
    };
    curve_node(
        ctx,
        state,
        &mut stack,
        Curve::default().term(Basis::Ramp, 1.0, delay, length),
    )
}

fn decay<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if matches!(
        stack.get(0),
        Value::UserData(data) if data.downcast_static::<LuaSource>().is_ok()
    ) {
        let generation = current_generation(ctx, state)?;
        let length = read_mono(ctx, stack.get(0), generation)?;
        let mut state = state.borrow_mut();
        state.spend_node(ctx)?;
        let source = state
            .active_mut(ctx)?
            .graph
            .decay(length)
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        stack.replace(ctx, lua_source(ctx, generation, [source])?);
        return Ok(CallbackReturn::Return);
    }
    let length = read_seconds(ctx, stack.get(0), "decay length")?;
    curve_node(
        ctx,
        state,
        &mut stack,
        Curve::decay(apteronotus_synth::CurveClock::NoteSeconds, length),
    )
}

fn window<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    make_window(ctx, state, stack, 0, None)
}

fn window_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let call_site = read_call_site(ctx, stack.get(0))?;
    make_window(
        ctx,
        state,
        stack,
        1,
        Some(call_site_span(call_site, "window")),
    )
}

fn make_window<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        let begin = read_pattern_time(
            ctx,
            stack.get(argument_offset),
            "transport window beginning",
        )?;
        let end = read_pattern_time(ctx, stack.get(argument_offset + 1), "transport window end")?;
        if end < begin {
            return Err(binding_error(
                ctx,
                "transport window end must not precede its beginning",
            ));
        }
        return transport_signal(
            ctx,
            state,
            &mut stack,
            PatternSignal::Window { begin, end },
            src,
        );
    }
    let dynamic = [stack.get(argument_offset), stack.get(argument_offset + 1)]
        .into_iter()
        .any(|value| {
            matches!(
                value,
                Value::UserData(data) if data.downcast_static::<LuaSource>().is_ok()
            )
        });
    if dynamic {
        let generation = current_generation(ctx, state)?;
        let begin = read_mono(ctx, stack.get(argument_offset), generation)?;
        let end = read_mono(ctx, stack.get(argument_offset + 1), generation)?;
        let mut state = state.borrow_mut();
        state.spend_node(ctx)?;
        let source = state
            .active_mut(ctx)?
            .graph
            .window(begin, end)
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        stack.replace(ctx, lua_source(ctx, generation, [source])?);
        return Ok(CallbackReturn::Return);
    }
    let begin = read_seconds(ctx, stack.get(argument_offset), "window beginning")?;
    let end = read_seconds(ctx, stack.get(argument_offset + 1), "window end")?;
    curve_node(
        ctx,
        state,
        &mut stack,
        Curve::window(apteronotus_synth::CurveClock::NoteSeconds, begin, end),
    )
}

fn transport_signal<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    signal: PatternSignal,
    src: Option<SrcSpan>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let pattern = match src {
        Some(src) => Pattern::signal_at(signal, src),
        None => Pattern::signal(signal),
    };
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
                routing: EventRouting::new(),
                external_trigger: None,
                hold_seconds: None,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn curve_node<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    curve: Curve,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.curve(curve);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn ring<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    if stack.len() == 2 {
        let hz = read_mono(ctx, stack.get(0), generation)?;
        let decay = read_mono(ctx, stack.get(1), generation)?;
        stack.replace(
            ctx,
            lua_processor(ctx, generation, Processor::Ring { hz, decay })?,
        );
        return Ok(CallbackReturn::Return);
    }
    let audio = read_mono(ctx, stack.get(0), generation)?;
    let hz = read_mono(ctx, stack.get(1), generation)?;
    let decay = read_mono(ctx, stack.get(2), generation)?;
    // ring expands to two multiplies and one band-pass.
    let mut state = state.borrow_mut();
    for _ in 0..3 {
        state.spend_node(ctx)?;
    }
    let source = stdlib::ring(&mut state.active_mut(ctx)?.graph, audio, hz, decay)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn pan<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_none() {
        return if let Ok(value) = read_setter_control_value(ctx, stack.get(0)) {
            replace_with_named_control(ctx, state, &mut stack, "pan", value)
        } else {
            replace_with_named_pattern_input(ctx, state, &mut stack, "pan")
        };
    }
    let generation = current_generation(ctx, state)?;
    if stack.len() == 1 {
        let position = read_mono(ctx, stack.get(0), generation)?;
        stack.replace(
            ctx,
            lua_processor(ctx, generation, Processor::Pan(position))?,
        );
        return Ok(CallbackReturn::Return);
    }
    let audio = read_mono(ctx, stack.get(0), generation)?;
    let position = read_mono(ctx, stack.get(1), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let (left, right) = state.active_mut(ctx)?.graph.pan(audio, position);
    stack.replace(ctx, lua_source(ctx, generation, [left, right])?);
    Ok(CallbackReturn::Return)
}

fn to<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(ctx, "to expects a bus handle"));
    };
    let bus = *data
        .downcast_static::<LuaBus>()
        .map_err(|_| binding_error(ctx, "to expects a bus handle"))?;
    if state.borrow().active.is_none() {
        let level = read_number(ctx, stack.get(1), "score send level")?;
        let mut routing = EventRouting::new();
        routing
            .send(bus.id, level)
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        stack.replace(ctx, UserData::new_static(&ctx, LuaEventSend(routing)));
        return Ok(CallbackReturn::Return);
    }

    let generation = current_generation(ctx, state)?;
    let level = read_mono(ctx, stack.get(1), generation)?;
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            generation,
            Processor::Send {
                bus: bus.id,
                channels: bus.channels,
                level,
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn duck<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if state.borrow().active.is_some() {
        return Err(binding_error(
            ctx,
            "duck is a score-level track control, not a graph processor",
        ));
    }
    let trigger = numeric_pattern_operand(ctx, stack.get(0))?;
    if !trigger.controls.is_empty()
        || !trigger.routing.sends().is_empty()
        || trigger.routing.duck_control().is_some()
        || trigger.hold_seconds.is_some()
    {
        return Err(binding_error(
            ctx,
            "duck trigger must be a plain rhythm pattern",
        ));
    }
    let amount = read_number(ctx, stack.get(1), "duck amount")?;
    state.borrow_mut().spend_pattern(ctx, &trigger.pattern)?;
    let mut routing = EventRouting::new();
    routing
        .duck(trigger.pattern, amount)
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    stack.replace(ctx, UserData::new_static(&ctx, LuaEventSend(routing)));
    Ok(CallbackReturn::Return)
}

fn stack_ports<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let mut inputs = read_pipe_inputs(ctx, stack.get(0), generation)?;
    inputs.extend(read_pipe_inputs(ctx, stack.get(1), generation)?);
    stack.replace(ctx, lua_bundle(ctx, generation, inputs)?);
    Ok(CallbackReturn::Return)
}

fn pipe<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    if is_processor(stack.get(0)) && is_processor(stack.get(1)) {
        let generation = processor_generation(state);
        let left = read_processor(ctx, stack.get(0), generation)?;
        let right = read_processor(ctx, stack.get(1), generation)?;
        if let Processor::Feedback { amount } = right {
            stack.replace(
                ctx,
                lua_processor(ctx, generation, close_feedback_loop(ctx, left, amount)?)?,
            );
            return Ok(CallbackReturn::Return);
        }
        // A mono prefix in a stereo master/send chain means "the same
        // processor per lane". Expand that prefix before checking the cable,
        // just as a mono suffix is expanded to match a multichannel source.
        let left = broadcast_mono_processor(left, processor_inputs(&right));
        let right = broadcast_mono_processor(right, processor_outputs(&left));
        ensure_chain(ctx, &left, &right)?;
        stack.replace(
            ctx,
            lua_processor(
                ctx,
                generation,
                Processor::Chain(Box::new(left), Box::new(right)),
            )?,
        );
        return Ok(CallbackReturn::Return);
    }

    if state.borrow().active.is_none()
        && let Value::UserData(data) = stack.get(0)
        && let Ok(input) = data.downcast_static::<LuaAudioInput>()
        && is_processor(stack.get(1))
    {
        if input.channels != 1 {
            return Err(binding_error(
                ctx,
                "audio-to-control analyzers currently require a mono input",
            ));
        }
        let processor = read_processor(ctx, stack.get(1), 0)?;
        let (min, max, default) = match &processor {
            Processor::EnvelopeFollower { .. } => (0.0, 1.0, 0.0),
            Processor::PitchTracker {
                min_hz,
                max_hz,
                default_hz,
                ..
            } => (*min_hz, *max_hz, *default_hz),
            Processor::OnsetDetector { .. } => (0.0, 1.0, 0.0),
            _ => {
                return Err(binding_error(
                    ctx,
                    "this program-scope audio processor does not produce a published control signal",
                ));
            }
        };
        let mut state = state.borrow_mut();
        let graph = std::mem::take(&mut state.shared_graph);
        state.active = Some(ActiveGraph {
            graph,
            generation: 0,
            lifetime: ActiveLifetime::Patch,
            input_channels: 0,
            patch_controls: Vec::new(),
        });
        let outputs = apply_processor(
            ctx,
            &mut state,
            &processor,
            &[Source::ExternalAudio {
                input: input.id,
                channel: 0,
            }],
        )?;
        let active = state
            .active
            .take()
            .expect("the shared analyzer graph was installed above");
        state.shared_graph = active.graph;
        let [source] = outputs.as_slice() else {
            return Err(binding_error(
                ctx,
                "an audio-to-control analyzer must produce one output",
            ));
        };
        let signal = publish_shared_signal(ctx, &mut state, *source, min, max, default)?;
        if matches!(processor, Processor::OnsetDetector { .. }) {
            let pattern = Pattern::Silence;
            state.spend_pattern(ctx, &pattern)?;
            stack.replace(
                ctx,
                lua_pattern(
                    ctx,
                    LuaPattern {
                        pattern,
                        controls: Vec::new(),
                        routing: EventRouting::new(),
                        external_trigger: Some(LuaExternalTrigger {
                            control: signal.control,
                            degrades: Vec::new(),
                        }),
                        hold_seconds: None,
                    },
                )?,
            );
        } else {
            stack.replace(ctx, lua_shared_signal(ctx, signal)?);
        }
        return Ok(CallbackReturn::Return);
    }

    if state.borrow().active.is_none()
        && let Some(input) = shared_signal_operand(ctx, state, stack.get(0))
        && is_processor(stack.get(1))
    {
        let input = input?;
        let processor = read_processor(ctx, stack.get(1), 0)?;
        if !matches!(processor, Processor::Slew { .. }) {
            return Err(binding_error(
                ctx,
                "this program-scope processor has no published range rule yet",
            ));
        }
        let mut state = state.borrow_mut();
        let graph = std::mem::take(&mut state.shared_graph);
        state.active = Some(ActiveGraph {
            graph,
            generation: 0,
            lifetime: ActiveLifetime::Patch,
            input_channels: 0,
            patch_controls: Vec::new(),
        });
        let outputs = if let Processor::Slew { response_time } = processor {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .slew_from(input.source, response_time, input.default)
                    .map_err(|error| binding_error(ctx, error.to_string()))?,
            ]
        } else {
            unreachable!("the shared scalar processor match above accepts only slew")
        };
        let active = state
            .active
            .take()
            .expect("the shared signal graph was installed above");
        state.shared_graph = active.graph;
        let [source] = outputs.as_slice() else {
            return Err(binding_error(
                ctx,
                "a shared scalar processor must produce one output",
            ));
        };
        // This shared follower is explicitly initialized from the published
        // default, so its startup remains inside the proven input range.
        let signal = publish_shared_signal(
            ctx,
            &mut state,
            *source,
            input.min,
            input.max,
            input.default,
        )?;
        stack.replace(ctx, lua_shared_signal(ctx, signal)?);
        return Ok(CallbackReturn::Return);
    }

    let generation = current_generation(ctx, state)?;
    if let Some(right) = as_bundle(stack.get(1), generation) {
        let mut inputs = read_pipe_inputs(ctx, stack.get(0), generation)?;
        inputs.extend(right.map_err(|message| binding_error(ctx, message))?);
        stack.replace(ctx, lua_bundle(ctx, generation, inputs)?);
        return Ok(CallbackReturn::Return);
    }

    let processor = broadcast_mono_processor(
        read_processor(ctx, stack.get(1), generation)?,
        read_pipe_inputs(ctx, stack.get(0), generation)?.len(),
    );
    let mut inputs = read_pipe_inputs(ctx, stack.get(0), generation)?;
    if inputs.len() == 1 && processor_accepts_duplicated_mono(&processor) {
        inputs.push(inputs[0]);
    }
    let mut state = state.borrow_mut();
    let outputs = apply_processor(ctx, &mut state, &processor, &inputs)?;
    stack.replace(ctx, lua_source(ctx, generation, outputs)?);
    Ok(CallbackReturn::Return)
}

fn close_feedback_loop<'gc>(
    ctx: Context<'gc>,
    processor: Processor,
    amount: f64,
) -> Result<Processor, Error<'gc>> {
    let invalid = || {
        binding_error(
            ctx,
            "feedback currently requires a fixed delay(...) optionally followed by a fixed lowpass(...)",
        )
    };
    let fixed_delay = |seconds: Source, range: DelayRange| {
        let Source::Const(seconds) = seconds else {
            return None;
        };
        (range.min_seconds() == seconds && range.max_seconds() == seconds).then_some(seconds)
    };

    let (delay_seconds, cutoff_q) = match processor {
        Processor::Delay { seconds, range } => {
            (fixed_delay(seconds, range).ok_or_else(invalid)?, None)
        }
        Processor::Chain(left, right) => {
            let Processor::Delay { seconds, range } = *left else {
                return Err(invalid());
            };
            let delay_seconds = fixed_delay(seconds, range).ok_or_else(invalid)?;
            let Processor::Filter {
                kind: FilterKind::Lowpass,
                cutoff_q: Some((Source::Const(cutoff), Source::Const(q))),
            } = *right
            else {
                return Err(invalid());
            };
            (delay_seconds, Some((cutoff, q)))
        }
        _ => return Err(invalid()),
    };
    Ok(Processor::FeedbackDelay {
        delay_seconds,
        cutoff_q,
        amount,
    })
}

fn processor_accepts_duplicated_mono(processor: &Processor) -> bool {
    match processor {
        Processor::Reverb { .. } | Processor::Limiter { .. } | Processor::Width { .. } => true,
        Processor::Chain(left, _) => processor_accepts_duplicated_mono(left),
        _ => false,
    }
}

fn broadcast_mono_processor(processor: Processor, channels: usize) -> Processor {
    if channels <= 1 || processor_inputs(&processor) != 1 || processor_outputs(&processor) != 1 {
        return processor;
    }
    let mut broadcast = processor.clone();
    for _ in 1..channels {
        broadcast = Processor::Stack(Box::new(broadcast), Box::new(processor.clone()));
    }
    broadcast
}

fn processor_add<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    processor_binary(
        ctx,
        state,
        &mut stack,
        |left, right| Processor::Sum(Box::new(left), Box::new(right)),
        true,
    )
}

fn processor_sub<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = processor_generation(state);
    let left = read_processor(ctx, stack.get(0), generation)?;
    let right = read_processor(ctx, stack.get(1), generation)?;
    ensure_parallel(ctx, &left, &right, true)?;
    let right = Processor::Scale(Box::new(right), Source::Const(-1.0));
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            generation,
            Processor::Sum(Box::new(left), Box::new(right)),
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn processor_mul<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = processor_generation(state);
    let left_is_processor = is_processor(stack.get(0));
    let right_is_processor = is_processor(stack.get(1));
    let processor = match (left_is_processor, right_is_processor) {
        (true, true) => {
            let left = read_processor(ctx, stack.get(0), generation)?;
            let right = read_processor(ctx, stack.get(1), generation)?;
            ensure_parallel(ctx, &left, &right, true)?;
            Processor::Product(Box::new(left), Box::new(right))
        }
        (true, false) => Processor::Scale(
            Box::new(read_processor(ctx, stack.get(0), generation)?),
            read_mono(ctx, stack.get(1), generation)?,
        ),
        (false, true) => Processor::Scale(
            Box::new(read_processor(ctx, stack.get(1), generation)?),
            read_mono(ctx, stack.get(0), generation)?,
        ),
        (false, false) => {
            return Err(binding_error(
                ctx,
                "processor multiplication expects at least one processor",
            ));
        }
    };
    stack.replace(ctx, lua_processor(ctx, generation, processor)?);
    Ok(CallbackReturn::Return)
}

fn processor_stack<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    processor_binary(
        ctx,
        state,
        &mut stack,
        |left, right| Processor::Stack(Box::new(left), Box::new(right)),
        false,
    )
}

fn processor_sum<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    // `&` is a bus: both processors see the same input and compatible outputs
    // are mixed. `~` is the independent-output branch.
    processor_binary(
        ctx,
        state,
        &mut stack,
        |left, right| Processor::Sum(Box::new(left), Box::new(right)),
        true,
    )
}

fn processor_branch<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = processor_generation(state);
    let left = read_processor(ctx, stack.get(0), generation)?;
    let right = read_processor(ctx, stack.get(1), generation)?;
    ensure_parallel(ctx, &left, &right, false)?;
    stack.replace(
        ctx,
        lua_processor(
            ctx,
            generation,
            Processor::Branch(Box::new(left), Box::new(right)),
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn processor_binary<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    make: impl FnOnce(Processor, Processor) -> Processor,
    require_matching_outputs: bool,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = processor_generation(state);
    let left = read_processor(ctx, stack.get(0), generation)?;
    let right = read_processor(ctx, stack.get(1), generation)?;
    if require_matching_outputs {
        ensure_parallel(ctx, &left, &right, true)?;
    }
    stack.replace(ctx, lua_processor(ctx, generation, make(left, right))?);
    Ok(CallbackReturn::Return)
}

fn ensure_chain<'gc>(
    ctx: Context<'gc>,
    left: &Processor,
    right: &Processor,
) -> Result<(), Error<'gc>> {
    let outputs = processor_outputs(left);
    let inputs = processor_inputs(right);
    if outputs != inputs {
        return Err(binding_error(
            ctx,
            format!(
                "cannot connect processor with {outputs} outputs to processor with {inputs} inputs"
            ),
        ));
    }
    Ok(())
}

fn ensure_parallel<'gc>(
    ctx: Context<'gc>,
    left: &Processor,
    right: &Processor,
    require_matching_outputs: bool,
) -> Result<(), Error<'gc>> {
    let left_inputs = processor_inputs(left);
    let right_inputs = processor_inputs(right);
    if left_inputs != right_inputs {
        return Err(binding_error(
            ctx,
            format!(
                "parallel processors require matching inputs ({left_inputs} and {right_inputs})"
            ),
        ));
    }
    let left_outputs = processor_outputs(left);
    let right_outputs = processor_outputs(right);
    if require_matching_outputs && left_outputs != right_outputs {
        return Err(binding_error(
            ctx,
            format!(
                "mixed processors require matching outputs ({left_outputs} and {right_outputs})"
            ),
        ));
    }
    Ok(())
}

fn processor_inputs(processor: &Processor) -> usize {
    match processor {
        Processor::Reverb { .. } | Processor::Limiter { .. } | Processor::Width { .. } => 2,
        Processor::Sine
        | Processor::Cosine
        | Processor::Saw
        | Processor::Triangle
        | Processor::Pluck { .. }
        | Processor::Shape { .. }
        | Processor::DcBlock
        | Processor::Delay { .. }
        | Processor::AllpassDelay { .. }
        | Processor::Fdn { .. }
        | Processor::EnvelopeFollower { .. }
        | Processor::PitchTracker { .. }
        | Processor::OnsetDetector { .. }
        | Processor::Chorus { .. }
        | Processor::Feedback { .. }
        | Processor::FeedbackDelay { .. }
        | Processor::Slew { .. }
        | Processor::Mul(_)
        | Processor::Ring { .. }
        | Processor::Pan(_) => 1,
        Processor::Send { channels, .. } => *channels,
        Processor::Pulse => 2,
        Processor::Filter { cutoff_q, .. } => {
            if cutoff_q.is_some() {
                1
            } else {
                3
            }
        }
        Processor::Chain(left, _) => processor_inputs(left),
        Processor::Stack(left, right) => processor_inputs(left) + processor_inputs(right),
        Processor::Sum(left, _)
        | Processor::Product(left, _)
        | Processor::Scale(left, _)
        | Processor::Branch(left, _) => processor_inputs(left),
    }
}

fn processor_outputs(processor: &Processor) -> usize {
    match processor {
        Processor::Pan(_)
        | Processor::Width { .. }
        | Processor::Reverb { .. }
        | Processor::Limiter { .. } => 2,
        Processor::Send { channels, .. } => *channels,
        Processor::Chain(_, right) => processor_outputs(right),
        Processor::Stack(left, right) | Processor::Branch(left, right) => {
            processor_outputs(left) + processor_outputs(right)
        }
        Processor::Sum(left, _) | Processor::Product(left, _) | Processor::Scale(left, _) => {
            processor_outputs(left)
        }
        _ => 1,
    }
}

fn apply_processor<'gc>(
    ctx: Context<'gc>,
    state: &mut BuildState,
    processor: &Processor,
    inputs: &[Source],
) -> Result<Vec<Source>, Error<'gc>> {
    let expected = processor_inputs(processor);
    if inputs.len() != expected {
        return Err(binding_error(
            ctx,
            format!(
                "processor expects {expected} input ports, received {}",
                inputs.len()
            ),
        ));
    }

    let one = |inputs: &[Source]| inputs[0];
    let outputs = match processor {
        Processor::Sine => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.sine(one(inputs))]
        }
        Processor::Cosine => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.cosine(one(inputs))]
        }
        Processor::Saw => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.saw(one(inputs))]
        }
        Processor::Triangle => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.triangle(one(inputs))]
        }
        Processor::Pulse => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.pulse(inputs[0], inputs[1])]
        }
        Processor::Pluck {
            frequency,
            gain_per_second,
            damping,
        } => {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .pluck(one(inputs), *frequency, *gain_per_second, *damping)
                    .map_err(|error| binding_error(ctx, error.to_string()))?,
            ]
        }
        Processor::Filter { kind, cutoff_q } => {
            let (audio, cutoff, q) = if let Some((cutoff, q)) = cutoff_q {
                (inputs[0], *cutoff, *q)
            } else {
                (inputs[0], inputs[1], inputs[2])
            };
            state.spend_node(ctx)?;
            let graph = &mut state.active_mut(ctx)?.graph;
            let output = match kind {
                FilterKind::Lowpass => graph.lowpass(audio, cutoff, q),
                FilterKind::Highpass => graph.highpass(audio, cutoff, q),
                FilterKind::Bandpass => graph.bandpass(audio, cutoff, q),
                FilterKind::Peak => graph.peak(audio, cutoff, q),
                FilterKind::Moog => graph.moog(audio, cutoff, q),
            };
            vec![output]
        }
        Processor::Shape { kind, amount } => {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .shape(one(inputs), *kind, *amount),
            ]
        }
        Processor::DcBlock => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.dcblock(one(inputs))]
        }
        Processor::Delay { seconds, range } => {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .delay(one(inputs), *seconds, *range),
            ]
        }
        Processor::AllpassDelay { seconds, gain } => {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .allpass_delay(one(inputs), *seconds, *gain),
            ]
        }
        Processor::Fdn {
            delays,
            damping,
            modulation_rate,
            modulation_depth,
            t60,
        } => {
            let ProcessorScalar::Bound { source, max } = t60 else {
                return Err(binding_error(
                    ctx,
                    "FDN send parameter was not resolved before graph publication",
                ));
            };
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.fdn(
                one(inputs),
                *source,
                FdnConfig {
                    delays: delays.clone(),
                    damping: *damping,
                    modulation_rate: *modulation_rate,
                    modulation_depth: *modulation_depth,
                    max_t60: *max,
                },
            )]
        }
        Processor::EnvelopeFollower { attack, release } => {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .envelope_follower(one(inputs), *attack, *release),
            ]
        }
        Processor::PitchTracker {
            min_hz,
            max_hz,
            default_hz,
            hold_seconds,
        } => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.pitch_tracker(
                one(inputs),
                *min_hz,
                *max_hz,
                *default_hz,
                *hold_seconds,
            )]
        }
        Processor::OnsetDetector {
            floor,
            hold_seconds,
        } => {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .onset_detector(one(inputs), *floor, *hold_seconds),
            ]
        }
        Processor::Width { amount } => {
            state.spend_node(ctx)?;
            state
                .active_mut(ctx)?
                .graph
                .width(inputs[0], inputs[1], *amount)
                .to_vec()
        }
        Processor::Reverb {
            room_size,
            time,
            damping,
        } => {
            state.spend_node(ctx)?;
            let (left, right) = state
                .active_mut(ctx)?
                .graph
                .reverb(inputs[0], inputs[1], *room_size, *time, *damping);
            vec![left, right]
        }
        Processor::Limiter { attack, release } => {
            state.spend_node(ctx)?;
            let (left, right) = state
                .active_mut(ctx)?
                .graph
                .limiter(inputs[0], inputs[1], *attack, *release);
            vec![left, right]
        }
        Processor::Chorus {
            seed,
            separation,
            variation,
            frequency,
        } => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.chorus(
                one(inputs),
                *seed,
                *separation,
                *variation,
                *frequency,
            )]
        }
        Processor::Feedback { .. } => {
            return Err(binding_error(
                ctx,
                "feedback must follow a fixed delay processor",
            ));
        }
        Processor::FeedbackDelay {
            delay_seconds,
            cutoff_q,
            amount,
        } => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.feedback_delay(
                one(inputs),
                *delay_seconds,
                *cutoff_q,
                *amount,
            )]
        }
        Processor::Slew { response_time } => {
            state.spend_node(ctx)?;
            vec![
                state
                    .active_mut(ctx)?
                    .graph
                    .slew(one(inputs), *response_time)
                    .map_err(|error| binding_error(ctx, error.to_string()))?,
            ]
        }
        Processor::Mul(gain) => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.mul(one(inputs), *gain)]
        }
        Processor::Ring { hz, decay } => {
            for _ in 0..3 {
                state.spend_node(ctx)?;
            }
            vec![
                stdlib::ring(&mut state.active_mut(ctx)?.graph, one(inputs), *hz, *decay)
                    .map_err(|error| binding_error(ctx, error.to_string()))?,
            ]
        }
        Processor::Pan(position) => {
            state.spend_node(ctx)?;
            let (left, right) = state.active_mut(ctx)?.graph.pan(one(inputs), *position);
            vec![left, right]
        }
        Processor::Send { bus, level, .. } => {
            state.active_mut(ctx)?.graph.send(*bus, inputs, *level);
            inputs.to_vec()
        }
        Processor::Chain(left, right) => {
            let intermediate = apply_processor(ctx, state, left, inputs)?;
            apply_processor(ctx, state, right, &intermediate)?
        }
        Processor::Stack(left, right) => {
            let split = processor_inputs(left);
            let mut outputs = apply_processor(ctx, state, left, &inputs[..split])?;
            outputs.extend(apply_processor(ctx, state, right, &inputs[split..])?);
            outputs
        }
        Processor::Sum(left, right) | Processor::Product(left, right) => {
            let left_outputs = apply_processor(ctx, state, left, inputs)?;
            let right_outputs = apply_processor(ctx, state, right, inputs)?;
            let mut outputs = Vec::with_capacity(left_outputs.len());
            for (left, right) in left_outputs.into_iter().zip(right_outputs) {
                state.spend_node(ctx)?;
                let graph = &mut state.active_mut(ctx)?.graph;
                outputs.push(if matches!(processor, Processor::Sum(_, _)) {
                    graph.add(left, right)
                } else {
                    graph.mul(left, right)
                });
            }
            outputs
        }
        Processor::Scale(inner, scale) => {
            let inner = apply_processor(ctx, state, inner, inputs)?;
            let mut outputs = Vec::with_capacity(inner.len());
            for output in inner {
                state.spend_node(ctx)?;
                outputs.push(state.active_mut(ctx)?.graph.mul(output, *scale));
            }
            outputs
        }
        Processor::Branch(left, right) => {
            let mut outputs = apply_processor(ctx, state, left, inputs)?;
            outputs.extend(apply_processor(ctx, state, right, inputs)?);
            outputs
        }
    };
    Ok(outputs)
}

fn current_generation<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
) -> Result<u64, Error<'gc>> {
    state
        .borrow()
        .active
        .as_ref()
        .map(|active| active.generation)
        .ok_or_else(|| binding_error(ctx, "graph primitive used outside voice.graph"))
}

fn processor_generation(state: &Rc<RefCell<BuildState>>) -> u64 {
    state
        .borrow()
        .active
        .as_ref()
        .map_or(0, |active| active.generation)
}

fn lua_pattern<'gc>(ctx: Context<'gc>, pattern: LuaPattern) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_pattern_meta") else {
        return Err(binding_error(
            ctx,
            "internal pattern metatable is unavailable",
        ));
    };
    let pattern = UserData::new_static(&ctx, pattern);
    pattern.set_metatable(&ctx, Some(metatable));
    Ok(pattern)
}

fn lua_chord<'gc>(ctx: Context<'gc>, chord: LuaChord) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_chord_meta") else {
        return Err(binding_error(
            ctx,
            "internal chord metatable is unavailable",
        ));
    };
    let chord = UserData::new_static(&ctx, chord);
    chord.set_metatable(&ctx, Some(metatable));
    Ok(chord)
}

fn lua_voice<'gc>(ctx: Context<'gc>, voice: usize) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_voice_meta") else {
        return Err(binding_error(
            ctx,
            "internal voice metatable is unavailable",
        ));
    };
    let voice = UserData::new_static(&ctx, LuaVoice(voice));
    voice.set_metatable(&ctx, Some(metatable));
    Ok(voice)
}

fn lua_patch<'gc>(ctx: Context<'gc>, patch: usize) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_patch_meta") else {
        return Err(binding_error(
            ctx,
            "internal patch metatable is unavailable",
        ));
    };
    let patch = UserData::new_static(&ctx, LuaPatch(patch));
    patch.set_metatable(&ctx, Some(metatable));
    Ok(patch)
}

fn lua_duration<'gc>(
    ctx: Context<'gc>,
    duration: LuaDuration,
) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_duration_meta") else {
        return Err(binding_error(
            ctx,
            "internal duration metatable is unavailable",
        ));
    };
    let duration = UserData::new_static(&ctx, duration);
    duration.set_metatable(&ctx, Some(metatable));
    Ok(duration)
}

fn lua_source<'gc>(
    ctx: Context<'gc>,
    generation: u64,
    channels: impl IntoIterator<Item = Source>,
) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_source_meta") else {
        return Err(binding_error(
            ctx,
            "internal graph signal metatable is unavailable",
        ));
    };
    let source = UserData::new_static(
        &ctx,
        LuaSource {
            generation,
            channels: channels.into_iter().collect(),
        },
    );
    source.set_metatable(&ctx, Some(metatable));
    Ok(source)
}

fn lua_audio_input<'gc>(
    ctx: Context<'gc>,
    id: AudioInputId,
    channels: usize,
) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_source_meta") else {
        return Err(binding_error(
            ctx,
            "internal graph signal metatable is unavailable",
        ));
    };
    let input = UserData::new_static(&ctx, LuaAudioInput { id, channels });
    input.set_metatable(&ctx, Some(metatable));
    Ok(input)
}

fn lua_shared_signal<'gc>(
    ctx: Context<'gc>,
    signal: LuaSharedSignal,
) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_source_meta") else {
        return Err(binding_error(
            ctx,
            "internal graph signal metatable is unavailable",
        ));
    };
    let signal = UserData::new_static(&ctx, signal);
    signal.set_metatable(&ctx, Some(metatable));
    Ok(signal)
}

fn lua_control<'gc>(ctx: Context<'gc>, id: ControlId) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_source_meta") else {
        return Err(binding_error(
            ctx,
            "internal graph signal metatable is unavailable",
        ));
    };
    let control = UserData::new_static(&ctx, LuaControl(id));
    control.set_metatable(&ctx, Some(metatable));
    Ok(control)
}

fn publish_shared_signal<'gc>(
    ctx: Context<'gc>,
    state: &mut BuildState,
    source: Source,
    min: f64,
    max: f64,
    default: f64,
) -> Result<LuaSharedSignal, Error<'gc>> {
    if !min.is_finite()
        || !max.is_finite()
        || !default.is_finite()
        || min > max
        || !(min..=max).contains(&default)
    {
        return Err(binding_error(
            ctx,
            "program signal has invalid range evidence",
        ));
    }
    if state.program.controls.specs().len() >= state.limits.controls {
        return Err(binding_error(
            ctx,
            format!("control limit of {} exceeded", state.limits.controls),
        ));
    }
    let ordinal = state.shared_signal_count;
    state.shared_signal_count += 1;
    let target = state
        .program
        .controls
        .add(ControlSpec::new(
            &format!("{INTERNAL_CONTROL_PREFIX}signal.{ordinal}"),
            min,
            max,
            default,
        ))
        .map_err(|error| binding_error(ctx, error.to_string()))?;
    state.spend_node(ctx)?;
    let writer = state.shared_graph.write_control(source, target);
    state.shared_writes.push(writer);
    Ok(LuaSharedSignal {
        source,
        control: target,
        min,
        max,
        default,
    })
}

fn lua_bundle<'gc>(
    ctx: Context<'gc>,
    generation: u64,
    inputs: Vec<Source>,
) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_bundle_meta") else {
        return Err(binding_error(
            ctx,
            "internal graph bundle metatable is unavailable",
        ));
    };
    let bundle = UserData::new_static(&ctx, LuaBundle { generation, inputs });
    bundle.set_metatable(&ctx, Some(metatable));
    Ok(bundle)
}

fn lua_processor<'gc>(
    ctx: Context<'gc>,
    generation: u64,
    processor: Processor,
) -> Result<UserData<'gc>, Error<'gc>> {
    let Value::Table(metatable) = ctx.get_global_value("__apteronotus_processor_meta") else {
        return Err(binding_error(
            ctx,
            "internal graph processor metatable is unavailable",
        ));
    };
    let processor = UserData::new_static(
        &ctx,
        LuaProcessor {
            generation,
            processor,
        },
    );
    processor.set_metatable(&ctx, Some(metatable));
    Ok(processor)
}

fn is_processor(value: Value<'_>) -> bool {
    let Value::UserData(data) = value else {
        return false;
    };
    data.downcast_static::<LuaProcessor>().is_ok()
}

fn read_processor<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    generation: u64,
) -> Result<Processor, Error<'gc>> {
    let Value::UserData(data) = value else {
        return Err(binding_error(ctx, "expected a graph processor"));
    };
    let processor = data
        .downcast_static::<LuaProcessor>()
        .map_err(|_| binding_error(ctx, "expected a graph processor"))?;
    if processor.generation != generation {
        return Err(binding_error(
            ctx,
            "a graph processor cannot be reused by another voice",
        ));
    }
    Ok(processor.processor.clone())
}

fn as_bundle(value: Value<'_>, generation: u64) -> Option<Result<Vec<Source>, &'static str>> {
    let Value::UserData(data) = value else {
        return None;
    };
    let bundle = data.downcast_static::<LuaBundle>().ok()?;
    Some(if bundle.generation == generation {
        Ok(bundle.inputs.clone())
    } else {
        Err("a graph bundle cannot be reused by another voice")
    })
}

fn read_pipe_inputs<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    generation: u64,
) -> Result<Vec<Source>, Error<'gc>> {
    if let Some(number) = value.to_number() {
        if number.is_finite() {
            return Ok(vec![Source::Const(number)]);
        }
        return Err(binding_error(ctx, "graph constants must be finite"));
    }
    let Value::UserData(data) = value else {
        return Err(binding_error(
            ctx,
            "a patch connection expects a signal or input bundle",
        ));
    };
    if let Ok(duration) = data.downcast_static::<LuaDuration>() {
        return match duration.unit {
            TimeUnit::Seconds => Ok(vec![Source::Const(duration.value)]),
            TimeUnit::Cycles => Err(binding_error(
                ctx,
                "a graph connection expects seconds, but bars are cycle time",
            )),
            TimeUnit::Beats {
                seconds_per_beat: Some(seconds_per_beat),
                ..
            } => Ok(vec![Source::Const(duration.value * seconds_per_beat)]),
            TimeUnit::Beats {
                seconds_per_beat: None,
                ..
            } => Err(binding_error(
                ctx,
                "a graph connection cannot convert beats under a changing tempo; use seconds",
            )),
        };
    }
    if let Ok(source) = data.downcast_static::<LuaSource>() {
        if source.generation != generation {
            return Err(binding_error(
                ctx,
                "a symbolic signal cannot be reused by another voice",
            ));
        }
        return Ok(source.channels.clone());
    }
    if let Ok(control) = data.downcast_static::<LuaControl>() {
        return Ok(vec![Source::Control(control.0)]);
    }
    if let Ok(signal) = data.downcast_static::<LuaSharedSignal>() {
        return Ok(vec![Source::Control(signal.control)]);
    }
    if let Ok(input) = data.downcast_static::<LuaAudioInput>() {
        return Ok((0..input.channels)
            .map(|channel| Source::ExternalAudio {
                input: input.id,
                channel,
            })
            .collect());
    }
    if let Ok(bundle) = data.downcast_static::<LuaBundle>() {
        if bundle.generation != generation {
            return Err(binding_error(
                ctx,
                "a graph bundle cannot be reused by another voice",
            ));
        }
        return Ok(bundle.inputs.clone());
    }
    Err(binding_error(
        ctx,
        "a patch connection expects a signal or input bundle",
    ))
}

fn read_mono<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    generation: u64,
) -> Result<Source, Error<'gc>> {
    if let Some(number) = value.to_number() {
        if number.is_finite() {
            return Ok(Source::Const(number));
        }
        return Err(binding_error(ctx, "graph constants must be finite"));
    }
    let Value::UserData(data) = value else {
        return Err(binding_error(ctx, "expected a number or graph signal"));
    };
    if let Ok(duration) = data.downcast_static::<LuaDuration>() {
        return match duration.unit {
            TimeUnit::Seconds => Ok(Source::Const(duration.value)),
            TimeUnit::Cycles => Err(binding_error(
                ctx,
                "a graph value expects seconds, but bars are cycle time",
            )),
            TimeUnit::Beats {
                seconds_per_beat: Some(seconds_per_beat),
                ..
            } => Ok(Source::Const(duration.value * seconds_per_beat)),
            TimeUnit::Beats {
                seconds_per_beat: None,
                ..
            } => Err(binding_error(
                ctx,
                "a graph value cannot convert beats under a changing tempo; use seconds",
            )),
        };
    }
    if let Ok(control) = data.downcast_static::<LuaControl>() {
        return Ok(Source::Control(control.0));
    }
    if let Ok(signal) = data.downcast_static::<LuaSharedSignal>() {
        return Ok(Source::Control(signal.control));
    }
    if let Ok(input) = data.downcast_static::<LuaAudioInput>() {
        return match input.channels {
            1 => Ok(Source::ExternalAudio {
                input: input.id,
                channel: 0,
            }),
            _ => Err(binding_error(
                ctx,
                "this primitive expects one channel; mix or select channels explicitly",
            )),
        };
    }
    let source = data
        .downcast_static::<LuaSource>()
        .map_err(|_| binding_error(ctx, "expected a graph signal"))?;
    if source.generation != generation {
        return Err(binding_error(
            ctx,
            "a symbolic signal cannot be reused by another voice",
        ));
    }
    match source.channels.as_slice() {
        [source] => Ok(*source),
        _ => Err(binding_error(
            ctx,
            "this primitive expects one channel; mix or select channels explicitly",
        )),
    }
}

fn read_arithmetic_channels<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    generation: u64,
) -> Result<Vec<Source>, Error<'gc>> {
    if let Value::UserData(data) = value {
        if data.downcast_static::<LuaBundle>().is_ok() {
            return Err(binding_error(
                ctx,
                "port bundles cannot participate in signal arithmetic",
            ));
        }
        if let Ok(source) = data.downcast_static::<LuaSource>() {
            if source.generation != generation {
                return Err(binding_error(
                    ctx,
                    "a symbolic signal cannot be reused by another voice",
                ));
            }
            return Ok(source.channels.clone());
        }
        if let Ok(input) = data.downcast_static::<LuaAudioInput>() {
            return Ok((0..input.channels)
                .map(|channel| Source::ExternalAudio {
                    input: input.id,
                    channel,
                })
                .collect());
        }
        if let Ok(signal) = data.downcast_static::<LuaSharedSignal>() {
            return Ok(vec![Source::Control(signal.control)]);
        }
    }
    Ok(vec![read_mono(ctx, value, generation)?])
}

fn align_arithmetic_channels<'gc>(
    ctx: Context<'gc>,
    left: Vec<Source>,
    right: Vec<Source>,
) -> Result<(Vec<Source>, Vec<Source>), Error<'gc>> {
    match (left.len(), right.len()) {
        (left_channels, right_channels) if left_channels == right_channels => Ok((left, right)),
        (1, right_channels) => Ok((vec![left[0]; right_channels], right)),
        (left_channels, 1) => Ok((left, vec![right[0]; left_channels])),
        (left_channels, right_channels) => Err(binding_error(
            ctx,
            format!(
                "signal arithmetic cannot align {left_channels} channels with {right_channels}"
            ),
        )),
    }
}

fn read_channels<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    generation: u64,
) -> Result<Vec<Source>, Error<'gc>> {
    if let Some(number) = value.to_number() {
        return Ok(vec![Source::Const(number)]);
    }
    let Value::UserData(data) = value else {
        return Err(binding_error(ctx, "voice.graph must return a graph signal"));
    };
    if let Ok(input) = data.downcast_static::<LuaAudioInput>() {
        return Ok((0..input.channels)
            .map(|channel| Source::ExternalAudio {
                input: input.id,
                channel,
            })
            .collect());
    }
    if let Ok(signal) = data.downcast_static::<LuaSharedSignal>() {
        return Ok(vec![Source::Control(signal.control)]);
    }
    let source = data
        .downcast_static::<LuaSource>()
        .map_err(|_| binding_error(ctx, "voice.graph must return a graph signal"))?;
    if source.generation != generation {
        return Err(binding_error(
            ctx,
            "voice.graph returned a signal from another voice",
        ));
    }
    Ok(source.channels.clone())
}

struct ParsedParamSpec {
    min: f64,
    max: f64,
    default: f64,
    unit: Option<String>,
    max_curve_seconds: Option<f64>,
}

fn read_param_spec<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<ParsedParamSpec, Error<'gc>> {
    let Value::Table(table) = value else {
        return Err(binding_error(ctx, "voice parameter spec must be a table"));
    };
    let field = |name, index| {
        let named = table.get_value(ctx, name);
        if named.is_nil() {
            table.get_value(ctx, index)
        } else {
            named
        }
    };
    let min = read_number(ctx, field("min", 1), "parameter minimum")?;
    let max = read_number(ctx, field("max", 2), "parameter maximum")?;
    let default = read_number(ctx, field("default", 3), "parameter default")?;
    let unit = {
        let unit = field("unit", 4);
        if unit.is_nil() {
            None
        } else {
            Some(read_string(ctx, unit, "parameter unit")?)
        }
    };
    let max_curve_seconds = {
        let value = field("max_curve_seconds", 5);
        if value.is_nil() {
            None
        } else {
            let seconds = read_seconds(ctx, value, "maximum curve horizon")?;
            if seconds < 0.0 {
                return Err(binding_error(
                    ctx,
                    "maximum curve horizon must be non-negative",
                ));
            }
            Some(seconds)
        }
    };
    if min > max {
        return Err(binding_error(
            ctx,
            "parameter minimum must not exceed maximum",
        ));
    }
    if !(min..=max).contains(&default) {
        return Err(binding_error(
            ctx,
            "parameter default must lie inside its range",
        ));
    }
    Ok(ParsedParamSpec {
        min,
        max,
        default,
        unit,
        max_curve_seconds,
    })
}

fn read_number<'gc>(ctx: Context<'gc>, value: Value<'gc>, what: &str) -> Result<f64, Error<'gc>> {
    value
        .to_number()
        .filter(|number| number.is_finite())
        .ok_or_else(|| binding_error(ctx, format!("{what} must be a finite number")))
}

fn read_seconds<'gc>(ctx: Context<'gc>, value: Value<'gc>, what: &str) -> Result<f64, Error<'gc>> {
    let Value::UserData(data) = value else {
        return read_number(ctx, value, what);
    };
    let Ok(duration) = data.downcast_static::<LuaDuration>() else {
        return read_number(ctx, value, what);
    };
    match duration.unit {
        TimeUnit::Seconds => Ok(duration.value),
        TimeUnit::Cycles => Err(binding_error(
            ctx,
            format!("{what} expects seconds, not bars/cycle time"),
        )),
        TimeUnit::Beats {
            seconds_per_beat: Some(seconds_per_beat),
            ..
        } => Ok(duration.value * seconds_per_beat),
        TimeUnit::Beats {
            seconds_per_beat: None,
            ..
        } => Err(binding_error(
            ctx,
            format!("{what} cannot convert beats under a changing tempo; use seconds"),
        )),
    }
}

fn read_pattern_time<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    what: &str,
) -> Result<Frac, Error<'gc>> {
    let Value::UserData(data) = value else {
        return Ok(Frac::approx(read_number(ctx, value, what)?, 1_000_000));
    };
    let Ok(duration) = data.downcast_static::<LuaDuration>() else {
        return Ok(Frac::approx(read_number(ctx, value, what)?, 1_000_000));
    };
    match duration.unit {
        TimeUnit::Cycles => Ok(Frac::approx(duration.value, 1_000_000)),
        TimeUnit::Beats {
            beats_per_cycle, ..
        } => Ok(Frac::approx(duration.value / beats_per_cycle, 1_000_000)),
        TimeUnit::Seconds => Err(binding_error(
            ctx,
            format!("{what} expects bars/cycle time, not seconds"),
        )),
    }
}

fn read_pattern_ratio<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    what: &str,
) -> Result<Frac, Error<'gc>> {
    Ok(Frac::approx(read_number(ctx, value, what)?, 1_000_000))
}

fn read_positive_i64<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    what: &str,
) -> Result<i64, Error<'gc>> {
    let number = read_number(ctx, value, what)?;
    if number < 1.0 || number.fract() != 0.0 || number > i64::MAX as f64 {
        return Err(binding_error(
            ctx,
            format!("{what} must be a positive integer"),
        ));
    }
    Ok(number as i64)
}

fn read_count<'gc>(ctx: Context<'gc>, value: Value<'gc>, what: &str) -> Result<usize, Error<'gc>> {
    let number = read_number(ctx, value, what)?;
    if number < 0.0 || number.fract() != 0.0 || number > usize::MAX as f64 {
        return Err(binding_error(
            ctx,
            format!("{what} must be a non-negative integer"),
        ));
    }
    Ok(number as usize)
}

fn read_delay_range<'gc>(
    ctx: Context<'gc>,
    seconds: Source,
    value: Value<'gc>,
) -> Result<DelayRange, Error<'gc>> {
    let range = if value.is_nil() {
        // A literal delay has one known allocation size. A symbolic delay does
        // not: requiring bounds here keeps allocation and publication policy
        // at construction time rather than sampling a runtime signal.
        let Source::Const(seconds) = seconds else {
            return Err(binding_error(
                ctx,
                "a symbolic delay time requires explicit allocation bounds",
            ));
        };
        DelayRange::fixed(seconds)
    } else if let Value::Table(table) = value {
        let field = |name, index| {
            let named = table.get_value(ctx, name);
            if named.is_nil() {
                table.get_value(ctx, index)
            } else {
                named
            }
        };
        DelayRange::new(
            read_seconds(ctx, field("min", 1), "delay minimum")?,
            read_seconds(ctx, field("max", 2), "delay maximum")?,
        )
    } else {
        DelayRange::new(0.0, read_seconds(ctx, value, "delay maximum")?)
    };
    range.map_err(|error| binding_error(ctx, error.to_string()))
}

fn is_event_seed(value: Value<'_>) -> bool {
    let Value::UserData(data) = value else {
        return false;
    };
    data.downcast_static::<LuaEventSeed>().is_ok()
}

fn read_stream<'gc>(ctx: Context<'gc>, value: Value<'gc>) -> Result<u64, Error<'gc>> {
    if let Some(number) = value.to_number() {
        if number >= 0.0 && number.fract() == 0.0 && number <= u64::MAX as f64 {
            return Ok(number as u64);
        }
        return Err(binding_error(
            ctx,
            "random stream must be a non-negative integer or string",
        ));
    }
    let Value::String(value) = value else {
        return Err(binding_error(
            ctx,
            "random stream must be a non-negative integer or string",
        ));
    };
    let bytes = value.as_bytes();
    // FNV-1a is spelled out so changing a dependency cannot reshuffle a song.
    // This maps the authored stream name only; event provenance is mixed later
    // by Rust when Op::InitRandom is instantiated.
    Ok(bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
    }))
}

fn read_call_site<'gc>(ctx: Context<'gc>, value: Value<'gc>) -> Result<u64, Error<'gc>> {
    let number = read_number(ctx, value, "call-site identity")?;
    if number < 0.0 || number.fract() != 0.0 || number > u64::MAX as f64 {
        return Err(binding_error(
            ctx,
            "call-site identity must be a non-negative integer",
        ));
    }
    Ok(number as u64)
}

fn read_document_base<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<Option<usize>, Error<'gc>> {
    let base = read_call_site(ctx, value)?;
    if base == 0 {
        return Ok(None);
    }
    usize::try_from(base)
        .map(Some)
        .map_err(|_| binding_error(ctx, "document byte offset does not fit this host"))
}

fn call_site_span(call_site: u64, name: &str) -> SrcSpan {
    let start =
        usize::try_from(call_site).unwrap_or_else(|_| usize::MAX.saturating_sub(name.len()));
    SrcSpan::new(start, start.saturating_add(name.len()))
}

fn read_string<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
    what: &str,
) -> Result<String, Error<'gc>> {
    let Value::String(value) = value else {
        return Err(binding_error(ctx, format!("{what} must be a string")));
    };
    value
        .to_str()
        .map(str::to_owned)
        .map_err(|_| binding_error(ctx, format!("{what} must be UTF-8")))
}

fn binding_error<'gc>(ctx: Context<'gc>, message: impl Into<String>) -> Error<'gc> {
    message.into().into_value(ctx).into()
}

fn pattern_nodes(pattern: &Pattern) -> usize {
    match pattern {
        Pattern::Silence | Pattern::Pure { .. } | Pattern::Signal { .. } => 1,
        Pattern::Stack(patterns)
        | Pattern::Group {
            members: patterns, ..
        }
        | Pattern::Slowcat(patterns)
        | Pattern::Choose {
            choices: patterns, ..
        } => 1usize.saturating_add(patterns.iter().map(pattern_nodes).sum()),
        Pattern::Timecat(parts) => 1usize.saturating_add(
            parts
                .iter()
                .map(|(_, pattern)| pattern_nodes(pattern))
                .sum(),
        ),
        Pattern::Fast { inner, .. }
        | Pattern::Shift { inner, .. }
        | Pattern::Hold { inner, .. }
        | Pattern::Rev(inner)
        | Pattern::Degrade { inner, .. }
        | Pattern::Ply { inner, .. }
        | Pattern::Segment { inner, .. }
        | Pattern::Range { inner, .. }
        | Pattern::Arp { inner, .. }
        | Pattern::GroupPrimary { inner }
        | Pattern::PrimaryAdd { inner, .. }
        | Pattern::Named { inner, .. } => 1usize.saturating_add(pattern_nodes(inner)),
        Pattern::When {
            then, otherwise, ..
        } => 1usize
            .saturating_add(pattern_nodes(then))
            .saturating_add(pattern_nodes(otherwise)),
        Pattern::Math { left, right, .. } => 1usize
            .saturating_add(pattern_nodes(left))
            .saturating_add(pattern_nodes(right)),
        Pattern::Merge {
            structure,
            controls,
        } => 1usize
            .saturating_add(pattern_nodes(structure))
            .saturating_add(pattern_nodes(controls.as_pattern())),
        Pattern::Timeline(timeline) => 1usize.saturating_add(timeline.events().len()),
    }
}
