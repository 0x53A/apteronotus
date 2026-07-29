use apteronotus_live::{
    ExternalOnset, PersistentRuntime, PitchScheduler, ProgramScheduler, RevisionSlot,
    RoutedRuntime, ScheduledRun, ScheduledTrack, Transport, schedule_external_routed,
};
use apteronotus_lua::{Limits, Program, evaluate};
use apteronotus_pattern::{ControlValue, Frac, Span, Value};
use apteronotus_synth::lower::{render, rms, zero_crossing_hz};
use apteronotus_synth::{Implicit, ParamId, ParamValue};
use fundsp::prelude32::AudioUnit;

const SR: f64 = 48_000.0;

fn render_routed_song(
    source: &str,
    end: Frac,
    seconds: f64,
) -> (Program, Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let program = evaluate(source).unwrap();
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
    let patches = program
        .runs
        .iter()
        .filter(|run| run.span.is_none())
        .map(|run| program.patch(run.patch).unwrap())
        .collect::<Vec<_>>();
    let mut runtime = PersistentRuntime::new(&program.buses, &program.controls, patches).unwrap();
    let mut persistent = runtime.take_processor();
    let mut sequencer = runtime.sequencer();
    sequencer.set_sample_rate(SR);
    persistent.set_sample_rate(SR);
    sequencer.allocate();
    persistent.allocate();
    let tracks = program
        .tracks
        .iter()
        .map(|track| {
            ScheduledTrack::with_routing_and_bindings(
                &track.pattern,
                program.voice(track.voice).unwrap(),
                &track.routing,
                &track.onset_bindings,
            )
        })
        .collect::<Vec<_>>();
    let runs = program
        .runs
        .iter()
        .filter_map(|run| {
            run.span.map(|span| {
                ScheduledRun::with_routing(program.patch(run.patch).unwrap(), span, &run.routing)
            })
        })
        .collect::<Vec<_>>();
    ProgramScheduler::default()
        .fill_routed_program_to_tempo_map(
            end,
            tracks,
            runs,
            &program.tempo,
            &mut sequencer,
            RoutedRuntime::new(runtime.layout(), runtime.controls()),
        )
        .unwrap();

    let frames = (SR * seconds) as usize;
    let channels = runtime.layout().total_channels();
    let mut stems = vec![Vec::with_capacity(frames); channels];
    let mut output = vec![Vec::with_capacity(frames); channels];
    let mut stem_frame = vec![0.0; channels];
    let mut output_frame = vec![0.0; channels];
    for _ in 0..frames {
        sequencer.tick(&[], &mut stem_frame);
        persistent.tick(&stem_frame, &mut output_frame);
        for channel in 0..channels {
            stems[channel].push(stem_frame[channel]);
            output[channel].push(output_frame[channel]);
        }
    }
    drop(persistent);
    drop(runtime);
    (program, stems, output)
}

const SOURCE: &str = r#"
local v = voice {
  graph = function(n)
    return sine(n.hz) * n.velocity * (1 + n.duration * 0) * 0.12
  end,
}
play(v, "c4 e4 g4")
"#;

#[test]
fn lua_program_reaches_the_live_sequencer_and_measured_audio() {
    let limits = Limits::default();
    let candidate = evaluate(SOURCE).unwrap();

    // Publication owns validation and activation; evaluation alone does not
    // authorize a scheduler change.
    let mut revisions = RevisionSlot::new(Program::default(), Frac::ZERO);
    revisions
        .submit(candidate, Frac::ZERO, |program| {
            program.validate(limits.graph_publication)
        })
        .unwrap();
    let revision = revisions.advance_to(Frac::ZERO);
    let program = revision.program;

    assert_eq!(program.tracks.len(), 1);
    let track = &program.tracks[0];
    let template = program
        .voice(track.voice)
        .expect("validated track has a voice");

    let transport = Transport::new(120.0).unwrap();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(template);
    sequencer.set_sample_rate(SR);
    let report = scheduler
        .fill_to(
            Frac::ONE,
            &track.pattern,
            template,
            transport,
            &mut sequencer,
        )
        .unwrap();
    assert_eq!(report.voices, 3);

    let audio = render(&mut sequencer, SR, 2.0);
    assert!(rms(&audio[0]) > 0.02);
    let measured = zero_crossing_hz(&audio[0][2_000..25_000], SR);
    assert!((measured - 261.63).abs() < 3.0, "measured {measured} Hz");
}

