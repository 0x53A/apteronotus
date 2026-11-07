//! Evaluation-only stream DAG. Lowered to owned graph data before publication.
use super::*;
use apteronotus_synth::spatial::{Emitter, Listener, Material, RoomSpec};
use apteronotus_synth::{GraphSend, Input};
#[derive(Clone, Debug)]
pub(super) enum Stream {
    Bus(LuaBus),
    External(LuaAudioInput),
    Node(usize, usize),
}
#[derive(Clone, Debug)]
enum StreamNode {
    Process(Stream, Processor),
    Patch(usize, Vec<Stream>),
    Room(RoomSpec, Vec<Stream>, Vec<Source>),
    Mono(Stream),
    Mix(Vec<Stream>),
}
#[derive(Clone, Debug)]
struct RoomDraft {
    size: [f64; 3],
    walls: Material,
    floor: Material,
    ceiling: Material,
    decay: f64,
    sources: Vec<(Stream, Emitter, Option<f64>)>,
    sealed: bool,
}
#[derive(Clone, Copy, Debug)]
struct RoomHandle(usize);
#[derive(Clone, Debug)]
struct ListenerHandle(Listener);
#[derive(Clone, Copy, Debug)]
struct StereoPair(f64);
#[derive(Default)]
pub(super) struct Streams {
    nodes: Vec<StreamNode>,
    rooms: Vec<RoomDraft>,
    output: Option<Stream>,
    drivers: HashMap<BusId, Stream>,
    track_routes: Vec<(usize, LuaBus)>,
    midi_routes: Vec<(usize, LuaBus)>,
    run_routes: Vec<(usize, LuaBus)>,
}
pub(super) fn install<'gc>(
    ctx: Context<'gc>,
    state: Rc<RefCell<BuildState>>,
) -> Result<(), Error<'gc>> {
    set_callback(ctx, "convolve", state.clone(), convolve)?;
    set_callback(ctx, "room", state.clone(), room)?;
    set_callback(ctx, "listener", state.clone(), listener)?;
    set_callback(ctx, "stereo_pair", state.clone(), stereo_pair)?;
    set_callback(ctx, "downmix", state.clone(), downmix)?;
    set_callback(ctx, "output", state.clone(), output)?;
    let meta = Table::new(&ctx);
    set_operator(ctx, meta, MetaMethod::Shr, state.clone(), super::pipe)?;
    ctx.set_global("__apteronotus_stream_meta", meta);
    let meta = Table::new(&ctx);
    set_operator(ctx, meta, MetaMethod::Index, state, room_index)?;
    ctx.set_global("__apteronotus_room_meta", meta);
    Ok(())
}
pub(super) fn bus_value<'gc>(ctx: Context<'gc>, bus: LuaBus) -> UserData<'gc> {
    let data = UserData::new_static(&ctx, bus);
    if let Value::Table(meta) = ctx.get_global_value("__apteronotus_stream_meta") {
        data.set_metatable(&ctx, Some(meta));
    }
    data
}
fn value<'gc>(ctx: Context<'gc>, stream: Stream) -> UserData<'gc> {
    let data = UserData::new_static(&ctx, stream);
    if let Value::Table(meta) = ctx.get_global_value("__apteronotus_stream_meta") {
        data.set_metatable(&ctx, Some(meta));
    }
    data
}
fn maybe_stream(value: Value<'_>) -> Option<Stream> {
    let Value::UserData(data) = value else {
        return None;
    };
    if let Ok(s) = data.downcast_static::<Stream>() {
        Some(s.clone())
    } else if let Ok(b) = data.downcast_static::<LuaBus>() {
        Some(Stream::Bus(*b))
    } else if let Ok(a) = data.downcast_static::<LuaAudioInput>() {
        Some(Stream::External(*a))
    } else {
        None
    }
}
fn read<'gc>(ctx: Context<'gc>, value: Value<'gc>) -> Result<Stream, Error<'gc>> {
    maybe_stream(value)
        .ok_or_else(|| binding_error(ctx, "expected an audio stream, bus or audio_input"))
}
fn channels(stream: &Stream) -> usize {
    match stream {
        Stream::Bus(b) => b.channels,
        Stream::External(a) => a.channels,
        Stream::Node(_, n) => *n,
    }
}
fn add<'gc>(
    ctx: Context<'gc>,
    state: &mut BuildState,
    node: StreamNode,
    n: usize,
) -> Result<Stream, Error<'gc>> {
    if state.active.is_some() {
        return Err(binding_error(
            ctx,
            "stream topology must be declared outside a voice or patch graph",
        ));
    }
    state.spend_node(ctx)?;
    let id = state.streams.nodes.len();
    state.streams.nodes.push(node);
    Ok(Stream::Node(id, n))
}
pub(super) fn pipe<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
) -> Result<bool, Error<'gc>> {
    let Some(stream) = maybe_stream(stack.get(0)) else {
        return Ok(false);
    };
    if state.borrow().active.is_some() {
        return Ok(false);
    }
    // Preserve the established audio_input-to-control analyzer boundary.
    let processor = if let Value::UserData(data) = stack.get(1)
        && let Ok(pattern) = data.downcast_static::<LuaPattern>()
        && pattern.controls.len() == 1
        && pattern.controls[0].name == "pan"
    {
        let control = &pattern.controls[0];
        let position = if let Some(id) = control.live {
            Source::Control(id)
        } else if let Some(ControlValue::Number(v)) = control.value {
            Source::Const(v)
        } else {
            return Err(binding_error(
                ctx,
                "stream pan requires a scalar or live control, not an event pattern",
            ));
        };
        Processor::Pan(position)
    } else {
        read_processor(ctx, stack.get(1), 0)?
    };
    if matches!(stream, Stream::External(_))
        && matches!(
            processor,
            Processor::EnvelopeFollower { .. }
                | Processor::PitchTracker { .. }
                | Processor::OnsetDetector { .. }
        )
    {
        return Ok(false);
    }
    let processor = broadcast_mono_processor(processor, channels(&stream));
    if processor_inputs(&processor) != channels(&stream) {
        return Err(binding_error(
            ctx,
            "stream processor input channel mismatch",
        ));
    }
    let n = processor_outputs(&processor);
    let next = add(
        ctx,
        &mut state.borrow_mut(),
        StreamNode::Process(stream, processor),
        n,
    )?;
    stack.replace(ctx, value(ctx, next));
    Ok(true)
}
fn fields<'gc>(ctx: Context<'gc>, t: Table<'gc>, known: &[&str]) -> Result<(), Error<'gc>> {
    for (key, _) in t.iter() {
        if !matches!(key,Value::String(k) if known.iter().any(|name|k.as_bytes()==name.as_bytes()))
        {
            return Err(binding_error(ctx, "unknown stream/room option"));
        }
    }
    Ok(())
}
fn table<'gc>(ctx: Context<'gc>, value: Value<'gc>, name: &str) -> Result<Table<'gc>, Error<'gc>> {
    if let Value::Table(t) = value {
        Ok(t)
    } else {
        Err(binding_error(ctx, format!("{name} expects a table")))
    }
}
fn number<'gc>(
    ctx: Context<'gc>,
    v: Value<'gc>,
    default: f64,
    name: &str,
) -> Result<f64, Error<'gc>> {
    if matches!(v, Value::Nil) {
        return Ok(default);
    }
    let n = v
        .to_number()
        .ok_or_else(|| binding_error(ctx, format!("{name} expects a number")))?;
    if !n.is_finite() {
        return Err(binding_error(ctx, format!("{name} must be finite")));
    }
    Ok(n)
}
fn vector<'gc>(ctx: Context<'gc>, v: Value<'gc>, name: &str) -> Result<[f64; 3], Error<'gc>> {
    let t = table(ctx, v, name)?;
    if t.length() != 3 {
        return Err(binding_error(
            ctx,
            format!("{name} requires three coordinates"),
        ));
    }
    let mut out = [0.; 3];
    for (i, x) in out.iter_mut().enumerate() {
        *x = number(ctx, t.get_value(ctx, (i + 1) as i64), f64::NAN, name)?;
        if !x.is_finite() {
            return Err(binding_error(ctx, "coordinates must be finite"));
        }
    }
    Ok(out)
}
fn material<'gc>(ctx: Context<'gc>, v: Value<'gc>) -> Result<Material, Error<'gc>> {
    if matches!(v, Value::Nil) {
        return Ok(Material::Wood);
    }
    match read_string(ctx, v, "surface material")?.as_str() {
        "wood" => Ok(Material::Wood),
        "plaster" => Ok(Material::Plaster),
        "curtain" => Ok(Material::Curtain),
        "stone" => Ok(Material::Stone),
        _ => Err(binding_error(
            ctx,
            "surface must be wood, plaster, curtain or stone",
        )),
    }
}
fn room<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let t = table(ctx, stack.get(0), "room")?;
    fields(ctx, t, &["size", "surfaces", "decay"])?;
    let size = vector(ctx, t.get_value(ctx, "size"), "room size")?;
    let surfaces = match t.get_value(ctx, "surfaces") {
        Value::Nil => Table::new(&ctx),
        v => table(ctx, v, "surfaces")?,
    };
    fields(ctx, surfaces, &["walls", "floor", "ceiling"])?;
    let decay = match t.get_value(ctx, "decay") {
        Value::Nil => 0.58,
        v => read_seconds(ctx, v, "room decay")?,
    };
    let draft = RoomDraft {
        size,
        walls: material(ctx, surfaces.get_value(ctx, "walls"))?,
        floor: material(ctx, surfaces.get_value(ctx, "floor"))?,
        ceiling: material(ctx, surfaces.get_value(ctx, "ceiling"))?,
        decay,
        sources: Vec::new(),
        sealed: false,
    };
    let mut s = state.borrow_mut();
    if s.active.is_some() {
        return Err(binding_error(ctx, "room must be declared outside a graph"));
    }
    s.spend_node(ctx)?;
    let id = s.streams.rooms.len();
    s.streams.rooms.push(draft);
    let data = UserData::new_static(&ctx, RoomHandle(id));
    if let Value::Table(meta) = ctx.get_global_value("__apteronotus_room_meta") {
        data.set_metatable(&ctx, Some(meta));
    }
    stack.replace(ctx, data);
    Ok(CallbackReturn::Return)
}
fn listener<'gc>(
    ctx: Context<'gc>,
    _: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let t = table(ctx, stack.get(0), "listener")?;
    fields(ctx, t, &["position", "facing"])?;
    let facing = match t.get_value(ctx, "facing") {
        Value::Nil => [0., 1., 0.],
        v => vector(ctx, v, "listener facing")?,
    };
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            ListenerHandle(Listener {
                position: vector(ctx, t.get_value(ctx, "position"), "listener position")?,
                facing,
            }),
        ),
    );
    Ok(CallbackReturn::Return)
}
fn stereo_pair<'gc>(
    ctx: Context<'gc>,
    _: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let t = table(ctx, stack.get(0), "stereo_pair")?;
    fields(ctx, t, &["spacing"])?;
    let width = number(ctx, t.get_value(ctx, "spacing"), 0.8, "spacing")?;
    if !(0.01..=10.).contains(&width) {
        return Err(binding_error(
            ctx,
            "stereo_pair spacing must be 0.01..10 metres",
        ));
    }
    stack.replace(ctx, UserData::new_static(&ctx, StereoPair(width)));
    Ok(CallbackReturn::Return)
}
fn downmix<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let input = read(ctx, stack.get(0))?;
    let s = add(ctx, &mut state.borrow_mut(), StreamNode::Mono(input), 1)?;
    stack.replace(ctx, value(ctx, s));
    Ok(CallbackReturn::Return)
}
fn output<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let stream = read(ctx, stack.get(0))?;
    let mut s = state.borrow_mut();
    if s.active.is_some() {
        return Err(binding_error(ctx, "output belongs outside graph staging"));
    }
    if s.streams.output.is_some() {
        return Err(binding_error(ctx, "output may be declared only once"));
    }
    if channels(&stream) != s.program.buses.main_channels() {
        return Err(binding_error(
            ctx,
            "output channel count must match main output (use explicit pan/downmix)",
        ));
    }
    s.streams.output = Some(stream);
    stack.clear();
    Ok(CallbackReturn::Return)
}
fn room_index<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(ctx, "expected room"));
    };
    let id = data
        .downcast_static::<RoomHandle>()
        .map_err(|_| binding_error(ctx, "expected room"))?
        .0;
    let method = read_string(ctx, stack.get(1), "room method")?;
    if !["place", "render"].contains(&method.as_str()) {
        return Err(binding_error(ctx, "unknown room method"));
    }
    let state = state.clone();
    let callback = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        let Value::UserData(receiver) = stack.get(0) else {
            return Err(binding_error(ctx, "use room methods with ':'"));
        };
        if receiver.downcast_static::<RoomHandle>().map(|h| h.0).ok() != Some(id) {
            return Err(binding_error(ctx, "use room methods with ':'"));
        }
        if method == "place" {
            let source = read(ctx, stack.get(1))?;
            let options = table(ctx, stack.get(2), "place")?;
            fields(ctx, options, &["name", "position", "layout"])?;
            let name = read_string(ctx, options.get_value(ctx, "name"), "source name")?;
            let position = vector(ctx, options.get_value(ctx, "position"), "source position")?;
            let layout = match options.get_value(ctx, "layout") {
                Value::Nil => None,
                Value::UserData(d) => Some(
                    d.downcast_static::<StereoPair>()
                        .map_err(|_| binding_error(ctx, "layout must be stereo_pair"))?
                        .0,
                ),
                _ => return Err(binding_error(ctx, "layout must be stereo_pair")),
            };
            if (channels(&source), layout.is_some())
                != (if layout.is_some() { 2 } else { 1 }, layout.is_some())
            {
                return Err(binding_error(
                    ctx,
                    "point sources require mono; use downmix or explicit stereo_pair for stereo",
                ));
            }
            let mut s = state.borrow_mut();
            let room = &mut s.streams.rooms[id];
            if room.sealed {
                return Err(binding_error(
                    ctx,
                    "place sources before rendering the room",
                ));
            }
            if room.sources.len() >= 16 {
                return Err(binding_error(ctx, "room emitter limit exceeded"));
            }
            room.sources
                .push((source, Emitter { name, position }, layout));
            stack.clear();
        } else {
            let options = table(ctx, stack.get(1), "room render")?;
            fields(
                ctx,
                options,
                &[
                    "listener",
                    "format",
                    "hrtf",
                    "direct",
                    "early",
                    "late",
                    "amount",
                    "bass_width",
                ],
            )?;
            let Value::UserData(l) = options.get_value(ctx, "listener") else {
                return Err(binding_error(ctx, "render requires a listener"));
            };
            let listener = l
                .downcast_static::<ListenerHandle>()
                .map_err(|_| binding_error(ctx, "render requires a listener"))?
                .0
                .clone();
            for (field, expected) in [("format", "binaural"), ("hrtf", "kemar")] {
                let v = options.get_value(ctx, field);
                if !matches!(v, Value::Nil) && read_string(ctx, v, field)? != expected {
                    return Err(binding_error(
                        ctx,
                        format!("{field} currently supports {expected}"),
                    ));
                }
            }
            let mut controls = Vec::new();
            for (field, default) in [
                ("direct", 1.),
                ("early", 1.),
                ("late", 1.),
                ("amount", 1.),
                ("bass_width", 0.2),
            ] {
                let v = options.get_value(ctx, field);
                controls.push(if matches!(v, Value::Nil) {
                    Source::Const(default)
                } else {
                    read_mono(ctx, v, 0)?
                });
            }
            let mut s = state.borrow_mut();
            let draft = s.streams.rooms[id].clone();
            let mut inputs = Vec::new();
            let mut emitters = Vec::new();
            for (source, emitter, layout) in draft.sources {
                if let Some(width) = layout {
                    for side in 0..2 {
                        let mut e = emitter.clone();
                        e.name.push_str(if side == 0 { ".L" } else { ".R" });
                        e.position[0] += if side == 0 { -width / 2. } else { width / 2. };
                        emitters.push(e);
                    }
                } else {
                    emitters.push(emitter);
                }
                inputs.push(source);
            }
            let spec = RoomSpec {
                size: draft.size,
                walls: draft.walls,
                floor: draft.floor,
                ceiling: draft.ceiling,
                decay: draft.decay,
                listener,
                emitters,
            };
            spec.validate().map_err(|e| binding_error(ctx, e))?;
            s.streams.rooms[id].sealed = true;
            let node = add(ctx, &mut s, StreamNode::Room(spec, inputs, controls), 2)?;
            stack.replace(ctx, value(ctx, node));
        }
        Ok(CallbackReturn::Return)
    });
    stack.replace(ctx, callback);
    Ok(CallbackReturn::Return)
}

