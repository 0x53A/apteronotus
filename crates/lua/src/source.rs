//! Minimal source attribution pass.
//!
//! Piccolo has no debug stack from which a callback can recover its Lua call
//! site. Direct calls to mini-notation entry points and seeded pattern
//! transforms are therefore rewritten to carry their original byte offset.
//! Strings, long strings and comments are skipped, so text that merely
//! mentions `play(` is untouched.
//!
//! This lexical first slice treats `play` and `pattern` as reserved in direct
//! call position. It skips declarations and method/field calls, but is not yet
//! scope-aware enough to recognize a later call through a shadowing local. See
//! `../DESIGN.md`; an AST-aware pass should remove that restriction.

pub(crate) fn inject_call_sites(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    let mut previous_word: Option<&str> = None;
    let mut last_significant = None;

    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"') {
            let end = quoted_end(bytes, index);
            let mut next = end;
            while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                next += 1;
            }
            if bytes.get(next..next + 2) == Some(b">>") {
                output.push_str("__pattern_at(");
                output.push_str(&index.to_string());
                output.push(',');
                output.push_str(&quoted_content_base(bytes, index, end).to_string());
                output.push(',');
                output.push_str(&source[index..end]);
                output.push(')');
            } else {
                output.push_str(&source[index..end]);
            }
            index = end;
            previous_word = None;
            last_significant = Some(b'"');
            continue;
        }

        if bytes[index..].starts_with(b"--") {
            let end = if let Some(end) = long_bracket_end(bytes, index + 2) {
                end
            } else {
                bytes[index..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| index + offset + 1)
            };
            output.push_str(&source[index..end]);
            index = end;
            continue;
        }

        if bytes[index] == b'['
            && let Some(end) = long_bracket_end(bytes, index)
        {
            output.push_str(&source[index..end]);
            index = end;
            previous_word = None;
            last_significant = Some(b']');
            continue;
        }

        if is_identifier_start(bytes[index]) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continue(bytes[index]) {
                index += 1;
            }
            let word = &source[start..index];
            let mut open = index;
            while open < bytes.len() && bytes[open].is_ascii_whitespace() {
                open += 1;
            }
            let target = match word {
                "pattern" => Some("__pattern_at"),
                "play" => Some("__play_at"),
                "degrade" => Some("__degrade_at"),
                "sometimes" => Some("__sometimes_at"),
                "ply" => Some("__ply_at"),
                "perlin" => Some("__perlin_at"),
                "sine" => Some("__sine_at"),
                "cosine" => Some("__cosine_at"),
                "rand" => Some("__rand_at"),
                "saw" => Some("__saw_at"),
                "step" => Some("__step_at"),
                "line" => Some("__line_at"),
                "window" => Some("__window_at"),
                "chord" => Some("__chord_at"),
                _ => None,
            };
            let is_direct_call = target.is_some()
                && bytes.get(open) == Some(&b'(')
                && !matches!(last_significant, Some(b'.' | b':'))
                && previous_word != Some("function");
            if is_direct_call {
                output.push_str(target.expect("target was checked"));
                output.push('(');
                output.push_str(&start.to_string());
                if matches!(word, "pattern" | "play") {
                    output.push(',');
                    output.push_str(&direct_literal_base(bytes, open).to_string());
                }
                let mut first_argument = open + 1;
                while first_argument < bytes.len() && bytes[first_argument].is_ascii_whitespace() {
                    first_argument += 1;
                }
                if bytes.get(first_argument) != Some(&b')') {
                    output.push(',');
                }
                index = open + 1;
                previous_word = None;
                last_significant = Some(b'(');
            } else {
                output.push_str(word);
                previous_word = Some(word);
                last_significant = word.as_bytes().last().copied();
            }
            continue;
        }

        let byte = bytes[index];
        let character = source[index..]
            .chars()
            .next()
            .expect("index is before the UTF-8 source end");
        output.push(character);
        index += character.len_utf8();
        if !byte.is_ascii_whitespace() {
            previous_word = None;
            last_significant = Some(byte);
        }
    }

    output
}

fn direct_literal_base(bytes: &[u8], open: usize) -> usize {
    let mut index = open + 1;
    let mut depth = 1usize;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"') {
            let end = quoted_end(bytes, index);
            if depth == 1 {
                return quoted_content_base(bytes, index, end);
            }
            index = end;
            continue;
        }
        if bytes[index..].starts_with(b"--") {
            index = if let Some(end) = long_bracket_end(bytes, index + 2) {
                end
            } else {
                bytes[index..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| index + offset + 1)
            };
            continue;
        }
        if bytes[index] == b'['
            && let Some((content, end)) = long_bracket_parts(bytes, index)
        {
            if depth == 1 {
                let body_end = long_bracket_body_end(bytes, index, end);
                let body = &bytes[content..body_end];
                if body.contains(&b'\r') {
                    return 0;
                }
                return content + usize::from(body.first() == Some(&b'\n'));
            }
            index = end;
            continue;
        }
        match bytes[index] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return 0;
                }
            }
            _ => {}
        }
        index += 1;
    }
    0
}

fn quoted_content_base(bytes: &[u8], start: usize, end: usize) -> usize {
    let content_end = end.saturating_sub(1).max(start + 1);
    if bytes[start + 1..content_end].contains(&b'\\') {
        0
    } else {
        start + 1
    }
}

