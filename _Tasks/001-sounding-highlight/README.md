# Sounding-source highlighting

Light the mini-notation token that is sounding right now, the way Strudel does,
without giving up the two rules the rest of the system is built on: a query is a
pure function of its window, and evaluation is an explicit boundary.

Revised after review. The review's substantive correction is that **activation
is a future event**: a compatible edit activates at the scheduling frontier,
roughly a lookahead ahead of the playback clock, so "the program the player
accepted" and "the program you are hearing" are not the same thing. That makes
stage 2 a small state machine rather than plumbing, and it is now stage 2's
whole subject.

## Scope

**In scope.** Pattern events. While a program is audible and the editor buffer
is *byte-identical* to the text that produced it, every event whose extent
covers the transport's current position lights its source token.

**Explicit non-goal, by decision.** Any attempt to keep highlighting alive
across an edit. The moment the buffer differs from the running text, highlights
vanish. A span-remapping diff is a later feature and must not be designed for
now — the equality gate is what keeps this feature from needing one.

**Out of scope here.** Graph-element contribution (see the last section — it is
a different question with a different answer), MIDI, file handling, transport
controls.

## The one real gap

`Event.src` already survives every combinator (`crates/pattern/src/event.rs:541`,
asserted by `crates/pattern/tests/mini.rs:188`), and `Span::sect` already has an
explicit zero-width case so "what is sounding at this instant" is a supported
query (`crates/pattern/src/span.rs:40-55`). What is missing is that mini-notation
spans are **local to the string literal**: the parser's offsets are offsets into
the string it was handed (`crates/pattern/src/mini.rs:401`). The `binding` the
Lua frontend supplies is the byte offset of the `play`/`pattern` *identifier*,
and it is only ever hashed into `EventOrigin` (`event.rs:463-471`), so it cannot
be recovered.

An event today says `3..5`. The editor needs `412..414`.

Note what the gap is *not*: a local span is not merely useless to the editor, it
is actively dangerous, because small offsets do point at real bytes near the top
of the document. Hence the rule adopted in stage 1: **a coordinate that cannot
be expressed in the document must be absent, not approximate.**

## Stage 1 — document coordinates for mini-notation spans

Crates: `pattern`, `lua`.

### 1.1 `mini::parse_in`

```rust
/// `base` places the string's bytes in the document that contains it.
/// `Some(0)` keeps the historical string-local behaviour; `None` means the
/// frontend could not locate the literal, and events are emitted with no span
/// at all rather than with coordinates that address the wrong text.
pub fn parse_in(src: &str, binding: u64, base: Option<usize>) -> Result<Pattern, ParseError>
```

`parse_at(src, binding)` becomes `parse_in(src, binding, Some(0))`; `parse(src)`
is unchanged. The parser gains one field and applies it where a span leaves it.

`SrcSpan` gains a **checked** offset:

```rust
pub fn offset(self, base: usize) -> Option<SrcSpan>   // None on u32 overflow
```

Checked rather than saturating, because a saturating offset would manufacture
exactly the kind of plausible-but-wrong coordinate this stage exists to
eliminate. `None` propagates as "no span", the same as an unknown base.

**The invariant that must be commented in the code:** the provenance hash keeps
using the *local* span.

```rust
origin: EventOrigin::source(self.binding, Some(local)),   // unchanged
src:    base.and_then(|b| local.offset(b)),                // new: the editor
```

`EventOrigin::source` mixes `binding ^ src.start` (`event.rs:463-471`), and
`Event::seed` mixes the origin (`event.rs:570-581`). Feeding it the rebased span
would therefore change **`Event::seed`, and so every `init_random` stream** —
per-voice randomness would shift whenever a call moved by a byte, as a side
effect of a highlighting feature.

