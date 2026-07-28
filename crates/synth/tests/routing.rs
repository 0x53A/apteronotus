use apteronotus_synth::lower::{render, rms};
use apteronotus_synth::stdlib::{RingError, ring};
use apteronotus_synth::{
    BusLayout, EventRouting, GraphBuilder, LowerError, Note, ParamSpec, RoutingError, Source,
    TemplateError, instantiate, instantiate_routed, n,
};

const SR: f64 = 48_000.0;

fn assert_channel_scaled(actual: &[f32], original: &[f32], scale: f32) {
    assert_eq!(actual.len(), original.len());
    let worst = actual
        .iter()
        .zip(original)
        .map(|(actual, original)| (actual - original * scale).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1.0e-6, "worst sample error was {worst}");
}

#[test]
fn layout_is_main_then_declaration_order() {
    let mut layout = BusLayout::new(2).unwrap();
    let reverb = layout.add_bus(2).unwrap();
    let sidechain = layout.add_bus(1).unwrap();

    assert_eq!(layout.main_range(), 0..2);
    assert_eq!(layout.bus_range(reverb), Some(2..4));
    assert_eq!(layout.bus_range(sidechain), Some(4..5));
    assert_eq!(layout.total_channels(), 5);
}

#[test]
fn graph_send_can_tap_an_internal_signal() {
    let mut layout = BusLayout::new(1).unwrap();
    let bus = layout.add_bus(1).unwrap();

    let mut g = GraphBuilder::new();
    let raw = g.sine(n::HZ);
    let dry = g.mul(raw, 0.5);
    g.send(bus, &[raw], 0.25);
    let voice = g.out_mono(dry).unwrap();

    let mut unit =
        instantiate_routed(&voice, &Note::new(440.0), &layout, &EventRouting::new()).unwrap();
    let audio = render(unit.as_mut(), SR, 0.05);

    assert_eq!(audio.len(), 2);
    // The send taps `raw`, not the attenuated finished output: 0.25 / 0.5.
    assert_channel_scaled(&audio[1], &audio[0], 0.5);
}

#[test]
fn event_send_copies_the_finished_voice() {
    let mut layout = BusLayout::new(2).unwrap();
    let bus = layout.add_bus(2).unwrap();

    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let voice = g.out_panned(osc).unwrap();
    let mut event = EventRouting::new();
    event.send(bus, 0.4).unwrap();

    let mut unit =
        instantiate_routed(&voice, &Note::new(440.0).pan(-0.25), &layout, &event).unwrap();
    let audio = render(unit.as_mut(), SR, 0.05);

    assert_eq!(audio.len(), 4);
    assert_channel_scaled(&audio[2], &audio[0], 0.4);
    assert_channel_scaled(&audio[3], &audio[1], 0.4);
}

#[test]
fn graph_and_event_sends_sum_on_the_same_lane() {
    let mut layout = BusLayout::new(1).unwrap();
    let bus = layout.add_bus(1).unwrap();

    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    g.send(bus, &[osc], 0.25);
    let voice = g.out_mono(osc).unwrap();
    let mut event = EventRouting::new();
    event.send(bus, 0.5).unwrap();

    let mut unit = instantiate_routed(&voice, &Note::new(440.0), &layout, &event).unwrap();
    let audio = render(unit.as_mut(), SR, 0.05);

    assert_channel_scaled(&audio[1], &audio[0], 0.75);
}

#[test]
fn unused_bus_lanes_are_silence() {
    let mut layout = BusLayout::new(1).unwrap();
    layout.add_bus(2).unwrap();

    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    let voice = g.out_mono(osc).unwrap();
    let mut unit =
        instantiate_routed(&voice, &Note::new(440.0), &layout, &EventRouting::new()).unwrap();
    let audio = render(unit.as_mut(), SR, 0.02);

    assert!(rms(&audio[0]) > 0.1);
    assert_eq!(rms(&audio[1]), 0.0);
    assert_eq!(rms(&audio[2]), 0.0);
}

#[test]
fn graph_send_tail_keeps_the_whole_voice_alive() -> Result<(), RingError> {
    let mut layout = BusLayout::new(1).unwrap();
    let bus = layout.add_bus(1).unwrap();

    let mut g = GraphBuilder::new();
    let impulse = g.impulse();
    let decay = g.param(ParamSpec::new("decay", 0.1, 3.0, 1.0));
    let resonant = ring(&mut g, impulse, 440.0, decay)?;
    g.send(bus, &[resonant], 1.0);
    let voice = g.out_mono(Source::Const(0.0)).unwrap();

    assert_eq!(voice.tail(), 3.0);
    Ok(())
}

#[test]
fn sends_are_never_silently_dropped() {
    let mut layout = BusLayout::new(1).unwrap();
    let bus = layout.add_bus(1).unwrap();
    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    g.send(bus, &[osc], 1.0);
    let voice = g.out_mono(osc).unwrap();

    assert!(matches!(
        instantiate(&voice, &Note::new(440.0)),
        Err(LowerError::LayoutRequired)
    ));
}

#[test]
fn routing_shape_errors_happen_before_the_backend() {
    assert_eq!(BusLayout::new(0), Err(RoutingError::ZeroChannels));

    let mut foreign = BusLayout::new(1).unwrap();
    foreign.add_bus(1).unwrap();
    let unknown = foreign.add_bus(1).unwrap();

    let mut layout = BusLayout::new(1).unwrap();
    let stereo_bus = layout.add_bus(2).unwrap();

    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    g.send(unknown, &[osc], 1.0);
    let voice = g.out_mono(osc).unwrap();
    assert!(matches!(
        instantiate_routed(&voice, &Note::new(440.0), &layout, &EventRouting::new()),
        Err(LowerError::Routing(RoutingError::UnknownBus(bus))) if bus == unknown
    ));

    let mut g = GraphBuilder::new();
    let osc = g.sine(n::HZ);
    g.send(stereo_bus, &[osc], 1.0);
    let voice = g.out_mono(osc).unwrap();
    assert!(matches!(
        instantiate_routed(&voice, &Note::new(440.0), &layout, &EventRouting::new()),
        Err(LowerError::Routing(RoutingError::BusChannelMismatch {
            expected: 2,
            found: 1,
            ..
        }))
    ));
}

#[test]
fn an_empty_graph_send_is_invalid_data() {
    let mut layout = BusLayout::new(1).unwrap();
    let bus = layout.add_bus(1).unwrap();
    let mut g = GraphBuilder::new();
    g.send(bus, &[], 1.0);

    assert_eq!(
        g.out_mono(Source::Const(0.0)),
        Err(TemplateError::EmptySend { send: 0 })
    );
}
