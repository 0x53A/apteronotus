use apteronotus_synth::spatial::{
    Emitter, Listener, Material, RoomSpec, RoomUnit, prepare_emitter,
};
use fundsp::prelude32::{AudioUnit, BufferVec};
fn scene() -> RoomSpec {
    RoomSpec {
        size: [8., 7., 3.1],
        walls: Material::Wood,
        floor: Material::Wood,
        ceiling: Material::Plaster,
        decay: 0.1,
        listener: Listener {
            position: [4., 1.3, 1.25],
            facing: [0., 1., 0.],
        },
        emitters: vec![Emitter {
            name: "Left".into(),
            position: [2.5, 3.2, 1.15],
        }],
    }
}
fn impulse(spec: RoomSpec, controls: [f32; 5], blocks: &[usize]) -> Vec<[f32; 2]> {
    let mut unit = RoomUnit::new(spec, 24000.).unwrap();
    let mut input = BufferVec::new(6);
    let mut output = BufferVec::new(2);
    let mut result = Vec::new();
    while result.len() < 12000 {
        let n = blocks[result.len() % blocks.len()].min(12000 - result.len());
        for i in 0..n {
            input
                .buffer_mut()
                .set_f32(0, i, if result.is_empty() && i == 0 { 1. } else { 0. });
            for (ch, &v) in controls.iter().enumerate() {
                input.buffer_mut().set_f32(ch + 1, i, v);
            }
        }
        unit.process(n, &input.buffer_ref(), &mut output.buffer_mut());
        for i in 0..n {
            result.push([
                output.buffer_ref().at_f32(0, i),
                output.buffer_ref().at_f32(1, i),
            ]);
        }
    }
    result
}
#[test]
fn direct_impulse_matches_prepared_response_and_preserves_head_cues() {
    let spec = scene();
    let response = prepare_emitter(&spec, 0, 24000.).unwrap();
    let output = impulse(spec, [1., 0., 0., 1., 0.], [1, 7, 32, 64].as_slice());
    for ear in 0..2 {
        for (i, x) in output.iter().enumerate() {
            assert!((x[ear] - response.direct[ear].get(i).copied().unwrap_or(0.)).abs() < 2e-6);
        }
    }
    let energies: [f32; 2] = std::array::from_fn(|ch| output.iter().map(|s| s[ch] * s[ch]).sum());
    assert!(energies[0] > energies[1] * 2.);
}
#[test]
fn path_controls_are_independent_and_bass_width_never_changes_direct() {
    let direct = impulse(scene(), [1., 1., 1., 0., 0.], &[64]);
    assert_eq!(direct, impulse(scene(), [1., 0., 0., 1., 1.], &[64]));
    let early = impulse(scene(), [0., 1., 0., 1., 1.], &[64]);
    let late = impulse(scene(), [0., 0., 1., 1., 1.], &[64]);
    let all = impulse(scene(), [1., 1., 1., 1., 1.], &[64]);
    for i in 0..all.len() {
        for ear in 0..2 {
            assert!((all[i][ear] - direct[i][ear] - early[i][ear] - late[i][ear]).abs() < 2e-6);
        }
    }
    let narrow = impulse(scene(), [0., 1., 1., 1., 0.], &[64]);
    for i in 0..all.len() {
        assert!(
            (narrow[i][0] + narrow[i][1] - early[i][0] - early[i][1] - late[i][0] - late[i][1])
                .abs()
                < 2e-6
        );
    }
}
#[test]
fn bounds_and_listener_orientation_are_checked() {
    let mut s = scene();
    s.listener.facing = [0., 0., 1.];
    assert!(s.validate().is_err());
    s = scene();
    s.emitters[0].position = s.listener.position;
    assert!(s.validate().is_err());
    s = scene();
    s.decay = 99.;
    assert!(s.validate().is_err());
    let a = impulse(scene(), [1., 0., 0., 0., 1.], &[64]);
    let mut turned = scene();
    turned.listener.facing = [0., -1., 0.];
    let b = impulse(turned, [1., 0., 0., 0., 1.], &[64]);
    let balance = |x: Vec<[f32; 2]>| x.iter().map(|s| s[0] * s[0] - s[1] * s[1]).sum::<f32>();
    assert!(balance(a) > 0.);
    assert!(balance(b) < 0.);
}
