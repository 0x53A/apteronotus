use crate::{Limits, Program, Track, VoiceId};
use apteronotus_pattern::{
    Basis, ControlValue, Curve, CurveClock, Frac, Pattern, Value as PatternValue, mini,
};
use apteronotus_synth::{
    Adsr, BusId, ControlId, ControlSpec, DelayRange, GraphBuilder, Implicit, Note, ParamId,
    ParamSpec, ParamValue, PatchTemplate, ShapeKind, Source, n, stdlib,
};
use piccolo::{
    Callback, CallbackReturn, Context, Error, IntoValue, MetaMethod, Table, UserData, Value,
};
use std::{cell::RefCell, rc::Rc};

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
  local controls = __begin_patch(spec.inputs, spec.params)
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
    limits: Limits,
}

pub(crate) struct ActiveGraph {
    graph: GraphBuilder,
    generation: u64,
    lifetime: ActiveLifetime,
    input_channels: usize,
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
    Saw,
    Pulse,
    Filter {
        kind: FilterKind,
        cutoff_q: Option<(Source, Source)>,
    },
    Shape {
        kind: ShapeKind,
        amount: f64,
    },
    DcBlock,
    Delay {
        seconds: Source,
        range: DelayRange,
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

#[derive(Clone, Copy, Debug)]
enum FilterKind {
    Lowpass,
    Highpass,
    Bandpass,
    Moog,
}

#[derive(Clone, Debug)]
struct LuaPattern {
    pattern: Pattern,
    controls: Vec<PatternControl>,
}

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
}

impl PatternTransform {
    fn apply(&self, pattern: Pattern) -> Pattern {
        match self {
            Self::Fast(factor) => pattern.fast(*factor),
            Self::Slow(factor) => pattern.slow(*factor),
            Self::Shift(by) => pattern.late(*by),
            Self::Rev => pattern.rev(),
            Self::Degrade { amount, seed } => pattern.degrade_by(*amount, *seed),
            Self::Segment(steps) => pattern.segment(*steps),
            Self::Range { min, max } => pattern.range(*min, *max),
            Self::Every { cycles, transform } => {
                pattern.every(*cycles, |branch| transform.apply(branch))
            }
            Self::Off { by, transform } => pattern.off(*by, |branch| transform.apply(branch)),
            Self::Sometimes {
                amount,
                seed,
                transform,
            } => pattern.sometimes_by(*amount, *seed, |branch| transform.apply(branch)),
        }
    }
}

#[derive(Clone, Debug)]
struct LuaPatternTransform(PatternTransform);

#[derive(Clone, Debug)]
struct PatternControl {
    name: String,
    value: ControlValue,
}

#[derive(Clone, Debug)]
struct LuaCurve(Curve);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TimeUnit {
    Seconds,
    Cycles,
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
    set_callback(ctx, "bus", state.clone(), bus)?;
    set_callback(ctx, "run", state.clone(), run)?;
    set_callback(ctx, "secs", state.clone(), seconds)?;
    set_callback(ctx, "ms", state.clone(), milliseconds)?;
    set_callback(ctx, "bars", state.clone(), bars)?;
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
    set_callback(ctx, "range", state.clone(), range_pattern)?;
    set_callback(ctx, "degrade", state.clone(), degrade_pattern)?;
    set_callback(ctx, "__degrade_at", state.clone(), degrade_pattern_at)?;
    set_callback(ctx, "every", state.clone(), every_pattern)?;
    set_callback(ctx, "off", state.clone(), off_pattern)?;
    set_callback(ctx, "sometimes", state.clone(), sometimes_pattern)?;
    set_callback(ctx, "__sometimes_at", state.clone(), sometimes_pattern_at)?;
    set_callback(ctx, "curve", state.clone(), event_curve)?;
    set_callback(ctx, "note", state.clone(), note_pattern)?;
    set_callback(ctx, "velocity", state.clone(), velocity)?;

