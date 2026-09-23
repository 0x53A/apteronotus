//! Compile-only diagnostics. No VM, bindings, program, or user code execution.
use std::{fmt, ops::Range};

use piccolo::compiler::{ParseErrorKind, compile_chunk, interning::BasicInterner, parse_chunk};

use crate::Limits;

/// The first syntax/compiler problem in the original, untransformed document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxDiagnostic {
    pub message: String,
    /// One-based Lua source line, if the compiler supplied one.
    pub line: Option<usize>,
    /// The entire original source line, excluding its newline. Piccolo does
    /// not expose token columns; this range deliberately makes no such claim.
    pub line_span: Option<Range<usize>>,
}

impl fmt::Display for SyntaxDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for SyntaxDiagnostic {}

/// Parse and compile the document without executing it. This checks Lua
/// grammar, lexical scopes and compiler limits, not graph types, mini-notation
/// inside strings, or runtime binding names. Even `while true do end` returns.
pub fn check_syntax(source: &str) -> Result<(), SyntaxDiagnostic> {
    check_syntax_with_limit(source, Limits::default().source_bytes)
}

/// The source-byte ceiling bounds the compilation input independently of
/// evaluation fuel. No VM exists here, so VM memory/fuel limits do not apply.
pub fn check_syntax_with_limit(source: &str, source_bytes: usize) -> Result<(), SyntaxDiagnostic> {
    if source.len() > source_bytes {
        return Err(SyntaxDiagnostic {
            message: format!(
                "source uses {} bytes, limit is {source_bytes}",
                source.len()
            ),
            line: None,
            line_span: None,
        });
    }
    let mut interner = BasicInterner::default();
    let chunk = parse_chunk(source.as_bytes(), &mut interner).map_err(|error| {
        let message = match error.kind {
            ParseErrorKind::LexError(error) => error.to_string(),
            kind => kind.to_string(),
        };
        at_line(source, error.line_number.0, message)
    })?;
    compile_chunk(&chunk, &mut interner)
        .map_err(|error| at_line(source, error.line_number.0, error.kind.to_string()))?;
    Ok(())
}

fn at_line(source: &str, zero_based: u64, message: String) -> SyntaxDiagnostic {
    let line = usize::try_from(zero_based).ok();
    SyntaxDiagnostic {
        message,
        line: line.and_then(|line| line.checked_add(1)),
        line_span: line.and_then(|line| source_line_span(source, line)),
    }
}

/// Lua counts CR, LF, CRLF and LFCR as one newline each. Keep the exact bytes
/// rather than normalising first and then accidentally reporting shifted spans.
fn source_line_span(source: &str, requested: usize) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    let (mut begin, mut cursor, mut line) = (0, 0, 0);
    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if ch == b'\r' || ch == b'\n' {
            if line == requested {
                return Some(begin..cursor);
            }
            cursor += 1;
            if cursor < bytes.len() && matches!(bytes[cursor], b'\r' | b'\n') && bytes[cursor] != ch
            {
                cursor += 1;
            }
            begin = cursor;
            line += 1;
        } else {
            cursor += 1;
        }
    }
    (line == requested).then_some(begin..bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checking_cannot_run_loops_errors_or_graph_builders() {
        for source in [
            "while true do end",
            "error('must not execute')",
            "tempo(0); play(nonexistent, 'not valid mini notation [[[[')",
            "local v = voice { graph = function(n) return n.hz >> sine() end }",
        ] {
            check_syntax(source).unwrap();
        }
    }

    #[test]
    fn compiler_rules_are_checked_without_running_the_chunk() {
        let error = check_syntax("local x <const> = 1\nx = 2").unwrap_err();
        assert!(error.message.contains("const"));
        assert_eq!(error.line, Some(2));
        assert_eq!(error.line_span, Some(20..25));
        assert!(check_syntax("goto missing").is_err());
    }

    #[test]
    fn original_unicode_and_lua_newline_bytes_survive() {
        for newline in ["\n", "\r", "\r\n", "\n\r"] {
            let source = format!("-- 水 {newline}local x = ){newline}");
            let error = check_syntax(&source).unwrap_err();
            assert_eq!(error.line, Some(2));
            assert_eq!(&source[error.line_span.unwrap()], "local x = )");
        }
        assert_eq!(source_line_span("a\n", 1), Some(2..2));
        assert_eq!(source_line_span("a", 1), None);
    }

    #[test]
    fn lexical_errors_retain_the_useful_message() {
        let error = check_syntax("local s = '\\q'").unwrap_err();
        assert!(error.message.contains("escape"), "{error}");
        assert!(check_syntax("local s = [=[unfinished").is_err());
    }

    #[test]
    fn source_limit_counts_bytes_and_does_not_invent_a_location() {
        let error = check_syntax_with_limit("-- 水", 5).unwrap_err();
        assert!(error.message.contains("6 bytes"));
        assert_eq!(error.line, None);
        assert_eq!(error.line_span, None);
    }

    #[test]
    fn deeply_nested_input_returns_a_diagnostic() {
        let source = format!("local x = {}1{}", "(".repeat(500), ")".repeat(500));
        assert!(
            check_syntax(&source)
                .unwrap_err()
                .message
                .contains("recursion")
        );
    }
}
