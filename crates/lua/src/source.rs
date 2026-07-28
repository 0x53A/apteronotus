//! Minimal source attribution pass.
//!
//! Piccolo has no debug stack from which a callback can recover its Lua call
//! site. Direct calls to the two mini-notation entry points are therefore
//! rewritten to carry their original byte offset. Strings, long strings and
//! comments are skipped, so text that merely mentions `play(` is untouched.
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
            output.push_str(&source[index..end]);
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
                output.push(',');
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
            return Some(index + equals + 2);
        }
        index += 1;
    }
    Some(bytes.len())
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
        let source = "local p = pattern(\"a\")\nplay(v, p)";
        assert_eq!(
            inject_call_sites(source),
            "local p = __pattern_at(10,\"a\")\n__play_at(23,v, p)"
        );
    }

    #[test]
    fn strings_comments_methods_and_declarations_are_not_rewritten() {
        let source = r#"
          -- play(v, "comment")
          local text = "pattern('string')"
          object:play(v)
          object.pattern("x")
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
