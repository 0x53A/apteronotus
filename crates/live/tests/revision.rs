use apteronotus_live::{Generation, RevisionSlot, SubmitError};
use apteronotus_pattern::Frac;
use std::sync::Arc;

#[test]
fn failed_validation_leaves_the_playing_program_and_queue_untouched() {
    let mut slot = RevisionSlot::new("old", Frac::ZERO);
    let old = Arc::clone(&slot.active().program);

    let result = slot.submit("broken", Frac::ONE, |_| Err("bad graph"));
    assert_eq!(result, Err(SubmitError::Validation("bad graph")));
    assert_eq!(slot.active().generation, Generation::INITIAL);
    assert_eq!(*slot.active().program, "old");
    assert_eq!(slot.pending().len(), 0);
    assert!(Arc::ptr_eq(&old, &slot.active().program));
}

#[test]
fn a_revision_activates_only_when_its_exact_boundary_is_reached() {
    let mut slot = RevisionSlot::new("old", Frac::ZERO);
    let generation = slot
        .submit("new", Frac::new(3, 2), |_| Ok::<_, ()>(()))
        .unwrap();
    assert_eq!(generation.get(), 1);

    assert_eq!(*slot.advance_to(Frac::ONE).program, "old");
    let active = slot.advance_to(Frac::new(3, 2));
    assert_eq!(*active.program, "new");
    assert_eq!(active.generation, generation);
}

#[test]
fn old_revision_arcs_survive_activation_for_sounding_voices() {
    let mut slot = RevisionSlot::new(String::from("old voice"), Frac::ZERO);
    let sounding_voice = Arc::clone(&slot.active().program);
    slot.submit(String::from("new voice"), Frac::ONE, |_| Ok::<_, ()>(()))
        .unwrap();
    slot.advance_to(Frac::ONE);

    assert_eq!(sounding_voice.as_str(), "old voice");
    assert_eq!(slot.active().program.as_str(), "new voice");
}

#[test]
fn queued_generations_are_monotonic_and_cannot_go_back_in_time() {
    let mut slot = RevisionSlot::new(0, Frac::ZERO);
    let one = slot.submit(1, Frac::ONE, |_| Ok::<_, ()>(())).unwrap();
    let two = slot.submit(2, Frac::int(2), |_| Ok::<_, ()>(())).unwrap();
    assert!(one < two);

    assert!(matches!(
        slot.submit(3, Frac::new(3, 2), |_| Ok::<_, ()>(())),
        Err(SubmitError::OutOfOrder {
            effective_at,
            not_before
        }) if effective_at == Frac::new(3, 2) && not_before == Frac::int(2)
    ));
    assert_eq!(slot.pending().len(), 2);
}