pub(super) fn destination<'gc>(
    ctx: Context<'gc>,
    v: Value<'gc>,
) -> Result<Option<LuaBus>, Error<'gc>> {
    if matches!(v, Value::Nil) {
        return Ok(None);
    }
    let options = table(ctx, v, "routing options")?;
    fields(ctx, options, &["out", "inputs"])?;
    let Value::UserData(b) = options.get_value(ctx, "out") else {
        return Err(binding_error(ctx, "routing options require out=bus"));
    };
    Ok(Some(*b.downcast_static::<LuaBus>().map_err(|_| {
        binding_error(ctx, "out must be a writable bus")
    })?))
}
pub(super) fn route_track(state: &mut BuildState, index: usize, bus: LuaBus) {
    state.streams.track_routes.push((index, bus));
}
pub(super) fn route_midi(state: &mut BuildState, index: usize, bus: LuaBus) {
    state.streams.midi_routes.push((index, bus));
}
pub(super) fn route_run(state: &mut BuildState, index: usize, bus: LuaBus) {
    state.streams.run_routes.push((index, bus));
}
fn redirect<'gc>(
    ctx: Context<'gc>,
    state: &BuildState,
    mut graph: apteronotus_synth::GraphTemplate,
    bus: LuaBus,
    routing: &EventRouting,
) -> Result<apteronotus_synth::GraphTemplate, Error<'gc>> {
    if state.program.buses.bus_channels(bus.id) != Some(bus.channels)
        || graph.channels() != bus.channels
    {
        return Err(binding_error(
            ctx,
            "out bus channel count must exactly match producer outputs",
        ));
    }
    // Event sends tap the original completed voice even when its main
    // destination changes. Preserve that tap before replacing main outputs.
    for send in routing.sends() {
        let count = state
            .program
            .buses
            .bus_channels(send.bus)
            .ok_or_else(|| binding_error(ctx, "unknown event send bus"))?;
        if graph.outputs.len() != count && graph.outputs.len() != 1 {
            return Err(binding_error(ctx, "event send channel mismatch"));
        }
        let mut outputs = Vec::new();
        for i in 0..count {
            let source = graph.outputs[i % graph.outputs.len()];
            let id = graph.nodes.len();
            graph.nodes.push(apteronotus_synth::Node {
                op: apteronotus_synth::Op::Mul,
                inputs: vec![Input::from(source), Input::from(Source::Const(send.level))],
                tail: 0.,
                src: None,
            });
            outputs.push(Input::from(Source::port(id, 0)));
        }
        graph.sends.push(GraphSend {
            bus: send.bus,
            outputs,
            src: None,
        });
    }
    graph.sends.push(GraphSend {
        bus: bus.id,
        src: None,
        outputs: graph.outputs.iter().copied().map(Input::from).collect(),
    });
    graph.outputs = vec![Source::Const(0.); state.program.buses.main_channels()];
    Ok(graph)
}
pub(super) fn route_producers<'gc>(
    ctx: Context<'gc>,
    state: &mut BuildState,
) -> Result<(), Error<'gc>> {
    for (track, bus) in std::mem::take(&mut state.streams.track_routes) {
        let old = state.program.tracks[track].voice.index();
        let graph = redirect(
            ctx,
            state,
            state.program.voices[old].clone(),
            bus,
            &state.program.tracks[track].routing,
        )?;
        if state.program.voices.len() >= state.limits.voices {
            return Err(binding_error(
                ctx,
                "routed voice publication limit exceeded",
            ));
        }
        for _ in &graph.nodes {
            state.spend_node(ctx)?;
        }
        state.program.tracks[track].routing = state.program.tracks[track].routing.without_sends();
        let new = state.program.voices.len();
        state.program.voices.push(graph);
        state.program.tracks[track].voice = VoiceId(new);
    }
    for (binding, bus) in std::mem::take(&mut state.streams.midi_routes) {
        let old = state.program.midi_bindings[binding].voice.index();
        let graph = redirect(
            ctx,
            state,
            state.program.voices[old].clone(),
            bus,
            &EventRouting::new(),
        )?;
        if state.program.voices.len() >= state.limits.voices {
            return Err(binding_error(
                ctx,
                "routed voice publication limit exceeded",
            ));
        }
        for _ in &graph.nodes {
            state.spend_node(ctx)?;
        }
        let new = state.program.voices.len();
        state.program.voices.push(graph);
        state.program.midi_bindings[binding].voice = VoiceId(new);
    }
    for (run, bus) in std::mem::take(&mut state.streams.run_routes) {
        let old = state.program.runs[run].patch.index();
        if state.program.patches[old].graph().inputs != 0 {
            return Err(binding_error(
                ctx,
                "input-bearing run with out requires explicit inputs={streams}",
            ));
        }
        let graph = redirect(
            ctx,
            state,
            state.program.patches[old].graph().clone(),
            bus,
            &state.program.runs[run].routing,
        )?;
        if state.program.patches.len() >= state.limits.patches {
            return Err(binding_error(
                ctx,
                "routed patch publication limit exceeded",
            ));
        }
        for _ in &graph.nodes {
            state.spend_node(ctx)?;
        }
        state.program.runs[run].routing = state.program.runs[run].routing.without_sends();
        let new = state.program.patches.len();
        state
            .program
            .patches
            .push(PatchTemplate::new(graph).map_err(|e| binding_error(ctx, e.to_string()))?);
        state
            .program
            .patch_controls
            .push(state.program.patch_controls[old].clone());
        state.program.runs[run].patch = crate::PatchId(new);
        for binding in &mut state.program.midi_violins {
            if binding.patch.index() == old {
                binding.patch = crate::PatchId(new);
            }
        }
    }
    Ok(())
}

