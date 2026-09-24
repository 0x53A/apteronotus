//! Guard the block renderer against channel loss, overrun of partial blocks,
//! accidental resets between calls, and excessive deviation from scalar DSP.
use apteronotus_synth::{Adsr, GraphBuilder, GraphTemplate, Note, lower, n};
use fundsp::prelude32::AudioUnit;

fn graph() -> GraphTemplate {
    let mut g = GraphBuilder::new();
    let sweep = g.decay(0.07.into()).unwrap();
    let sweep = g.mul(sweep, 4000.0);
    let cutoff = g.add(sweep, 700.0);
    let noise = g.pink();
    let left = g.lowpass(noise, cutoff, 0.7);
    let tone = g.harmonics(n::HZ, vec![0.6, 0.25, 0.1, 0.05]);
    let right = g.lowpass(tone, cutoff, 0.8);
    let env = g.adsr(Adsr::new(0.002, 0.08, 0.15, 0.05));
    let left = g.mul(left, env);
    let right = g.mul(right, env);
    g.out(&[left, right]).unwrap()
}

fn scalar(unit: &mut dyn AudioUnit, rate: f64, seconds: f64) -> Vec<Vec<f32>> {
    unit.set_sample_rate(rate);
    unit.allocate();
    let mut out = vec![Vec::new(); unit.outputs()];
    let mut frame = vec![0.0; unit.outputs()];
    for _ in 0..(rate * seconds) as usize {
        unit.tick(&[], &mut frame);
        for (c, value) in frame.iter().enumerate() {
            out[c].push(*value);
        }
    }
    out
}

fn compare(expected: &[Vec<f32>], actual: &[Vec<f32>]) {
    assert_eq!(actual.len(), expected.len());
    for (expected, actual) in expected.iter().zip(actual) {
        assert_eq!(actual.len(), expected.len());
        let mut squared_error = 0.0;
        for (&a, &b) in expected.iter().zip(actual) {
            assert!(b.is_finite());
            let error = f64::from(a - b);
            // Block interpolation/SIMD changes rounding, not sound design.
            assert!(error.abs() < 0.0005, "sample error {error}");
            squared_error += error * error;
        }
        let rms_error = (squared_error / expected.len().max(1) as f64).sqrt();
        assert!(rms_error < 0.00005, "RMS error {rms_error}");
    }
}

#[test]
fn stereo_partial_blocks_and_continuations_match_scalar_rendering() {
    let graph = graph();
    let note = Note::new(523.25).duration(0.1).seed(92);
    for rate in [44100.0, 48000.0, 96000.0] {
        let mut tick = lower::instantiate(&graph, &note).unwrap();
        let mut block = lower::instantiate(&graph, &note).unwrap();
        for frames in [0, 1, 63, 64, 65, 127, 129, 4099, 20003] {
            let seconds = frames as f64 / rate;
            let expected = scalar(tick.as_mut(), rate, seconds);
            let actual = lower::render(block.as_mut(), rate, seconds);
            compare(&expected, &actual);
        }
        // Both renderers have passed gate+release; no rounded-up tail block or
        // restart is allowed to reintroduce the note.
        let tail = lower::render(block.as_mut(), rate, 0.02);
        assert!(tail.iter().flatten().all(|x| *x == 0.0));
    }
}

#[test]
fn seeded_noise_stays_reproducible_and_mono_length_is_exact() {
    let mut g = GraphBuilder::new();
    let noise = g.noise();
    let graph = g.out_mono(noise).unwrap();
    let note = Note::new(440.0).seed(19);
    let mut tick = lower::instantiate(&graph, &note).unwrap();
    let mut block = lower::instantiate(&graph, &note).unwrap();
    let seconds = 1027.0 / 48000.0;
    assert_eq!(
        scalar(tick.as_mut(), 48000.0, seconds),
        lower::render(block.as_mut(), 48000.0, seconds)
    );
}