    set_callback(ctx, "sine", state.clone(), sine)?;
    set_callback(ctx, "dc", state.clone(), dc)?;
    set_callback(ctx, "saw", state.clone(), saw)?;
    set_callback(ctx, "pulse", state.clone(), pulse)?;
    set_callback(ctx, "noise", state.clone(), noise)?;
    set_callback(ctx, "impulse", state.clone(), impulse)?;
    set_callback(ctx, "init_random", state.clone(), init_random)?;
    set_callback(ctx, "init_rand", state.clone(), init_random)?;
    set_callback(ctx, "lowpass", state.clone(), lowpass)?;
    set_callback(ctx, "highpass", state.clone(), highpass)?;
    set_callback(ctx, "bandpass", state.clone(), bandpass)?;
    set_callback(ctx, "moog", state.clone(), moog)?;
    set_callback(ctx, "shape", state.clone(), shape)?;
    set_callback(ctx, "dcblock", state.clone(), dcblock)?;
    set_callback(ctx, "delay", state.clone(), delay)?;
    set_callback(ctx, "add", state.clone(), add)?;
    set_callback(ctx, "sub", state.clone(), sub)?;
    set_callback(ctx, "mul", state.clone(), mul)?;
    set_callback(ctx, "div", state.clone(), div)?;
    set_callback(ctx, "neg", state.clone(), neg)?;
    set_callback(ctx, "mix", state.clone(), mix)?;
    set_callback(ctx, "adsr", state.clone(), adsr)?;
    set_callback(ctx, "step", state.clone(), step)?;
    set_callback(ctx, "ramp", state.clone(), ramp)?;
    set_callback(ctx, "line", state.clone(), ramp)?;
    set_callback(ctx, "decay", state.clone(), decay)?;
    set_callback(ctx, "window", state.clone(), window)?;
    set_callback(ctx, "ring", state.clone(), ring)?;
    set_callback(ctx, "pan", state.clone(), pan)?;
    set_callback(ctx, "to", state.clone(), to)?;

    let source_meta = Table::new(&ctx);
    set_operator(ctx, source_meta, MetaMethod::Add, state.clone(), add)?;
    set_operator(ctx, source_meta, MetaMethod::Sub, state.clone(), sub)?;
    set_operator(ctx, source_meta, MetaMethod::Mul, state.clone(), mul)?;
    set_operator(ctx, source_meta, MetaMethod::Div, state.clone(), div)?;
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
        MetaMethod::Shr,
        state.clone(),
        merge_patterns,
    )?;
    ctx.set_global("__apteronotus_pattern_meta", pattern_meta);
    ctx.set_global(
        "rev",
        UserData::new_static(&ctx, LuaPatternTransform(PatternTransform::Rev)),
    );

    let voice_meta = Table::new(&ctx);
    set_operator(ctx, voice_meta, MetaMethod::Index, state, voice_index)?;
    ctx.set_global("__apteronotus_voice_meta", voice_meta);

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
    stack.replace(ctx, UserData::new_static(&ctx, LuaPatch(id)));
    Ok(CallbackReturn::Return)
}

fn control<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(table) = stack.get(0) else {
        return Err(binding_error(ctx, "control expects a specification table"));
    };
    let name = read_string(ctx, table.get_value(ctx, "name"), "control name")?;
    let Value::Table(range) = table.get_value(ctx, "range") else {
        return Err(binding_error(
            ctx,
            "control.range must be a two-number table",
        ));
    };
    let min = read_number(ctx, range.get_value(ctx, 1), "control minimum")?;
    let max = read_number(ctx, range.get_value(ctx, 2), "control maximum")?;
    let default = read_number(ctx, table.get_value(ctx, "default"), "control default")?;
    let mut spec = ControlSpec::new(&name, min, max, default);
    let unit = table.get_value(ctx, "unit");
    if !unit.is_nil() {
        spec = spec.with_unit(&read_string(ctx, unit, "control unit")?);
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
    stack.replace(ctx, UserData::new_static(&ctx, LuaControl(id)));
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
    let mut state = state.borrow_mut();
    if patch >= state.program.patches.len() {
        return Err(binding_error(
            ctx,
            "patch handle does not belong to this edit",
        ));
    }
    state.program.runs.push(crate::PatchId(patch));
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
        UserData::new_static(
            &ctx,
            LuaDuration {
                value,
                unit: TimeUnit::Seconds,
            },
        ),
    );
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
        UserData::new_static(
            &ctx,
            LuaDuration {
                value,
                unit: TimeUnit::Seconds,
            },
        ),
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
        UserData::new_static(
            &ctx,
            LuaDuration {
                value,
                unit: TimeUnit::Cycles,
            },
        ),
    );
    Ok(CallbackReturn::Return)
}

fn pattern<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    parse_pattern(ctx, state, &mut stack, 0, 0)
}

