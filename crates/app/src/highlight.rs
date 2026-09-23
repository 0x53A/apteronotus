//! A purely lexical Lua colouriser for the editor.
//!
//! This is presentation only. It never parses, never reports, and never
//! decides whether a document is valid — evaluation remains the single
//! explicit boundary at which a program becomes real. Its one job is to make
//! mini-notation strings and the Apteronotus vocabulary visible while typing.

use crate::style;
use eframe::egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat};
use eframe::egui::{Color32, FontId};
use std::ops::Range;

/// Lua's own reserved words.
const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// The sandbox vocabulary: what a document is here to call.
///
/// This mirrors `bindings::install` and the Lua prelude. It is deliberately
/// only what actually exists — colouring a name the sandbox does not bind
/// would promise a function that is not there.
const BUILTINS: &[&str] = &[
    // program structure
    "voice",
    "patch",
    "control",
    "control_input",
    "audio_input",
    "bus",
    "send",
    "run",
    "master",
    "play",
    "tempo",
    "key",
    "timeline",
    "at",
    "span",
    "pattern",
    "note",
    "chord",
    "anchor",
    "voicing",
    "root_notes",
    "octave",
    "hold",
    "velocity",
    "control_signal",
    "at_onset",
    "curve",
    "phase",
    // pattern transforms
    "fast",
    "slow",
    "shift",
    "late",
    "early",
    "rev",
    "segment",
    "range",
    "degrade",
    "every",
    "off",
    "sometimes",
    "ply",
    "arp",
    "duck",
    // sources
    "sine",
    "cosine",
    "saw",
    "soft_saw",
    "perlin",
    "pulse",
    "triangle",
    "noise",
    "pink",
    "impulse",
    "rand",
    "dc",
    "zero",
    "init_random",
    "init_rand",
    // processing
    "lowpass",
    "highpass",
    "bandpass",
    "peak",
    "moog",
    "pluck",
    "string_resonator",
    "harmonics",
    "organ_pipe",
    "flue_pipe",
    "organ",
    "shape",
    "dcblock",
    "delay",
    "predelay",
    "diffuse",
    "fdn",
    "param",
    "reverb",
    "limiter",
    "chorus",
    "ensemble",
    "envelope_follower",
    "pitch_tracker",
    "onset_detector",
    "feedback",
    "slew",
    "gate_env",
    "ring",
    "ringmod",
    "tremolo",
    "width",
    "pan",
    "mix",
    "scale",
    "exp2",
    "semitones",
    "note_hz",
    "window",
    "to",
    "add",
    "sub",
    "mul",
    "div",
    "clamp",
    "neg",
    // envelopes and the curve basis
    "adsr",
    "step",
    "ramp",
    "line",
    "decay",
    // time
    "secs",
    "ms",
    "bars",
    "beats",
    // the Lua subset the sandbox keeps
    "math",
    "table",
    "string",
    "coroutine",
    "ipairs",
    "pairs",
    "next",
    "tostring",
    "tonumber",
    "select",
    "type",
    "error",
    "pcall",
    "assert",
    "setmetatable",
    "getmetatable",
    "rawget",
    "rawset",
    "rawequal",
    "rawlen",
];

/// Build a colourised, unwrapped layout job for `source`.
pub fn layout_for_revision(
    source: &str,
    font: FontId,
    audible_source: Option<&str>,
    active: &[Range<usize>],
) -> LayoutJob {
    // TextEdit can invoke its layouter after an edit within this same frame.
    // Spans computed at frame start are valid only for their exact document.
    layout(
        source,
        font,
        if audible_source == Some(source) {
            active
        } else {
            &[]
        },
    )
}