#[test]
fn literal_voicing_and_typed_arp_spacing_reach_measured_audio() {
    let program = evaluate(
        r#"
        tempo(120)
        local keys = voice {
          graph = function(n)
            return sine(n.hz) * n.velocity * 0.035
          end,
        }
        local voiced = chord("Dm(add9)")
                     >> anchor("a4")
                     >> voicing("open-5")
        play(keys, voiced >> arp("outside-in", beats(0.5)) >> velocity(0.7))
        "#,
    )
    .unwrap();
    let track = &program.tracks[0];
    let template = program.voice(track.voice).unwrap();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(template);
    sequencer.set_sample_rate(SR);
    let report = scheduler
        .fill_to(
            Frac::ONE,
            &track.pattern,
            template,
            Transport::new(120.0).unwrap(),
            &mut sequencer,
        )
        .unwrap();
    assert_eq!(report.voices, 5);
    let audio = render(&mut sequencer, SR, 2.0);
    assert!(rms(&audio[0]) > 0.01);
}

#[test]
fn lua_percussion_labels_reach_audio_without_a_fake_pitch() {
    let program = evaluate(
        r#"
        local kick = voice {
          graph = function()
            return sine(70) * decay(ms(80)) * 0.12
          end,
        }
        play(kick, "bd ~ x ~")
        "#,
    )
    .unwrap();
    let track = &program.tracks[0];
    let template = program.voice(track.voice).unwrap();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(template);
    sequencer.set_sample_rate(SR);

    scheduler
        .fill_to(
            Frac::ONE,
            &track.pattern,
            template,
            Transport::default(),
            &mut sequencer,
        )
        .unwrap();
    let audio = render(&mut sequencer, SR, 0.5);
    assert!(rms(&audio[0]) > 0.005);
}

#[test]
fn lua_note_phase_control_remains_live_through_the_production_path() {
    let source = r#"
    local pad = voice {
      params = {
        pressure = { 0, 1, 0 },
      },
      graph = function(n)
        return sine(n.hz) * n.pressure * 0.12
      end,
    }
    local rise = curve {
      { phase(0), 0 },
      { phase(1), 1 },
    }
    play(pad, pattern("c4") >> hold(bars(1)) >> pad.pressure(rise))
    "#;
    let program = evaluate(source).unwrap();
    let track = &program.tracks[0];
    let template = program.voice(track.voice).unwrap();
    let transport = Transport::new(120.0).unwrap();
    let mut scheduler = PitchScheduler::default();
    let mut sequencer = PitchScheduler::sequencer(template);
    sequencer.set_sample_rate(SR);
    scheduler
        .fill_to(
            Frac::ONE,
            &track.pattern,
            template,
            transport,
            &mut sequencer,
        )
        .unwrap();

    let audio = render(&mut sequencer, SR, 2.0);
    let early = rms(&audio[0][2_000..12_000]);
    let late = rms(&audio[0][72_000..88_000]);
    assert!(
        late > early * 3.0,
        "curve appears onset-sampled: early {early}, late {late}"
    );
}