Two things this does *not* affect, contrary to an earlier draft of this plan:
mini-notation `?` derives from the parser's own seed counter at the local
position (`mini.rs:312`), and group identity from `GroupNode::from_source(binding,
local_start)` (`mini.rs:371`). Both are computed at parse time from local
offsets and are untouched either way. The decision stands; only the reason
needed correcting, and the test in 1.5 must name the right consumers.

### 1.2 Parse-error coordinates

`ParseError.span` is mandatory today and its `Display` prints `(at byte N)`
(`mini.rs:31-35`). With `base: None` there is no honest document coordinate, and
printing the local number under the same label breaks the stage's own rule.

`ParseError` therefore keeps `span` as an explicitly **string-local** coordinate
and gains `document: Option<SrcSpan>`, set when a base is known. `Display` reads
`(at byte N)` for a document coordinate and `(at byte N of the pattern text)`
otherwise, so the two are never confused by a reader or by a later diagnostic
renderer. Check the Lua tests that assert on message text.

### 1.3 `source.rs` learns to find the literal

The injection pass already lexes strings, long brackets and comments, so it can
report where the literal starts. When it rewrites a direct call it emits **two**
numbers instead of one:

```
play(v, "c4 e4")        →  __play_at(0,9,v, "c4 e4")
local p = pattern("a")  →  __pattern_at(10,19,"a")
play(v, p)              →  __play_at(0,0,v, p)
```

- first number: the identifier offset, exactly as today (provenance, unchanged);
- second: the byte offset of the first **content byte** of the first string
  literal at paren depth 1 of that call, or `0` for "unknown".

`0` is unambiguous as a sentinel: a literal's contents can never begin at byte 0,
because the call's own name precedes it.

Rules for the second number:

| literal form | base |
|---|---|
| `"…"` / `'…'` with no backslash | offset just past the quote |
| `"…"` containing a backslash | `0` — escapes make source and value bytes differ in length, and mini-notation has no use for them |
| `[[…]]` / `[==[…]==]` containing no `\r` | offset past the opener, plus one if the first byte is `\n` |
| long string containing any `\r` | `0` — see below |
| argument is not a literal | `0` — irrelevant, that value carries rebased spans from its own site |
| no literal before the matching `)` | `0` |

The `\r` exclusion is not paranoia. Piccolo's lexer reads `\n`, `\r`, `\n\r` and
`\r\n` as a single `\n` **everywhere inside a long string**
(`vendor/piccolo/src/compiler/lexer.rs:507-518`) and additionally drops a line
ending immediately after the opener (`:719-723`). After one interior paired
newline the runtime string and the document no longer differ by a constant, so a
single base is wrong from that point on. LF-only long strings stay exact. Test
both an initial CRLF and an interior CRLF.

Only `pattern` and `play` need the second number; `degrade`, `sometimes`,
`sine`, `saw`, `perlin`, `step`, `line`, `window`, `cosine` take no string and
keep their single injected offset.

The depth-1 restriction is what makes `play(v, pattern("a"))` correct: `play`
finds no depth-1 literal and passes `0`, while the inner `pattern` call passes
`"a"`'s own base.

This stays a lexical pass. `DESIGN.md` already records that an AST-aware
rewriter is the eventual answer for shadowed locals; nothing here makes that
harder.

### 1.4 Bindings

`pattern_at` / `play_at` read two injected numbers, so `argument_offset` becomes
`2`, and they call `parse_in(src, binding, base)` where `base == 0` maps to
`None`. `play` reached through a `LuaPattern` userdata is untouched: those spans
were rebased when `pattern()` ran.

### 1.5 The arithmetic-coercion gap

One call site parses with no attribution at all: `numeric_pattern_operand`
(`bindings.rs:1657`) coerces an inline string operand in pattern arithmetic via
plain `mini::parse`. Under the new rule it passes `None`, so an inline
arithmetic operand does not highlight. Record it in `crates/lua/DESIGN.md` as
the *arithmetic-coercion gap*; it wants the same `_at` treatment later.