fn quoted_end(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

fn long_bracket_end(bytes: &[u8], start: usize) -> Option<usize> {
    long_bracket_parts(bytes, start).map(|(_, end)| end)
}

fn long_bracket_parts(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut marker_end = start + 1;
    while bytes.get(marker_end) == Some(&b'=') {
        marker_end += 1;
    }
    if bytes.get(marker_end) != Some(&b'[') {
        return None;
    }
    let equals = marker_end - start - 1;
    let mut index = marker_end + 1;
    while index < bytes.len() {
        if bytes[index] == b']'
            && bytes.get(index + 1..index + 1 + equals) == Some(&bytes[start + 1..marker_end])
            && bytes.get(index + 1 + equals) == Some(&b']')
        {
            return Some((marker_end + 1, index + equals + 2));
        }
        index += 1;
    }
    Some((marker_end + 1, bytes.len()))
}

fn long_bracket_body_end(bytes: &[u8], start: usize, end: usize) -> usize {
    let mut marker_end = start + 1;
    while bytes.get(marker_end) == Some(&b'=') {
        marker_end += 1;
    }
    let closer_len = marker_end - start + 1;
    end.saturating_sub(closer_len)
}

const fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

const fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::inject_call_sites;

    #[test]
    fn direct_calls_receive_original_byte_offsets() {
        let source = "local p = pattern(\"a\") >> degrade(0.2) >> sometimes(0.1, rev)\nlocal x = perlin(0.1)\nplay(v, p)";
        assert_eq!(
            inject_call_sites(source),
            "local p = __pattern_at(10,19,\"a\") >> __degrade_at(26,0.2) >> __sometimes_at(42,0.1, rev)\nlocal x = __perlin_at(72,0.1)\n__play_at(84,0,v, p)"
        );
    }

    #[test]
    fn transport_signal_calls_receive_original_byte_offsets() {
        let source = "local a = step(bars(2))\nlocal b = line(0, 1, bars(2)) + sine(0.5)\nlocal c = window(0, 1) * saw(2)";
        assert_eq!(
            inject_call_sites(source),
            "local a = __step_at(10,bars(2))\nlocal b = __line_at(34,0, 1, bars(2)) + __sine_at(56,0.5)\nlocal c = __window_at(76,0, 1) * __saw_at(91,2)"
        );
    }

    #[test]
    fn empty_signal_calls_do_not_receive_a_trailing_comma() {
        assert_eq!(inject_call_sites("sine()"), "__sine_at(0)");
    }

    #[test]
    fn literal_chords_receive_construction_site_identity() {
        assert_eq!(
            inject_call_sites("local harmony = chord(\"Dm(add9)\")"),
            "local harmony = __chord_at(16,\"Dm(add9)\")"
        );
    }

    #[test]
    fn mini_literal_on_the_left_of_a_transform_keeps_its_site() {
        assert_eq!(
            inject_call_sites(r#"play(v, "c4 e4" >> velocity(0.5))"#),
            r#"__play_at(0,9,v, __pattern_at(8,9,"c4 e4") >> velocity(0.5))"#
        );
    }

    #[test]
    fn mini_entry_points_receive_only_exact_literal_content_offsets() {
        assert_eq!(
            inject_call_sites(r#"pattern("a")"#),
            r#"__pattern_at(0,9,"a")"#
        );
        assert_eq!(
            inject_call_sites(r#"play(v, "c4")"#),
            r#"__play_at(0,9,v, "c4")"#
        );
        assert_eq!(
            inject_call_sites(r#"pattern("a\tb")"#),
            r#"__pattern_at(0,0,"a\tb")"#
        );
        assert_eq!(
            inject_call_sites("pattern([[a b]])"),
            "__pattern_at(0,10,[[a b]])"
        );
        assert_eq!(
            inject_call_sites("pattern([[\na b]])"),
            "__pattern_at(0,11,[[\na b]])"
        );
        assert_eq!(
            inject_call_sites("pattern([[\r\na b]])"),
            "__pattern_at(0,0,[[\r\na b]])"
        );
        assert_eq!(
            inject_call_sites("pattern([[a\r\nb]])"),
            "__pattern_at(0,0,[[a\r\nb]])"
        );
    }

    #[test]
    fn outer_calls_ignore_nested_literals_and_nonliteral_arguments() {
        assert_eq!(
            inject_call_sites(r#"play(v, pattern("a"))"#),
            r#"__play_at(0,0,v, __pattern_at(8,17,"a"))"#
        );
        assert_eq!(inject_call_sites("play(v, p)"), "__play_at(0,0,v, p)");
    }

    #[test]
    fn strings_comments_methods_and_declarations_are_not_rewritten() {
        let source = r#"
          -- play(v, "comment")
          local text = "pattern('string')"
          object:play(v)
          object.pattern("x")
          object.degrade(0.5)
          object.sometimes(0.5, rev)
          object.perlin(0.1)
          local function play(v) return v end
        "#;
        assert_eq!(inject_call_sites(source), source);
    }

    #[test]
    fn long_strings_and_comments_are_not_rewritten() {
        let source = "--[=[ pattern(\"x\") ]=]\nlocal x = [==[play(v, \"x\")]==]";
        assert_eq!(inject_call_sites(source), source);
    }
}