#[test]
fn transformed_multitrack_lua_score_reaches_measured_audio() {
    let source = r#"
    local low = voice {
      graph = function(n)
        return sine(n.hz) * n.velocity * 0.08 >> pan(-0.25)
      end,
    }
    local high = voice {
      graph = function(n)
        return saw(n.hz) * n.velocity * 0.035 >> pan(0.25)
      end,
    }

    play(low, pattern("c3 e3") >> every(2, rev) >> velocity(0.8))
    play(high, pattern("g4 ~") >> fast(2) >> off(0.25, rev)
                                >> degrade(0) >> velocity(0.55))
    "#;
    let program = evaluate(source).unwrap();
    let tracks = program
        .tracks
        .iter()
        .map(|track| {
            ScheduledTrack::new(
                &track.pattern,
                program.voice(track.voice).expect("validated voice"),
            )
        })
        .collect::<Vec<_>>();
    let first = program.voice(program.tracks[0].voice).unwrap();
    let mut sequencer = PitchScheduler::sequencer(first);
    sequencer.set_sample_rate(SR);
    let report = ProgramScheduler::default()
        .fill_to(
            Frac::ONE,
            tracks,
            Transport::new(120.0).unwrap(),
            &mut sequencer,
        )
        .unwrap();

    assert_eq!(report.voices, 6);
    let audio = render(&mut sequencer, SR, 2.0);
    assert!(rms(&audio[0]) > 0.025);
    assert!(rms(&audio[1]) > 0.025);
}

#[test]
fn lua_routed_voices_and_persistent_runs_share_one_live_audio_path() {
    let source = r#"
    local level = control {
      name = "field",
      range = { 0, 1 },
      default = 0.04,
    }
    local room = bus { channels = 2 }

    local drone = patch {
      graph = function()
        local tone = sine(110) * level * 0.15
        local stereo = tone >> pan(0)
        return stereo >> to(room, 0.5)
      end,
    }
    run(drone)

    local room_return = patch {
      inputs = 4,
      graph = function(c)
        return c.inputs[3] >> pan(0)
      end,
    }
    run(room_return)

    local struck = voice {
      graph = function(n)
        local tone = sine(n.hz) * decay(ms(90)) * 0.12
        return tone >> pan(n.pan)
      end,
    }
    play(struck, pattern("c4 ~") >> to(room, 0.4))
    "#;
    let program = evaluate(source).unwrap();
    let patches = program
        .runs
        .iter()
        .filter(|run| run.span.is_none())
        .map(|run| program.patch(run.patch).unwrap())
        .collect::<Vec<_>>();
    let mut runtime = PersistentRuntime::new(&program.buses, &program.controls, patches).unwrap();
    let mut persistent = runtime.take_processor();
    let mut sequencer = runtime.sequencer();
    sequencer.set_sample_rate(SR);
    persistent.set_sample_rate(SR);
    sequencer.allocate();
    persistent.allocate();

    let tracks = program
        .tracks
        .iter()
        .map(|track| {
            ScheduledTrack::with_routing(
                &track.pattern,
                program.voice(track.voice).expect("validated voice"),
                &track.routing,
            )
        })
        .collect::<Vec<_>>();
    let mut scheduler = ProgramScheduler::default();
    scheduler
        .fill_routed_to(
            Frac::new(1, 2),
            tracks.iter().copied(),
            Transport::new(120.0).unwrap(),
            &mut sequencer,
            runtime.layout(),
            runtime.controls(),
        )
        .unwrap();

    let frames = SR as usize;
    let mut main = (0..2)
        .map(|_| Vec::with_capacity(frames))
        .collect::<Vec<_>>();
    let mut stems = vec![0.0; runtime.layout().total_channels()];
    let mut output = vec![0.0; runtime.layout().total_channels()];
    for frame in 0..frames {
        if frame == frames / 2 {
            scheduler
                .fill_routed_to(
                    Frac::ONE,
                    tracks.iter().copied(),
                    Transport::new(120.0).unwrap(),
                    &mut sequencer,
                    runtime.layout(),
                    runtime.controls(),
                )
                .unwrap();
            let field = program.controls.id("field").unwrap();
            runtime.controls().set(field, 0.8).unwrap();
        }
        sequencer.tick(&[], &mut stems);
        persistent.tick(&stems, &mut output);
        main[0].push(output[0]);
        main[1].push(output[1]);
    }

    let struck = rms(&main[0][1_000..8_000]);
    let quiet_drone = rms(&main[0][16_000..22_000]);
    let raised_drone = rms(&main[0][32_000..44_000]);
    assert!(struck > quiet_drone * 1.5);
    assert!(
        raised_drone > quiet_drone * 8.0,
        "shared control did not reach the persistent patch: quiet {quiet_drone}, raised {raised_drone}"
    );
    assert_eq!(runtime.runs(), 2);
}

