use apteronotus_live::{
    PersistentRuntime, PitchScheduler, ProgramScheduler, RevisionSlot, ScheduledTrack, Transport,
};
use apteronotus_lua::{Limits, Program, evaluate};
use apteronotus_pattern::Frac;
use apteronotus_synth::lower::{render, rms, zero_crossing_hz};
use fundsp::prelude32::AudioUnit;

const SR: f64 = 48_000.0;

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
      clock = "note_phase",
      { basis = "ramp", coefficient = 1, delay = 0, length = 1 },
    }
    play(pad, pattern("c4") >> pad.pressure(rise))
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
    local room = bus { channels = 1 }

    local drone = patch {
      graph = function()
        local tone = sine(110) * level * 0.15
        return (tone >> to(room, 0.5)) >> pan(0)
      end,
    }
    run(drone)

    local room_return = patch {
      inputs = 3,
      graph = function(c)
        return c.inputs[3] >> delay(ms(3)) >> mul(0.5) >> pan(0)
      end,
    }
    run(room_return)

    local struck = voice {
      graph = function(n)
        local tone = sine(n.hz) * decay(ms(90)) * 0.12
        return (tone >> to(room, 0.4)) >> pan(n.pan)
      end,
    }
    play(struck, "c4 ~")
    "#;
    let program = evaluate(source).unwrap();
    let patches = program
        .runs
        .iter()
        .map(|id| program.patch(*id).unwrap())
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
            ScheduledTrack::new(
                &track.pattern,
                program.voice(track.voice).expect("validated voice"),
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
