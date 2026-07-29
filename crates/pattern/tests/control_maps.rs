use apteronotus_pattern::{
    Basis, ControlMap, ControlPattern, ControlValue, Curve, CurveClock, Frac, GroupNode, Pattern,
    PatternMathOp, Signal, Span, SrcSpan, Value, ValueLimitError, ValueLimits, mini,
};

fn velocity(value: f64) -> Pattern {
    Pattern::num(value).named("velocity").unwrap()
}

fn field_number(value: &Value, name: &str) -> f64 {
    value
        .as_map()
        .and_then(|map| map.get(name))
        .and_then(ControlValue::as_f64)
        .unwrap()
}

#[test]
fn merge_preserves_left_structure_and_is_slice_idempotent() {
    let pattern = mini::parse("c4 e4 g4 c5")
        .unwrap()
        .merge(velocity(0.7))
        .unwrap();
    assert_eq!(pattern.density(), 4.0);

    let whole = pattern.onsets(Span::cycle(0));
    let mut sliced = pattern.onsets(Span::new(Frac::ZERO, Frac::new(1, 3)));
    sliced.extend(pattern.onsets(Span::new(Frac::new(1, 3), Frac::ONE)));
    let signature = |events: &[apteronotus_pattern::Event]| {
        events
            .iter()
            .map(|event| (event.whole.unwrap().begin, event.value.clone(), event.group))
            .collect::<Vec<_>>()
    };
    assert_eq!(signature(&whole), signature(&sliced));

    let primary = whole
        .iter()
        .map(|event| {
            event
                .value
                .as_map()
                .unwrap()
                .get("value")
                .and_then(ControlValue::as_str)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(primary, ["c4", "e4", "g4", "c5"]);
    for event in whole {
        assert_eq!(field_number(&event.value, "velocity"), 0.7);
    }
}

#[test]
fn simultaneous_group_members_sample_one_map_and_keep_provenance() {
    let chord = mini::parse("[c4,e4,g4]").unwrap();
    let merged = chord.merge(velocity(0.35)).unwrap();
    let events = merged.onsets(Span::cycle(0));
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|event| event.group.is_some()));
    assert!(
        events
            .iter()
            .all(|event| field_number(&event.value, "velocity") == 0.35)
    );

    let group = Pattern::group(
        GroupNode::new(99),
        vec![Pattern::word("a"), Pattern::word("b")],
    )
    .named("tone")
    .unwrap();
    assert!(
        group
            .onsets(Span::cycle(0))
            .iter()
            .all(|event| event.group.is_some())
    );
}

#[test]
fn right_fields_override_and_first_means_structural_query_order() {
    let notes = mini::parse("c4 e4").unwrap();
    let overridden = notes
        .clone()
        .merge(velocity(0.2))
        .unwrap()
        .merge(velocity(0.8))
        .unwrap();
    assert!(
        overridden
            .onsets(Span::cycle(0))
            .iter()
            .all(|event| field_number(&event.value, "velocity") == 0.8)
    );

    let simultaneous = Pattern::stack(vec![velocity(0.25), velocity(0.9)]);
    let first = notes.merge(simultaneous).unwrap();
    assert!(
        first
            .onsets(Span::cycle(0))
            .iter()
            .all(|event| field_number(&event.value, "velocity") == 0.25)
    );
}

#[test]
fn curves_are_carried_whole_and_never_sampled_by_merge() {
    let curve = Curve::new(CurveClock::NotePhase, 0.1)
        .term(Basis::Ramp, 0.8, 0.0, 1.0)
        .term(Basis::Step, -0.1, 0.75, 0.0);
    let control = Pattern::pure(Value::curve(curve.clone()))
        .named("pressure")
        .unwrap();
    let event = Pattern::word("c4")
        .merge(control)
        .unwrap()
        .onsets(Span::cycle(0))
        .remove(0);
    assert_eq!(
        event.value.as_map().unwrap().get("pressure"),
        Some(&ControlValue::Curve(curve))
    );
}

#[test]
fn map_width_curve_terms_and_field_spans_are_bounded() {
    let mut map = ControlMap::new();
    map.insert("a", ControlValue::Number(1.0), None).unwrap();
    map.insert("b", ControlValue::Bool(true), None).unwrap();
    let width = Pattern::pure(Value::Map(map))
        .validate_value_limits(ValueLimits {
            map_fields: 1,
            curve_terms: 8,
        })
        .unwrap_err();
    assert!(matches!(width, ValueLimitError::MapFields { .. }));

    let src = SrcSpan::new(12, 23);
    let curve = Curve::new(CurveClock::NoteSeconds, 0.0)
        .term(Basis::Step, 1.0, 0.0, 0.0)
        .term(Basis::Ramp, 1.0, 0.0, 1.0);
    let map = ControlMap::named("pressure", ControlValue::Curve(curve), Some(src)).unwrap();
    let terms = Pattern::pure(Value::Map(map))
        .validate_value_limits(ValueLimits {
            map_fields: 8,
            curve_terms: 1,
        })
        .unwrap_err();
    assert_eq!(terms.src(), Some(src));
}