#[test]
fn lua_timeline_and_tempo_map_reach_finite_measured_audio() {
    let source = r#"
    tempo {
      { at = bars(0), bpm = 60 },
      { at = bars(2), bpm = 120 },
    }
    local tone = voice {
      graph = function(n)
        return sine(n.hz) * n.velocity * 0.08
      end,
    }
    play(tone, timeline {
      at(bars(0), pattern("c4")),
      at(bars(2), pattern("e4")),
    })
    "#;
    let program = evaluate(source).unwrap();
    let track = &program.tracks[0];
    assert_eq!(
        track
            .pattern
            .onsets(apteronotus_pattern::Span::new(Frac::ZERO, Frac::int(6)))
            .len(),
        2
    );

    let template = program.voice(track.voice).unwrap();
    let tracks = [ScheduledTrack::new(&track.pattern, template)];
    let mut sequencer = PitchScheduler::sequencer(template);
    sequencer.set_sample_rate(SR);
    ProgramScheduler::default()
        .fill_to_tempo_map(Frac::int(3), tracks, &program.tempo, &mut sequencer)
        .unwrap();

    let audio = render(&mut sequencer, SR, 10.0);
    let first = zero_crossing_hz(&audio[0][4_000..40_000], SR);
    let middle = rms(&audio[0][(SR as usize * 5)..(SR as usize * 7)]);
    let second = zero_crossing_hz(&audio[0][(SR as usize * 8 + 4_000)..(SR as usize * 9)], SR);
    assert!((first - 261.63).abs() < 3.0, "measured {first} Hz");
    assert!(
        middle < 1.0e-4,
        "finite timeline repeated or leaked: {middle}"
    );
    assert!((second - 329.63).abs() < 3.0, "measured {second} Hz");
}

#[test]
fn transport_signal_gain_is_sampled_at_each_note_onset() {
    let source = r#"
    local tone = voice {
      graph = function(n)
        return sine(n.hz) * n.velocity * 0.08
      end,
    }
    local main = step(bars(1))
    play(tone, pattern("c4 ~") >> velocity(0.2 + 0.8 * main))
    "#;
    let program = evaluate(source).unwrap();
    let track = &program.tracks[0];
    let template = program.voice(track.voice).unwrap();
    let tracks = [ScheduledTrack::new(&track.pattern, template)];
    let mut sequencer = PitchScheduler::sequencer(template);
    sequencer.set_sample_rate(SR);
    ProgramScheduler::default()
        .fill_to_tempo_map(Frac::int(2), tracks, &program.tempo, &mut sequencer)
        .unwrap();

    let audio = render(&mut sequencer, SR, 4.0);
    let quiet = rms(&audio[0][4_000..36_000]);
    let loud = rms(&audio[0][100_000..132_000]);
    assert!(
        loud > quiet * 4.0,
        "transport expression was not sampled at note onset: quiet {quiet}, loud {loud}"
    );
}

