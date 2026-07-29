//! The mini-notation.
//!
//! `"bd*2 [~ sd] <hh cp>(3,8)"` — the only part of the system that is a
//! genuine domain-specific language rather than a library. It is small on
//! purpose: everything here desugars into the handful of nodes in
//! [`crate::pattern`], so the algebra has no special cases for notation.
//!
//! | | |
//! |---|---|
//! | `a b c` | a sequence filling one cycle |
//! | `~` | a rest |
//! | `[a b]` | a subsequence, one step of the enclosing sequence |
//! | `<a b>` | alternation — one per cycle |
//! | `a, b` | stacked, both at once |
//! | `a*2` `a/2` | faster, slower |
//! | `a!3` | repeated as three steps |
//! | `a@3` | given three times the width of its neighbours |
//! | `a?` `a?0.3` | dropped at random, reproducibly |
//! | `a(3,8)` `a(3,8,2)` | spread over a euclidean rhythm, optionally rotated |

use crate::event::{EventOrigin, GroupNode, SrcSpan, Value};
use crate::frac::Frac;
use crate::pattern::Pattern;

#[derive(Clone, PartialEq, Debug)]
pub struct ParseError {
    pub message: String,
    pub span: SrcSpan,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.span.start)
    }
}

impl std::error::Error for ParseError {}

/// Limits on what a single string may expand into.
///
/// This text arrives from a live editor, on every keystroke, while audio is
/// running. `bd!99999999` and `bd(3,999999999)` are four characters away from
/// anything reasonable, and without a ceiling they allocate or spin until the
/// process dies — taking the sound with them, which is the one failure mode a
/// live instrument may not have.
///
/// So expansion is bounded, and exceeding a bound is an ordinary diagnostic:
/// the caller reports it and keeps playing whatever was last valid. The
/// numbers are far above anything musical and far below anything dangerous,
/// and they are also what makes overflow inside [`crate::frac`] unreachable
/// from user text.
pub mod limits {
    /// Nesting depth of `[` and `<`.
    pub const DEPTH: u32 = 64;
    /// Patterns a single string may expand into.
    pub const NODES: usize = 20_000;
    /// Largest `!` count, `*`/`/`/`@` factor, and euclidean step count.
    pub const COUNT: i64 = 1024;
    /// Events one cycle may produce. Speed factors multiply down the tree, so
    /// this cannot be counted while parsing and is checked on the built
    /// pattern instead — see [`crate::Pattern::density`].
    pub const EVENTS: f64 = 4096.0;
}

/// Parse mini-notation into a pattern.
pub fn parse(src: &str) -> Result<Pattern, ParseError> {
    parse_at(src, 0)
}

/// Parse with the identity of the source-language binding or call site.
///
/// Byte spans inside mini-notation are local to the string. The outer frontend
/// supplies `binding` so two identical strings written at different call sites
/// remain distinct event provenance.
pub fn parse_at(src: &str, binding: u64) -> Result<Pattern, ParseError> {
    let mut p = Parser {
        chars: src.char_indices().collect(),
        end: src.len(),
        i: 0,
        seed: 0,
        depth: 0,
        nodes: 0,
        binding,
    };
    let pat = p.stack(&[])?;
    p.skip_ws();
    if !p.at_end() {
        return Err(p.err_here("unexpected character"));
    }
    let density = pat.density();
    if !matches!(
        density.partial_cmp(&limits::EVENTS),
        Some(core::cmp::Ordering::Less | core::cmp::Ordering::Equal)
    ) {
        return Err(ParseError {
            message: format!(
                "produces about {density:.0} events per cycle, more than the limit of {}",
                limits::EVENTS
            ),
            span: SrcSpan::new(0, src.len()),
        });
    }
    Ok(pat)
}