fn pattern_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let binding = read_call_site(ctx, stack.get(0))?;
    parse_pattern(ctx, state, &mut stack, 1, binding)
}

fn parse_pattern<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    source_index: usize,
    binding: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let source = read_string(ctx, stack.get(source_index), "pattern source")?;
    let pattern =
        mini::parse_at(&source, binding).map_err(|error| binding_error(ctx, error.to_string()))?;
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(
        ctx,
        lua_pattern(
            ctx,
            LuaPattern {
                pattern,
                controls: Vec::new(),
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
    play_bound(ctx, state, &mut stack, 0, 0)
}

fn play_at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let binding = read_call_site(ctx, stack.get(0))?;
    play_bound(ctx, state, &mut stack, 1, binding)
}

fn play_bound<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    binding: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(voice) = stack.get(argument_offset) else {
        return Err(binding_error(ctx, "play voice must be a voice handle"));
    };
    let voice = voice
        .downcast_static::<LuaVoice>()
        .map_err(|_| binding_error(ctx, "play voice must be a voice handle"))?
        .0;
    let pattern = match stack.get(argument_offset + 1) {
        Value::String(source) => {
            let source = source
                .to_str()
                .map_err(|_| binding_error(ctx, "pattern source must be UTF-8"))?;
            mini::parse_at(source, binding)
                .map_err(|error| binding_error(ctx, error.to_string()))?
        }
        Value::UserData(data) => data
            .downcast_static::<LuaPattern>()
            .map_err(|_| binding_error(ctx, "play pattern must come from pattern()"))?
            .pattern
            .clone(),
        _ => return Err(binding_error(ctx, "play expects a pattern or string")),
    };

    let mut state = state.borrow_mut();
    if voice >= state.program.voices.len() {
        return Err(binding_error(
            ctx,
            "voice handle does not belong to this edit",
        ));
    }
    if state.program.tracks.len() >= state.limits.tracks {
        return Err(binding_error(
            ctx,
            format!("track limit of {} exceeded", state.limits.tracks),
        ));
    }
    if let Value::UserData(data) = stack.get(argument_offset + 1)
        && let Ok(lua_pattern) = data.downcast_static::<LuaPattern>()
    {
        let template = &state.program.voices[voice];
        for control in &lua_pattern.controls {
            validate_voice_control(ctx, template, &control.name, &control.value)?;
        }
    }
    state.spend_pattern(ctx, &pattern)?;
    state.program.tracks.push(Track {
        voice: VoiceId(voice),
        pattern,
    });
    stack.clear();
    Ok(CallbackReturn::Return)
}

fn merge_patterns<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let left = read_lua_pattern(ctx, stack.get(0))?;
    let right = stack.get(1);
    let (pattern, controls) = if let Value::UserData(data) = right
        && let Ok(transform) = data.downcast_static::<LuaPatternTransform>()
    {
        (
            transform.0.apply(left.pattern.clone()),
            left.controls.clone(),
        )
    } else {
        let right = read_lua_pattern(ctx, right)?;
        let pattern = left
            .pattern
            .clone()
            .merge(right.pattern.clone())
            .map_err(|error| binding_error(ctx, error.to_string()))?;
        let mut controls = left.controls.clone();
        controls.extend(right.controls.clone());
        (pattern, controls)
    };
    state.borrow_mut().spend_pattern(ctx, &pattern)?;
    stack.replace(ctx, lua_pattern(ctx, LuaPattern { pattern, controls })?);
    Ok(CallbackReturn::Return)
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
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
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
    let transform = read_pattern_transform(ctx, stack.get(1))?.0.clone();
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
    let transform = read_pattern_transform(ctx, stack.get(1))?.0.clone();
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

fn make_sometimes_transform<'gc>(
    ctx: Context<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
    argument_offset: usize,
    call_site: u64,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let amount = read_probability(ctx, stack.get(argument_offset), "sometimes probability")?;
    let transform = read_pattern_transform(ctx, stack.get(argument_offset + 1))?
        .0
        .clone();
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
        control_param_id(ctx, template, &name)?;
    }

    let state = state.clone();
    let callback_name = name.clone();
    let callback = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        let value = read_control_value(ctx, stack.get(0))?;
        {
            let state = state.borrow();
            let template =
                state.program.voices.get(voice).ok_or_else(|| {
                    binding_error(ctx, "voice handle does not belong to this edit")
                })?;
            validate_voice_control(ctx, template, &callback_name, &value)?;
        }
        replace_with_named_control(ctx, &state, &mut stack, &callback_name, value)
    });
    stack.replace(ctx, callback);
    Ok(CallbackReturn::Return)
}

