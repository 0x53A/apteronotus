//! Query semantics, and the invariants the scheduler will lean on.

use apteronotus_pattern::{Frac, Pattern, Signal, Span, mini};

fn f(n: i64, d: i64) -> Frac {
    Frac::new(n, d)
}

fn words(p: &Pattern, cycle: i64) -> Vec<String> {
    let mut v: Vec<_> = p
        .onsets(Span::cycle(cycle))
        .into_iter()
        .map(|e| (e.part.begin, e.value.to_string()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, s)| s).collect()
}

// ---------------------------------------------------------------- invariants

#[test]
fn querying_twice_gives_the_same_answer() {
    // The scheduler runs ahead of the audio clock; the editor asks about the
    // same instant again to know what to highlight. If these disagreed, the
    // highlight would drift away from the sound.
    let p = mini::parse("bd*8? [sd cp]  , <a b>*3").unwrap();
    let span = Span::new(f(7, 3), f(11, 2));
    assert_eq!(p.query(span), p.query(span));
}

#[test]
fn splitting_a_window_does_not_change_the_onsets() {
    // Buffer boundaries are an implementation detail of the audio callback and
    // must never be audible.
    let p = mini::parse("bd*7? sd(3,8) , <hh cp>*5").unwrap();
    let whole = Span::new(f(0, 1), f(4, 1));

    let all: Vec<_> = p
        .onsets(whole)
        .into_iter()
        .map(|e| (e.part.begin, e.value.to_string()))
        .collect();

    let mut piecewise = Vec::new();
    let mut t = whole.begin;
    let step = f(1, 7); // deliberately not aligned to anything
    while t < whole.end {
        let next = (t + step).min(whole.end);
        piecewise.extend(
            p.onsets(Span::new(t, next))
                .into_iter()
                .map(|e| (e.part.begin, e.value.to_string())),
        );
        t = next;
    }

    let mut a = all;
    let mut b = piecewise;
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

#[test]
fn a_bisected_note_is_continued_not_restruck() {
    let p = mini::parse("bd").unwrap();
    let second_half = p.query(Span::new(f(1, 2), f(1, 1)));
    assert_eq!(second_half.len(), 1);
    assert!(!second_half[0].has_onset(), "a mid-note query retriggered");
    assert_eq!(second_half[0].whole.unwrap(), Span::cycle(0));
    assert_eq!(second_half[0].part, Span::new(f(1, 2), f(1, 1)));
}

#[test]
fn a_zero_width_query_finds_what_is_sounding() {
    let p = mini::parse("bd sd").unwrap();
    let now = Span::new(f(1, 4), f(1, 4));
    let evs = p.query(now);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].value.to_string(), "bd");
}

#[test]
fn degrade_decides_the_same_way_from_any_window() {
    // The choice hangs on the event's own position, not the query's, so a
    // window that clips a note must not resurrect it.
    let p = mini::parse("bd*16?").unwrap();
    let full: Vec<_> = p
        .onsets(Span::cycle(0))
        .into_iter()
        .map(|e| e.part.begin)
        .collect();
    let mut split = Vec::new();
    for k in 0..3 {
        let span = Span::new(f(k, 3), f(k + 1, 3));
        split.extend(p.onsets(span).into_iter().map(|e| e.part.begin));
    }
    assert_eq!(full, split);
}

// ---------------------------------------------------------------- combinators

#[test]
fn fast_and_slow_are_inverse() {
    let p = mini::parse("bd sd hh").unwrap();
    let there_and_back = p.clone().fast(f(3, 2)).slow(f(3, 2));
    for c in 0..4 {
        assert_eq!(words(&there_and_back, c), words(&p, c));
    }
}

#[test]
fn rev_mirrors_within_the_cycle() {
    let p = mini::parse("a b c d").unwrap().rev();
    assert_eq!(words(&p, 0), vec!["d", "c", "b", "a"]);
    let times: Vec<_> = p
        .onsets(Span::cycle(0))
        .into_iter()
        .map(|e| e.part.begin)
        .collect();
    let mut times = times;
    times.sort();
    assert_eq!(times, vec![f(0, 1), f(1, 4), f(1, 2), f(3, 4)]);
}

#[test]
fn rev_is_an_involution() {
    let p = mini::parse("a b [c d]").unwrap();
    assert_eq!(words(&p.clone().rev().rev(), 0), words(&p, 0));
}

#[test]
fn every_picks_its_cycles() {
    let p = mini::parse("a b").unwrap().every(3, Pattern::rev);
    assert_eq!(words(&p, 0), vec!["b", "a"]);
    assert_eq!(words(&p, 1), vec!["a", "b"]);
    assert_eq!(words(&p, 2), vec!["a", "b"]);
    assert_eq!(words(&p, 3), vec!["b", "a"]);
}

#[test]
fn shifting_moves_events_in_time() {
    let p = mini::parse("a b").unwrap().late(f(1, 4));
    let t: Vec<_> = p
        .onsets(Span::cycle(0))
        .into_iter()
        .map(|e| e.part.begin)
        .collect();
    let mut t = t;
    t.sort();
    assert_eq!(t, vec![f(1, 4), f(3, 4)]);
}

#[test]
fn off_overlays_a_delayed_copy() {
    let p = mini::parse("bd").unwrap().off(f(1, 4), |q| q);
    let t: Vec<_> = p
        .onsets(Span::cycle(0))
        .into_iter()
        .map(|e| e.part.begin)
        .collect();
    let mut t = t;
    t.sort();
    assert_eq!(t, vec![f(0, 1), f(1, 4)]);
}

