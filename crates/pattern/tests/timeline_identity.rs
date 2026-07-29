use apteronotus_pattern::{
    Frac, GroupNode, Pattern, Span, Timeline, TimelineError, TimelineEvent, TimelineId, Value,
    mini, parse_at,
};

fn f(n: i64, d: i64) -> Frac {
    Frac::new(n, d)
}

#[test]
fn a_timeline_is_finite_and_does_not_repeat() {
    let timeline = Timeline::new(
        TimelineId::new(7),
        Span::new(Frac::ZERO, Frac::int(4)),
        vec![
            TimelineEvent::new(Span::new(f(1, 2), f(3, 2)), Value::text("a"), 10),
            TimelineEvent::new(Span::new(Frac::int(3), f(7, 2)), Value::text("b"), 11),
        ],
    )
    .unwrap();
    let pattern = Pattern::timeline(timeline);

    assert_eq!(pattern.onsets(Span::new(Frac::ZERO, Frac::int(8))).len(), 2);
    assert!(
        pattern
            .query(Span::new(Frac::int(4), Frac::int(8)))
            .is_empty()
    );

    let fragment = pattern.query(Span::new(Frac::ONE, f(5, 4)));
    assert_eq!(fragment.len(), 1);
    assert!(!fragment[0].has_onset());
    assert_eq!(fragment[0].whole, Some(Span::new(f(1, 2), f(3, 2))));
}

#[test]
fn recorded_ordinals_are_validated_not_reconstructed() {
    let event = |ordinal| {
        TimelineEvent::new(
            Span::new(Frac::ZERO, Frac::ONE),
            Value::text("hit"),
            ordinal,
        )
    };
    assert_eq!(
        Timeline::new(TimelineId::new(1), Span::cycle(0), vec![event(4), event(4)]),
        Err(TimelineError::DuplicateOrdinal(4))
    );
}

#[test]
fn simultaneous_recorded_events_have_stable_distinct_seeds() {
    let timeline = Timeline::new(
        TimelineId::new(99),
        Span::cycle(0),
        vec![
            TimelineEvent::new(Span::cycle(0), Value::text("x"), 20),
            TimelineEvent::new(Span::cycle(0), Value::text("x"), 21),
        ],
    )
    .unwrap();
    let pattern = Pattern::timeline(timeline);

    let whole = pattern.onsets(Span::cycle(0));
    let sliced = pattern.onsets(Span::new(Frac::ZERO, f(1, 3)));
    assert_eq!(whole.len(), 2);
    assert_ne!(whole[0].seed(), whole[1].seed());
    assert_eq!(
        whole.iter().map(|event| event.seed()).collect::<Vec<_>>(),
        sliced.iter().map(|event| event.seed()).collect::<Vec<_>>()
    );
}

#[test]
fn bracketed_chords_are_groups_but_plain_stacks_are_not() {
    let chord = mini::parse("[a,c,e]").unwrap().onsets(Span::cycle(0));
    assert_eq!(chord.len(), 3);
    let group = chord[0].group.unwrap();
    assert!(
        chord
            .iter()
            .all(|event| event.group.unwrap().key == group.key)
    );
    assert_eq!(
        chord
            .iter()
            .map(|event| event.group.unwrap().index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(chord.iter().all(|event| event.group.unwrap().count == 3));

    let parallel = mini::parse("a,c,e").unwrap().onsets(Span::cycle(0));
    assert!(parallel.iter().all(|event| event.group.is_none()));
}

#[test]
fn group_identity_is_query_idempotent_and_member_seeds_are_distinct() {
    let group = Pattern::group(
        GroupNode::new(123),
        vec![Pattern::word("a"), Pattern::word("a"), Pattern::word("a")],
    );
    let whole = group.onsets(Span::cycle(0));
    let sliced = group.onsets(Span::new(Frac::ZERO, f(1, 7)));

    assert_eq!(whole[0].group.unwrap().key, sliced[0].group.unwrap().key);
    let mut seeds = whole.iter().map(|event| event.seed()).collect::<Vec<_>>();
    seeds.sort_unstable();
    seeds.dedup();
    assert_eq!(seeds.len(), 3);
}

#[test]
fn degrade_preserves_original_group_coordinates_when_it_makes_holes() {
    let group = mini::parse("[a,c,e,g]").unwrap();
    let mut found = None;
    for seed in 0..100 {
        let events = group.clone().degrade_by(0.5, seed).onsets(Span::cycle(0));
        if !events.is_empty() && events.len() < 4 {
            found = Some(events);
            break;
        }
    }
    let events = found.expect("some deterministic seed should make a partial group");
    assert!(events.iter().all(|event| event.group.unwrap().count == 4));
    let indices = events
        .iter()
        .map(|event| event.group.unwrap().index)
        .collect::<Vec<_>>();
    assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn identical_mini_strings_at_different_bindings_have_distinct_provenance() {
    let first = parse_at("c4", 10).unwrap().onsets(Span::cycle(0));
    let second = parse_at("c4", 11).unwrap().onsets(Span::cycle(0));
    assert_ne!(first[0].seed(), second[0].seed());
    assert_eq!(
        first[0].seed(),
        parse_at("c4", 10).unwrap().onsets(Span::cycle(0))[0].seed()
    );
}