#[test]
fn no_right_event_still_lifts_the_left_primary_value() {
    let event = Pattern::word("c4")
        .merge(Pattern::silence())
        .unwrap()
        .onsets(Span::cycle(0))
        .remove(0);
    assert_eq!(
        event
            .value
            .as_map()
            .and_then(|map| map.get("value"))
            .and_then(ControlValue::as_str),
        Some("c4")
    );
}

#[test]
fn leaf_controls_are_rejected_and_silence_is_vacuously_valid() {
    let leaf = Pattern::word("forgot-a-setter");
    assert!(Pattern::word("c4").merge(leaf).is_err());
    assert!(ControlPattern::try_from(Pattern::silence()).is_ok());

    let transformed_silence = Pattern::silence().degrade_by(0.5, 7).every(2, Pattern::rev);
    assert!(ControlPattern::try_from(transformed_silence).is_ok());
}

#[test]
fn an_onset_on_a_control_boundary_selects_the_slot_beginning_there() {
    let controls = mini::parse("0 1").unwrap().named("pan").unwrap();
    let events = mini::parse("c4 e4")
        .unwrap()
        .merge(controls)
        .unwrap()
        .onsets(Span::cycle(0));
    assert_eq!(events[0].whole.unwrap().begin, Frac::ZERO);
    assert_eq!(field_number(&events[0].value, "pan"), 0.0);
    assert_eq!(events[1].whole.unwrap().begin, Frac::new(1, 2));
    assert_eq!(field_number(&events[1].value, "pan"), 1.0);
}

#[test]
fn structural_first_is_stable_through_rev_every_and_sliced_queries() {
    let right = Pattern::stack(vec![velocity(0.25), velocity(0.9)])
        .rev()
        .every(2, Pattern::rev);
    let merged = mini::parse("c4 e4 g4 c5").unwrap().merge(right).unwrap();

    let whole = merged.onsets(Span::cycle(0));
    let mut sliced = merged.onsets(Span::new(Frac::ZERO, Frac::new(3, 5)));
    sliced.extend(merged.onsets(Span::new(Frac::new(3, 5), Frac::ONE)));
    let values = |events: &[apteronotus_pattern::Event]| {
        events
            .iter()
            .map(|event| {
                (
                    event.whole.unwrap().begin,
                    field_number(&event.value, "velocity"),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(values(&whole), values(&sliced));
    assert!(
        whole
            .iter()
            .all(|event| { field_number(&event.value, "velocity") == 0.25 })
    );
}

#[test]
fn event_signal_arithmetic_samples_the_transport_signal_at_each_merge_onset() {
    let values = mini::parse("1 2")
        .unwrap()
        .math(
            PatternMathOp::Mul,
            Pattern::signal(Signal::Line {
                from: 0.0,
                to: 1.0,
                length: Frac::ONE,
            }),
        )
        .unwrap()
        .named("gain")
        .unwrap();
    let notes = mini::parse("60*4").unwrap().merge(values).unwrap();
    let gains = notes
        .onsets(Span::cycle(0))
        .into_iter()
        .map(|event| field_number(&event.value, "gain"))
        .collect::<Vec<_>>();

    // The second note remains inside the first `1` control slot, but the line
    // is sampled at that note's onset, not the control slot's beginning.
    assert_eq!(gains, vec![0.0, 0.25, 1.0, 1.5]);
}

#[test]
fn arithmetic_between_two_event_patterns_waits_for_explicit_join_semantics() {
    let error = mini::parse("1 2")
        .unwrap()
        .math(PatternMathOp::Add, mini::parse("3 4").unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("explicit temporal join"));
}

#[test]
fn arithmetic_rejects_nonnumeric_events_with_their_source_span() {
    let error = mini::parse("c4 e4")
        .unwrap()
        .math(PatternMathOp::Mul, Pattern::signal(Signal::Constant(2.0)))
        .unwrap_err();

    assert!(error.to_string().contains("numeric event values"));
    assert!(error.to_string().contains("source bytes 0..2"));
}

#[test]
fn continuous_arithmetic_preserves_signal_source_attribution() {
    let src = SrcSpan::new(12, 16);
    let expression = Pattern::signal(Signal::Constant(0.5))
        .math(
            PatternMathOp::Mul,
            Pattern::signal_at(Signal::Step { at: Frac::ZERO }, src),
        )
        .unwrap();

    assert_eq!(expression.query(Span::cycle(0))[0].src, Some(src));
}