Timelines are **not** such a gap, contrary to an earlier draft. `at()` receives
`LuaPattern` values and copies `event.src` into the captured `TimelineEvent`
(`bindings.rs:1233`), so correctly rebased `pattern(...)` spans already survive
placement. Pin that with an acceptance test rather than assuming it.

### 1.6 Tests

Pattern crate:

- a base shifts every event span by exactly that base;
- **`Event::seed` and `EventOrigin` are bit-identical with and without a base**,
  and so are group provenance and complete event selection under `?` — the
  silence guarantee, stated over the consumers that actually depend on it;
- `None` produces `src: None` while leaving values and seeds untouched;
- checked offset overflow yields `None`, not a wrapped span;
- the existing char-boundary and span-ordering properties (`tests/mini.rs:265-272`)
  hold under a base.

Lua crate:

- `inject_call_sites` cases extended with the second number: escapes, LF long
  bracket, initial CRLF, interior CRLF, nested call, non-literal argument;
- **acceptance:** evaluate a document, query track 0 at cycle 0, slice the
  *original document* with `event.src`, assert it reads `"c4"`;
- **acceptance:** the same through `timeline`/`at`, asserting the captured
  event's span still slices the document correctly.

## Stage 2 — audible state, not accepted state

Crate: `app`. This is the stage the review corrected, and it is not plumbing.

For a compatible edit the player activates the candidate at
`scheduler.frontier()` (`crates/app/src/player.rs:225`) — a cycle roughly a
lookahead *ahead* of the playback clock — fills the window past it, and only
then emits `Active` (`:289`). Promoting the new program to "what is being heard"
on that message would highlight the new pattern while the old one is still
coming out of the speakers. A hard reset is the opposite case: the old stream is
paused and the transport restarts, so its activation is effective immediately at
cycle zero.

### 2.1 What `Active` must carry

```rust
program:      Arc<Program>,   // the Arc the revision slot already holds
origin:       Instant,        // transport zero; hard_reset takes a new one
effective_at: Frac,           // the cycle at which this program becomes audible
```

`effective_at` replaces today's `boundary: String`; the UI formats it for the
readout, so there is one representation rather than a display string beside a
number. `hard_reset` resets `clock_started`, so `origin` must be read after
activation rather than cached.

A tempo change forces a hard reset (`needs_hard_reset`, `player.rs:600-614`), so
during a compatible edit the old and new programs share a tempo map and
`cycle_to_seconds(effective_at)` is unambiguous. Note this in a comment: it is
what makes a single origin sufficient for both programs.

### 2.2 What the app holds: a mirror of `RevisionSlot`

The app must not invent this structure — the runtime already has it.
`RevisionSlot` holds one `active` revision and a `VecDeque` of `pending` ones,
promotes every revision whose boundary the frontier has passed
(`crates/live/src/revision.rs:106-115`), and rejects a candidate whose
`effective_at` precedes the queued boundary
(`SubmitError::OutOfOrder`, `:85-90`). `Revision.effective_at` (`:27`) is
literally the field stage 2.1 sends.

So the app mirrors it:

```rust
struct Sounding {
    source:       String,      // the exact text that produced `program`
    program:      Arc<Program>,
    origin:       Instant,
    effective_at: Frac,
}

audible: Option<Sounding>,
staged:  VecDeque<Sounding>,
```

`Run` records `pending: Option<(u64, String)>`; a matching `Active` pushes a
`Sounding` onto the back of `staged`. Once per frame, **every** staged entry
whose `effective_at` the latency-adjusted transport has reached is promoted, in
order, the last one winning — the same loop `advance_to` runs. A hard reset
arrives with `effective_at == 0` and promotes on the first frame without a
special case.

