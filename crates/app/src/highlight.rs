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
    "bus",
    "run",
    "play",
    "pattern",
    "note",
    "velocity",
    "curve",
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
    // sources
    "sine",
    "saw",
    "pulse",
    "noise",
    "impulse",
    "dc",
    "init_random",
    "init_rand",
    // processing
    "lowpass",
    "highpass",
    "bandpass",
    "moog",
    "shape",
    "dcblock",
    "delay",
    "ring",
    "pan",
    "mix",
    "window",
    "to",
    "add",
    "sub",
    "mul",
    "div",
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
pub fn layout(source: &str, font: FontId) -> LayoutJob {
    let mut job = LayoutJob {
        text: source.to_owned(),
        ..Default::default()
    };
    // A code editor scrolls sideways; it does not reflow. Wrapping would also
    // desynchronise the line-number gutter, which counts newlines.
    job.wrap.max_width = f32::INFINITY;
    for (range, color) in spans(source) {
        job.sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(range.start)..ByteIndex(range.end),
            format: TextFormat::simple(font.clone(), color),
        });
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
    use super::{spans, style};

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
}