#[test]
fn shipped_supersaws_song_reaches_routed_persistent_measured_audio() {
    let source = include_str!("../../../songs/supersaws.eod");
    let program = evaluate(source).unwrap();
    let patches = program
        .runs
        .iter()
        .filter(|run| run.span.is_none())
        .map(|run| program.patch(run.patch).unwrap())
        .collect::<Vec<_>>();
    let mut runtime = PersistentRuntime::new(&program.buses, &program.controls, patches).unwrap();
    let mut persistent = runtime.take_processor();
    let mut sequencer = runtime.sequencer();
    sequencer.set_sample_rate(SR);
    persistent.set_sample_rate(SR);
    sequencer.allocate();
    persistent.allocate();

    let tracks = program
        .tracks
        .iter()
        .map(|track| {
            ScheduledTrack::with_routing(
                &track.pattern,
                program.voice(track.voice).unwrap(),
                &track.routing,
            )
        })
        .collect::<Vec<_>>();
    ProgramScheduler::default()
        .fill_routed_to_tempo_map(
            Frac::ONE,
            tracks,
            &program.tempo,
            &mut sequencer,
            runtime.layout(),
            runtime.controls(),
        )
        .unwrap();

    let frames = (SR * 2.25) as usize;
    let mut stems = vec![0.0; runtime.layout().total_channels()];
    let mut output = vec![0.0; runtime.layout().total_channels()];
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut room = Vec::with_capacity(frames);
    let room_lane = program.buses.main_channels();
    for _ in 0..frames {
        sequencer.tick(&[], &mut stems);
        persistent.tick(&stems, &mut output);
        left.push(output[0]);
        right.push(output[1]);
        room.push(stems[room_lane]);
    }

    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
    assert!(rms(&left) > 0.002);
    assert!(rms(&right) > 0.002);
    assert!(
        rms(&room) > 0.0001,
        "the noise-hat send never reached its bus"
    );
}

#[test]
fn shipped_synthwave_song_reaches_routed_persistent_measured_audio() {
    let source = include_str!("../../../songs/synthwave.eod");
    let program = evaluate(source).unwrap();
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
    let patches = program
        .runs
        .iter()
        .filter(|run| run.span.is_none())
        .map(|run| program.patch(run.patch).unwrap())
        .collect::<Vec<_>>();
    let mut runtime = PersistentRuntime::new(&program.buses, &program.controls, patches).unwrap();
    let mut persistent = runtime.take_processor();
    let mut sequencer = runtime.sequencer();
    sequencer.set_sample_rate(SR);
    persistent.set_sample_rate(SR);
    sequencer.allocate();
    persistent.allocate();

    let tracks = program
        .tracks
        .iter()
        .map(|track| {
            ScheduledTrack::with_routing(
                &track.pattern,
                program.voice(track.voice).unwrap(),
                &track.routing,
            )
        })
        .collect::<Vec<_>>();
    ProgramScheduler::default()
        .fill_routed_to_tempo_map(
            Frac::ONE,
            tracks,
            &program.tempo,
            &mut sequencer,
            runtime.layout(),
            runtime.controls(),
        )
        .unwrap();

    let frames = (SR * 2.5) as usize;
    let mut stems = vec![0.0; runtime.layout().total_channels()];
    let mut output = vec![0.0; runtime.layout().total_channels()];
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut verb = Vec::with_capacity(frames);
    let verb_lane = program.buses.main_channels();
    for _ in 0..frames {
        sequencer.tick(&[], &mut stems);
        persistent.tick(&stems, &mut output);
        left.push(output[0]);
        right.push(output[1]);
        verb.push(stems[verb_lane]);
    }

    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
    assert!(rms(&left) > 0.0005);
    assert!(rms(&right) > 0.0005);
    assert!(
        rms(&verb) > 0.0001,
        "the pad send never reached the reverb bus"
    );
}

