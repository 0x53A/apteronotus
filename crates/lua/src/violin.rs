//! Evaluation-only builders for retained violin gestures.
use super::*;
use apteronotus_synth::bowed::{BowedConfig, BowedKind, BowedModel};
use apteronotus_synth::bowed_gesture::{BowAction, BowCommand};
use apteronotus_synth::bowed_host::BowedScore;

#[derive(Clone, Debug)]
struct Violin {
    patch: usize,
    key: ControlId,
    labels: Vec<String>,
    kind: BowedKind,
    live: Option<[ControlId; 3]>,
}
#[derive(Clone, Debug)]
struct Gesture {
    patch: usize,
    action: BowAction,
    over: Option<LuaDuration>,
}
#[derive(Clone, Debug)]
struct Placement {
    start: Frac,
    seconds: Option<f64>,
    gesture: Gesture,
    ordinal: u64,
}
#[derive(Clone, Debug)]
struct Gestures(Vec<Placement>);

pub(super) fn install<'gc>(
    ctx: Context<'gc>,
    state: Rc<RefCell<BuildState>>,
) -> Result<(), Error<'gc>> {
    set_callback(ctx, "violin", state.clone(), build)?;
    set_callback(ctx, "cello", state.clone(), build_cello)?;
    let meta = Table::new(&ctx);
    set_operator(ctx, meta, MetaMethod::Index, state, index)?;
    ctx.set_global("__apteronotus_violin_meta", meta);
    Ok(())
}
pub(super) fn patch(value: Value<'_>) -> Option<usize> {
    let Value::UserData(data) = value else {
        return None;
    };
    data.downcast_static::<Violin>().ok().map(|v| v.patch)
}
fn build_cello<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    build_kind(ctx, state, stack, BowedKind::Cello)
}
fn build<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    build_kind(ctx, state, stack, BowedKind::Violin)
}
fn build_kind<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
    kind: BowedKind,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let spec = match stack.get(0) {
        Value::Nil => Table::new(&ctx),
        Value::Table(t) => t,
        _ => return Err(binding_error(ctx, "violin expects an options table")),
    };
    for (key, _) in spec.iter() {
        if !matches!(key,Value::String(s) if [b"name".as_slice(),b"string",b"mode",b"model",b"max_hz"].contains(&s.as_bytes()))
        {
            return Err(binding_error(
                ctx,
                "violin options are name, string, mode, model and max_hz",
            ));
        }
    }
    let model = match spec.get_value(ctx, "model") {
        Value::Nil => {
            if kind == BowedKind::Cello {
                BowedModel::C3
            } else {
                BowedModel::C2
            }
        }
        value => match read_string(ctx, value, "violin model")?.as_str() {
            "c1" => BowedModel::C1,
            "c2" => BowedModel::C2,
            "c3" => BowedModel::C3,
            "c4" => BowedModel::C4,
            _ => return Err(binding_error(ctx, "violin model must be c1, c2, c3 or c4")),
        },
    };
    let name = match spec.get_value(ctx, "name") {
        Value::Nil => if kind == BowedKind::Cello {
            "Cello"
        } else {
            "Violin"
        }
        .to_string(),
        v => read_string(ctx, v, "violin name")?,
    };
    let live_mode = match spec.get_value(ctx, "mode") {
        Value::Nil => false,
        x => match read_string(ctx, x, "violin mode")?.as_str() {
            "score" => false,
            "live" => true,
            _ => return Err(binding_error(ctx, "violin mode must be score or live")),
        },
    };
    let max_hz = match spec.get_value(ctx, "max_hz") {
        Value::Nil => 660.0,
        value => read_number(ctx, value, "violin max_hz")?,
    };
    if !max_hz.is_finite() || !(660.0..=2000.0).contains(&max_hz) {
        return Err(binding_error(
            ctx,
            "violin max_hz must be finite and within 660..2000",
        ));
    }
    if live_mode && max_hz != 660.0 {
        return Err(binding_error(
            ctx,
            "extended violin range currently requires score mode",
        ));
    }
    let all = if kind == BowedKind::Cello {
        [
            ("C", 65.40639132514966),
            ("G", 97.99885899543733),
            ("D", 146.8323839587038),
            ("A", 220.0),
        ]
    } else {
        [
            ("G", 195.99771799087463),
            ("D", 293.6647679174076),
            ("A", 440.0),
            ("E", 659.2551138257398),
        ]
    };
    let chosen = match spec.get_value(ctx, "string") {
        Value::Nil => None,
        v => Some(read_string(ctx, v, "violin string")?),
    };
    let strings: Vec<_> = all
        .into_iter()
        .filter(|(label, _)| chosen.as_ref().is_none_or(|s| s == label))
        .collect();
    if strings.is_empty() {
        return Err(binding_error(
            ctx,
            "string must belong to this bowed instrument",
        ));
    }
    let mut s = state.borrow_mut();
    if s.active.is_some() {
        return Err(binding_error(
            ctx,
            "violin must be declared outside a graph",
        ));
    }
    if s.program.patches.len() >= s.limits.voices
        || s.program
            .controls
            .specs()
            .len()
            .saturating_add(if live_mode { 4 } else { 1 })
            > s.limits.controls
    {
        return Err(binding_error(ctx, "violin publication limit exceeded"));
    }
    let key = s
        .program
        .controls
        .add(ControlSpec::new(
            &format!("{name} expression"),
            0.0,
            1.4,
            1.0,
        ))
        .map_err(|e| binding_error(ctx, e.to_string()))?;
    let live: Option<[ControlId; 3]> = if live_mode {
        Some(
            [
                s.program.controls.add(ControlSpec::new(
                    &format!("{name} bow speed"),
                    -1.0,
                    1.0,
                    0.4,
                )),
                s.program.controls.add(ControlSpec::new(
                    &format!("{name} bow pressure"),
                    0.0,
                    1.0,
                    0.55,
                )),
                s.program.controls.add(ControlSpec::new(
                    &format!("{name} bow position"),
                    0.12,
                    0.45,
                    0.12,
                )),
            ]
            .map(|r| r.map_err(|e| binding_error(ctx, e.to_string())))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .unwrap(),
        )
    } else {
        None
    };
    let score = BowedScore {
        live: live_mode,
        clock: None,
        config: BowedConfig {
            kind,
            min_hz: if kind == BowedKind::Cello {
                65.0
            } else {
                195.0
            },
            max_hz,
            model,
        },
        opens: strings.iter().map(|(_, hz)| *hz).collect(),
        commands: Vec::new(),
        coordinates: Vec::new(),
        reference_rate: 48_000.0,
    };
    let mut graph = GraphBuilder::new();
    let tone = if let Some(controls) = live {
        let mut applied = [Source::Const(0.0); 3];
        for (index, control) in controls.into_iter().enumerate() {
            applied[index] = graph
                .slew_from(Source::Control(control), 0.01, [0.4, 0.55, 0.12][index])
                .map_err(|e| binding_error(ctx, e.to_string()))?;
        }
        graph.bowed_performance_with_controls(key, score, applied)[0]
    } else {
        graph.bowed_performance(key, score)[0]
    };
    let scaled = graph.mul(tone, 0.2);
    let expression = graph
        .slew_from(Source::Control(key), 0.015, 1.0)
        .map_err(|e| binding_error(ctx, e.to_string()))?;
    let scaled = graph.mul(scaled, expression);
    let stereo = graph.pan(scaled, 0.0);
    let graph = graph
        .out(&[stereo.0, stereo.1])
        .map_err(|e| binding_error(ctx, e.to_string()))?;
    s.graph_nodes = s.graph_nodes.saturating_add(graph.nodes.len());
    if s.graph_nodes > s.limits.graph_nodes {
        return Err(binding_error(ctx, "violin graph node limit exceeded"));
    }
    let patch = s.program.patches.len();
    s.program
        .patches
        .push(PatchTemplate::new(graph).map_err(|e| binding_error(ctx, e.to_string()))?);
    s.program.patch_controls.push(Vec::new());
    let v = UserData::new_static(
        &ctx,
        Violin {
            patch,
            key,
            live,
            kind,
            labels: strings
                .into_iter()
                .map(|(label, _)| label.to_string())
                .collect(),
        },
    );
    let Value::Table(meta) = ctx.get_global_value("__apteronotus_violin_meta") else {
        unreachable!()
    };
    v.set_metatable(&ctx, Some(meta));
    stack.replace(ctx, v);
    Ok(CallbackReturn::Return)
}
fn index<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    mut stack: piccolo::Stack<'gc, '_>,
) -> Result<CallbackReturn<'gc>, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Err(binding_error(ctx, "violin method requires an instrument"));
    };
    let v = data
        .downcast_static::<Violin>()
        .map_err(|_| binding_error(ctx, "violin method requires an instrument"))?
        .clone();
    let method = read_string(ctx, stack.get(1), "violin method")?;
    if let Some(controls) = v.live
        && let Some(index) = ["speed", "pressure", "position"]
            .iter()
            .position(|name| *name == method)
    {
        stack.replace(ctx, lua_control(ctx, controls[index])?);
        return Ok(CallbackReturn::Return);
    }
    if method == "expression" {
        stack.replace(ctx, lua_control(ctx, v.key)?);
        return Ok(CallbackReturn::Return);
    }
    if ![
        "bow",
        "lift_bow",
        "stop",
        "slide",
        "release_finger",
        "damp",
        "vibrato",
        "volume",
    ]
    .contains(&method.as_str())
    {
        return Err(binding_error(
            ctx,
            format!("unknown violin method {method:?}"),
        ));
    }
    let state = state.clone();
    let callback = Callback::from_fn(&ctx, move |ctx, _, mut stack| {
        if patch(stack.get(0)) != Some(v.patch) {
            return Err(binding_error(
                ctx,
                "use violin methods with ':' on their instrument",
            ));
        }
        let options = match stack.get(1) {
            Value::Nil => Table::new(&ctx),
            Value::Table(t) => t,
            _ => {
                return Err(binding_error(
                    ctx,
                    "violin gesture expects an options table",
                ));
            }
        };
        let mut known = vec!["over"];
        if ["bow", "stop", "slide", "release_finger", "damp", "vibrato"].contains(&method.as_str())
        {
            known.push("string");
        }
        match method.as_str() {
            "bow" => known.extend(["speed", "force", "position"]),
            "stop" | "slide" => known.extend(["pitch", "hz"]),
            "damp" | "volume" => known.push("amount"),
            "vibrato" => known.extend(["cents", "rate"]),
            _ => (),
        }
        for (k, _) in options.iter() {
            if !matches!(k,Value::String(s) if known.iter().any(|n|s.as_bytes()==n.as_bytes())) {
                return Err(binding_error(ctx, "unknown violin gesture field"));
            }
        }
        let selected =
            match options.get_value(ctx, "string") {
                Value::Nil => None,
                x => {
                    let label = read_string(ctx, x, "violin string")?;
                    Some(
                        v.labels.iter().position(|s| *s == label).ok_or_else(|| {
                            binding_error(ctx, "string not declared by this violin")
                        })? as u8,
                    )
                }
            };
        let required = || {
            selected.ok_or_else(|| binding_error(ctx, "this gesture requires an explicit string"))
        };
        let number = |field: &'static str| {
            let x = options.get_value(ctx, field);
            if x.is_nil() {
                Ok(None)
            } else {
                read_number(ctx, x, field).map(Some)
            }
        };
        let over = match options.get_value(ctx, "over") {
            Value::Nil => None,
            Value::UserData(d) => Some(
                *d.downcast_static::<LuaDuration>()
                    .map_err(|_| binding_error(ctx, "gesture over must be seconds or beats"))?,
            ),
            _ => {
                return Err(binding_error(
                    ctx,
                    "gesture over must be typed seconds or beats",
                ));
            }
        };
        if over.is_some_and(|d| !d.value.is_finite() || d.value < 0.0) {
            return Err(binding_error(
                ctx,
                "gesture duration must be nonnegative and finite",
            ));
        }
        let action = match method.as_str() {
            "bow" => BowAction::Bow {
                string: selected,
                speed: number("speed")?,
                force: number("force")?,
                position: number("position")?,
                over: 0,
            },
            "lift_bow" => BowAction::Lift { over: 0 },
            "stop" | "slide" => {
                let pitch = options.get_value(ctx, "pitch");
                let measured = number("hz")?;
                let hz = match (pitch.is_nil(), measured) {
                    (true, Some(hz)) if hz.is_finite() && hz > 0.0 => hz,
                    (false, None) => Pitch::parse(&read_string(ctx, pitch, "stopped pitch")?)
                        .map_err(|e| binding_error(ctx, e.to_string()))?
                        .hz(),
                    _ => {
                        return Err(binding_error(
                            ctx,
                            "stop/slide requires exactly one of pitch or positive finite hz",
                        ));
                    }
                };
                BowAction::Stop {
                    string: required()?,
                    hz,
                    over: 0,
                }
            }
            "release_finger" => BowAction::ReleaseFinger {
                string: required()?,
                over: 0,
            },
            "damp" => BowAction::Damp {
                string: required()?,
                amount: number("amount")?
                    .ok_or_else(|| binding_error(ctx, "damp requires amount"))?,
                over: 0,
            },
            "vibrato" => BowAction::Vibrato {
                string: required()?,
                cents: number("cents")?.unwrap_or(12.0),
                rate: number("rate")?.unwrap_or(5.0),
                over: 0,
            },
            "volume" => BowAction::Expression {
                amount: number("amount")?
                    .ok_or_else(|| binding_error(ctx, "volume requires amount"))?,
                over: 0,
            },
            _ => unreachable!(),
        };
        if state.borrow().active.is_some() {
            return Err(binding_error(
                ctx,
                "violin gestures must be built outside a graph",
            ));
        }
        stack.replace(
            ctx,
            UserData::new_static(
                &ctx,
                Gesture {
                    patch: v.patch,
                    action,
                    over,
                },
            ),
        );
        Ok(CallbackReturn::Return)
    });
    stack.replace(ctx, callback);
    Ok(CallbackReturn::Return)
}
pub(super) fn at<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    start: Frac,
) -> Result<bool, Error<'gc>> {
    let Value::UserData(data) = stack.get(1) else {
        return Ok(false);
    };
    let Ok(g) = data.downcast_static::<Gesture>() else {
        return Ok(false);
    };
    let gesture = g.clone();
    if stack.len() != 2 {
        return Err(binding_error(
            ctx,
            "gesture placement has no capture duration",
        ));
    }
    if start < Frac::ZERO {
        return Err(binding_error(ctx, "gesture placement cannot be negative"));
    }
    let seconds = match stack.get(0) {
        Value::UserData(d) => d
            .downcast_static::<LuaDuration>()
            .ok()
            .filter(|d| matches!(d.unit, TimeUnit::Seconds))
            .map(|d| d.value),
        _ => None,
    };
    let mut s = state.borrow_mut();
    let ordinal = s.violin_ordinal;
    s.violin_ordinal = s
        .violin_ordinal
        .checked_add(1)
        .ok_or_else(|| binding_error(ctx, "gesture identity overflow"))?;
    stack.replace(
        ctx,
        UserData::new_static(
            &ctx,
            Placement {
                start,
                seconds,
                gesture,
                ordinal,
            },
        ),
    );
    Ok(true)
}
pub(super) fn timeline<'gc>(
    ctx: Context<'gc>,
    table: Table<'gc>,
    stack: &mut piccolo::Stack<'gc, '_>,
) -> Result<bool, Error<'gc>> {
    let Value::UserData(first) = table.get_value(ctx, 1) else {
        return Ok(false);
    };
    if first.downcast_static::<Placement>().is_err() {
        return Ok(false);
    }
    if table.length() as usize > apteronotus_synth::bowed_gesture::MAX_COMMANDS {
        return Err(binding_error(
            ctx,
            "gesture timeline storage limit exceeded",
        ));
    }
    let mut list = Vec::new();
    for i in 1..=table.length() {
        let Value::UserData(data) = table.get_value(ctx, i) else {
            return Err(binding_error(
                ctx,
                "gesture timeline requires at placements",
            ));
        };
        list.push(
            data.downcast_static::<Placement>()
                .map_err(|_| {
                    binding_error(ctx, "cannot mix gestures and note patterns in one timeline")
                })?
                .clone(),
        );
    }
    stack.replace(ctx, UserData::new_static(&ctx, Gestures(list)));
    Ok(true)
}
pub(super) fn play<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
    offset: usize,
) -> Result<bool, Error<'gc>> {
    let Some(patch) = patch(stack.get(offset)) else {
        return Ok(false);
    };
    if stack.len() != offset + 2 {
        return Err(binding_error(
            ctx,
            "violin play expects exactly an instrument and gesture timeline",
        ));
    }
    let Value::UserData(data) = stack.get(offset + 1) else {
        return Err(binding_error(ctx, "violin play expects a gesture timeline"));
    };
    let list = data
        .downcast_static::<Gestures>()
        .map_err(|_| binding_error(ctx, "violin play expects a gesture timeline"))?
        .0
        .clone();
    let mut s = state.borrow_mut();
    if !s.tempo_declared {
        return Err(binding_error(
            ctx,
            "declare tempo before compiling a violin gesture timeline",
        ));
    }
    let mut graph = s.program.patches[patch].graph().clone();
    let node = graph
        .nodes
        .iter_mut()
        .find(|n| matches!(n.op, apteronotus_synth::Op::BowedPerformance { .. }))
        .ok_or_else(|| binding_error(ctx, "missing violin runtime"))?;
    let apteronotus_synth::Op::BowedPerformance { score, .. } = &mut node.op else {
        unreachable!()
    };
    if score.live {
        return Err(binding_error(
            ctx,
            "live violin cannot also own a scored gesture timeline",
        ));
    }
    score.clock = Some(apteronotus_transport::CompiledTempo::new(&s.program.tempo));
    for p in list {
        if p.gesture.patch != patch {
            return Err(binding_error(ctx, "gesture belongs to a different violin"));
        }
        let at = p
            .seconds
            .unwrap_or_else(|| s.program.tempo.cycle_to_seconds(p.start));
        let mut beats = None;
        let seconds = match p.gesture.over {
            None => 0.0,
            Some(d) => match d.unit {
                TimeUnit::Seconds => d.value,
                TimeUnit::Cycles => {
                    return Err(binding_error(ctx, "gesture durations use seconds or beats"));
                }
                TimeUnit::Beats { .. } => {
                    let begin = score.clock.as_ref().unwrap().cycle_at(at);
                    let end = begin + d.value / s.program.tempo.beats_per_cycle();
                    if d.value > 0.0 {
                        beats = Some(apteronotus_synth::bowed_gesture::BeatRamp { begin, end });
                    }
                    s.program.tempo.seconds_at_f64(end) - at
                }
            },
        };
        let over = (seconds * score.reference_rate).ceil() as u64;
        let mut action = p.gesture.action;
        match &mut action {
            BowAction::Bow { over: o, .. }
            | BowAction::Lift { over: o }
            | BowAction::Stop { over: o, .. }
            | BowAction::ReleaseFinger { over: o, .. }
            | BowAction::Damp { over: o, .. }
            | BowAction::Vibrato { over: o, .. }
            | BowAction::Expression { over: o, .. } => *o = over,
        }
        score
            .coordinates
            .push(apteronotus_synth::bowed_host::GestureCoordinate {
                ordinal: p.ordinal,
                seconds: at,
                duration: seconds,
            });
        score.commands.push(BowCommand {
            sample: (at * score.reference_rate).ceil() as u64,
            ordinal: p.ordinal,
            action,
            beats,
        });
    }
    score.prepare(24_000.0).map_err(|e| binding_error(ctx, e))?;
    s.program.patches[patch] =
        PatchTemplate::new(graph).map_err(|e| binding_error(ctx, e.to_string()))?;
    stack.clear();
    Ok(true)
}