fn velocity<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let value = read_control_value(ctx, stack.get(0))?;
    replace_with_named_control(ctx, state, &mut stack, "velocity", value)
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
            },
        )?,
    );
    Ok(CallbackReturn::Return)
}

fn event_curve<'gc>(
    ctx: Context<'gc>,
    _state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::Table(table) = stack.get(0) else {
        return Err(binding_error(ctx, "curve expects a table"));
    };
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
                    value,
                }],
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

fn read_pattern_transform<'gc>(
    ctx: Context<'gc>,
    value: Value<'gc>,
) -> Result<&'gc LuaPatternTransform, Error<'gc>> {
    let Value::UserData(data) = value else {
        return Err(binding_error(ctx, "expected a pattern transform"));
    };
    data.downcast_static::<LuaPatternTransform>()
        .map_err(|_| binding_error(ctx, "expected a pattern transform"))
}

macro_rules! generator_one {
    ($name:ident, $method:ident, $processor:ident) => {
        fn $name<'gc>(
            ctx: Context<'gc>,
            state: &Rc<RefCell<BuildState>>,
            mut stack: piccolo::Stack<'gc, '_>,
        ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
            let generation = current_generation(ctx, state)?;
            if stack.is_empty() {
                stack.replace(ctx, lua_processor(ctx, generation, Processor::$processor)?);
                return Ok(CallbackReturn::Return);
            }
            let input = read_mono(ctx, stack.get(0), generation)?;
            let mut state = state.borrow_mut();
            state.spend_node(ctx)?;
            let source = state.active_mut(ctx)?.graph.$method(input);
            stack.replace(ctx, lua_source(ctx, generation, [source])?);
            Ok(CallbackReturn::Return)
        }
    };
}

generator_one!(sine, sine, Sine);
generator_one!(saw, saw, Saw);

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
    let source = state.active_mut(ctx)?.graph.init_random(stream, min, max);
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
            let generation = current_generation(ctx, state)?;
            if stack.len() <= 2 {
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
filter!(moog, moog, Moog);

fn shape<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let offset = usize::from(stack.len() >= 3);
    let kind = match read_string(ctx, stack.get(offset), "shape kind")?.as_str() {
        "tanh" => ShapeKind::Tanh,
        "atan" => ShapeKind::Atan,
        "soft" | "softsign" => ShapeKind::Softsign,
        "clip" => ShapeKind::Clip,
        "crush" => ShapeKind::Crush,
        other => return Err(binding_error(ctx, format!("unknown shape kind {other:?}"))),
    };
    let amount = read_number(ctx, stack.get(offset + 1), "shape amount")?;
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
    let generation = current_generation(ctx, state)?;
    if stack.is_empty() {
        stack.replace(ctx, lua_processor(ctx, generation, Processor::DcBlock)?);
        return Ok(CallbackReturn::Return);
    }
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
    let generation = current_generation(ctx, state)?;
    let processor_form = stack.len() <= 2;
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

macro_rules! binary {
    ($name:ident, $method:ident) => {
        fn $name<'gc>(
            ctx: Context<'gc>,
            state: &Rc<RefCell<BuildState>>,
            mut stack: piccolo::Stack<'gc, '_>,
        ) -> Result<CallbackReturn<'gc>, Error<'gc>> {
            let generation = current_generation(ctx, state)?;
            let a = read_mono(ctx, stack.get(0), generation)?;
            let b = read_mono(ctx, stack.get(1), generation)?;
            let mut state = state.borrow_mut();
            state.spend_node(ctx)?;
            let source = state.active_mut(ctx)?.graph.$method(a, b);
            stack.replace(ctx, lua_source(ctx, generation, [source])?);
            Ok(CallbackReturn::Return)
        }
    };
}

binary!(add, add);
binary!(sub, sub);
binary!(div, div);

fn mul<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    if stack.len() == 1 {
        let gain = read_mono(ctx, stack.get(0), generation)?;
        stack.replace(ctx, lua_processor(ctx, generation, Processor::Mul(gain))?);
        return Ok(CallbackReturn::Return);
    }
    let a = read_mono(ctx, stack.get(0), generation)?;
    let b = read_mono(ctx, stack.get(1), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.mul(a, b);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
    Ok(CallbackReturn::Return)
}