struct Parser {
    chars: Vec<(usize, char)>,
    end: usize,
    i: usize,
    /// Bumped for every `?`, so two degrades in one string do not fall on the
    /// same events. Derived from position in the source, so it is stable
    /// across runs and the same text always sounds the same.
    seed: u64,
    depth: u32,
    nodes: usize,
    binding: u64,
}

struct Step {
    weight: Frac,
    repeat: i64,
    pat: Pattern,
}

impl Parser {
    fn at_end(&self) -> bool {
        self.i >= self.chars.len()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).map(|(_, c)| *c)
    }

    fn pos(&self) -> usize {
        self.chars.get(self.i).map(|(b, _)| *b).unwrap_or(self.end)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.i += 1;
        }
    }

    /// Byte offset just past the current character.
    ///
    /// Not `pos() + 1`: a rejected `«` is two bytes, and an editor slicing a
    /// range that splits it panics. At the end of input this equals `pos()`,
    /// giving a zero-width caret rather than a range past the last byte.
    fn pos_after(&self) -> usize {
        self.chars
            .get(self.i)
            .map(|(b, c)| b + c.len_utf8())
            .unwrap_or(self.end)
    }

    /// A diagnostic covering the character the parser is looking at.
    fn err_here(&self, msg: &str) -> ParseError {
        ParseError {
            message: msg.to_string(),
            span: SrcSpan::new(self.pos(), self.pos_after()),
        }
    }

    /// A diagnostic covering everything from `start` to here.
    fn err_at(&self, start: usize, msg: &str) -> ParseError {
        ParseError {
            message: msg.to_string(),
            span: SrcSpan::new(start, self.pos().max(start)),
        }
    }

    /// Charge `n` patterns against the expansion budget.
    fn spend(&mut self, n: usize, start: usize) -> Result<(), ParseError> {
        self.nodes = self.nodes.saturating_add(n);
        if self.nodes > limits::NODES {
            return Err(self.err_at(
                start,
                &format!("pattern expands past {} steps", limits::NODES),
            ));
        }
        Ok(())
    }

    /// A count or factor, rejected if it is outside what music needs.
    fn bounded(&self, start: usize, n: i64, what: &str) -> Result<i64, ParseError> {
        if !(1..=limits::COUNT).contains(&n) {
            return Err(self.err_at(
                start,
                &format!("{what} must be between 1 and {}", limits::COUNT),
            ));
        }
        Ok(n)
    }

    /// Comma-separated groups, played together.
    fn stack(&mut self, closers: &[char]) -> Result<Pattern, ParseError> {
        Ok(Pattern::stack(self.stack_layers(closers)?))
    }

    fn stack_layers(&mut self, closers: &[char]) -> Result<Vec<Pattern>, ParseError> {
        let mut layers = vec![self.sequence(closers)?];
        while self.eat(',') {
            layers.push(self.sequence(closers)?);
        }
        Ok(layers)
    }

    /// Whitespace-separated steps sharing one cycle.
    fn sequence(&mut self, closers: &[char]) -> Result<Pattern, ParseError> {
        let steps = self.steps(closers)?;
        Ok(weighted(steps))
    }

    /// The same, but each step gets a whole cycle in turn.
    fn alternation(&mut self, closers: &[char]) -> Result<Pattern, ParseError> {
        let steps = self.steps(closers)?;
        let mut items = Vec::new();
        for s in steps {
            for _ in 0..s.repeat.max(1) {
                items.push(s.pat.clone());
            }
        }
        Ok(Pattern::cat(items))
    }

    fn steps(&mut self, closers: &[char]) -> Result<Vec<Step>, ParseError> {
        let mut steps: Vec<Step> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(',') => break,
                Some(c) if closers.contains(&c) => break,
                Some(_) => {}
            }
            // A bare `!` repeats whatever came before it.
            if self.peek() == Some('!') && !steps.is_empty() {
                let start = self.pos();
                self.bump();
                let raw = self.opt_int().unwrap_or(2);
                let n = self.bounded(start, raw, "repeat count")?;
                self.spend(n as usize, start)?;
                let last = steps.last_mut().unwrap();
                last.repeat += n - 1;
                continue;
            }
            steps.push(self.step()?);
        }
        Ok(steps)
    }

    fn step(&mut self) -> Result<Step, ParseError> {
        let start = self.pos();
        let mut pat = self.atom()?;
        let mut weight = Frac::ONE;
        let mut repeat = 1i64;

        loop {
            match self.peek() {
                Some('*') => {
                    self.bump();
                    let f = self.factor()?;
                    pat = pat.fast(f);
                }
                Some('/') => {
                    self.bump();
                    let f = self.factor()?;
                    pat = pat.slow(f);
                }
                Some('!') => {
                    // Only a *suffixed* count binds to this step; a bare `!`
                    // is handled one level up so it can repeat the previous
                    // step instead.
                    let save = self.i;
                    self.bump();
                    match self.opt_int() {
                        Some(n) => {
                            repeat = self.bounded(start, n, "repeat count")?;
                            self.spend(repeat as usize, start)?;
                        }
                        None => {
                            self.i = save;
                            break;
                        }
                    }
                }
                Some('@') => {
                    self.bump();
                    let w = self.factor()?;
                    if w.is_negative() {
                        return Err(self.err_at(start, "weight must not be negative"));
                    }
                    weight = w;
                }
                Some('?') => {
                    self.bump();
                    let amount = self.opt_number().unwrap_or(0.5);
                    if !(0.0..=1.0).contains(&amount) {
                        return Err(self.err_at(start, "degrade amount must be within 0..1"));
                    }
                    self.seed = self
                        .seed
                        .wrapping_mul(0x100_0001)
                        .wrapping_add(start as u64 + 1);
                    pat = pat.degrade_by(amount, self.seed);
                }
                Some('(') => {
                    self.bump();
                    let (k, n, rot) = self.euclid_args(start)?;
                    pat = pat.euclid(k, n, rot);
                }
                _ => break,
            }
        }

        Ok(Step {
            weight,
            repeat,
            pat,
        })
    }

    fn atom(&mut self) -> Result<Pattern, ParseError> {
        let start = self.pos();
        self.spend(1, start)?;
        // Depth is counted around the whole atom, including the bracket
        // recursion below, so deeply nested text is rejected rather than
        // overflowing the parser's own stack.
        self.depth += 1;
        if self.depth > limits::DEPTH {
            self.depth -= 1;
            return Err(self.err_at(start, &format!("nested more than {} deep", limits::DEPTH)));
        }
        let out = self.atom_inner(start);
        self.depth -= 1;
        out
    }

    fn atom_inner(&mut self, start: usize) -> Result<Pattern, ParseError> {
        match self.peek() {
            None => Err(self.err_here("expected a step")),
            Some('~') => {
                self.bump();
                Ok(Pattern::Silence)
            }
            Some('[') => {
                self.bump();
                let layers = self.stack_layers(&[']'])?;
                if !self.eat(']') {
                    return Err(self.err_at(start, "unclosed `[`"));
                }
                if layers.len() > 1 {
                    Ok(Pattern::group(
                        GroupNode::from_source(self.binding, start),
                        layers,
                    ))
                } else {
                    Ok(Pattern::stack(layers))
                }
            }
            Some('<') => {
                self.bump();
                let mut layers = vec![self.alternation(&['>'])?];
                while self.eat(',') {
                    layers.push(self.alternation(&['>'])?);
                }
                if !self.eat('>') {
                    return Err(self.err_at(start, "unclosed `<`"));
                }
                Ok(Pattern::stack(layers))
            }
            Some(c) if c == ']' || c == '>' || c == ')' => {
                Err(self.err_here("unmatched closing bracket"))
            }
            Some(_) => {
                let (text, span) = self.word(start)?;
                let value = match text.parse::<f64>() {
                    Ok(x) => Value::number(x),
                    Err(_) => Value::text(text),
                };
                Ok(Pattern::Pure {
                    value,
                    src: Some(span),
                    origin: EventOrigin::source(self.binding, Some(span)),
                })
            }
        }
    }

    /// A note name, sample name or number. Kept liberal — `c#4`, `bd`, `-1.5`,
    /// `hh'closed` are all one word.
    fn word(&mut self, start: usize) -> Result<(String, SrcSpan), ParseError> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            let ok = c.is_alphanumeric() || matches!(c, '#' | '\'' | '_' | '.' | '-' | ':');
            if !ok {
                break;
            }
            s.push(c);
            self.bump();
        }
        if s.is_empty() {
            return Err(self.err_here("expected a step"));
        }
        Ok((s, SrcSpan::new(start, self.pos())))
    }

    fn opt_int(&mut self) -> Option<i64> {
        let save = self.i;
        let mut s = String::new();
        if self.peek() == Some('-') {
            s.push('-');
            self.bump();
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            s.push(self.bump().unwrap());
        }
        match s.parse::<i64>() {
            Ok(n) => Some(n),
            Err(_) => {
                self.i = save;
                None
            }
        }
    }

    fn opt_number(&mut self) -> Option<f64> {
        let save = self.i;
        let mut s = String::new();
        if self.peek() == Some('-') {
            s.push('-');
            self.bump();
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
            s.push(self.bump().unwrap());
        }
        match s.parse::<f64>() {
            Ok(x) => Some(x),
            Err(_) => {
                self.i = save;
                None
            }
        }
    }

    /// The argument of `*`, `/` or `@`. Integers stay exact; a decimal is
    /// snapped to the nearest simple ratio so that `*1.5` really is three
    /// halves and its events still line up with everything else.
    fn factor(&mut self) -> Result<Frac, ParseError> {
        let start = self.pos();
        let Some(x) = self.opt_number() else {
            return Err(self.err_here("expected a number"));
        };
        if !x.is_finite() || x == 0.0 {
            return Err(self.err_at(start, "factor must not be zero"));
        }
        if x.abs() > limits::COUNT as f64 {
            return Err(self.err_at(
                start,
                &format!("factor must be between 1 and {}", limits::COUNT),
            ));
        }
        Ok(if x.fract() == 0.0 {
            Frac::int(x as i64)
        } else {
            Frac::approx(x, 1000)
        })
    }

    fn euclid_args(&mut self, start: usize) -> Result<(i64, i64, i64), ParseError> {
        self.skip_ws();
        let k = self
            .opt_int()
            .ok_or_else(|| self.err_here("expected a pulse count"))?;
        self.skip_ws();
        if !self.eat(',') {
            return Err(self.err_here("expected `,` in euclidean rhythm"));
        }
        self.skip_ws();
        let n = self
            .opt_int()
            .ok_or_else(|| self.err_here("expected a step count"))?;
        self.skip_ws();
        let rot = if self.eat(',') {
            self.skip_ws();
            self.opt_int()
                .ok_or_else(|| self.err_here("expected a rotation"))?
        } else {
            0
        };
        self.skip_ws();
        if !self.eat(')') {
            return Err(self.err_at(start, "unclosed `(`"));
        }
        let n = self.bounded(start, n, "euclidean step count")?;
        self.spend(n as usize, start)?;
        Ok((k, n, rot))
    }
}

fn weighted(steps: Vec<Step>) -> Pattern {
    let mut parts: Vec<(Frac, Pattern)> = Vec::new();
    for s in steps {
        for _ in 0..s.repeat.max(1) {
            parts.push((s.weight, s.pat.clone()));
        }
    }
    match parts.len() {
        0 => Pattern::Silence,
        1 if parts[0].0 == Frac::ONE => parts.into_iter().next().unwrap().1,
        _ => Pattern::Timecat(parts),
    }
}
