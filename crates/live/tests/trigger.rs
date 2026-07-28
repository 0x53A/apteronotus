use apteronotus_live::{ExternalTrigger, TriggerRecorder};
use apteronotus_pattern::{Frac, Pattern, Span, TimelineId, Value};

#[test]
fn a_live_trigger_is_not_a_pattern_until_recorded() {
    let mut recorder = TriggerRecorder::new();
    recorder
        .record(ExternalTrigger::new(
            Frac::new(1, 4),
            Frac::new(1, 8),
            Value::S("hit".into()),
        ))
        .unwrap();
    let timeline = recorder.finish(TimelineId::new(4), Span::cycle(0)).unwrap();
    let events = Pattern::timeline(timeline).onsets(Span::cycle(0));

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].whole.unwrap().begin, Frac::new(1, 4));
}

#[test]
fn simultaneous_arrivals_keep_captured_ordinals_and_distinct_seeds() {
    let mut recorder = TriggerRecorder::new();
    for value in ["a", "b"] {
        recorder
            .record(ExternalTrigger::new(
                Frac::new(1, 3),
                Frac::new(1, 6),
                Value::S(value.into()),
            ))
            .unwrap();
    }
    let timeline = recorder.finish(TimelineId::new(5), Span::cycle(0)).unwrap();
    assert_eq!(timeline.events()[0].ordinal, 0);
    assert_eq!(timeline.events()[1].ordinal, 1);

    let events = Pattern::timeline(timeline).onsets(Span::cycle(0));
    assert_ne!(events[0].seed(), events[1].seed());
}