pub(super) fn midi<'gc>(
    ctx: Context<'gc>,
    state: &Rc<RefCell<BuildState>>,
    stack: &mut piccolo::Stack<'gc, '_>,
) -> Result<bool, Error<'gc>> {
    let Value::UserData(data) = stack.get(0) else {
        return Ok(false);
    };
    let Ok(v) = data.downcast_static::<Violin>() else {
        return Ok(false);
    };
    if v.live.is_none() || v.labels.len() != 1 {
        return Err(binding_error(
            ctx,
            "midi(violin) requires mode='live' and one declared string",
        ));
    }
    let channel = match stack.get(1) {
        Value::Nil => None,
        Value::Table(options) => {
            for (key, _) in options.iter() {
                if !matches!(key,Value::String(s) if s.as_bytes()==b"channel") {
                    return Err(binding_error(
                        ctx,
                        "violin MIDI options support channel only",
                    ));
                }
            }
            match options.get_value(ctx, "channel") {
                Value::Nil => None,
                x => {
                    let c = read_number(ctx, x, "MIDI channel")?;
                    if c.fract() != 0.0 || !(1.0..=16.0).contains(&c) {
                        return Err(binding_error(ctx, "MIDI channel must be 1..16"));
                    }
                    Some(c as u8 - 1)
                }
            }
        }
        _ => return Err(binding_error(ctx, "MIDI options must be a table")),
    };
    let mut s = state.borrow_mut();
    if s.active.is_some() || s.program.midi_violins.iter().any(|b| b.key == v.key) {
        return Err(binding_error(
            ctx,
            "duplicate or graph-local violin MIDI binding",
        ));
    }
    if s.program.midi_violins.len() + s.program.midi_bindings.len() + s.program.tracks.len()
        >= s.limits.tracks
    {
        return Err(binding_error(ctx, "violin MIDI binding limit exceeded"));
    }
    let min_key = if v.kind == BowedKind::Cello {
        match v.labels[0].as_str() {
            "C" => 36,
            "G" => 43,
            "D" => 50,
            "A" => 57,
            _ => unreachable!(),
        }
    } else {
        match v.labels[0].as_str() {
            "G" => 55,
            "D" => 62,
            "A" => 69,
            "E" => 76,
            _ => unreachable!(),
        }
    };
    s.program.midi_violins.push(crate::MidiViolinBinding {
        key: v.key,
        patch: crate::PatchId(v.patch),
        channel,
        min_key,
        max_key: 76,
    });
    stack.clear();
    Ok(true)
}
