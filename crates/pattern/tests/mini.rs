//! Mini-notation: what the text means.

use apteronotus_pattern::{Frac, Pattern, Span, mini};

fn f(n: i64, d: i64) -> Frac {
    Frac::new(n, d)
}

/// Onsets in one cycle as `(begin, end, value)`, sorted by time.
fn hits(src: &str, cycle: i64) -> Vec<(Frac, Frac, String)> {
    let p = mini::parse(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut v: Vec<_> = p
        .onsets(Span::cycle(cycle))
        .into_iter()
        .map(|e| {
            let w = e.whole.expect("an onset always has a whole");
            (w.begin, w.end, e.value.to_string())
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
    v
}

fn times(src: &str, cycle: i64) -> Vec<Frac> {
    hits(src, cycle).into_iter().map(|(b, _, _)| b).collect()
}

fn words(src: &str, cycle: i64) -> Vec<String> {
    hits(src, cycle).into_iter().map(|(_, _, v)| v).collect()
}

#[test]
fn a_bare_word_fills_the_cycle() {
    assert_eq!(hits("bd", 0), vec![(f(0, 1), f(1, 1), "bd".into())]);
}

#[test]
fn a_sequence_divides_the_cycle() {
    assert_eq!(times("bd sd hh", 0), vec![f(0, 1), f(1, 3), f(2, 3)]);
    // ...and thirds stay exact, which is the whole reason time is rational.
    assert_eq!(hits("bd sd hh", 0)[2].1, f(1, 1));
}

#[test]
fn rests_leave_holes() {
    assert_eq!(words("bd ~ sd ~", 0), vec!["bd", "sd"]);
    assert_eq!(times("bd ~ sd ~", 0), vec![f(0, 1), f(1, 2)]);
}

#[test]
fn brackets_nest_as_one_step() {
    assert_eq!(times("bd [sd sd]", 0), vec![f(0, 1), f(1, 2), f(3, 4)]);
    assert_eq!(times("bd [~ sd]", 0), vec![f(0, 1), f(3, 4)]);
}

#[test]
fn commas_stack() {
    let mut w = words("bd sd, hh hh hh", 0);
    w.sort();
    assert_eq!(w, vec!["bd", "hh", "hh", "hh", "sd"]);
}

#[test]
fn angle_brackets_alternate_across_cycles() {
    assert_eq!(words("<a b c>", 0), vec!["a"]);
    assert_eq!(words("<a b c>", 1), vec!["b"]);
    assert_eq!(words("<a b c>", 2), vec!["c"]);
    assert_eq!(words("<a b c>", 3), vec!["a"]);
}

#[test]
fn alternation_advances_per_outer_cycle_not_per_slot() {
    // The classic trap: `<a b>` must step once per bar, not once per slot.
    assert_eq!(words("<a b> c", 0), vec!["a", "c"]);
    assert_eq!(words("<a b> c", 1), vec!["b", "c"]);
}

#[test]
fn star_and_slash_scale_time() {
    assert_eq!(times("bd*4", 0), vec![f(0, 1), f(1, 4), f(1, 2), f(3, 4)]);
    // A slowed subsequence spans two cycles, one step in each.
    assert_eq!(words("[bd sd]/2", 0), vec!["bd"]);
    assert_eq!(words("[bd sd]/2", 1), vec!["sd"]);
}

#[test]
fn fractional_speed_stays_exact() {
    // *1.5 is three halves, not 1.4999999.
    let t = times("bd*1.5", 0);
    assert_eq!(t, vec![f(0, 1), f(2, 3)]);
}

#[test]
fn bang_repeats() {
    assert_eq!(times("bd!3", 0), vec![f(0, 1), f(1, 3), f(2, 3)]);
    // A bare `!` repeats whatever preceded it.
    assert_eq!(words("bd ! sd", 0), vec!["bd", "bd", "sd"]);
}

#[test]
fn at_gives_a_step_more_room() {
    assert_eq!(times("bd@3 sd", 0), vec![f(0, 1), f(3, 4)]);
    assert_eq!(hits("bd@3 sd", 0)[0].1, f(3, 4));
}

#[test]
fn euclid_spreads_onsets() {
    // E(3,8) is x..x..x.
    assert_eq!(times("bd(3,8)", 0), vec![f(0, 1), f(3, 8), f(6, 8)]);
    // E(5,8) is x.xx.xx.
    assert_eq!(times("bd(5,8)", 0).len(), 5);
    // Rotation shifts which step comes first.
    assert_ne!(times("bd(3,8,1)", 0), times("bd(3,8)", 0));
}

#[test]
fn question_mark_degrades_reproducibly() {
    let a = words("bd*16?", 0);
    let b = words("bd*16?", 0);
    assert_eq!(a, b, "the same text must always sound the same");
    assert!(
        a.len() < 16 && !a.is_empty(),
        "dropped {} of 16",
        16 - a.len()
    );

    // Two degrades in one string must not fall on the same events, or the
    // second layer would be a copy of the first.
    let p = mini::parse("bd*16? , sd*16?").unwrap();
    let onsets = p.onsets(Span::cycle(0));
    let bd: Vec<_> = onsets
        .iter()
        .filter(|e| e.value.to_string() == "bd")
        .collect();
    let sd: Vec<_> = onsets
        .iter()
        .filter(|e| e.value.to_string() == "sd")
        .collect();
    let bd_at: Vec<_> = bd.iter().map(|e| e.part.begin).collect();
    let sd_at: Vec<_> = sd.iter().map(|e| e.part.begin).collect();
    assert_ne!(bd_at, sd_at, "both `?` chose identically");
}

#[test]
fn degrade_amount_is_honoured() {
    let few = words("bd*64?0.9", 0).len();
    let many = words("bd*64?0.1", 0).len();
    assert!(few < many, "?0.9 kept {few}, ?0.1 kept {many}");
}

#[test]
fn numbers_parse_as_numbers() {
    let p = mini::parse("400 1200").unwrap();
    let vs: Vec<f64> = p
        .onsets(Span::cycle(0))
        .into_iter()
        .filter_map(|e| e.value.as_f64())
        .collect();
    assert_eq!(vs, vec![400.0, 1200.0]);
}

#[test]
fn note_names_survive_intact() {
    assert_eq!(
        words("c#4 fs2 hh'closed bd:3", 0),
        vec!["c#4", "fs2", "hh'closed", "bd:3"]
    );
}

#[test]
fn source_spans_point_at_the_text() {
    let src = "bd sd";
    let p = mini::parse(src).unwrap();
    let evs = p.onsets(Span::cycle(0));
    let mut spans: Vec<_> = evs.iter().map(|e| e.src.expect("no span")).collect();
    spans.sort_by_key(|s| s.start);
    assert_eq!(&src[spans[0].start as usize..spans[0].end as usize], "bd");
    assert_eq!(&src[spans[1].start as usize..spans[1].end as usize], "sd");
}

#[test]
fn spans_survive_transformation() {
    // Highlighting has to keep working through the combinators, or the editor
    // lights up the wrong word the moment anyone writes `every`.
    let src = "bd sd";
    let p = mini::parse(src).unwrap().fast(Frac::int(3)).rev();
    for e in p.onsets(Span::cycle(2)) {
        let s = e.src.expect("span lost in transformation");
        assert!(matches!(
            &src[s.start as usize..s.end as usize],
            "bd" | "sd"
        ));
    }
}

#[test]
fn combined_modifiers_compose() {
    let t = times("bd*2 [~ sd]", 0);
    assert_eq!(t, vec![f(0, 1), f(1, 4), f(3, 4)]);
}

#[test]
fn empty_input_is_silence() {
    assert_eq!(mini::parse("").unwrap(), Pattern::Silence);
    assert_eq!(mini::parse("   ").unwrap(), Pattern::Silence);
}

#[test]
fn errors_carry_a_position() {
    for bad in ["[bd", "<bd", "bd]", "bd(3", "bd*", "bd*0"] {
        let e = mini::parse(bad).unwrap_err();
        assert!(
            e.span.start as usize <= bad.len(),
            "{bad:?} reported byte {}",
            e.span.start
        );
        assert!(!e.message.is_empty(), "{bad:?} gave an empty message");
    }
}

#[test]
fn supersaws_score_notation_remains_parseable() {
    let song = include_str!("../../../songs/supersaws.eod");
    for source in [
        "g#2@3 g#2@2 g#2!4 g#2@2 g#2",
        "[0.08 0.08 0.14]*16/3",
        "200@3 200@2 300!4 1000@2 200",
    ] {
        assert!(
            song.contains(source),
            "the guard must follow the notation actually shipped in supersaws.eod"
        );
        mini::parse(source).unwrap_or_else(|error| panic!("{source:?}: {error}"));
    }
}

// ---------------------------------------------------------------- robustness

/// Every diagnostic must be sliceable. An editor paints these ranges, and a
/// range past the end or through the middle of a multibyte character panics.
#[test]
fn error_spans_are_always_valid_ranges() {
    let bad = [
        "[bd",
        "<bd",
        "bd]",
        "bd(3",
        "bd*",
        "bd*0",
        "bd!0",
        "«",
        "bd «",
        "é(",
        "bd*99999",
        "bd!99999",
        "bd(3,99999)",
        "~*",
        "bd@",
        "bd?9",
        "",
    ];
    for src in bad {
        let Err(e) = mini::parse(src) else { continue };
        let (s, t) = (e.span.start as usize, e.span.end as usize);
        assert!(s <= t, "{src:?}: inverted span {s}..{t}");
        assert!(
            t <= src.len(),
            "{src:?}: span {s}..{t} past end {}",
            src.len()
        );
        assert!(src.is_char_boundary(s), "{src:?}: {s} splits a character");
        assert!(src.is_char_boundary(t), "{src:?}: {t} splits a character");
        let _ = &src[s..t]; // must not panic
    }
}

/// Live text arrives on every keystroke while audio is running. Four
/// characters must not be able to hang or exhaust the process.
#[test]
fn absurd_expansions_are_diagnostics_not_hangs() {
    for src in [
        "bd!99999999",
        "bd(3,999999999)",
        "bd*99999999999999999999",
        "bd*1000000 sd*1000000",
        "bd@99999999",
        "[[bd*32]*32]*32",
        "[bd!512]*512",
    ] {
        let r = mini::parse(src);
        assert!(r.is_err(), "{src:?} was accepted");
    }
}

#[test]
fn deep_nesting_is_rejected_before_the_stack_runs_out() {
    let deep = format!("{}bd{}", "[".repeat(5000), "]".repeat(5000));
    assert!(mini::parse(&deep).is_err());
}

#[test]
fn generous_but_musical_input_still_parses() {
    // The limits must sit far above anything anyone would write.
    for src in [
        "bd!64",
        "bd(7,16)",
        "bd*32",
        "hh*16 , bd*8 , sd(5,16)",
        "bd@64 sd",
    ] {
        assert!(mini::parse(src).is_ok(), "{src:?} was rejected");
    }
}