#[test]
fn sometimes_by_partitions_rather_than_duplicating() {
    // Every event must land in exactly one of the two branches: the touched
    // set and the untouched set are complements, not overlapping samples.
    let base = mini::parse("bd*32").unwrap();
    let marked = base.clone().sometimes_by(0.5, 9, |q| q.late(f(1, 128)));
    assert_eq!(
        marked.onsets(Span::cycle(0)).len(),
        base.onsets(Span::cycle(0)).len()
    );
}

#[test]
fn stack_keeps_every_layer() {
    let p = Pattern::stack(vec![
        mini::parse("bd*2").unwrap(),
        mini::parse("hh*3").unwrap(),
    ]);
    assert_eq!(p.onsets(Span::cycle(0)).len(), 5);
}

// ---------------------------------------------------------------- continuous

#[test]
fn a_signal_has_no_onset() {
    let p = Pattern::signal(Signal::Sine);
    let evs = p.query(Span::cycle(0));
    assert_eq!(evs.len(), 1);
    assert!(evs[0].whole.is_none());
    assert!(
        !evs[0].has_onset(),
        "a continuous signal would trigger a voice"
    );
}

#[test]
fn signals_are_sampled_at_the_midpoint() {
    let saw = Pattern::signal(Signal::Saw);
    let v = saw.query(Span::new(f(0, 1), f(1, 2)))[0]
        .value
        .as_f64()
        .unwrap();
    assert!((v - 0.25).abs() < 1e-12, "got {v}");

    let sine = Pattern::signal(Signal::Sine);
    let v = sine.query(Span::cycle(0))[0].value.as_f64().unwrap();
    assert!((v - 0.5).abs() < 1e-12, "got {v}");
}

#[test]
fn signals_stay_in_the_unit_interval() {
    for sig in [
        Signal::Sine,
        Signal::Cosine,
        Signal::Saw,
        Signal::Isaw,
        Signal::Tri,
        Signal::Square,
        Signal::Rand(3),
        Signal::Perlin(3),
    ] {
        for k in 0..200 {
            let t = f(k, 37);
            let v = sig.at(t);
            assert!((0.0..=1.0).contains(&v), "{sig:?} gave {v} at {t}");
        }
    }
}

#[test]
fn perlin_is_continuous_at_cycle_joins() {
    let before = Signal::Perlin(5).at(f(999_999, 1_000_000));
    let after = Signal::Perlin(5).at(f(1_000_001, 1_000_000));
    assert!((before - after).abs() < 0.02, "{before} -> {after}");
}

#[test]
fn segment_gives_a_signal_onsets() {
    let p = Pattern::signal(Signal::Saw).segment(4);
    let evs = p.onsets(Span::cycle(0));
    assert_eq!(evs.len(), 4);
    let mut vs: Vec<f64> = evs.iter().map(|e| e.value.as_f64().unwrap()).collect();
    vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for (got, want) in vs.iter().zip([0.125, 0.375, 0.625, 0.875]) {
        assert!((got - want).abs() < 1e-12, "{got} != {want}");
    }
}

#[test]
fn range_maps_onto_an_interval() {
    let p = Pattern::signal(Signal::Saw).segment(2).range(100.0, 200.0);
    let mut vs: Vec<f64> = p
        .onsets(Span::cycle(0))
        .into_iter()
        .map(|e| e.value.as_f64().unwrap())
        .collect();
    vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!((vs[0] - 125.0).abs() < 1e-12, "{vs:?}");
    assert!((vs[1] - 175.0).abs() < 1e-12, "{vs:?}");
}

// ---------------------------------------------------------------- euclid

#[test]
fn bjorklund_matches_the_known_rhythms() {
    use apteronotus_pattern::bjorklund;
    let s = |v: Vec<bool>| {
        v.iter()
            .map(|b| if *b { 'x' } else { '.' })
            .collect::<String>()
    };
    assert_eq!(s(bjorklund(3, 8)), "x..x..x.");
    assert_eq!(s(bjorklund(2, 5)), "x.x..");
    assert_eq!(s(bjorklund(5, 8)), "x.xx.xx.");
    assert_eq!(s(bjorklund(4, 4)), "xxxx");
    assert_eq!(s(bjorklund(0, 4)), "....");
    assert_eq!(bjorklund(3, 0).len(), 0);
}

#[test]
fn negative_cycles_behave() {
    // The transport can be scrubbed backwards; nothing may panic or wrap.
    let p = mini::parse("<a b c> d").unwrap();
    assert_eq!(words(&p, -1), words(&p, 2));
    assert_eq!(words(&p, -3), words(&p, 0));
}

// ---------------------------------------------------------------- robustness

#[test]
fn overlapping_windows_repeat_onsets_by_design() {
    // Not a bug to fix here: two identical simultaneous onsets are legal, so
    // this level cannot tell an intended pair from a re-query. The contract is
    // that *adjacent* windows tile exactly, and the scheduler owes a
    // monotonic frontier. Pinned so nobody "fixes" it with a dedup.
    let p = mini::parse("bd sd").unwrap();
    let a = p.onsets(Span::new(f(0, 1), f(3, 4))).len();
    let b = p.onsets(Span::new(f(1, 2), f(1, 1))).len();
    assert_eq!((a, b), (2, 1), "onset at 1/2 is in both windows");

    let tiled =
        p.onsets(Span::new(f(0, 1), f(1, 2))).len() + p.onsets(Span::new(f(1, 2), f(1, 1))).len();
    assert_eq!(tiled, p.onsets(Span::cycle(0)).len());
}

#[test]
fn a_stack_of_a_line_on_itself_keeps_both_onsets() {
    // The reason `onsets` must not deduplicate.
    let one = mini::parse("bd").unwrap();
    let two = Pattern::stack(vec![one.clone(), one]);
    assert_eq!(two.onsets(Span::cycle(0)).len(), 2);
}