pub(super) fn has_output(state: &BuildState) -> bool {
    state.streams.output.is_some() || !state.streams.drivers.is_empty()
}
pub(super) struct CompiledStreams {
    pub output: Option<Vec<Source>>,
    pub buses: HashMap<BusId, Vec<Source>>,
}
pub(super) fn compile<'gc>(
    ctx: Context<'gc>,
    state: &mut BuildState,
) -> Result<CompiledStreams, Error<'gc>> {
    let mut cache = HashMap::new();
    let mut visiting = Vec::new();
    let mut bus_cache = HashMap::new();
    let mut bus_visiting = Vec::new();
    // Validate and compile every declared driver, even disconnected components.
    // Declaration order is stable; HashMap iteration must never seed DSP identity.
    let buses: Vec<_> = state
        .program
        .buses
        .bus_channel_counts()
        .iter()
        .enumerate()
        .filter_map(|(i, &n)| {
            state
                .program
                .buses
                .bus_id(i)
                .map(|id| LuaBus { id, channels: n })
        })
        .collect();
    for bus in buses {
        compile_stream(
            ctx,
            state,
            &Stream::Bus(bus),
            &mut cache,
            &mut visiting,
            &mut bus_cache,
            &mut bus_visiting,
        )?;
    }
    let output = state
        .streams
        .output
        .clone()
        .map(|st| {
            compile_stream(
                ctx,
                state,
                &st,
                &mut cache,
                &mut visiting,
                &mut bus_cache,
                &mut bus_visiting,
            )
        })
        .transpose()?;
    Ok(CompiledStreams {
        output,
        buses: bus_cache,
    })
}
pub(super) fn mix<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
) -> Result<bool, Error<'gc>> {
    let Value::Table(t) = stack.get(0) else {
        return Ok(false);
    };
    if t.length() == 0 || maybe_stream(t.get_value(ctx, 1)).is_none() {
        return Ok(false);
    }
    let streams = (1..=t.length())
        .map(|i| read(ctx, t.get_value(ctx, i)))
        .collect::<Result<Vec<_>, _>>()?;
    let n = channels(&streams[0]);
    if streams.iter().any(|st| channels(st) != n) {
        return Err(binding_error(
            ctx,
            "mix streams must have identical channel layouts",
        ));
    }
    let st = add(ctx, &mut state.borrow_mut(), StreamNode::Mix(streams), n)?;
    stack.replace(ctx, value(ctx, st));
    Ok(true)
}
fn compile_stream<'gc>(
    ctx: Context<'gc>,
    s: &mut BuildState,
    stream: &Stream,
    cache: &mut HashMap<usize, Vec<Source>>,
    visiting: &mut Vec<usize>,
    bus_cache: &mut HashMap<BusId, Vec<Source>>,
    bus_visiting: &mut Vec<BusId>,
) -> Result<Vec<Source>, Error<'gc>> {
    match stream {
        Stream::External(a) => Ok((0..a.channels)
            .map(|channel| Source::ExternalAudio {
                input: a.id,
                channel,
            })
            .collect()),
        Stream::Bus(b) => {
            if let Some(v) = bus_cache.get(&b.id) {
                return Ok(v.clone());
            }
            if bus_visiting.contains(&b.id) {
                return Err(binding_error(ctx, "audio stream routing contains a cycle"));
            }
            let range = s
                .program
                .buses
                .bus_range(b.id)
                .ok_or_else(|| binding_error(ctx, "stream bus belongs to another program"))?;
            let mut result = range.map(Source::Input).collect::<Vec<_>>();
            if let Some(driver) = s.streams.drivers.get(&b.id).cloned() {
                bus_visiting.push(b.id);
                let driven =
                    compile_stream(ctx, s, &driver, cache, visiting, bus_cache, bus_visiting)?;
                bus_visiting.pop();
                for (a, b) in result.iter_mut().zip(driven) {
                    s.spend_node(ctx)?;
                    *a = s.active_mut(ctx)?.graph.add(*a, b);
                }
            }
            bus_cache.insert(b.id, result.clone());
            Ok(result)
        }
        Stream::Node(id, _) => {
            if let Some(v) = cache.get(id) {
                return Ok(v.clone());
            }
            if visiting.contains(id) {
                return Err(binding_error(ctx, "audio stream routing contains a cycle"));
            }
            visiting.push(*id);
            let node = s
                .streams
                .nodes
                .get(*id)
                .ok_or_else(|| binding_error(ctx, "invalid stream node"))?
                .clone();
            let result = match node {
                StreamNode::Process(input, p) => {
                    let inputs =
                        compile_stream(ctx, s, &input, cache, visiting, bus_cache, bus_visiting)?;
                    apply_processor(ctx, s, &p, &inputs)?
                }
                StreamNode::Mix(streams) => {
                    let mut result = vec![Source::Const(0.); channels(&streams[0])];
                    for st in streams {
                        let values =
                            compile_stream(ctx, s, &st, cache, visiting, bus_cache, bus_visiting)?;
                        for (out, v) in result.iter_mut().zip(values) {
                            s.spend_node(ctx)?;
                            *out = s.active_mut(ctx)?.graph.add(*out, v);
                        }
                    }
                    result
                }
                StreamNode::Mono(input) => {
                    let inputs =
                        compile_stream(ctx, s, &input, cache, visiting, bus_cache, bus_visiting)?;
                    let mut sum = Source::Const(0.);
                    for v in &inputs {
                        s.spend_node(ctx)?;
                        sum = s.active_mut(ctx)?.graph.add(sum, *v);
                    }
                    s.spend_node(ctx)?;
                    vec![s.active_mut(ctx)?.graph.mul(sum, 1. / inputs.len() as f64)]
                }
                StreamNode::Room(spec, streams, controls) => {
                    let mut inputs = Vec::new();
                    for st in streams {
                        inputs.extend(compile_stream(
                            ctx,
                            s,
                            &st,
                            cache,
                            visiting,
                            bus_cache,
                            bus_visiting,
                        )?);
                    }
                    inputs.extend(controls);
                    s.spend_node(ctx)?;
                    s.active_mut(ctx)?
                        .graph
                        .spatial_room(spec, &inputs)
                        .to_vec()
                }
                StreamNode::Patch(patch, streams) => {
                    let mut inputs = Vec::new();
                    for st in streams {
                        inputs.extend(compile_stream(
                            ctx,
                            s,
                            &st,
                            cache,
                            visiting,
                            bus_cache,
                            bus_visiting,
                        )?);
                    }
                    let graph = s.program.patches[patch].graph().clone();
                    let mut ports: Vec<Source> = Vec::new();
                    let remap = |source: Source, ports: &[Source]| -> Source {
                        match source {
                            Source::Input(i) => inputs[i],
                            Source::Port { node, channel } => {
                                let Source::Port { node, .. } = ports[node] else {
                                    unreachable!()
                                };
                                Source::Port { node, channel }
                            }
                            _ => source,
                        }
                    };
                    for node in &graph.nodes {
                        s.spend_node(ctx)?;
                        let incoming = node
                            .inputs
                            .iter()
                            .map(|i| Input {
                                source: remap(i.source, &ports),
                                src: i.src,
                            })
                            .collect::<Vec<_>>();
                        let result =
                            s.active_mut(ctx)?
                                .graph
                                .append_node(apteronotus_synth::Node {
                                    op: node.op.clone(),
                                    inputs: incoming,
                                    tail: node.tail,
                                    src: node.src,
                                });
                        ports.push(result);
                    }
                    graph.outputs.iter().map(|v| remap(*v, &ports)).collect()
                }
            };
            visiting.pop();
            cache.insert(*id, result.clone());
            Ok(result)
        }
    }
}