fn neg<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let generation = current_generation(ctx, state)?;
    let input = read_mono(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    state.spend_node(ctx)?;
    let source = state.active_mut(ctx)?.graph.neg(input);
    stack.replace(ctx, lua_source(ctx, generation, [source])?);
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
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let delay = if stack.is_empty() {
        0.0
    } else {
        read_seconds(ctx, stack.get(0), "step delay")?
    };
    curve_node(
        ctx,
        state,
        &mut stack,
        Curve::default().term(Basis::Step, 1.0, delay, 0.0),
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
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let begin = read_seconds(ctx, stack.get(0), "window beginning")?;
    let end = read_seconds(ctx, stack.get(1), "window end")?;
    curve_node(
        ctx,
        state,
        &mut stack,
        Curve::window(apteronotus_synth::CurveClock::NoteSeconds, begin, end),
    )
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
        let value = read_control_value(ctx, stack.get(0))?;
        return replace_with_named_control(ctx, state, &mut stack, "pan", value);
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
    let generation = current_generation(ctx, state)?;
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(ctx, "to expects a bus handle"));
    };
    let bus = *data
        .downcast_static::<LuaBus>()
        .map_err(|_| binding_error(ctx, "to expects a bus handle"))?;
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
    let generation = current_generation(ctx, state)?;

    if is_processor(stack.get(0)) && is_processor(stack.get(1)) {
        let left = read_processor(ctx, stack.get(0), generation)?;
        let right = read_processor(ctx, stack.get(1), generation)?;
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

    if let Some(right) = as_bundle(stack.get(1), generation) {
        let mut inputs = read_pipe_inputs(ctx, stack.get(0), generation)?;
        inputs.extend(right.map_err(|message| binding_error(ctx, message))?);
        stack.replace(ctx, lua_bundle(ctx, generation, inputs)?);
        return Ok(CallbackReturn::Return);
    }

    let processor = read_processor(ctx, stack.get(1), generation)?;
    let inputs = read_pipe_inputs(ctx, stack.get(0), generation)?;
    let mut state = state.borrow_mut();
    let outputs = apply_processor(ctx, &mut state, &processor, &inputs)?;
    stack.replace(ctx, lua_source(ctx, generation, outputs)?);
    Ok(CallbackReturn::Return)
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
    let generation = current_generation(ctx, state)?;
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
    let generation = current_generation(ctx, state)?;
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
    let generation = current_generation(ctx, state)?;
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
    let generation = current_generation(ctx, state)?;
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
        Processor::Sine
        | Processor::Saw
        | Processor::Shape { .. }
        | Processor::DcBlock
        | Processor::Delay { .. }
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
        Processor::Pan(_) => 2,
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
        Processor::Saw => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.saw(one(inputs))]
        }
        Processor::Pulse => {
            state.spend_node(ctx)?;
            vec![state.active_mut(ctx)?.graph.pulse(inputs[0], inputs[1])]
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
        };
    }
    if let Ok(control) = data.downcast_static::<LuaControl>() {
        return Ok(Source::Control(control.0));
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
        Pattern::Silence | Pattern::Pure { .. } | Pattern::Signal(_) => 1,
        Pattern::Stack(patterns)
        | Pattern::Group {
            members: patterns, ..
        }
        | Pattern::Slowcat(patterns) => {
            1usize.saturating_add(patterns.iter().map(pattern_nodes).sum())
        }
        Pattern::Timecat(parts) => 1usize.saturating_add(
            parts
                .iter()
                .map(|(_, pattern)| pattern_nodes(pattern))
                .sum(),
        ),
        Pattern::Fast { inner, .. }
        | Pattern::Shift { inner, .. }
        | Pattern::Rev(inner)
        | Pattern::Degrade { inner, .. }
        | Pattern::Segment { inner, .. }
        | Pattern::Range { inner, .. }
        | Pattern::Named { inner, .. } => 1usize.saturating_add(pattern_nodes(inner)),
        Pattern::When {
            then, otherwise, ..
        } => 1usize
            .saturating_add(pattern_nodes(then))
            .saturating_add(pattern_nodes(otherwise)),
        Pattern::Merge {
            structure,
            controls,
        } => 1usize
            .saturating_add(pattern_nodes(structure))
            .saturating_add(pattern_nodes(controls.as_pattern())),
        Pattern::Timeline(timeline) => 1usize.saturating_add(timeline.events().len()),
    }
}