#[test]
fn duck_compiles_a_rhythm_into_a_live_track_envelope() {
    let program = evaluate(
        r#"
        tempo(120)
        local tone = voice {
          graph = function()
            return dc(0.2) >> pan(0)
          end,
        }
        play(tone, pattern("x") >> hold(bars(1)) >> duck("1 0", 0.8))
        "#,
    )
    .unwrap();
    let track = &program.tracks[0];
    let template = program.voice(track.voice).unwrap();
    let layout = apteronotus_synth::BusLayout::new(2).unwrap();
    let controls = apteronotus_synth::ControlStore::new(&program.controls);
    let mut sequencer = fundsp::prelude32::Sequencer::new(
        0,
        layout.total_channels(),
        fundsp::prelude32::ReplayMode::None,
    );
    sequencer.set_sample_rate(SR);
    ProgramScheduler::default()
        .fill_routed_to_tempo_map(
            Frac::ONE,
            [ScheduledTrack::with_routing(
                &track.pattern,
                template,
                &track.routing,
            )],
            &program.tempo,
            &mut sequencer,
            &layout,
            &controls,
        )
        .unwrap();

    let audio = render(&mut sequencer, SR, 2.0);
    let dipped = rms(&audio[0][2_000..10_000]);
    let released = rms(&audio[0][20_000..44_000]);
    assert!(
        released > dipped * 2.0,
        "duck was onset-sampled instead of remaining live: dipped {dipped}, released {released}"
    );
}

#[test]
fn shipped_waves_song_reaches_routed_persistent_measured_audio() {
    let source = include_str!("../../../songs/waves.eod");
    let program = evaluate(source).unwrap();
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
    let patches = program
        .runs
        .iter()
        .filter(|run| run.span.is_none())
        .map(|run| program.patch(run.patch).unwrap())
        .collect::<Vec<_>>();
    let mut runtime = PersistentRuntime::new(&program.buses, &program.controls, patches).unwrap();
    let mut persistent = runtime.take_processor();
    let mut sequencer = runtime.sequencer();
    sequencer.set_sample_rate(SR);
    persistent.set_sample_rate(SR);
    sequencer.allocate();
    persistent.allocate();

    let tracks = program
        .tracks
        .iter()
        .map(|track| {
            ScheduledTrack::with_routing(
                &track.pattern,
                program.voice(track.voice).unwrap(),
                &track.routing,
            )
        })
        .collect::<Vec<_>>();
    ProgramScheduler::default()
        .fill_routed_to_tempo_map(
            Frac::ONE,
            tracks,
            &program.tempo,
            &mut sequencer,
            runtime.layout(),
            runtime.controls(),
        )
        .unwrap();

    let frames = (SR * 2.0) as usize;
    let mut stems = vec![0.0; runtime.layout().total_channels()];
    let mut output = vec![0.0; runtime.layout().total_channels()];
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for _ in 0..frames {
        sequencer.tick(&[], &mut stems);
        persistent.tick(&stems, &mut output);
        left.push(output[0]);
        right.push(output[1]);
    }

    assert!(left.iter().chain(&right).all(|sample| sample.is_finite()));
    assert!(rms(&left) > 0.001);
    assert!(rms(&right) > 0.001);
}

#[test]
fn shipped_poles_song_reaches_routed_persistent_measured_audio() {
    let (program, stems, output) =
        render_routed_song(include_str!("../../../songs/poles.eod"), Frac::ONE, 2.75);
    let room_lane = program.buses.main_channels();

    assert!(
        output[0]
            .iter()
            .chain(&output[1])
            .all(|sample| sample.is_finite())
    );
    assert!(rms(&output[0]) > 0.001);
    assert!(rms(&output[1]) > 0.001);
    assert!(
        rms(&stems[room_lane]) > 0.0001,
        "cowbell/cymbal graph sends never reached the room bus"
    );
}

#[test]
fn shipped_jamming_song_reaches_persistent_audio_with_silent_input_fallback() {
    let (program, _stems, output) =
        render_routed_song(include_str!("../../../songs/jamming.eod"), Frac::ONE, 2.0);
    assert!(program.tracks[0].external_trigger.is_some());
    assert!(!program.tracks[0].onset_bindings.is_empty());
    assert!(program.voices.iter().any(|voice| !voice.sends.is_empty()));
    assert!(program.patches.iter().any(|patch| {
        patch
            .graph()
            .nodes
            .iter()
            .any(|node| matches!(node.op, apteronotus_synth::Op::Fdn { .. }))
    }));
    assert!(
        output[0]
            .iter()
            .chain(&output[1])
            .all(|sample| sample.is_finite())
    );
    assert!(rms(&output[0]) > 0.001);
    assert!(rms(&output[1]) > 0.001);
}