**A single `staged` slot would be wrong, and the intermediate program really is
audible.** Two compatible Runs can both be accepted before the first boundary is
reached: the player fills revision *N* from its boundary to its lookahead target
and then revision *N+1* takes over at a later frontier, so *N*'s material is
already in the sequencer and sounds over `[boundary_N, boundary_{N+1})`.
Replacing rather than queueing skips that state, and the failure is observable
whenever the buffer matches an *older* program's text — run A, edit to B and
run, undo back to A and run again: with one slot, `audible` never becomes B, the
buffer equals A, and the editor lights A's tokens while B is playing. Queueing
keeps highlights dark across B's window, which is correct.

Monotonicity comes for free: the slot rejects out-of-order boundaries, so the
queue is sorted by construction and may assert it. Its length is bounded by how
many Runs a human lands inside one lookahead, and every promotion drains it.

`Stopped` clears both. `Error` clears **neither**: the previous program is still
playing and is still what should be lit.

Between an `Active` and its promotion — about a lookahead — the buffer already
differs from `audible.source`, so highlights are simply dark for those ~200 ms.
That is the honest answer and it costs nothing.

### 2.3 A new transport origin invalidates everything queued

A hard reset builds a fresh `RevisionSlot` and restarts the clock
(`player.rs:411-415`), so a queued `effective_at` measured against the old origin
means nothing against the new one. When an incoming `Active` carries an origin
different from the current one, **clear `audible` and `staged` before pushing**.
Same rule, same reason, on `Stopped`.

### 2.4 A restored fallback must clear the highlight, not keep it

There is one path where what is sounding changes with no `Active` at all: a
scheduling failure during lookahead makes the player resubmit the previous
program through the same slot and report a string
(`restore_fallback`, `player.rs:526-557`). If the failing program had already
been promoted, the app would go on lighting it while its predecessor plays — and
the buffer very likely still matches it, since the user just ran it.

The conservative rule is one line and cannot lie: **any `RuntimeError` clears
`audible` and `staged`.** Highlights go dark until the next successful Run,
which is the honest report of "we no longer know what is sounding". Refining
this later means giving the player a structured `Reverted { program, origin,
effective_at }` event so the restored generation can be lit instead; that is a
strictly better answer and a strictly larger change.

### 2.5 Eligibility is a playback question, not a banner question

`Status` is presentation: `lib.rs:161` sets `Status::Error` while the previous
program keeps playing. Highlight eligibility must therefore be
`audible.is_some() && self.source == audible.source`, never `Status::Active`.
Worth doing properly: `Status` describes the last command's outcome, `audible`
describes what is coming out of the speakers, and this feature is the first
place the difference is observable.

### 2.6 Assertion

`Program` is plain owned data (`crates/lua/src/program.rs:37`) and already
crosses a channel from the evaluation thread, so it is `Send + Sync`. Add a
static assertion in `player.rs`, where the reason is written down, so a future
`Rc` inside the evaluator breaks the build there rather than at a use site.

### 2.7 Tests

The promotion logic must be a free function over `(audible, staged, now)` so all
of this is testable without a clock, a device or a UI:

- two visibly different tokens across one compatible edit: the old token still
  lights after `Active`, and the swap happens at `effective_at`, not before —
  the regression test for the stage;
- **two `Active`s before the first boundary**: both are retained, the first is
  promoted at its own boundary, the second at its own, and neither is skipped;
- the same source run twice in quick succession behaves identically, since the
  queue never inspects the text;
- an `Active` bearing a new origin discards everything queued against the old
  one;
- a `RuntimeError` clears the state and highlights stop.

## Stage 3 — deriving the active spans

Per frame, only when 2.5's eligibility holds:

```rust
let elapsed = audible.origin.elapsed().as_secs_f64() - HIGHLIGHT_LATENCY;
let now   = tempo.seconds_to_cycle(elapsed)?;
let back  = tempo.seconds_to_cycle(elapsed - FLASH_SECONDS)?;
let begin = back.min(now).max(audible.effective_at);
let window = Span::new(begin, now);
```

then `track.pattern.query(window)` for every track, keeping `event.src` of every
event with `whole.is_some()`.

