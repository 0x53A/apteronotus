use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! {static MEASURING:Cell<bool>=const {Cell::new(false)};static ALLOCATIONS:Cell<usize>=const {Cell::new(0)};}
struct Counted;
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(p, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

#[test]
fn gesture_delivery_rendering_and_reset_do_not_allocate() {
    use apteronotus_synth::bowed::{BowedKind, BowedModel};
    use apteronotus_synth::bowed_gesture::{BowAction, c2_config};
    use apteronotus_synth::bowed_host::BowedScore;
    use apteronotus_synth::{
        ControlLayout, ControlSpec, ControlStore, GraphBuilder, PatchTemplate, instantiate_patch,
    };
    for (model, kind, min_hz, open_hz) in [
        (BowedModel::C2, BowedKind::Violin, 196.0, 293.6648),
        (BowedModel::C3, BowedKind::Violin, 196.0, 293.6648),
        (BowedModel::C4, BowedKind::Violin, 196.0, 293.6648),
        (BowedModel::C3, BowedKind::Cello, 65.0, 65.40639132514966),
    ] {
        let mut layout = ControlLayout::new();
        let key = layout
            .add(ControlSpec::new("Violin", 0.0, 1.0, 1.0))
            .unwrap();
        let score = BowedScore {
            config: apteronotus_synth::bowed::BowedConfig {
                model,
                kind,
                min_hz,
                ..c2_config()
            },
            live: true,
            clock: None,
            opens: vec![open_hz],
            commands: vec![],
            coordinates: vec![],
            reference_rate: 48000.0,
        };
        let mut graph = GraphBuilder::new();
        let dry = graph.bowed_performance(key, score)[0];
        let patch = PatchTemplate::new(graph.out(&[dry]).unwrap()).unwrap();
        let controls = ControlStore::new(&layout);
        let mut unit = instantiate_patch(&patch, &controls).unwrap();
        unit.set_sample_rate(48000.0);
        unit.allocate();
        let handle = controls.bowed_handle(key).unwrap();
        handle
            .send(
                Some(0.0),
                &[
                    BowAction::Stop {
                        string: 0,
                        hz: if kind == BowedKind::Cello {
                            73.41619197935188
                        } else {
                            440.0
                        },
                        // Install the low-C finger target before fading vibrato;
                        // joint publication bounds conservatively include both endpoints.
                        over: if kind == BowedKind::Cello { 0 } else { 100 },
                    },
                    BowAction::Bow {
                        string: Some(0),
                        speed: Some(0.4),
                        force: Some(1.0),
                        position: Some(0.12),
                        over: 60,
                    },
                    BowAction::Vibrato {
                        string: 0,
                        cents: 12.0,
                        rate: 5.2,
                        over: 4800,
                    },
                ],
            )
            .unwrap();
        handle
            .send(
                Some(0.1),
                &[BowAction::Vibrato {
                    string: 0,
                    cents: 0.0,
                    rate: 5.2,
                    over: 4800,
                }],
            )
            .unwrap();
        handle
            .send(Some(0.2), &[BowAction::Lift { over: 60 }])
            .unwrap();
        let mut output = [0.0];
        MEASURING.with(|v| v.set(true));
        for _ in 0..20000 {
            unit.tick(&[], &mut output);
        }
        unit.reset();
        MEASURING.with(|v| v.set(false));
        assert_eq!(ALLOCATIONS.with(Cell::get), 0);
        assert_eq!(handle.diagnostics().rejected, 0);
    }
}

#[test]
fn reciprocal_bridge_and_four_strings_render_and_reset_without_allocation() {
    for physical in [false, true] {
        reciprocal_allocation_case(physical);
    }
}
fn reciprocal_allocation_case(physical: bool) {
    use apteronotus_synth::bowed::{BowedConfig, BowedControls, BowedModel, BowedString};
    use apteronotus_synth::bowed_bridge::{BridgeSpec, ModalBridge};
    use fundsp::prelude32::AudioUnit;
    let mut bridge =
        ModalBridge::new(BridgeSpec::experiment(1.).unwrap(), 48000., &[1.; 4]).unwrap();
    let mut strings: Vec<_> = [196., 294.8366557, 440., 659.255114]
        .iter()
        .enumerate()
        .map(|(i, hz)| {
            let mut s = BowedString::new(
                BowedConfig {
                    model: BowedModel::C4,
                    min_hz: 195.,
                    max_hz: 700.,
                    ..Default::default()
                },
                48000.,
            )
            .unwrap();
            s.set_controls(BowedControls {
                hz: *hz,
                speed: 0.4,
                force: if i == 1 { 0.35 } else { 0. },
                position: 0.24,
            })
            .unwrap();
            if physical {
                s.configure_contact(apteronotus_synth::bowed_contact::BowContactCalibration {
                    impedance: 1.,
                    velocity_unit: 5.,
                    normal_force_unit: 1.,
                    static_friction: 0.8,
                    sliding_friction: 0.8 / 15.,
                    slip_knee: 0.2,
                })
                .unwrap();
            }
            s.connect_bridge(bridge.load(i).unwrap()).unwrap();
            s
        })
        .collect();
    ALLOCATIONS.with(|v| v.set(0));
    MEASURING.with(|v| v.set(true));
    for _ in 0..48000 {
        let input = std::array::from_fn(|i| strings[i].bridge_incident() as f64);
        let frame = bridge.step(input);
        for s in &mut strings {
            s.next_sample_at_bridge(frame.velocity);
        }
    }
    bridge.reset();
    for s in &mut strings {
        s.reset();
    }
    MEASURING.with(|v| v.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn polarized_bridge_and_two_string_planes_do_not_allocate() {
    use apteronotus_synth::bowed::{BowedConfig, BowedControls, BowedModel, BowedString};
    use apteronotus_synth::bowed_bridge::{BridgeSpec, PolarizedBridge};
    use fundsp::prelude32::AudioUnit;
    let mut bridge = PolarizedBridge::new(
        [
            BridgeSpec::experiment(0.5).unwrap(),
            BridgeSpec::experiment(1.).unwrap(),
        ],
        0.53,
        48000.,
        &[0.26],
    )
    .unwrap();
    let mut strings = std::array::from_fn::<_, 2, _>(|axis| {
        let mut s = BowedString::new(
            BowedConfig {
                model: BowedModel::C4,
                ..Default::default()
            },
            48000.,
        )
        .unwrap();
        s.set_controls(BowedControls {
            hz: 294.8,
            speed: 0.4,
            force: if axis == 0 { 0.3 } else { 0. },
            position: 0.18,
        })
        .unwrap();
        s.connect_bridge(bridge.load(0).unwrap()).unwrap();
        s
    });
    ALLOCATIONS.with(|v| v.set(0));
    MEASURING.with(|v| v.set(true));
    for _ in 0..48000 {
        let a = [
            [
                strings[0].bridge_incident() as f64,
                strings[1].bridge_incident() as f64,
            ],
            [0.; 2],
            [0.; 2],
            [0.; 2],
        ];
        let frame = bridge.step(a);
        for axis in 0..2 {
            strings[axis].next_sample_at_bridge(frame.velocity[axis]);
        }
    }
    bridge.reset();
    for s in &mut strings {
        s.reset();
    }
    MEASURING.with(|v| v.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}