#[test]
fn shipped_jamming_song_hears_host_audio_and_strikes_its_tracked_bell() {
    let program = evaluate(include_str!("../../../songs/jamming.eod")).unwrap();
    let patches = program
        .runs
        .iter()
        .filter(|run| run.span.is_none())
        .map(|run| program.patch(run.patch).unwrap())
        .collect::<Vec<_>>();
    let mut runtime = PersistentRuntime::with_audio_inputs(
        &program.buses,
        &program.controls,
        &program.audio_inputs,
        patches,
    )
    .unwrap();
    assert_eq!(runtime.external_channels(), 1);
    let mut persistent = runtime.take_processor_with_audio_inputs();
    persistent.set_sample_rate(SR);
    persistent.allocate();

    let track = &program.tracks[0];
    assert_eq!(track.external_controls.len(), 1);
    assert_eq!(track.external_degrades.len(), 1);
    let trigger = track.external_trigger.unwrap();
    let hz_control = track
        .onset_bindings
        .iter()
        .find(|binding| binding.param == ParamId::Implicit(Implicit::Hz))
        .unwrap()
        .control;

    let lanes = runtime.layout().total_channels();
    let mut input = vec![0.0; lanes + runtime.external_channels()];
    let mut output = vec![0.0; lanes];
    let mut saw_trigger = false;
    for frame in 0..(SR as usize / 4) {
        input[lanes] = if frame < 256 {
            0.0
        } else {
            (core::f64::consts::TAU * 330.0 * frame as f64 / SR).sin() as f32 * 0.3
        };
        persistent.tick(&input, &mut output);
        saw_trigger |= runtime.controls().value(trigger).unwrap() >= 0.5;
    }
    assert!(
        saw_trigger,
        "the host-fed onset never reached the trigger control"
    );
    let tracked_hz = runtime.controls().value(hz_control).unwrap();
    assert!(
        (tracked_hz - 330.0).abs() < 5.0,
        "pitch tracker published {tracked_hz} Hz"
    );

    let at = Span::new(Frac::ZERO, Frac::ZERO);
    let event_bindings = track
        .external_controls
        .iter()
        .map(|binding| {
            let event = binding.pattern.query(at).into_iter().next().unwrap();
            let value = match event.value {
                Value::Leaf(ControlValue::Number(value)) => ParamValue::Number(value),
                Value::Leaf(ControlValue::Curve(curve)) => ParamValue::Curve(curve),
                other => panic!("unexpected external control value {other:?}"),
            };
            (binding.param, value)
        })
        .collect::<Vec<_>>();

    let mut sequencer = runtime.sequencer();
    sequencer.set_sample_rate(SR);
    schedule_external_routed(
        ExternalOnset {
            at_seconds: 0.01,
            gate_seconds: 0.01,
            event_seed: 7,
        },
        program.voice(track.voice).unwrap(),
        &track.routing,
        &track.onset_bindings,
        &event_bindings,
        &mut sequencer,
        RoutedRuntime::new(runtime.layout(), runtime.controls()),
    )
    .unwrap();
    let audio = render(&mut sequencer, SR, 0.5);
    assert!(rms(&audio[0]) > 0.0001);
    assert!(rms(&audio[1]) > 0.0001);
}

#[test]
fn shipped_techno_song_reaches_routed_persistent_measured_audio() {
    let (program, stems, output) =
        render_routed_song(include_str!("../../../songs/techno.eod"), Frac::ONE, 2.0);
    let plate_lane = program.buses.main_channels();

    assert!(
        output[0]
            .iter()
            .chain(&output[1])
            .all(|sample| sample.is_finite())
    );
    assert!(rms(&output[0]) > 0.001);
    assert!(rms(&output[1]) > 0.001);
    assert!(
        rms(&stems[plate_lane]) > 0.0001,
        "clap/stab sends never reached the plate bus"
    );
}