The `max(effective_at)` clamp is the second half of the review's first finding:
without it, the retrospective flash window reaches back past the moment this
program became audible and lights events the previous program was playing.

Five things this shape buys, each of which is why not to use the obvious
alternative:

- **`query`, not `onsets`.** A held note must stay lit for its whole extent, and
  `query` returns the fragment overlapping the window whether or not it contains
  the onset — the `whole`/`part` distinction doing the job it exists for.
- **A window, not a point.** A point query at 25 fps misses events shorter than
  a frame; `"bd*16"` at 120 bpm would flicker. `FLASH_SECONDS ≈ 0.06` covers the
  frame gap *and* gives every event a visible minimum. It stays derived: the
  window is a pure function of `now`, so there is no flash timer and no decay
  state.
- **`whole.is_some()`.** A continuous signal has a value everywhere and an onset
  nowhere; lighting it would leave its call site permanently on.
- **No scheduler involvement.** Having the scheduler report `(span, start, end)`
  per pushed voice is more faithful to what reached fundsp, but it retains
  history to answer a question that is a pure function of coordinates.
- **`seconds_to_cycle` returns `Result`.** On error: no highlight, no
  diagnostic. This is decoration and must never interrupt playback.

### 3.1 The cap has to be inside the loop

`Pattern::query` builds its complete `Vec<Event>` before returning
(`crates/pattern/src/pattern.rs:283`), so truncating the collected set afterwards
protects the layouter and nothing else. Parse-time density limits bound one
string, but a Lua-side `fast(n)` multiplies it afterwards, and on wasm this work
shares the event loop with lookahead advancement.

So: check the cap **between tracks** and stop querying once it is reached, and
record that a genuinely hard bound needs a bounded/visiting query API in
`pattern` — worth opening as its own small task, since diagnostics will want the
same thing. Test that a program exceeding the cap still renders and still
schedules.

### 3.2 Normalisation

Sort, coalesce overlaps, clamp to `source.len()`, snap to char boundaries. The
equality gate makes an out-of-range span impossible, but egui *slices* with
these, so defend anyway.

`HIGHLIGHT_LATENCY` compensates output buffering so the light matches what is
heard: 0 natively to start, ~43 ms on wasm (2048 frames at 48 kHz). One
constant, refined once there is something to look at.

## Stage 4 — rendering

`highlight::layout(source, font, active: &[Range<usize>])`. `spans()` stays
exactly as it is — lexical, presentation-only — and a new pass splits each
lexical section at active-range boundaries, setting `TextFormat.background` and
lifting the foreground.

Colour: `DISCHARGE_WASH` already exists as a translucent premultiplied accent
and is what selection uses; a sounding token should read as *lit*, so the first
attempt is `DISCHARGE_WASH` behind `BRIGHT`. Hard on/off, rectangular, no
easing — the design document allows exactly one animation and this is not it.

Tests: sections remain contiguous, advancing and char-boundary-safe with active
ranges present (the existing `spans_cover_the_source_exactly_once` property,
lifted to `layout`); a highlight strictly inside a string literal splits it into
three sections with the outer two unchanged.

Watch: the layouter runs per frame and egui caches galleys by job content, so a
changing highlight rebuilds the galley every frame. Fine at the size of the
shipped examples; if a large document drags, cache on `(text, hash(active))`.

## Stage 5 — wasm parity

Nothing structural. The pump path is the same and `Instant` is `web-time`'s
already. Only `HIGHLIGHT_LATENCY` differs.

## Stage 6 — documentation

- `crates/app/DESIGN.md`: a "Sounding" subsection under *Code colour*, stating
  the equality gate, the derived window and the audible-versus-accepted
  distinction.
- `crates/app/README.md`: remove "sounding-source highlighting" from the
  not-present list; state the gate as a deliberate limitation.
- `crates/lua/DESIGN.md`: mini spans are document-absolute; the
  arithmetic-coercion path is the remaining unattributed one.
