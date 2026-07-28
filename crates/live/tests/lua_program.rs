use apteronotus_live::{PitchScheduler, RevisionSlot, Transport};
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