#[test]
fn finite_patch_score_drives_live_controls_and_routes_its_audio() {
    let (program, stems, output) = render_routed_song(
        r#"
        tempo(120)
        local room = send {
          graph = delay(ms(40)) >> feedback(0.35),
          level = 0.25,
        }
        local lead = patch {
          controls = {
            pitch = note_control("c4"),
            gate = gate_control(false),
            pressure = control { range = { 0, 1 }, default = 0.1 },
          },
          graph = function(c)
            local amp = gate_env(c.gate, ms(5), ms(30), 0.8, ms(80))
            return sine(note_hz(c.pitch)) * amp * c.pressure * 0.2 >> pan(0)
          end,
        }
        local score = timeline {
          at(bars(0), note("c4") >> hold(secs(0.4))
            >> lead.pressure(curve {
                 { phase(0), 0.1 },
                 { phase(0.5), 0.9 },
                 { phase(1), 0.2 },
               }))
        }
        play(lead, score >> to(room, 0.5))
        "#,
        Frac::new(1, 2),
        0.8,
    );

    let room_lane = program.buses.main_channels();
    let early = rms(&output[0][(0.03 * SR) as usize..(0.12 * SR) as usize]);
    let middle = rms(&output[0][(0.20 * SR) as usize..(0.32 * SR) as usize]);
    let released = rms(&output[0][(0.62 * SR) as usize..]);
    assert!(
        middle > early * 1.5,
        "curve did not rise: early {early}, middle {middle}"
    );
    assert!(
        released < middle * 0.2,
        "gate did not release: middle {middle}, released {released}"
    );
    assert!(rms(&stems[room_lane]) > 0.0001);
}

#[test]
fn shipped_neon_song_reaches_routed_persistent_measured_audio() {
    let (program, stems, output) = render_routed_song(
        include_str!("../../../songs/neon.eod"),
        Frac::new(1, 4),
        2.0,
    );
    let hall_lane = program.buses.main_channels();

    assert!(
        output[0]
            .iter()
            .chain(&output[1])
            .all(|sample| sample.is_finite())
    );
    assert!(rms(&output[0]) > 0.0001);
    assert!(rms(&output[1]) > 0.0001);
    assert!(rms(&stems[hall_lane]) > 0.00001);
}

#[test]
fn lua_finite_run_is_instantiated_and_removed_on_its_transport_span() {
    let (_program, stems, output) = render_routed_song(
        r#"
        tempo(120)
        local weather = patch {
          graph = function()
            return sine(110) * 0.08 >> pan(0)
          end,
        }
        run(weather, span(bars(0), bars(0.125)))
        "#,
        Frac::ONE,
        0.7,
    );

    let sounding = rms(&output[0][(0.03 * SR) as usize..(0.20 * SR) as usize]);
    let stopped = rms(&output[0][(0.35 * SR) as usize..]);
    assert!(sounding > 0.01);
    assert!(stopped < 1.0e-6);
    assert!(rms(&stems[0]) > 0.005);
}

#[test]
fn unbound_optional_audio_uses_silence_without_muting_the_owned_rack() {
    let (_program, _stems, output) = render_routed_song(
        r#"
        local room = audio_input {
          name = "room",
          channels = 1,
          fallback = "silence",
        }
        local field = control_input {
          name = "field",
          range = { 0, 1 },
          default = 0.5,
        }
        local rack = patch {
          graph = function()
            return (room + sine(110) * field * 0.08) >> pan(0)
          end,
        }
        run(rack)
        "#,
        Frac::new(1, 4),
        0.5,
    );

    assert!(
        output[0]
            .iter()
            .chain(&output[1])
            .all(|sample| sample.is_finite())
    );
    assert!(rms(&output[0]) > 0.01);
    assert!(rms(&output[1]) > 0.01);
}