/// Keep Find visible while its own field has focus. Reject stale ranges even
/// when TextEdit asks for a new layout during the frame that changed the text.
pub fn mark_search(job: &mut LayoutJob, matched: Option<&(&str, Range<usize>)>) {
    let Some((source, matched)) = matched else {
        return;
    };
    if job.text != *source || source.get(matched.clone()).is_none() || matched.is_empty() {
        return;
    }
    let mut sections = Vec::with_capacity(job.sections.len() + 2);
    for section in std::mem::take(&mut job.sections) {
        let start = section.byte_range.start.0;
        let end = section.byte_range.end.0;
        let mut cuts = vec![start, end];
        if start < matched.start && matched.start < end {
            cuts.push(matched.start);
        }
        if start < matched.end && matched.end < end {
            cuts.push(matched.end);
        }
        cuts.sort_unstable();
        for cut in cuts.windows(2) {
            let mut part = section.clone();
            part.byte_range = ByteIndex(cut[0])..ByteIndex(cut[1]);
            if cut[0] < matched.end && matched.start < cut[1] {
                part.format.background = style::SEARCH_WASH;
                part.format.underline = eframe::egui::Stroke::new(1.0, style::CAUTION);
            }
            sections.push(part);
        }
    }
    job.sections = sections;
}

pub fn layout(source: &str, font: FontId, active: &[Range<usize>]) -> LayoutJob {
    let mut job = LayoutJob {
        text: source.to_owned(),
        ..Default::default()
    };
    // A code editor scrolls sideways; it does not reflow. Wrapping would also
    // desynchronise the line-number gutter, which counts newlines.
    job.wrap.max_width = f32::INFINITY;
    for (range, color) in spans(source) {
        let mut boundaries = vec![range.start, range.end];
        for sounding in active {
            if sounding.start < range.end && range.start < sounding.end {
                boundaries.push(sounding.start.max(range.start));
                boundaries.push(sounding.end.min(range.end));
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        for bounds in boundaries.windows(2) {
            let section = bounds[0]..bounds[1];
            let sounding = active
                .iter()
                .any(|active| active.start < section.end && section.start < active.end);
            let mut format =
                TextFormat::simple(font.clone(), if sounding { style::BRIGHT } else { color });
            if sounding {
                format.background = style::DISCHARGE_WASH;
            }
            job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(section.start)..ByteIndex(section.end),
                format,
            });
        }
    }
    job
}

fn spans(source: &str) -> Vec<(Range<usize>, Color32)> {
    let bytes = source.as_bytes();
    let mut spans = Vec::new();
    let mut at = 0;

    while at < bytes.len() {
        let start = at;
        let color = match bytes[at] {
            b'-' if bytes.get(at + 1) == Some(&b'-') => {
                at += 2;
                match long_bracket(bytes, at) {
                    Some(end) => at = end,
                    None => at = line_end(bytes, at),
                }
                style::CODE_COMMENT
            }
            b'"' | b'\'' => {
                at = quoted(bytes, at);
                style::CODE_STRING
            }
            b'[' if long_bracket(bytes, at).is_some() => {
                at = long_bracket(bytes, at).expect("checked above");
                style::CODE_STRING
            }
            b'0'..=b'9' => {
                at = number(bytes, at);
                style::CODE_NUMBER
            }
            b'.' if matches!(bytes.get(at + 1), Some(b'0'..=b'9')) => {
                at = number(bytes, at);
                style::CODE_NUMBER
            }
            byte if is_word_start(byte) => {
                while at < bytes.len() && is_word(bytes[at]) {
                    at += 1;
                }
                let word = &source[start..at];
                if KEYWORDS.contains(&word) {
                    style::CODE_KEYWORD
                } else if BUILTINS.contains(&word) {
                    style::CODE_BUILTIN
                } else {
                    style::TEXT
                }
            }
            byte if byte.is_ascii_whitespace() => {
                while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                style::TEXT
            }
            _ => {
                // Advance by a whole character so a multi-byte glyph is never
                // split across two sections.
                at += char_width(bytes[at]);
                style::CODE_PUNCT
            }
        };
        spans.push((start..at, color));
    }

    spans
}