- `CLAUDE.md` and `_Tasks/000-architecture/README.md`: the `lua` and `app` rows.

## Effort

| stage | size |
|---|---|
| 1 spans in document coordinates | half a day; mechanical but spans three crates and needs the provenance-invariance test |
| 2 audible vs. staged state | half a day; a state machine with a timing test, not plumbing |
| 3 derivation and cap | two hours |
| 4 rendering | two hours with tests |
| 5–6 wasm, docs | an hour |

Two sessions. The first estimate said one because it treated stage 2 as field
additions.

## Risks

- **The provenance invariant** is the one place a mistake is expensive, because
  it changes how existing songs sound rather than breaking a build. Test first.
- **The promotion boundary** is the one place a mistake is *invisible* in tests
  that do not model time: a wrong promotion looks right whenever the edit
  happens to be similar. Hence the tests in 2.7, and hence promotion being a
  free function over `(audible, staged, now)` rather than something tangled into
  the frame callback.
- **The lexical injection pass** is not scope-aware; a shadowed `play` local
  already misbehaves today, and this adds a second number to the same
  misbehaviour. Not a regression, but it moves the AST rewriter up the list.
- **Unbounded query** is mitigated, not solved, until `pattern` grows a bounded
  query.

## The graph question

Highlighting a *graph* element is not the same question, and the difference is
structural: **graph topology is fixed once built** — "add a filter" is a mix
with an automated contribution, not a rebuild. Every node in an instantiated
voice runs for the whole life of that voice. So "is this element active?" is
always yes, and the useful question is *does this element currently contribute
anything?*

Three tiers, cheapest first; only the first two derive.

1. **Voice-level.** Any note sounding from voice *V* lights *V*'s
   `graph = function(n) … end` body. This needs **graph-expression spans**,
   which are half-built: `Node.src` and `Input.src` exist in the template
   (`crates/synth/src/template.rs:531,575,587`) and `GraphBuilder::at` populates
   them, but exactly one binding currently calls it (`bindings.rs:1215`), so
   almost every node carries `None`. Finishing that pass is already on the
   roadmap for diagnostics.

   It is *not* free from stage 3's query, though. A voice outlives its pattern
   event: the scheduler ends it at `lifetime.end_after_onset(gate)`
   (`crates/live/src/scheduler.rs:384`), so a long release keeps contributing
   after the note's gate closes. Either define tier 1 honestly as "pattern gate
   active" — cheap, slightly wrong at the tail — or derive the real lifetime
   from the onset plus `template.lifetime_for(&note)`, which is pure and
   therefore legitimate, but needs the app to build a `Note` the way
   `note_from_value` does. If tier 1 is built, expose that as a helper from
   `live` rather than reimplementing it in the app.

2. **Control- and curve-driven contribution.** A term whose level is a program
   control or a curve can be evaluated *off* the audio thread: control values
   are readable now (`runtime.controls().value(id)`), and a `Curve` is immutable
   data evaluable at any `t`. So "this branch is multiplied by something
   currently at zero" is derivable, and a faded-out effect correctly reads as
   not contributing. Per-note curves inherit tier 3's polyphony problem, because
   there is one curve instance per sounding note.

3. **Signal-dependent contribution** — a gain driven by an LFO, or anything
   downstream of audio. This one genuinely retains, and the only honest answer
   is metering: a tap publishing a level the UI reads at frame rate. fundsp's
   `Monitor` is the realtime-safe mechanism (an atomic store, no allocation).
   The complication is polyphony: a voice graph is instantiated per note, so one
   marked node has many concurrent instances writing one cell, and the semantics
   have to be "any instance is contributing" — an aggregate with decay, i.e.
   retained, racy, and fine for a decoration but not something to build before
   tiers 1 and 2 exist. Persistent patches, being single instances, are clean.

Recommended order: ship pattern highlighting (this document), then graph spans
for diagnostics, then tier 1, then tier 2. Tier 3 only if watching a mix
actually calls for it.
