use apteronotus_synth::{
    GraphBuilder, Source,
    convolution::{ConvolutionUnit, ImpulseResponse},
};
use fundsp::prelude32::{AudioUnit, BufferVec};
#[test]
fn partitioned_matches_direct_across_partial_blocks_and_reset() {
    let kernel: Vec<f32> = (0..213).map(|i| (i as f32 * 0.31).sin() * 0.02).collect();
    let source: Vec<f32> = (0..1700).map(|i| (i as f32 * 0.73).cos() * 0.3).collect();
    let mut unit = ConvolutionUnit::new(
        ImpulseResponse {
            sample_rate: 48000.,
            samples: kernel.clone(),
        },
        48000.,
    )
    .unwrap();
    let mut expected = vec![0.; source.len()];
    for i in 0..source.len() {
        for j in 0..kernel.len().min(i + 1) {
            expected[i] += kernel[j] * source[i - j];
        }
    }
    let mut actual = Vec::new();
    let mut input = BufferVec::new(1);
    let mut output = BufferVec::new(1);
    let mut cursor = 0;
    for length in [1, 7, 64, 19, 3].into_iter().cycle() {
        let n = length.min(source.len() - cursor);
        for i in 0..n {
            input.buffer_mut().set_f32(0, i, source[cursor + i]);
        }
        unit.process(n, &input.buffer_ref(), &mut output.buffer_mut());
        for i in 0..n {
            actual.push(output.buffer_ref().at_f32(0, i));
        }
        cursor += n;
        if cursor == source.len() {
            break;
        }
    }
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| (a - b).abs() < 2e-6)
    );
    unit.reset();
    for (x, expected) in source.iter().zip(actual) {
        let mut y = [0.];
        unit.tick(&[*x], &mut y);
        assert!((y[0] - expected).abs() < 2e-6);
    }
}
#[test]
fn rate_change_preserves_response_time_and_template_horizon() {
    let mut kernel = vec![0.; 480];
    kernel[240] = 1.;
    let response = ImpulseResponse {
        sample_rate: 48000.,
        samples: kernel,
    };
    let mut unit = ConvolutionUnit::new(response.clone(), 48000.).unwrap();
    unit.set_sample_rate(24000.);
    let mut peaks = Vec::new();
    for i in 0..260 {
        let mut y = [0.];
        unit.tick(&[if i == 0 { 1. } else { 0. }], &mut y);
        peaks.push(y[0]);
    }
    assert_eq!(
        peaks
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0,
        120
    );
    assert!((peaks.iter().sum::<f32>() - 1.).abs() < 0.005);
    let mut graph = GraphBuilder::with_inputs(1);
    let filtered = graph.convolve(Source::Input(0), response);
    let graph = graph.out(&[filtered]).unwrap();
    assert!(graph.tail() >= 0.01);
    assert_eq!(graph.cost().data_entries, 480);
}
#[test]
fn invalid_responses_fail_before_preparation() {
    for response in [
        ImpulseResponse {
            sample_rate: 0.,
            samples: vec![1.],
        },
        ImpulseResponse {
            sample_rate: 48000.,
            samples: vec![],
        },
        ImpulseResponse {
            sample_rate: 48000.,
            samples: vec![f32::NAN],
        },
    ] {
        assert!(ConvolutionUnit::new(response, 48000.).is_err());
    }
}

#[test]
fn long_response_partitions_have_no_added_latency_on_partial_blocks() {
    let kernel: Vec<f32> = (0..5003).map(|i| (i as f32 * 0.31).sin() * 0.01).collect();
    let mut unit = ConvolutionUnit::new(
        ImpulseResponse {
            sample_rate: 48000.,
            samples: kernel.clone(),
        },
        48000.,
    )
    .unwrap();
    let mut input = BufferVec::new(1);
    let mut output = BufferVec::new(1);
    let pulses = [(0, 0.5), (101, 0.25), (2000, -0.1)];
    let mut at = 0;
    for block in [1, 7, 64, 19, 3].into_iter().cycle() {
        let n = block.min(7100 - at);
        for i in 0..n {
            input.buffer_mut().set_f32(
                0,
                i,
                pulses
                    .iter()
                    .find(|(t, _)| *t == at + i)
                    .map_or(0., |(_, v)| *v),
            );
        }
        unit.process(n, &input.buffer_ref(), &mut output.buffer_mut());
        for i in 0..n {
            let expected: f32 = pulses
                .iter()
                .map(|(t, v)| {
                    (at + i)
                        .checked_sub(*t)
                        .and_then(|j| kernel.get(j))
                        .copied()
                        .unwrap_or(0.)
                        * v
                })
                .sum();
            assert!((output.buffer_ref().at_f32(0, i) - expected).abs() < 2e-6);
        }
        at += n;
        if at == 7100 {
            break;
        }
    }
}