pub(super) fn run_pipeline<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    patch: usize,
    options: Value<'gc>,
) -> Result<bool, Error<'gc>> {
    let Value::Table(options) = options else {
        return Ok(false);
    };
    let inputs = match options.get_value(ctx, "inputs") {
        Value::Nil => return Ok(false),
        v => table(ctx, v, "run inputs")?,
    };
    let out = destination(ctx, Value::Table(options))?.unwrap();
    let mut streams = Vec::new();
    for i in 1..=inputs.length() {
        streams.push(read(ctx, inputs.get_value(ctx, i))?);
    }
    let mut s = state.borrow_mut();
    let graph = s
        .program
        .patches
        .get(patch)
        .ok_or_else(|| binding_error(ctx, "unknown patch"))?
        .graph();
    if graph.inputs != streams.iter().map(channels).sum::<usize>() || graph.inputs == 0 {
        return Err(binding_error(
            ctx,
            "explicit patch input streams must match its declared input channels",
        ));
    }
    if !graph.sends.is_empty() {
        return Err(binding_error(
            ctx,
            "explicit stream patches must route outputs through out, not hidden graph sends",
        ));
    }
    if graph.channels() != out.channels
        || s.program.buses.bus_channels(out.id) != Some(out.channels)
    {
        return Err(binding_error(ctx, "patch output bus channel mismatch"));
    }
    if s.streams.drivers.contains_key(&out.id) {
        return Err(binding_error(
            ctx,
            "bus already has an explicit patch driver",
        ));
    }
    let stream = add(ctx, &mut s, StreamNode::Patch(patch, streams), out.channels)?;
    s.streams.drivers.insert(out.id, stream);
    Ok(true)
}

fn convolve<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let options = table(ctx, stack.get(0), "convolve")?;
    fields(ctx, options, &["sample_rate", "samples"])?;
    let sample_rate = number(
        ctx,
        options.get_value(ctx, "sample_rate"),
        f64::NAN,
        "sample_rate",
    )?;
    let samples = table(ctx, options.get_value(ctx, "samples"), "impulse samples")?;
    if samples.length() as usize > state.borrow().limits.graph_publication.data_entries {
        return Err(binding_error(ctx, "impulse exceeds publication data limit"));
    }
    let samples = (1..=samples.length())
        .map(|i| {
            number(ctx, samples.get_value(ctx, i), f64::NAN, "impulse sample").map(|v| v as f32)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let response = apteronotus_synth::convolution::ImpulseResponse {
        sample_rate,
        samples,
    };
    if !response.validate() {
        return Err(binding_error(
            ctx,
            "invalid impulse response: require finite samples, 8–192 kHz and at most ten seconds",
        ));
    }
    let generation = if state.borrow().active.is_some() {
        current_generation(ctx, state)?
    } else {
        0
    };
    stack.replace(
        ctx,
        lua_processor(ctx, generation, Processor::Convolve(response))?,
    );
    Ok(CallbackReturn::Return)
}