fn char_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn line_end(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && bytes[at] != b'\n' {
        at += 1;
    }
    at
}

fn number(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len()
        && (bytes[at].is_ascii_alphanumeric()
            || bytes[at] == b'.'
            || ((bytes[at] == b'+' || bytes[at] == b'-')
                && matches!(bytes[at - 1], b'e' | b'E' | b'p' | b'P')))
    {
        at += 1;
    }
    at
}

fn quoted(bytes: &[u8], mut at: usize) -> usize {
    let quote = bytes[at];
    at += 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'\n' => return at,
            byte if byte == quote => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// If a `[=*[` long bracket opens at `at`, return the index just past its close.
fn long_bracket(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'[') {
        return None;
    }
    let mut level = 0;
    let mut cursor = at + 1;
    while bytes.get(cursor) == Some(&b'=') {
        level += 1;
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'[') {
        return None;
    }
    cursor += 1;

    while cursor < bytes.len() {
        if bytes[cursor] == b']' {
            let mut closing = 0;
            let mut probe = cursor + 1;
            while bytes.get(probe) == Some(&b'=') {
                closing += 1;
                probe += 1;
            }
            if closing == level && bytes.get(probe) == Some(&b']') {
                return Some(probe + 1);
            }
        }
        cursor += 1;
    }
    Some(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::{layout, layout_for_revision, mark_search, spans, style};
    use eframe::egui::FontId;

    fn colored(source: &str, needle: &str) -> Vec<eframe::egui::Color32> {
        let start = source.find(needle).expect("needle is in the source");
        let range = start..start + needle.len();
        spans(source)
            .into_iter()
            .filter(|(span, _)| span.start < range.end && range.start < span.end)
            .map(|(_, color)| color)
            .collect()
    }

    #[test]
    fn spans_cover_the_source_exactly_once() {
        let source = "local v = voice { graph = function(n) return sine(n.hz) end }\n-- x\n";
        let mut at = 0;
        for (span, _) in spans(source) {
            assert_eq!(span.start, at, "spans must be contiguous");
            assert!(span.end > span.start, "spans must advance");
            at = span.end;
        }
        assert_eq!(at, source.len());
    }

    #[test]
    fn find_marks_exact_unicode_across_lexical_sections_and_rejects_stale_spans() {
        let source = "-- 水\nlocal x = 1";
        let start = source.find('水').unwrap();
        let end = source.find(" x").unwrap();
        let matched = (source, start..end);
        let mut job = layout(source, FontId::monospace(14.0), &[]);
        mark_search(&mut job, Some(&matched));
        let selected: String = job
            .sections
            .iter()
            .filter(|section| section.format.background == style::SEARCH_WASH)
            .map(|section| &source[section.byte_range.start.0..section.byte_range.end.0])
            .collect();
        assert_eq!(selected, "水\nlocal");
        let changed = "-- 🌊\nlocal x = 1";
        let mut job = layout(changed, FontId::monospace(14.0), &[]);
        mark_search(&mut job, Some(&matched));
        assert!(
            job.sections
                .iter()
                .all(|section| section.format.background != style::SEARCH_WASH)
        );
        for section in job.sections {
            assert!(changed.is_char_boundary(section.byte_range.start.0));
            assert!(changed.is_char_boundary(section.byte_range.end.0));
        }
    }

    #[test]
    fn mini_notation_strings_are_one_span() {
        assert_eq!(
            colored("play(v, \"c4 e4 g4\")", "\"c4 e4 g4\""),
            vec![style::CODE_STRING]
        );
    }

    #[test]
    fn keywords_builtins_and_names_are_distinguished() {
        let source = "local tone = sine(220)";
        assert_eq!(colored(source, "local"), vec![style::CODE_KEYWORD]);
        assert_eq!(colored(source, "sine"), vec![style::CODE_BUILTIN]);
        assert_eq!(colored(source, "tone"), vec![style::TEXT]);
        assert_eq!(colored(source, "220"), vec![style::CODE_NUMBER]);
    }

    #[test]
    fn comments_run_to_the_end_of_their_line_and_no_further() {
        let source = "-- sine(1)\nsine(1)";
        assert_eq!(colored(source, "-- sine(1)"), vec![style::CODE_COMMENT]);
        assert_eq!(colored(&source[11..], "sine"), vec![style::CODE_BUILTIN]);
    }

    #[test]
    fn long_brackets_close_at_their_own_level() {
        let source = "local s = [==[ ]] still ]==] .. 'x'";
        assert_eq!(
            colored(source, "[==[ ]] still ]==]"),
            vec![style::CODE_STRING]
        );
    }

    #[test]
    fn non_ascii_text_is_never_split_mid_character() {
        // The span boundaries must stay on character boundaries or slicing the
        // source for a layout section would panic.
        let source = "local x = 1 -- ♪\n§ y";
        for (span, _) in spans(source) {
            assert!(source.is_char_boundary(span.start));
            assert!(source.is_char_boundary(span.end));
        }
    }

    #[test]
    fn active_ranges_preserve_contiguous_char_safe_layout_sections() {
        let source = "local x = \"café bd\" -- ♪\n";
        let start = source.find("café").unwrap();
        let active = std::iter::once(start..start + "café".len()).collect::<Vec<_>>();
        let job = layout(source, FontId::monospace(14.0), &active);
        let mut at = 0;
        for section in &job.sections {
            let start = section.byte_range.start.0;
            let end = section.byte_range.end.0;
            assert_eq!(start, at);
            assert!(end > start);
            assert!(source.is_char_boundary(start));
            assert!(source.is_char_boundary(end));
            at = end;
        }
        assert_eq!(at, source.len());
    }

    #[test]
    fn a_sounding_token_splits_its_string_and_lifts_only_the_middle() {
        let source = "play(v, \"c4 e4\")";
        let start = source.find("c4").unwrap();
        let end = start + 2;
        let active_ranges = std::iter::once(start..end).collect::<Vec<_>>();
        let job = layout(source, FontId::monospace(14.0), &active_ranges);
        let active = job
            .sections
            .iter()
            .find(|section| section.byte_range.start.0 == start && section.byte_range.end.0 == end)
            .unwrap();
        assert_eq!(active.format.color, style::BRIGHT);
        assert_eq!(active.format.background, style::DISCHARGE_WASH);
        assert!(job.sections.iter().any(|section| {
            section.byte_range.start.0 < start
                && section.byte_range.end.0 > source.find('"').unwrap()
                && section.format.background == eframe::egui::Color32::TRANSPARENT
        }));
        assert!(job.sections.iter().any(|section| {
            section.byte_range.end.0 > end
                && section.byte_range.start.0 < source.rfind('"').unwrap() + 1
                && section.format.background == eframe::egui::Color32::TRANSPARENT
        }));
    }

    #[test]
    fn changing_unicode_text_during_layout_cannot_reuse_old_sounding_offsets() {
        let previous = "play(v, \"c4 e4\")";
        let changed = "play(v, \"水 e4\")";
        let start = previous.find("c4").unwrap();
        let active: Vec<_> = std::iter::once(start..start + 2).collect();
        let job = layout_for_revision(changed, FontId::monospace(14.0), Some(previous), &active);
        for section in job.sections {
            assert!(changed.is_char_boundary(section.byte_range.start.0));
            assert!(changed.is_char_boundary(section.byte_range.end.0));
            assert_eq!(
                section.format.background,
                eframe::egui::Color32::TRANSPARENT
            );
        }
        let unchanged =
            layout_for_revision(previous, FontId::monospace(14.0), Some(previous), &active);
        assert!(
            unchanged
                .sections
                .iter()
                .any(|section| section.format.background == style::DISCHARGE_WASH)
        );
    }
}
