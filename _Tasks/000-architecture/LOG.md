# 000 — Architecture: log

Append-only. Why things are the way they are, including the roads not taken —
which is the part `/CLAUDE.md` cannot hold, because it only records
conclusions.

---

## The goals everything else is derived from

These were never stated up front; they accumulated over the design session and
are written down here retroactively, because almost every decision below is
downstream of one of them.

**G1 — It must keep making sound.** This is the one that does the most work. A
live instrument may not stop because of a typo, an integer overflow, an
allocation on the wrong thread or a garbage collection. Every failure mode gets
converted into a diagnostic while the last valid program keeps playing. It is
why parse limits exist, why `Frac` uses `i128` intermediates, why no host
closure may reach the audio graph, and why the lookahead has to be long enough
to hide a GC pause.

**G2 — The same text always sounds the same.** Reproducibility is structural,
not promised. It is why randomness is a pure function of position and seed and
is spelled out rather than imported, why time is rational rather than float,
and why queries may not advance a cursor.

**G3 — Real synthesis, not sample triggering.** Stated outright: *"I'd actually
like to have a real synthesizer in v1, not just playing notes from a
wavetable."* It is why `poles.eod` exists, why the voice model is a function
from note to graph, and why every DSP primitive is bound in its modulatable
form.

**G4 — The editor must be able to show what is sounding.** This single
requirement is what forced the pattern to be an AST rather than a tree of
closures. A closure cannot say where it came from.

**G5 — It has to reach the browser eventually.** Possibly embedded in
`idiosepius` as study background audio. This rules out shipping a JS engine, and
rules out anything needing a C toolchain on `wasm32-unknown-unknown`. It is the
only surviving argument against Lua.

**G6 — The language decision stays reversible as long as possible.** Everything
is arranged so that picking wrong is recoverable.

**G7 — Layers must be liftable.** Each seam should be narrow enough to become a
process or ABI boundary later without disturbing what sits above it.

**G8 — Verifiable without ears.** `crates/pattern` has no audio and no
dependencies precisely so that its correctness is a test-suite question rather
than a listening question.

---

## Session 1 — 2026-07-28

### The opening question, and why it was the wrong shape

Asked: reuse an embeddable language (Lua?) or write a DSL — and explicitly, no
JS engine, Strudel's is too big.

The reframe that unblocked it: **Tidal and Strudel are not DSLs, they are
libraries in a host language.** Haskell donates the currying and operators;
Strudel uses JS method chaining. The only real DSL in either is the
mini-notation, a string parsed at runtime by a small hand-written parser.

So it was never one decision. It is two:

1. The mini-notation — write it ourselves, unconditionally. It is what you type
   80 % of the time and no embedded language provides it.
2. The host language — the actual open question, and one that could be deferred.

### Scope, established

Started as "a synthesizer inspired by Tidal/Strudel". Widened, knowingly
(*"in the spirit of scope creep"*), to study background audio for `idiosepius`
— *lulling waves, or alternatively, hardcore techno*. Those two poles are a
genuinely good spec: one exercises continuous signals and multi-second
envelopes, the other a rigid grid. Between them they cover the design. They are
now `waves.eod` and `techno.eod`.

### Rejected: the component model as the song format

Proposed: define a WIT interface and load arbitrary conforming blobs. The usual
objection — browsers do not support the component model — does not apply,
because `~/src/wasm-in-wasm` already runs `wasm_component_layer` over `wasmi`
inside a wasm app, with a `wit-derive` macro generating host bindings.

Rejected in that form, for a reason unrelated to feasibility: **producing a
component requires a compiler, and there is no rustc in a browser.** Live coding
is an edit-to-hear loop under ~100 ms. The component model buys distribution,
versioning and sandboxing; it does not buy authoring.

Kept in a different form: if it ever happens, **the component is the language
runtime, not the song.** The host owns editor, clock and DSP; the user's text is
data passed into a component that returns events. Swap the component, swap the
language. That serves G6 better than a Rust trait would.

Also settled there, and still binding: **the boundary is events, never
samples.** wasmi is an interpreter, and every list crossing the boundary is a
copy through `realloc`. Free at control rate, ruinous at 48 kHz.

### Rejected: in-browser compilation of an existing language

Counter-proposal: the source need not be Rust — WAT compiles in under a
millisecond via the `wat` crate, and other languages might compile fast enough.

Surveyed honestly and the list is thin. WAT works but is assembly.
AssemblyScript's compiler is TypeScript, so it needs the JS engine G5 forbids.
Zig, Grain and MoonBit are each either unproven as embeddable compilers or too
large.

Which lands on **write your own compiler** — and at that point (A) and (B)
collapse: your language emits blobs like everyone else, one loading path, no
interpreter and compiler to keep in sync.

The genuinely important thing that fell out: **plain core modules can go to the
host's real engine.** `WebAssembly.instantiate` JITs in the browser; cranelift
does natively. Neither is the component model and neither needs to be. That is
the only route to user-authored *audio-rate* DSP, and it is why wasmi is the
wrong engine for that tier and the right one for the pattern tier.

**Deferred, not abandoned.** Called overkill for v1 and correctly so.

### `~/src/da-beat` — fundsp, and the architecture already built

An earlier prototype: cpal + fundsp + midir, ~500 lines, no commits.

- **fundsp is the DSP answer.** Pure Rust to the bottom, `no_std` capable, the
  only C-adjacent dependency behind a default feature. Its operator algebra
  *is* a patchbay, which means we do not have to design one.
- **`fundsp::Sequencer` is the scheduling architecture, already written.**
  Absolute-time `push`, a `backend()` split where the frontend allocates and the
  backend renders, communicating over bounded lock-free channels. That is the
  control-thread/audio-thread discipline for free — and it is the same trick
  Strudel plays with Web Audio's `currentTime`.
- **What must change:** da-beat's `Channel` is a persistent graph with `Shared`
  pitch, i.e. a monosynth — which is why `DoomChannel` clobbers its own note.
  Patterns need polyphony, so the trait becomes a **factory**: a function from
  note to graph, instantiated per event. That serves G3 directly.

### `~/src/UAS/fpga/pisound` — offered as inspiration, kept as one constraint

An FPGA modular synth: a uniform module ABI (sample port, per-sample strobe,
config register bus) and an N×N runtime patch matrix.

Explicitly not going to be a backend. What it changed: it demonstrated the same
abstraction the software side has, which validated the seam (G7), and it
supplied one instruction — **the instrument and parameter set must be
declarative data, not identifiers hardcoded in a parser.** From one table come
the language's builtins, the editor's completions, the validator, and any future
backend's mapping. `gen_patch.py` and `wit-derive` are the same instinct twice
already; this is the third.

### The rate hierarchy, and the bug in my own examples

Question asked: *is the sample the lowest primitive?* Yes — `AudioNode::tick`
is the floor and everything in fundsp implements it. But that produced the
constraint the whole design now hangs on: **four rates, and a different answer
at each.**

The example patches written earlier contained `env(function(t) ... end)`. That
cannot work, and the reason is worse than speed: once `Sequencer::push` hands a
voice to the backend it is rendered **on the audio thread**, so an interpreter
callback there can allocate, which can collect, which is a dropout. G1 forbids
it outright.

Fix: **compile the expression sublanguage** — arithmetic, math, note params,
`t`/`x` — to a flat register machine over `f32`. Free at control rate. This is
the "write your own compiler" thread from earlier cashing out at a hundredth of
the size, and it needs no wasm.

Noted for later: this is also an argument against `mlua` specifically, since
compiling a closure body needs its AST and a bytecode VM will not hand one over.

### Curves: the abstraction question, then the better representation

Asked whether higher-level gestures could be built on "voice = note → graph" —
*increase volume linearly over x seconds*, *add a filter, ramp up, hold,
decrease*.

Answered yes, with three placements that use **different clocks** (note onset /
transport / bus) kept syntactically visible, because silently inferring which is
maddening. And with the rule that **"adding a filter" is a mix, not a rebuild** —
topology is fixed once built, so the effect is always present and its
contribution is automated. That is also how a hardware voice works.

Then the improvement, from the signal-theory side: an intro gate is just
`ε(t) − ε(t−T)`. So represent a curve as a **sum of shifted basis functions**
rather than a breakpoint list. Curves become a vector space — addition, scaling
and shifting are closed, and two breakpoint lists cannot be added without
merging their time grids. Breakpoints and ADSR survive as sugar over it.

Two consequences worth more than the elegance:

- It **unifies with the continuous-pattern side.** A curve is `t → value`; so is
  a Tidal continuous pattern. `shift` is `e^{−sT}`, `slow` is `F(s/a)/a`. One
  implementation, one vocabulary.
- A declarative curve is **compilable and realtime-safe by construction**, where
  a closure is not. So the constraint from G1 produced the nicer API rather than
  a worse one. Nobody should later "improve" it by accepting arbitrary
  functions.

### ε versus δ, and percussion as pole placement

The split is principled rather than arbitrary: **ε and ramp mean something at
both rates; δ only means something at audio rate** (sampled at 2 ms and
interpolated, an impulse is a 4 ms triangle). So the control basis is
step/ramp/decay/sine, and δ lives only in the DSP set.

Which makes δ the interesting one: ping a second-order section with it and its
impulse response is `e^{−ζω₀t}·sin(ω_d t)` — a struck bar. Percussion becomes
pole placement, and `t_se ≈ 3/(ζω₀)` gives the audible decay length directly, so
a drum is derivable rather than tweaked. `poles.eod` is a whole kit built this
way, with no oscillators and no samples.

### Naming

Four rounds. `Pyrosoma`/`Ctenophora` (too cephalopod-adjacent), `Bode` (two
namesakes — Hendrik's plot and Harald's frequency shifter — but too generic),
`Seiche`/`Sofar`/`Cavitation` (aquatic), and finally electric fish.

**Apteronotus** — the black ghost knifefish, whose electric organ discharge is
among the most stable biological oscillators known. (`Eigenmannia` was the near
miss: it detunes itself from neighbours to avoid beat frequencies, and has
"eigen" in the name. Apteronotus won on stability being the better metaphor.)

Song files are `.eod` — *electric organ discharge*, which is both the term for
what the fish emits and an accurate description of what the file is.

### Milestone 1 — `crates/pattern`

Built: rational time, spans, events, deterministic randomness, the pattern
algebra and the mini-notation, with zero dependencies.

The decisions inside it, each tied to a goal:

- **Rational time (G2).** Thirds must stay exact or nested triplets drift apart.
- **AST, not closures (G4).** Both reference implementations use closures; the
  tree is what makes source spans survive `every 4 rev`. There is a test for
  exactly that.
- **`whole` vs `part` (G1).** A buffer boundary bisecting a note must continue
  it, not restrike it.
- **Hand-written `rand` (G2).** A dependency improving its generator would
  silently reshuffle every pattern — the same reasoning as `idiosepius`'s
  `option_order`.
- **Notation desugars into thirteen node types (G7).** No special cases, so the
  tree stays serialisable and liftable.
- **`Timecat`, not `fastcat`.** They differ for nested alternation: `"<a b> c"`
  must step once per bar, not once per slot.

### Songs before runtime

Direction given: *"let's start in reverse, build a few songs, then implement
them."* Four songs written as the specification, in a syntax nothing parses.

This paid immediately. Ten findings, of which the load-bearing ones:

- **Events need a parameter map.** Every song needs `>> gain(...) >> pan(...)`
  and `Value` holds one scalar. Everything else is blocked on it.
- **Three things I wrote cannot work**: portamento has no previous note to
  glide from; `duck` is not a per-note parameter; `arp` needs chords to survive
  a `Stack` query. All three are in `README.md` as open questions.
- **Writing the songs made Lua *more* viable, not less.** Because the audio-
  thread rule forced every envelope to be a declarative curve, there is no
  longer anything to introspect — which removes the mlua blocker identified
  earlier. Only G5 (wasm) still argues against it.

### External review

Five findings, taken with the stated grain of salt. Outcome:

- **Valid, fixed**: error spans could point past end-of-input or split a
  multibyte character (an editor slicing them panics); `Frac` could overflow on
  `i64::MIN` through `abs()` and negation.
- **Valid, and the reviewer's example was wrong in an instructive way**:
  unbounded expansion is real, but `bd!1024!1024!1024` is harmless because
  repeats overwrite. The actual hole is *multiplicative* nesting —
  `[[bd*32]*32]*32` is ten AST nodes and 32 768 events, which no node budget can
  see. So limits are now parse-time (depth, nodes, counts) **plus** a
  `Pattern::density()` check on the built tree. Serves G1.
- **Already known**: the control map.
- **A documentation bug, not a code bug**: `onsets()` claimed to make
  overlapping scheduler queries safe. It does not, and it must not — two
  identical simultaneous onsets are legal music, so deduplication would be a bug
  in the other direction. The fix belongs one layer up, as a monotonic frontier
  in the scheduler, which is what both Tidal and Strudel do.

### What the reference implementations actually do

Checked because the frontier question above needed an answer.

Both share the model exactly: query-based pure patterns, `whole`/`part`, a
separate mini-notation parser, and — notably — **exact rationals** in both
(Haskell `Rational`, JS `Fraction`), which independently confirms G2's cost is
one people keep choosing to pay.

They differ in topology. **Tidal is three processes**: editor → GHCi (patterns)
→ SuperCollider (all audio), glued by timestamped OSC; `sound "bd"` is a string
thrown at something that may not exist. **Strudel is one process, two clocks**:
a coarse `setInterval` drives querying, the sample-accurate `AudioContext` clock
drives playback.

We are Strudel's topology with a Rust audio thread, `fundsp::Sequencer` in Web
Audio's role. Deliberate differences: AST instead of closures (G4), typed
`InstrumentSpec` instead of strings over OSC, and a merge rule that samples at
onset rather than emitting one event per right-hand event.

### Keeping the language decision open

Confirmed reversible, because nothing is committed: `crates/pattern` contains no
language, and its combinators take Rust closures that are **applied at build
time and never survive into the tree** — the same property that makes spans
work. A Lua binding would pass a Lua function to `every`, it would run once, and
a plain `When` node would land in the AST.

The price of keeping G6 is one structural decision, and it must be paid **before
`synth` is written**: the binding gets its own crate, and `pattern` and `synth`
never learn what the host language is.

The stronger move, and the next task: **transcribe the four songs into Rust
against the `synth` builder API before writing any interpreter.** If
`synthwave.eod` is awkward in plain Rust, the API is wrong and no language will
rescue it.

---

## Session 2 — 2026-07-28

Two independent architecture proposals were on the table (this directory's
notes and `codex-architecture.md`). This session settled the fork between them,
then discovered that the evidence base under *both* was too small, then
replaced it.

### The fork: when does `graph = function(n)` run?

Per note (an executable factory returning a live graph) or once per edit with a
symbolic `n` (a data `GraphTemplate` instantiated by the runtime)?

First pass at deciding it: read the four songs. There is **not one `if` in any
of them**, every loop is over literal constants (`for i = -3, 3`,
`ipairs({2810, ...})`, `ipairs(partials)` where `partials` is a table literal),
and every single use of `n.*` is a *value* in an arithmetic or port position —
`n.hz * r`, `n.ring / i`, `shape("tanh", n.drive)`, `mix(osc, wet, n.wet)`,
`pan(n.pan)`. Never a condition. So all four stage cleanly with a symbolic `n`
at **zero expressive cost**, and codex's warning that "this is a real
restriction" turns out not to be charged.

Two corrections to the earlier notes fell out:

- **`songs/CLAUDE.md` finding #8 was misattributed.** "Voice-build code needs
  real loops and tables" was offered as evidence for a scripting language at
  voice-build *rate*. It is not: those loops iterate constants, so they run at
  edit time and staging permits them freely. It argues for a real language at
  *evaluation* time, which nothing disputes.
- **Templates need Init-rate arithmetic on note parameters**, not merely "bake
  the scalars in". `ring(n.hz * r, n.ring / i)` solves a resonator bandwidth
  from a decay time, so there is a small expression to evaluate at
  instantiation. That is codex's `Rate::Init` earning its place.

Where this landed differs from codex's implementation order, though. It puts a
complete portable graph IR — versioned primitive registry, serialization,
cross-backend semantics — before any sound. That is weeks of designing an IR
against imagined requirements, in a project whose method has been the opposite
and whose first goal is *keep making sound*. **Keep the conclusion, take the
cheaper route**: make the builder's output data with a symbolic `n`, lower it
straight to fundsp, and defer the registry, versioning and bundle format. The
thing to avoid is not the factory as an implementation detail; it is
`fn build(&self, note) -> Box<dyn AudioUnit>` becoming the *public contract* by
accident.

### The evidence base was circular, so it was replaced

Four songs, written by the author, under a rule that already forbade closures in
graphs. That is not evidence. Replaced with a real corpus:

**723 snippets, 249 000 characters**, from three sources — Strudel's own docs,
tunes and test corpus (609 snippets; the repository has moved from GitHub to
`codeberg.org/uzu/strudel`), `github.com/eefano/strudel-songs-collection` (110
files: 86 songs, 13 helper functions, 9 experiments, 2 jam sessions), and four
complete songs decoded out of the base64 payloads in `strudel.cc/#…` links
collected by `github.com/terryds/awesome-strudel`.

**Caveat recorded up front:** 86 of the 110 song files are by one author, so his
guitar-tablature idiom and his tuple-indexing helper are over-represented. The
docs corpus has the opposite bias — pedagogical one-liners. Neither is a random
sample of real usage, and the percentages below should be read with that in
mind. `corpus/audit.py` re-runs the whole thing if a better corpus turns up.

*(Figures below are quoted against the 110-file eefano collection. Re-running
`corpus/audit.py` prints denominators of 114, because it folds the four
base64-decoded songs into the song bucket rather than counting them apart; a
couple of feature regexes there are also slightly broader than the hand counts.
The shape of every finding is unchanged.)*

### What the corpus says

Classified into seven buckets rather than representable yes/no:

| class | constructs | songs |
|---|---|---|
| **1 — directly representable** | every parameter setter (`gain` 525, `s` 449, `room` 262, `lpf` 220, `clip`, `velocity`, `pan`, `release`, `speed`, `delay`, `attack` …); `slow` `fast` `struct` `mask` `late` `rev` `off` `segment` `degradeBy` `stack` `timeCat` `ply` `iter` `chunk`; `add` `sub` `mul` `range` | ~all |
| **2 — construction-time helper** | `register` (44 definitions), `arrange`, `apply`, `all`, and every closure passed to `layer` (112), `superimpose` (21), `off` (20), `sometimes` (9), `jux`, `echoWith` | 36/110 define their own |
| **3 — needs a new bounded IR node** | join family `pickRestart` 182 / `pickOut` 77 / `pick` 40 → **65/110**; scales `scale` 178 / `mode` 38 → **63/110**; chords and voicing `voicing` 124 / `anchor` 94 / `chord` 62 / `dict` / `rootNotes` → **53/110**; tuple field-select → 18/110; `arp` | — |

### Which class-3 findings survive the sampling bias

The per-class counts above hide something the two corpora disagree about.
Splitting them (songs = eefano, docs = Strudel's own) separates a real feature
from one author's habit:

| feature | eefano songs | docs snippets |
|---|---|---|
| scales / modes | 63/110 | **75** |
| chords & voicing | 53/110 | **25** |
| join family (`pick*`) | 65/110 | **3** |
| `register` (own helpers) | 36/110 | 1 |
| per-event closure | 38/110 | 4 |
| tuple / multi-value | 18/110 | **0** |

**Scales and the chord/voicing layer are the robust findings** — heavily used in
both corpora, by different people, for different purposes. Those two are real
and are the largest omission from both architecture proposals.

Everything else in class 3 is eefano-weighted, and the tuple idiom is *entirely*
his: 18 songs, zero doc snippets. Designing `Value::List` around it would be
designing around one person's style. The join family is genuine Strudel API
(verified below) but only three official snippets exercise it, so its 65/110 is
one author reaching for it constantly rather than evidence that everyone does.

This is exactly the bias flagged when the corpus was assembled, and it is why
the corpus should influence *which pattern nodes come next*, not decide what the
foundation is.
| **4 — external / live input** | MIDI (2 songs, 13 doc snippets), keyboard/DOM (3), hydra (1) | 4/110 |
| **5 — arbitrary query-time callback** | `markovchain.js`, `stacktricks.js` | **2/110** |
| **6 — setup only** | `samples` (46), `bank` (123), `setcpm`/`setcps`; visualisation `color` 154, `_scope` 33, `pianoroll` 23 | 42/110 visualise |
| **7 — outside our semantics** | hydra, csound, async setup | 0–1/110 |

**The unexpected result: in 110 real songs there is not one `Math.random`, not
one `Date.now` or `performance.now`, not one `eval`, not one `fetch`, and not
one query-time array generation.** Strudel hands users unrestricted JavaScript
and they essentially never reach for ambient impurity. G2 is close to free in
practice, which is a much stronger position than "a cost people keep choosing to
pay".

Keeping the two questions apart, as they should be:

- *Run the original program unchanged* — ~0 %. It is JavaScript. Never a goal.
- *Express the resulting music* — everything except **one** song, once class 3
  exists.

### The closures are mostly not what they look like

38 of 110 songs use a query-time closure, which reads like a refutation of
AST-not-closures. All 54 call sites were dumped and read. The overwhelming
majority are userland workarounds for features already planned:

- **~20 sites are one copy-pasted idiom**, verbatim across ten songs:
  `withValue(v => Array.isArray(v) ? (i < v.length ? v[i] : d) : …)`. That is
  hand-rolled "get field *i* of a multi-value event". A control map plus a
  field-select node *probably* absorbs it — but see the caveat below, because
  these values are ordered, not named, and the difference is not cosmetic.
- **~6 are string parsing** of chord and note symbols
  (`v.endsWith('m') ? [.., 'minor'] : [.., 'major']`, digging `#`/`b` out of
  note names). That belongs in the parser, not at query rate.
- **~5 are plain arithmetic**: `v + 1`, `v % r`, `440 * 2**(n/12)`. A compiled
  expression, per the existing rate hierarchy.
- **The rest return a pattern** and then `.innerJoin()` — which is the missing
  join node again, not an argument for closures.

After subtraction, **2 files in 110** genuinely need a retained query-time
callback: `markovchain.js` and `stacktricks.js`. And `markovchain.js` achieves
it by mutating a global keyed on `hap.whole.begin` — it depends on query order
and would fail `tests/algebra.rs`'s overlapping-window test. Forbidding it is
correct, not a limitation.

**Therefore: dynamic query-time patterns are not a v1 feature.** 1/110 for a
musical reason. The cost is not only realtime safety — a retained closure has no
source span, so G4 degrades exactly where the pattern is most dynamic, which is
the worst place to lose the editor highlight. Keep it as a designed door with
budgets and an explicit marker; do not build it.

### `Value::Map` does not obviously subsume ordered multi-values

The claim "the tuple idiom disappears once events carry a control map" was made
too quickly. A control map solves `{note, gain, pan}`. It does not
self-evidently solve `[string, fret, articulation]`, where position *is* the
meaning. Reading the actual sites, they split three ways:

- **Genuinely ordered and homogeneous.** `blackbird.js` and the tablature
  experiments index six guitar strings by number; `clandeisiciliani.js`
  destructures `(chord symbol, anchor index)` pairs; `timeconv.js` handles
  `(magnitude, unit)`. Position carries meaning and the arity is fixed.
- **Ordered only because the notation could not produce names.**
  `pumpupthejam.js` and `rhythmofthenight.js` define a helper called `as` whose
  entire body is `Object.fromEntries(mapping.map((prop, i) => [prop, v[i]]))` —
  literally "give these positional values these names", applied the instant the
  values leave mini-notation. This is a user routing around a missing feature.
- **Actually a typed musical value.** `(root, quality)` from chord-symbol
  parsing, and `(magnitude, unit)`, are not tuples that want generic indexing;
  they want to be chord symbols and durations. Those belong in a music layer.

So the question is open and worth answering deliberately: `Value::List(Vec<..>)`
as a notation-level construct with a `Named`/`as` node lifting positions into a
map, versus making the map the only multi-value form. The evidence leans towards
**both, in that order** — the notation produces positional groups, and naming is
a separate explicit step — but it is one author's corpus, and the decision
should be tested against a second one before it is fixed.

### The select/join family: the temporal mode cannot be implicit

A bare `Pick { selector, branches }` node is not enough, and Strudel's own
source says so. `packages/core/pick.mjs` exports:

```
pick  pickF  pickOut  pickRestart  pickReset  squeeze
```

each with a `mod` variant. That is **two orthogonal dimensions**:

- *alignment* — how the branch's own time relates to the selecting event: in
  (plain `pick`), out, restart, reset, squeeze. `reset` and `restart` differ
  only when the branches vary between cycles, which is precisely when it
  matters.
- *out-of-range policy* — clamp at the maximum (default) versus wrap (`mod`).

So the node is closer to:

```rust
Pattern::Select {
    selector: Box<Pattern>,
    branches: Vec<Pattern>,
    mode: SelectMode,      // In, Out, Restart, Reset, Squeeze
    range: RangePolicy,    // Clamp | Wrap
}
```

Names can wait; the explicitness cannot. And two things every mode needs a
definition for before any of it is written: **`density()`**, since the parse-time
budget has to see through a select, and **source-span propagation**, since a
squeezed branch's spans have to survive into the editor highlight. Neither is
obvious for `squeeze`.

Worth noting that real usage barely exercises the alignment modifiers on
*arithmetic* — `.add.out`, `.add.mix`, `.add.in` appear five times in the whole
corpus — so the generality is needed for select, not everywhere.

### Scales and voicings do not belong in `crates/pattern`

`crates/pattern` has no dependencies and no musical domain knowledge, and it
should keep both properties. Typed pitches and accidentals, scales and modes, chord
symbols, voicing dictionaries, anchors and inversions, and pitch ↔ frequency
conversion are pure deterministic transformations, but they are a *music*
concern, not a time-algebra concern. They want their own layer —
`crates/music` — sitting beside `pattern` rather than inside it.

The part that needs testing rather than assuming: some of those inputs are
themselves patterned in real songs (`.transpose("<~@16 -12@24 …>")`,
`.anchor(...)` fed a pattern). So the boundary between construction-time
expansion and a runtime node is not obvious, and picking wrong puts music theory
back inside the query loop.

### What the corpus cannot settle, and where to look instead

Bluntly: **it does not test the graph fork at all.** Strudel has no
user-defined synth graphs — sound is samples plus a fixed set of superdough
synths, and every `.lpf` (220), `.gain` (525), `.attack`, `.fm` is a setter on a
*declared parameter*. That is strong independent support for
`InstrumentSpec`/`ParamSpec` as data, which both proposals already agreed on,
but nothing in 249 000 characters builds a graph.

The corpus that does bear on it is **SuperCollider SynthDefs** — and note that
SynthDef *already is* the staged model: the function runs once with symbolic
UGen inputs and emits a portable graph as bytes over OSC, with no language
present at play time. That is a large existence proof on the staging side. A
second audit over SuperDirt's synths, sccode.org, Faust examples and
Mutable-Instruments-style voices is the equivalent pull, and is what should
stress `GraphTemplate`, persistent patches, feedback and audio-rate routing.
Not done yet.

### The requirement changed: full language on the web, serialization optional

Stated directly: the full language must run in the browser; being able to
serialize a graph and play it later independently is "kinda neat" but not
required, because rendering to an audio file covers that need. And the fallback
for Lua-in-the-browser is acceptable: reimplement Lua itself in Rust.

This **removes portability as the justification for staging** — but staging
survives on its other merits: validation before publication, predictable
resource use, no graph-topology work per onset, source locations on graph nodes,
preparation away from the audio thread, and clean graph replacement. So the
decision stands; only its basis changed. G5 is now a hard v1 requirement rather
than a distant argument, and serialization drops to three lazy milestones:
data-only *in principle* now, a temporary encoding with a native round trip once
several instruments play, a versioned schema only if it is ever actually needed.

### The web-Lua route has a hole worth catching before the spike

mlua on `wasm32-unknown-emscripten` is real. But **wasm-bindgen does not
support the emscripten target** (`rustwasm/wasm-bindgen#2722`), and the WASM
emitted by rustc for the two targets is ABI-incompatible. So it is not "link
mlua into the web app". It is either move the *entire* application to emscripten
— taking egui/eframe with it, off its supported path — or ship two modules
talking through JS, which puts a serialisation boundary between Lua and the
pattern IR that every combinator call has to cross.

Which makes the pure-Rust option the *first* thing to try rather than the
fallback. **piccolo** (`github.com/kyren/piccolo`, formerly luster, by the
author of `gc-arena`) is a Lua VM in pure Rust: `wasm32-unknown-unknown`, one
module, direct interop, no emscripten toolchain. More to the point it is written
stackless/trampoline-style explicitly for sandboxing and resilience against
untrusted-script DoS — which is the instruction-budget and
interrupt-an-infinite-loop requirement as a *design goal* rather than a debug
hook bolted on afterwards. That serves G1 better than mlua does.

Its coverage is the open risk: piccolo is explicitly experimental and its
stdlib, coroutine and metatable support need measuring. But `CLAUDE.md` already
concluded the realistic route is *a small language that looks like Lua* — under
that target piccolo's gaps are the specification rather than a defect. If the
evaluator ends up hand-written, `full_moon` (StyLua's parser) supplies Lua
syntax in pure Rust.

### Decisions taken this session

- **Staging wins.** `graph = function(n)` runs once per edit with symbolic note
  parameters. A dynamic per-note factory stays available as an explicitly marked
  door, not the default.
- **The merge rule is committed**, not a coin flip: if a voice is instantiated
  at onset with its parameters baked in, Tidal's one-output-per-right-hand-event
  rule would instantiate N voices for one note.
- **`key()` is not decoration and must not be deleted.** It was "define or
  delete" on the evidence of four songs; on 110 plus the docs it is one of the
  two robust findings (`scale` 178 calls, 63/110 songs, 75 doc snippets). Its
  *semantics* stay open: the intended meaning is a program-level tonal context
  that degree notation resolves against. It must be neither decorative nor
  hidden mutable scheduler state — those are the two failure modes to avoid.
- **A chord and voicing layer exists and appears in neither proposal.** 53/110
  songs, 25 doc snippets. The largest single omission this audit found, and it
  belongs in a new `crates/music`, not in `crates/pattern`.
- **A select/join node is required, with an explicit temporal mode.** Not a bare
  `Pick`. See above: five alignment modes and a clamp/wrap policy, plus a
  `density()` and span rule per mode. In nearly every real use the branch list
  is a build-time literal array, so it stays serialisable with spans intact.
- **Multi-value events: ordered versus named is unresolved**, deliberately. See
  above. Do not assume `Value::Map` absorbs the tuple idiom.
- **External input gets a node, not an escape hatch.** MIDI and keyboard state
  are legitimate and appear in real songs; an arbitrary callback reading ambient
  state is not the mechanism. Something like
  `Pattern::External { source: ControlId, sampling: SamplingMode }`, so the
  dependency is declared, the value is sampled at a defined time, and a program
  that uses it is visibly not reproducible. The order-dependent Markov chain
  stays unsupported on purpose: it violates pure querying, which is the property
  `tests/algebra.rs` exists to defend.
- **`+` is mix, `&` is fan-out.** Consequence to remember: `poles.eod` wants
  both rings *summed*, so its `ring(587, …) & ring(845, …) * 0.7` becomes `+`.
  The songs get edited, not just the documentation.
- **Ordinals**: prefer names; where a user-visible ordinal must exist it is
  1-based. IR vector indices stay ordinary 0-based implementation details.
- **Visualisation is not an afterthought.** 42/110 songs call `color`,
  `pianoroll` or `_scope`. That is 38 % of real songs, and it is G4's constituency.

### Addendum — the sandbox contract, and a correction

Two things arrived after the above was written.

**"Full language" was the wrong phrase, and it was mine.** Objected to, rightly:
we were never going to implement OS-level file access, and a Lua sandbox is
*expected* to expose a subset. So the requirement is not a complete PUC Lua
distribution — it is **the same Apteronotus sandbox, with full relevant language
semantics, on desktop and web**. That distinction does real work: it moves
piccolo's missing `io`/`os`/`package` from "significant gap" to "feature of the
sandbox", while keeping `string` and `table` as genuine blockers, because those
are core language behaviour and not peripheral. The exposed environment is
enumerated in `/CLAUDE.md` so it cannot quietly drift between the two targets,
which is the whole point of writing it down.

Two consequences worth keeping: `math.random` is replaced by the pattern
algebra's seeded randomness rather than merely omitted (G2), and `load()` is
omitted initially — the corpus contains *zero* dynamic evaluation, and it would
complicate source attribution and resource accounting for no demonstrated
musical benefit.

**The emscripten fallback was overstated here, and the overstatement was mine.**
The claim above — that a two-module route puts a JS boundary "that every
combinator call must cross" — is wrong. Lua would construct the entire program
*inside* the emscripten module, against the same Rust pattern and graph crates
compiled into both modules, and transfer **one encoded `Program` per edit**.
Since both modules ship from the same build they can share an exact schema
version, so the format need never be stable or public. That is less elegant than
piccolo but far cheaper than described, and it preserves real PUC compatibility.
The ladder is therefore: piccolo → fill piccolo's bounded gaps → emscripten with
one transfer per edit → our own VM.

**Piccolo verified at 0.3.3 (published June 2026)**, since the above was written
partly from memory. Present: closures with proper upvalues, proper tail calls,
varargs, coroutines yielding transparently through Rust callbacks, `_ENV`, fully
recursive metamethods, safe-downcasting userdata, an incremental cycle-detecting
GC in the style of PUC 5.3/5.4, execution fuel, and accurate memory accounting
inside its `gc-arena`. Missing or sparse: `io`, `file`, `os`, `package`,
`string`, `table`, `utf8`; no stack traces; no debugger, probably ever; poor
error messages; frequent pre-1.0 API breakage.

The missing `debug` library has a consequence that deserves its own line:
**exact call sites must come from source transformation, not a stack walk.**
That is the better answer regardless — it yields byte-accurate spans where
mlua's debug info only yields lines, which matters for G4 — but it is work to be
done rather than a property to be assumed.

---

## Session 3 — 2026-07-28

One test case, run in parallel by both threads: *could we build Vangelis's
Blade Runner out of raw components?*

It turned out to be a better probe than either corpus, for a reason worth
stating. The Strudel corpus provably cannot test the graph model — Strudel has
no user-defined synthesis, so 249 000 characters contain zero graphs. The four
songs were written by the author, under the rules the architecture already
imposed, which session 2 already flagged as circular. Blade Runner is neither:
an externally specified target, with a documented instrument, chosen by nobody
for being convenient.

### The synthesis was never the problem

The Yamaha CS-80 is textbook subtractive and contains no wavetable anywhere.
Per channel: one VCO producing sawtooth, with square/pulse and sine derived
from it by waveshaping, plus noise in the same mix; then a resonant high-pass
and a resonant low-pass in series; then a VCA. Two complete channels per key
across eight voices — sixteen VCOs, and the thickness is *layering*, not unison
detune of one oscillator. Yamaha's own manual attributes the organic quality
partly to component tolerances making successive voice circuits measurably
different, which we can implement directly as seeded per-voice offsets and get
determinism for free where the original had drift.

Every operation in that path is already in the primitive list, and the voice
stages cleanly under the symbolic-`n` rule with nothing left over. The CS-80's
unusual filter envelope — initial level, attack overshoot, settle — is *easier*
in the basis-sum representation than in an ADSR, because an overshoot is
literally a decay basis added onto a step. A rare case of the realtime
constraint producing the nicer API rather than a worse one, which is the same
thing the curve representation did in session 1.

So the conclusion is stronger than "we could probably imitate it." The
synthesis graph is already sufficient. **Everything the test exposed is on the
other side: the score model, the gesture model, and control routing.**

### Three corrections, recorded because they were mine

**Blade Runner does not break the query model.** The claim that "the pattern
algebra is cyclic by construction" was too strong. `query(timespan) -> events`
says nothing about repetition; what is cyclic is the *constructor set* —
`Slowcat` steps per cycle, `Fast` and `Shift` are cycle-relative, and
`When { modulo, offset }` is literally modular arithmetic on cycle number. A
finite `Timeline { events, extent }` node queries purely, is idempotent over
overlapping windows, carries spans per event, and has the most trivial
`density()` of any node in the tree. It costs nothing and it does not displace
anything. The algebra needed a constructor, not a replacement.

**A ribbon gesture does not inherently need `patch`.** A bend inside one
sustained note is a note-relative curve read continuously by a per-note voice,
and most of the famous gestures on that record are exactly that — long held
lead notes. The claim survives only in narrowed form: **legato** pitch sliding
*through* a note boundary needs persistence, and so does a live ribbon steering
whichever voice is currently sounding. The rule is `voice` unless the
persistence is musically observable, which is a better rule than "gestures sit
beside `patch`."

**The CS-80's two filters share one envelope.** The HPF and LPF each have their
own cutoff and resonance, but they are driven by a single per-channel IL–AL–A–D–R
envelope generator with separate depths, not by two independent envelopes. This
makes the voice sketch simpler rather than more complex — one curve, two depths
— and it is still the right stress test for the basis sum.

### The missing abstraction, and the shape it should take

Both threads independently reached the same gap: **a sounding voice needs
control-rate inputs that remain writable after onset.** Codex's framing was
that parameters are sampled at onset and this is insufficient. That is slightly
off about what already exists — `adsr(secs(0.9), …)` inside a voice graph *is*
a per-voice control-rate signal on the note clock, and that is curve placement
#1 from `CLAUDE.md`. The graph side has been done since session 1.

The gap is narrower and sharper than that: **`ControlMap` values are scalars, so
an event can hand a voice a number but not a signal.**

Stated that way the fix is one variant, not new machinery:

```rust
enum Value { Number(f64), String(..), Bool(..), Curve(BasisSum, Clock), … }
```

`n.pressure` being control-rate then falls out of the existing rate rules with
nothing added to the graph model. Check it against the committed merge rule: a
zero-width query at the left onset returns the curve *as a value* — you do not
sample it, you carry it whole and bind it to a port at Init rate as a signal.
Still pure, still data, still no closure that could reach the audio thread.

This is preferred over a parallel `GestureMap` beside `ControlMap`, for two
reasons. It avoids two kinds of parameter with two plumbings and two merge
paths. And it gets polyphonic aftertouch right: the musical point of poly-AT is
that **each note in a chord carries a different contour**, and a gesture map
merged onto a phrase applies one curve to every event in it. If curves are
ordinary `Value`s then `pressure("<soft swell hard>")` is a pattern like any
other and per-note variation is free.

> **Correction, same session.** The last sentence is wrong and the reason is
> the merge rule. Simultaneous events share an onset, so a zero-width query
> hands all of them the same value; alternation varies per cycle, not per
> member of a chord. `Value::Curve` makes independent contours *possible*, not
> automatic. The first reason above — avoiding two plumbings — still stands,
> and the correct home for per-voice variation is construction time. See
> "The merge rule cannot see inside a simultaneity" below.

Codex's three-way taxonomy — Init parameter, authored gesture, live mapping —
is right and worth keeping. The point is only that Init versus gesture is a
property of the *value's type*, not of which map it lives in, and that
`GestureClock::{NoteRelative, TransportRelative}` is the clock that `CLAUDE.md`
already requires to stay syntactically visible. It belongs on the value.

**This is the urgent finding in the whole session.** Step 2 fixes the shape of
`Value`, it is the next code to be written, and the only open question recorded
against it was *ordered versus named*. Nobody had raised *signal versus scalar*.

### Where the two threads disagreed: `Mapping` and `ParameterPath`

Codex proposed a time-scoped external-control routing object:

```
map { during, source, target = acid.cutoff, transform, combine = "add" }
```

Pushed back on, and the disagreement is only partly resolved.

The objection: `ParameterPath` introduces a **global namespace of addressable
parameters** into a system that is otherwise entirely lexical and dataflow.
Today `play(acid, notes >> cutoff(riser))` is a value flowing into a merge, and
you can read what determines a parameter from the code beside it. `map` is
action at a distance into a named slot; once two mappings target one path you
inherit precedence, ordering, and *why is my cutoff wrong* as a debugging
category. That is DAW automation-lane semantics grafted onto a functional
pattern algebra.

And most of it is already expressible. `synthwave.eod` line 104 is
`1 - line(0, 1, bars(8)) >> shift(bars(56))` — signals compose with arithmetic
and shift, so `combine = "add"` is `+`, `during` is `window`, `transform` is
`scale` and arithmetic, and `enter`/`leave` is the window's edge shape, which
the basis sum gives for free by swapping `step` for `line`:

```lua
play(acid, notes >> cutoff(riser + knob("filter") * window(bars(16), bars(32)) * 1200))
```

**What survives the objection, and is agreed:** `InputSpec` and logical-name
binding. `knob("filter")` naming a logical control that the shell binds to CC 74
or an on-screen slider, device identity never entering the song, and a missing
device degrading to its default with a diagnostic rather than stopping playback.
That last part is G1 and is not negotiable.

**What the objection does not cover, conceded:** `ParameterTarget::Voices`.
Routing live poly-aftertouch on note 64 to the voice currently sounding note 64
genuinely cannot be written as a signal in a score, because voice identity does
not exist at score level. That needs a mechanism.

So the resolution is that two different things were bundled. Voice-addressed
live expression is a **note-expression routing** problem — narrow, real, and it
lands on the same graph port the authored curve uses, which is the good half of
Codex's diagram. General automation with `ParameterPath` + `CombineMode` +
`during` is a second system that duplicates the signal algebra, and should not
be built. The residue of `map` beyond note-expression routing is a *binding UI*
over `InputSpec` — grab a knob, assign it live without editing text — which is a
host feature, not a language one.

### Two wrinkles neither thread caught

**Normalized gesture time does not survive live notes.** Curves written
`{0.00 … 1.00}` over "that note's lifetime" assume the lifetime is known at
onset. It is, for a sequenced note; it is not for a held MIDI key. So either
gestures are in absolute seconds from onset with the normalized form as sugar
that requires a known duration, or live input cannot share the lane. This is
the same assumption `CLAUDE.md` already leans on when it notes that
`adsr_live` wants a gate signal but "a sequencer-instantiated voice knows its
length up front." Gestures are where that assumption first costs something.

**A tempo map breaks tempo-relative durations baked into persistent graphs.**
`tempo(104)` is currently a scalar and cycles → seconds is one multiplication.
Under a tempo map it becomes a piecewise integral, and `beats(0.75)` no longer
has a single answer — it depends on *when*. `synthwave.eod`'s
`tape = send { graph = delay(beats(0.75)) … }` is exactly this: a tempo-relative
duration inside a persistent graph that outlives any particular tempo. Absolute
`secs(…)` durations are unaffected, so the seam holds; but the songs' rule that
"tempo-relative and absolute are different things and guessing is the classic
sequencer bug" gets sharper, and tempo-relative durations in persistent graphs
need either resolution at instantiation or prohibition.

Two smaller notes on the tempo map. It should be explicit whether
`{ bars(0), 52 }, { bars(4), 47 }` steps or ramps — an accelerando is a musical
requirement and the sketch is ambiguous. And if tempo is a signal, position is
its integral, which is elegant but not closed: step and ramp integrate to ramp
and quadratic, so the basis would need extending or the map restricting to
piecewise-linear.

Also worth keeping apart: a **tempo map** is composed, deterministic and
reproducible, and the transport stays a known function of time. **Rubato** as
humanization is per-event timing jitter, which is `late` fed a seeded signal.
Both are wanted; they are different mechanisms and conflating them would lose
G2 for no reason.

### Live audio input

Agreed in full, with one addition. Codex concludes a live input belongs in a
`patch` or bus because it exists continuously. Probably true, but there may be a
blunter reason: `fundsp::Sequencer::push` takes units that are generators, and a
voice consuming an audio input is not one. If that holds, audio input cannot use
the voice lifetime *at all* on the first backend regardless of the conceptual
argument, and lands on `Net`/bus. **To be checked against fundsp's actual
signatures** before anyone designs around it.

The rule that audio → control crossings are named nodes (`envelope_follower`,
`rms`, `pitch_tracker`, `onset_detector`) and never implicit port coercion is
the same rule as "higher-rate signals cannot be silently collapsed," and should
be stated once for both.

**Recording at the input boundary** is the right answer on reproducibility: a
recorded lane is data, so it converts a non-reproducible performance back into a
reproducible program, and offline rendering comes free. It is the honest
complement to `External` being *visibly* impure rather than quietly so.

One landmine surfaced while writing `jamming.eod`: an **audio onset detector
projected into the pattern layer cannot participate in lookahead.** You cannot
query the future of a live trigger stream. Any external source that generates
*events* rather than values can only ever schedule at now-plus-minimum-latency,
which means it is a different kind of object from a pattern and must not
silently look like one. This is precisely what writing songs ahead of the
runtime is for.

### The reverb is the highest-leverage item for this target

Both threads reached it. A large modulated space is a bigger fraction of that
record than any oscillator decision, and fundsp's built-in reverb being merely
serviceable is what would land the result at *close but not it*. The
consolation is that predelay → early reflections → diffusion allpasses →
modulated feedback matrix → damping is composition of delays, allpasses and
matrix mixing, so under the two-tier rule it is stdlib, not a Rust primitive
and certainly not one opaque `blade_runner_reverb()`. Only the interpolating
delay lines are irreducibly stateful.

### Two new specification songs

Neither transcribes the copyrighted composition; both are original cues that
require the same machinery.

`neon.eod` (Codex) — finite through-composed arrangement, tempo changes and
off-grid entrances, extended jazz voicings, two-layer CS-80-style raw
synthesis, deterministic per-voice drift, multi-second note-relative pressure
and bend gestures, a persistent monophonic lead, a large modulated reverb send,
and one external control mapped to lead expression.

`jamming.eod` — the other half, which `neon.eod` does not reach and no Strudel
snippet can. Named for the jamming avoidance response: *Apteronotus* and
*Eigenmannia* shift their discharge frequency away from a neighbour's to avoid
beating against it, which is a feedback loop through the outside world — emit,
sense, move. The song makes a live input the neighbour and detunes away from
it. It stresses `audio_in` as a graph source, the audio → control crossing,
`patch` as a persistent *rack* rather than as a monosynth (a second and quite
different justification for the lifetime), the pattern algebra driving effect
parameters rather than notes, external triggers and their lookahead problem,
and a piece that is deliberately not reproducible until its input is recorded.
`Eigenmannia` was the near miss in session 1's naming; this is what it was
holding the door for.

### What the two songs revealed once both existed

Written independently against the same conclusions, which makes their
disagreements informative rather than merely annoying.

**`neon.eod` refutes its own author's struct.** The Rust sketch in the same
session proposed `struct VoiceEvent { initial: ControlMap, gestures: GestureMap }`
— two maps, two plumbings. The notation it then produced is
`>> pressure(curve { … })`: an ordinary parameter setter taking a curve value,
formally identical to `>> gain(0.8)`. That only works if `Value` can hold a
curve. Two threads reached the same place from opposite directions, one through
the type system and one through the notation, and **when the notation and the
struct disagree the notation is the specification** — that is the entire premise
of writing songs before the runtime.

**It also answers the normalized-time wrinkle**, better than either thread's
prose did. `phase(0.00 … 1.00)` is legal only where `hold(…)` makes duration
known; a live note uses absolute note-clock `secs(…)` offsets and an explicit
gate. Both forms exist, and the illegal combination becomes a diagnostic instead
of a silent misinterpretation. That is the right shape and it should survive.

**Three syntactic divergences to reconcile before implementing either.**
External source declaration (`audio_input { … }` individually versus one
`inputs { }` block with `audio_in(…)` accessors); `velocity` versus `vel`, which
is finding #7 arriving in the one place it was certain to; and `tempo { … }`
versus `tempo(72)`, which should be one construct with the scalar as sugar for a
single-entry map rather than two.

**And one thing neither song can say.** A `voice` sounds when it is `play`ed; a
`patch` has no events to be played by, so it is unclear whether declaring one
instantiates it or whether there is a missing `run(…)`. The four cyclic songs
never had to decide because none of them contained a persistent graph that was
not a `send`.

### The merge rule cannot see inside a simultaneity

The strongest correction of the session, and it lands on the claim this thread
was most confident about.

`Value::Curve` was argued for partly on the grounds that it makes per-note
gesture variation fall out for free. It does not. The merge rule samples the
right-hand pattern with a zero-width query at the *left event's onset* — and the
three events of a struck chord share one onset, so they receive one value.
`pressure("<soft swell hard>")` alternates per cycle, not per chord member.
`neon.eod`'s `held_chord` therefore gives every voiced note the same pressure
contour today, which means the pair of songs does not yet demonstrate
polyphonic expression at all, only per-*note* expression on monophonic material.

The general statement is more useful than the gesture-specific one, and it is
not a limitation of curves: **no merged parameter can vary across simultaneous
events.** Pan, velocity, detune, send level, all of them. A strummed chord works
because its onsets genuinely differ; a struck one cannot. This is a property of
the merge rule that was committed in session 2 for good reasons — one voice per
left onset, no combinatorial blowup — and it is the cost of that choice, now
priced.

Two things follow.

**It is the same blind spot as `arp`.** Finding #6 in `songs/CLAUDE.md` says the
arpeggiator has to recover "these three share a `whole`" because a query returns
a `Stack`'s events unordered. That is the identical gap seen from the other
side: the query and merge layer cannot address individual members of a
simultaneity. Two findings, one cause, and probably one mechanism.

**The fix is construction time, in `crates/music`.** A voicing knows how many
notes it produced and in what order, so it is the only layer that can attach
per-voice values before the notes become a `Stack`. Two shapes, choice open: a
voicing expands to an ordered list that gets values zipped onto it — which drags
in the ordered-versus-named question and would settle it by fiat rather than on
evidence — or the music layer takes per-voice value *specs* and does the zip
internally, never exposing a list to the pattern layer. The second is preferable
given session 2's finding that the tuple idiom is entirely one author's, and it
costs only a build-time closure, which the combinators already permit and which
never survives into the tree. No query-time closure is required either way,
which is the important part.

### Syntax divergences, resolved

`jamming.eod` lost four of five and has been brought into line.

**External sources are lexical.** `local neighbour = audio_input { name = … }`,
not an `inputs { }` block with `control("field")` string accessors. This thread
proposed the block form and then argued against `ParameterPath` on the grounds
that a string-keyed global namespace is action at a distance in a language that
is otherwise lexical — the same objection applies to its own syntax, and it was
not noticed until both songs sat side by side. The `name =` string survives as
the *external* identity the host binds against; the local binding is the
internal one. Device identity still never enters the song.

**`velocity` everywhere**, including `n.velocity`. Four saved characters are not
worth two spellings of one concept.

**`tempo(72)` desugars to a one-entry `TempoMap`**, with missing `over` meaning a
step and explicit `over` meaning interpolation. That also answers the
step-versus-ramp ambiguity flagged earlier in this session.

**Declarations are inert; `play(patch, notes)` drives a persistent instrument
and `run(patch, span)` activates an autonomous rack.** This is the better answer
because it covers both songs with one rule and gives time-scoping for free. The
property worth naming: `play` is the same call for a `voice` and a `patch`, so
moving an instrument between polyphonic and persistent does not touch the score
— which is the same payoff `params`-as-data gave, arriving again.

**An external trigger is its own type**, a live event stream with no query or
lookahead contract, so it cannot be mistaken for a pattern. The elegant part is
what recording does to it: a recorded trigger stream becomes an ordinary finite
`Timeline`. One conversion closes the lookahead hole and the reproducibility
hole at once, and it is the same conversion the recorded-input-lane story
already needed. Three session-3 findings — `Timeline`, live triggers, recorded
reproducibility — turn out to be one mechanism.

### Documentation corrections

`neon.eod` maps its external control to lead amplitude and expression, not to
per-voice brightness; the earlier entry in this session said brightness. And it
does not yet demonstrate genuinely different polyphonic contours, for the merge
reason above — so "one external control mapped to per-voice brightness" in the
song description should read "one external control mapped to lead expression,"
and polyphonic contour variation remains unexercised by either song until the
music layer can build it.

### The residues close, and one of them opens something larger

**Per-voice values need a builder, not a list.** `voicing(…)` returns a
temporary with stable member indices and `:each(function(note, index, count))`
attaches per-member values before the notes become a `Stack`. The closure runs
once and disappears — the same discipline `every` and `off` already follow, and
the reason session 1 could claim the language decision stays reversible. So
construction-time ordering is available without `Value::List` ever entering
query-time event values, which matters because it means the ordered-versus-named
question can now be decided on the corpus evidence rather than under pressure
from a requirement that arrived sideways.

**And the same metadata is the answer to `arp`.** This is the part worth more
than the immediate fix. Finding #6 has stood since the songs were written: `arp`
must recover "these three share a `whole`" from an unordered `Stack` query,
which makes it unlike every other transform. With group identity recorded at
construction, it reads the grouping instead of inferring it.

Following that one step further turns it from a convenience into a correctness
question. Nothing today stops `arp` arpeggiating across `stack(bassline,
melody)` — two independent lines that merely coincide in time are not a chord,
and `[a3,c4,e4]` is, and a `Stack` cannot currently tell them apart. So group
identity should be recorded wherever it is *known*: by the voicing builder, and
by the mini-notation parser, which already sees `[a,c,e]` as a syntactic group
and throws that information away. Left open: whether identity survives a
`degrade` that removes a member, and what it means under `rev` and `every`.

**The remaining small answers.** `run(patch)` means program lifetime — until
stopped or replaced by a live edit — and `run(patch, span)` is the bounded form,
with the unbounded case being the *absence* of a span rather than a sentinel.
That last detail matters more than it looks: an artificial `Frac` standing for
infinity is exactly the kind of value that overflows an `i128` intermediate, and
avoiding sentinels of that shape is why `frac.rs` is careful in the first place.
`hz(…)` is a unitful pitch setter rather than proof that every derived port
needs a setter; `n.hz` is the resolved control-rate signal the graph sees, and
`note("d4")` is the same input in different units. The implicit voice contract
is fixed as pitch, velocity, gate/duration, pan and a stable voice ID with
documented defaults — the ID being load-bearing, since voice-addressed control
routing has nothing to address without it. Finding #7 closes.

### Shared signals have identity, and that is a correctness property

The sharpest observation of the session, and it arrived as an aside.

Lexical inputs imply that `audio_input` returns a **program-scope resource
handle**, not a node owned by one graph-builder arena. `jamming.eod` reads one
input from its sensing block, two patches and the score; each graph lowers the
handle to its own input node while the program keeps one external stream
identity. Straightforward enough.

The consequence one level up is not. *Derived* signals are shared too:
`their_hz = them >> pitch_tracker{…}` is referenced by two patches and by the
score. A pitch tracker is **stateful**. Duplicating that subgraph per
referencing graph is therefore not a wasted-cycles question — two trackers can
disagree, and a piece whose entire premise is one system listening to one thing
stops being that. So **a shared derived signal must lower to one node with
fan-out**, and common subexpression identity is a *correctness* property
wherever the shared node carries state, not an optimisation to be added later.

Two things follow. `&` is already fan-out inside a graph, so this is the same
concept crossing a graph boundary rather than a new one — which suggests the
signal DAG is program-scope with graphs as views onto it, not a set of
independent trees that happen to name the same leaves. And it pushes stateful
analysis nodes onto a persistent lifetime for a third independent reason, after
"it exists continuously" and the `Sequencer`-takes-generators question.

Worth noting where this came from: it is not visible in either architecture
proposal, in the corpus, or in the four original songs, because none of them
ever referenced one stateful signal from two places. `jamming.eod` does it four
times on its first page.

**Correction to how that was first stated.** "Common subexpression identity is a
correctness property" was the wrong phrasing and invites the wrong
implementation. What is required is **identity preservation, not CSE**: two
separately written but structurally identical trackers must remain *distinct*,
because wanting two independent smoothing or detector histories is a legitimate
thing to write, and a structural pass would silently merge them. So identity
comes from the **binding site rather than the expression shape** — bound once and
referenced twice is one node, written out twice is two. That is both more
correct and cheaper than CSE, since build-time arena allocation supplies it
without hashing anything.

The model that falls out: a program-scope arena owns persistent inputs,
analysers and control signals; lexical bindings hold stable handles into it;
voice and patch templates reference handles; anything depending on `n.*` stays
template-local and instantiates per voice. Pure expressions duplicate freely,
stateful ones never — and the primitive registry already has to record which is
which, because the two-tier rule ("internal state or a per-sample feedback path
makes it a Rust node") is the same distinction. No new machinery.

It also needs no monolithic backend graph, which matters because a monolith
would not fit `Sequencer`. A persistent analyser publishes to a shared control
bus that independently instantiated graphs read — which is exactly the role
`CLAUDE.md` reserved for fundsp's `Shared` back in session 1: "global,
continuously varying controls that outlive any note." The slot had been sitting
there unconnected to any requirement.

**And one thing this opens.** Does a stateful node keep its identity across a
live edit? If it does not, every keystroke resets the pitch tracker and the
envelope follower — audible, during precisely the activity the whole system
exists for. `codex-architecture.md` ruled that state migration is an
optimisation and correctness must never depend on it; this looks like the one
place that rule does not hold. The plausible source of a stable cross-generation
identity is the call-site IDs from source transformation, which were designed for
byte-accurate spans and turn out to be the only thing in the design that both
survives an edit and names a construction site. Two requirements, one mechanism,
again.

### Identity, twice, and the rules are not the same

`at_onset` acquires a type rather than a convention: `Signal<T> → Init<T>`,
sampling at the triggering event's timestamp, and the **only** legal collapse
from signal to init. Absent it, a signal parameter stays live for the voice's
lifetime. Enforced by the rate checker, not by documentation — which means the
`hz(at_onset(their_hz))` bug in `jamming.eod` would have been a compile error
rather than something caught by reasoning about a four-second bell ring. Note it
is the exact mirror of the rule already written down: lower-rate values lift into
higher-rate inputs freely, higher-rate signals may never be silently collapsed.
`at_onset` is that collapse, named.

The group-transform questions have unremarkable answers, which is the point:
`degrade` preserves group identity and *original* member indices with holes
where members went — load-bearing, because `:each`'s `count` must not shift and
member 3 must keep contour 3; `rev` preserves indices, reversing time and not
voicing order; `every` preserves whatever its transform preserves; `arp`
*consumes* the group and emits ordinary separated events; a revoicing
deliberately creates a new one.

The constraint underneath them is the one that matters. **Group identity must be
derived, not allocated during a query** — a pure function of the group's AST
node and its exact occurrence span. Otherwise two overlapping queries disagree
about the same chord, and both the editor highlight and any voice routing keyed
on it flicker. That is structurally identical to `rand.rs` hashing the exact
rational rather than drawing from a generator, for the same reason, and
`tests/algebra.rs` already exists to defend the property.

Which gives **three identities with three different allocation rules**, and
conflating them would be a quiet disaster:

| identity | derived or allocated | when | query-visible |
|---|---|---|---|
| group | derived from AST node + occurrence span | at query | **yes** |
| stateful signal | binding site in the program arena | at build, must survive edits | no |
| voice | ordinary counter | at instantiation, control thread | no |

The first two are semantic and neither is an optimisation detail. The third can
be a counter precisely *because* it is not query-visible, which is the property
worth checking before anyone makes the other two counters too.

> **Correction, same session, twice over.** The discriminator in that last
> sentence is wrong, and the table is missing a row. See below.

### The voice counter breaks G2, and the discriminator was the wrong one

`neon.eod` contains `init_rand(n.id, "oscillator", …)` — per-voice detuning
seeded from voice identity, which is how the CS-80's component tolerances get
reproduced deterministically instead of drifting. If `n.id` is a runtime
counter, that seed changes with query chunking, with the ordering of
simultaneous events, and between offline and live scheduling. The same text
stops sounding the same.

The middle one is the worst, and it is a hazard this session created for itself:
a `Stack`'s members come back from a query *unordered*, which is the whole
premise of finding #6. So a chord's voices would receive counter values in an
arbitrary order and the per-voice drift would reshuffle between runs of the same
file. G2 is not a preference here; `waves.eod` is specified as giving the same
nine minutes every time it is opened.

So voice identity splits in two:

- **`event_seed`** — derived from event provenance, occurrence span and
  group-member identity. This is what `init_rand` consumes.
- **`voice_handle`** — the runtime counter, used *only* to address an
  instantiated voice, which is what voice-addressed control routing needs. That
  use is inherently live and non-reproducible already, so a counter is honest
  there.

Live external events take nondeterministic seeds until recorded, and the
`Timeline` that recording produces then supplies stable event identities. That
is the third distinct problem the trigger → `Timeline` conversion has now
solved, after lookahead and reproducibility.

**And the discriminator stated earlier was wrong.** "Is it query-visible" is not
the test — a counter allocated on the control thread is not query-visible and
still destroyed reproducibility. The test is: **does anything reproducible
depend on it?** If yes it must be derived from position. A counter is only
admissible for a token consumed at runtime for addressing and never observable
in the output. `voice_handle` qualifies; the moment anything seeds from it, it is
the wrong identity.

Corrected, there are four:

| identity | rule |
|---|---|
| group key | derived from group AST node + occurrence span |
| event seed | derived from event provenance + occurrence + member identity |
| stateful signal node | allocated from its lexical binding, within one program |
| voice handle | runtime counter, addressing only |

### Cross-edit continuity is a relation, not an identity

The other correction, also to this thread. The guess that source-transformation
call-site IDs could supply stable cross-generation identity does not hold: byte
offsets move when text is inserted, AST ordinals move when a node is added, and
generated IDs are regenerated. A call-site ID names a site *within one program*.
Matching two programs is a reconciliation relation computed *between* them, and
it belongs beside the identity table rather than in it — an identity accidentally
inferred from source coordinates is exactly the failure mode to avoid.

So the existing rule stands untouched: new program state starts clean, old and
new crossfade with old tails draining, and reuse happens only where a stable key
*and* a compatibility fingerprint agree. Migration improves continuity and does
not define correctness.

That also dissolves the worry that produced the bad guess, and in a better way
than the mechanism it was reaching for. Analyser state has a **horizon**:
`jamming.eod`'s pitch tracker holds 140 ms, its envelope follower releases in
320 ms, its onset detector holds 90 ms. Retain a second of input, re-run them,
and every one converges — no key matching, no compatibility fingerprint, and it
works across edits that a migration pass could not reconcile at all. State whose
horizon is long or unbounded — a 7.5 s reverb tail, a feedback network — is not
a function of recent input, but that case was already covered by
crossfade-and-drain.

Which leaves a narrower and more useful question than the one asked before: **is
there any state that needs migrating rather than warming or draining?** Quite
possibly none, in v1. That would retire a mechanism rather than add one.

### Answered: state migration is out of v1

Classifying what is actually stateful settles it, and the classification is
short enough to be checkable against a real song rather than argued about.

**Finite-horizon analysis state** — pitch trackers, envelope followers, onset
detectors, compressors. Retain input history and warm the replacement off the
audio thread. Each primitive **declares its own required history** rather than a
global figure being assumed: the horizon of `envelope_follower(ms(6), ms(320))`
is a function of its release, and a declared horizon makes the retained buffer a
computed maximum over referenced analysers instead of a guess. That also bounds
its memory, which the resource-validation list already wants. Where a horizon
depends on a parameter that is itself a live signal, `ParamSpec`'s range gives
the bound — the pieces compose.

**Audible tails** — reverbs, delays, feedback networks, and active voices. Keep
the old instance running, stop feeding it where appropriate, drain or crossfade.
This is the existing default and needs nothing new.

**User-owned musical memory** — loopers, capture buffers, recorded control and
input lanes. This is the class neither thread had, and it is the one where
getting it wrong is worst: a looper's buffer is *the user's material*, so losing
it on an edit is data loss rather than a glitch. It lives in a persistent
program resource, so replacing a graph does not replace it. Note this
consolidates with the recorded-input lanes from earlier in the session — a
recorded lane and a looper buffer are the same kind of resource.

Anything outside the three may reset until a song proves otherwise, which keeps
migration out until it earns its complexity.

Two consequences worth carrying forward.

**The arena needs a disposability classification, not just ownership.**
Discarding an analyser is free — it re-warms. Discarding a looper buffer is data
loss. Same arena, opposite disposal rules, and the distinction has to be
declared rather than inferred. Open residue: editing a looper's *declaration*
(shrinking its buffer) is a user-visible operation and must not be a silent
consequence of a text edit.

**Derive from transport wherever possible**, which empties part of what would
otherwise look like class 2. A fixed-rate LFO's phase in a persistent rack is a
function of transport time, not an accumulator, so an edit costs it nothing. The
rule does not extend to a frequency-modulated oscillator, whose phase genuinely
integrates — but that case is a `patch` keeping its instance, i.e. class 2, and
already handled. This is the same principle as seeded randomness, group keys and
event seeds: derive from position rather than accumulate. Fourth appearance.

### The recorded `Timeline` needs an ordinal

One detail follows from the seed discussion and it is easy to get wrong. A
captured `Timeline` must preserve an **event ordinal or ID, not only a
timestamp**. Two live events can share an instant, and `Pattern::onsets`
deliberately does not deduplicate precisely because identical simultaneous
onsets are legal music. Keying recorded identity on time alone would therefore
either collapse such a pair or order it arbitrarily — and an arbitrary order
reshuffles the derived `event_seed`, which is the reproducibility bug from two
entries up arriving by a different route.

So the same property that forbids deduplication forbids identification by
timestamp. One property, two consequences, and the ordinal must be *captured*
rather than re-derived at playback, since arrival order is not recoverable after
the fact.

With that, the trigger → `Timeline` conversion supplies four things: queryability,
reproducibility, deterministic event identity, and an offline-renderable
boundary. It has gone from an incidental observation to the most reused
mechanism the session produced.

### Evaluation is not a destructive operation

The rule durable resources need, and it is a safety property rather than a
convenience. **Evaluating source may acquire a durable resource; it may never
destructively mutate its schema.**

```lua
local loop = audio_buffer { name = "night-loop", capacity = secs(30), channels = 2 }
```

The local is lexical, `name` is the durable storage identity — the same split as
an external input's binding, which is the third time that shape has been the
right one. On a live edit: growing the capacity preserves and extends; shrinking
it, or changing channel layout or format, is *incompatible* and produces a
diagnostic while the previous program keeps playing; truncation or replacement
requires an explicit user action; and deleting the declaration merely orphans
the resource, which stays recoverable until discarded on purpose.

The hazard being closed is worth stating precisely, because the obvious reading
is wrong. It is not syntax errors — those fail evaluation and the old program
survives, which is G1 working as designed. It is a **transiently valid** edit. A
live-coding system re-evaluates while you type, so `capacity = secs(30)` on its
way to `secs(300)` passes through `secs(3)`, which parses. If shrinking
truncated, a keystroke would cost 27 seconds of the user's recording. The rule
makes that transient a no-op.

The framing that makes it memorable: G1 converts every failure into a diagnostic
while the last valid program keeps sounding. This is the identical mechanism
applied to a different asset — **G1 protects the sound; this protects the
material.**

It also adds a scope level that the earlier arena sketch missed. Durable
resources sit *above* the program arena: name-keyed, surviving every generation,
discarded only deliberately. The program arena is per-generation, binding-keyed
and freely disposable. Template-local state instantiates per voice. Three
scopes, three lifetimes, and conflating the first two is how an edit eats a
recording.

### What the whole thread was actually about

Stated last because it took the whole session to see:

> **If state is a pure function of stable coordinates, derive it. Retain history
> only where the output genuinely depends on history.**

This is not a new principle. It is the founding constraint of `crates/pattern` —
a query is a pure function of its timespan, which is exactly what
`tests/algebra.rs` defends by slicing a window at 1/7 and demanding the same
onsets, and exactly why the crate is testable without a sound card (G8). It was
simply never recognised as general, because until this session nothing above the
pattern layer existed to test it against.

It has now arrived six times: seeded randomness from position rather than a
generator; group keys from AST node plus occurrence span; event seeds from event
provenance rather than an allocation counter; a fixed-rate LFO's phase from
transport time rather than an accumulator; tempo as a signal whose integral is
position; and pure querying itself. Every one of those was reached
independently, by someone solving a local problem, which is the usual sign that
the principle was designed in at the bottom and everything built on top inherits
the pressure.

Its practical value is a test applicable per primitive rather than a slogan. A
reverb's output depends on all past input, so it retains. A pitch tracker's
depends on recent input, so it retains a bounded horizon and must declare it. An
LFO's depends only on `t`, so it derives and an edit costs it nothing. And the
payoff compounds: derived state never needs migrating, warming or crossfading,
because it was never mutable. That is why the live-update system — the part of
this design that looked most likely to become a swamp — has so little left to
reconcile.

Recorded in `/CLAUDE.md` beside the rate hierarchy, since it is peer to it in
how much it decides.

### Evaluation is a hotkey, and the durable-resource argument had to be restated

Decided by the user, closing the session: there is no need to re-evaluate on
every keystroke; an explicit command is the better live-coding model. Typing
continuously updates parsing, highlighting and diagnostics; only the command
evaluates into a candidate program, validates it, and activates atomically at a
musical boundary, with the current program surviving any failure. Automatic
evaluation stays available as a later preference and is not the architectural
default.

This required going back and fixing how the durable-resource rule had just been
written down, which is worth recording as a caution rather than quietly
correcting. The transiently-valid-edit scenario — `capacity = secs(30)` passing
through `secs(3)` on its way to `secs(300)` while the system re-evaluates
mid-keystroke — was used as the *justification* for the rule. Under hotkey
evaluation that scenario largely evaporates, so a justification had been written
that the very next decision undercut.

The rule is unaffected; only its basis was wrong. The durable statement is the
separation itself: **evaluation proposes a runtime configuration and never
authorises destruction of user material.** That holds at any cadence, and a
deliberate evaluation of a mistaken edit must not eat a recording either.
Destructive operations belong to an explicit command path. The transient case is
now a *consequence* the rule also covers, should automatic evaluation ever be
offered, rather than the reason it exists.

Two things the hotkey decision buys, neither of which was the point of it.

**The validation pass gets real time.** Everything on the "all unbounded work
happens before publication" list — density estimates, graph validation, resource
estimation, asset decoding, lowering — is affordable when publication is
user-triggered instead of racing a typist. That also relaxes the piccolo
evaluation fuel budget, which had been sized against an implicit
edit-to-hear-under-100 ms assumption that only applies from the *keypress*, not
from every character.

**The editor has two cadences.** Parsing, highlighting, completions from the
primitive registry and syntax diagnostics run while typing; program-level
diagnostics and sounding-event highlights run on command. So the
source-transformation work is *not* relaxed by this — it serves the fast
cadence, and G4's requirement that the editor show what is sounding still needs
byte-accurate spans available continuously.

---

**Architecture pass ends here.** The recorded order stands: the browser-language
spike first, because it is the only open assumption that can still invalidate
the crate layout, then the `Value`/control-map shape, which every later musical
feature depends on and which now carries three decisions rather than one —
signal versus scalar, ordered versus named, and group identity. Further pressure
should come from implementation rather than from more prose.

---

## Implementation checkpoint — 2026-07-28

The browser-language assumption above is retired. A vendored Piccolo revision
now builds both natively and for `wasm32-unknown-unknown`; the sandbox enforces
fuel and measured memory limits and returns only owned Rust program data. Direct
`pattern(...)` and `play(...)` calls receive source-transformed byte-offset
identities. Graph-expression spans and the complete diagnostic source map remain.

The first real path now crosses every intended seam: Lua evaluates a
`voice`/`play` program, transactional publication validates it, `VoiceId`
resolves to a data-only `GraphTemplate`, the monotonic scheduler submits notes
to fundsp, and an offline test measures the rendered pitch. The same path has a
native cpal example.

Engine implementation also reached finite timelines, group/event provenance,
ramped tempo maps, deterministic per-event graph initialization, routed
buses/sends, persistent patches and controls, trigger recording, conservative
voice tails, caller-supplied graph budgets, and an allocation-bounded
interpolating delay. All five `poles.eod` voices are offline framework fixtures.

The expensive boundary did not move: structured event controls still require a
decision about named versus ordered values and scalar versus curve-valued event
parameters. Feedback topology likewise needs an explicit IR representation; a
delay node does not make arbitrary graph cycles legal. Current status is kept in
`README.md`; this log remains append-only.

---

## Native GUI checkpoint — 2026-07-28

The first desktop GUI deliberately reuses the production path instead of
wrapping the native sound example: editor Run → fresh Lua evaluation → owned
`Program` → transactional revision → atomic multi-track scheduling →
`GraphTemplate` instantiation → fundsp frontend/backend → cpal.

The non-obvious ordering is preflight first, publication second. Every track in
the candidate's first scheduling window lowers into a throwaway sequencer before
the active revision changes. The live sequencer is then filled from the same
exact frontier. Per-window preparation is all-or-nothing, and the app retains
the preceding program as a fallback if a latent error only appears in a later
window. Previously submitted voices are not cancelled, so their conservative
tails drain across an edit; crossfading persistent processors remains a
different, still-deferred lifetime problem.

The app rejects persistent patches, controls, live graph inputs and bus sends
for now. This is host capability negotiation, not a language restriction:
those values already exist in the owned program, but silently discarding their
audio would make a successful Run lie. The first cpal stream also fixes the
output channel count for its lifetime; changing it requires restart until
device-stream replacement has explicit semantics.

---

## The strobe question — 2026-07-29

Asked as a user question rather than a design one: how do you write a strobe?
It turned out to be the shortest path to the third curve placement, so the
reasoning is kept.

The gate is `e(t mod T) − e((t mod T) − T_on)`. The modulo has to apply before
the on-time is subtracted; `e(t mod T) − e((t − T_on) mod T)` wraps back to a
non-negative phase, so both steps are 1 and the whole thing cancels to zero.

### Two implementations, and what each one exposed

The first was `pulse(hz, duty) >> shape("clip", 100)` — an audio-rate square
used as a VCA. It works and it is the right sound for an aggressive effect, but
clipping at that gain discards fundsp's band-limiting, so it is a naive square
with full aliasing rather than merely a click. Worth being precise about,
because the aliasing is not caused by the strobe rate.

The second was a summed window train, `Σ [step(kT) − step(kT + T_on)]`, built by
an ordinary Lua loop. That is the one that fits, and it is the case the exact
piecewise-linear range partition was built for: 2N step terms with ±1
coefficients that cancel, so `prove_range` proves `[0, 1]` exactly instead of
falling back to an enclosure, and `activity()` returns a finite horizon because
the terminal sums to zero. Both budgets already cover it — `window()` in a graph
emits `Op::Curve` nodes counted by `GraphLimits.nodes`, and event-carried curves
are capped separately by `ValueLimits`. Its two costs are that the voice lives to
the last window regardless of gate, and that the count is explicit.

A property worth stating rather than treating as a consolation: the control path
is *inherently declicked*. A step sampled at 2 ms and interpolated is a 4 ms
triangle — the same observation that already lives under ε versus δ — so the
edge softens for free. You choose the path by whether you want the click, which
is a good design property, but it does mean a genuinely hard strobe edge is only
available at audio rate and will alias.

### The real finding, which was not about strobes

`Op::Sine` and `Op::Pulse` lower to free-running fundsp oscillators, so their
phase is an accumulator. The derive-from-coordinates section of `/CLAUDE.md`
lists "a fixed-rate LFO's phase from transport time rather than an accumulator"
as one of the five places the principle turned up — and the implementation had
not earned that line. In a per-note voice nothing is wrong, because the note
clock is the correct coordinate. In a persistent `patch` phase starts when the
instance does, so a hard reset re-phases it. Compatible-edit arena reuse hides
this, which is why nobody had noticed. `/CLAUDE.md` now carries the correction
next to the claim.

### Rejected: `transport_seconds()`, `%` and `lt(a, b)`

The obvious fix is three new signal operations: an absolute transport
coordinate, a Euclidean remainder, and a comparison. Rejected on three grounds.
`lt` produces a hard edge at signal rate whose position is signal-dependent,
which is exactly the construction the DSP notes say must be a Rust node because
it aliases — and it opens signal-rate branching generally, which the rate
hierarchy exists to prevent. `%` earns its place only by feeding `lt`. And
together they convert declarative, data-only curve terms into an imperative
expression, which costs `prove_range`, `activity()` and serialisability at once.
That is a large amount of architecture surrendered for one effect.

### Taken: the periodic transport clock

One clock variant instead. A `Curve` gains a transport clock carrying a period
and is evaluated at `t mod period`, so `window(0, T_on)` becomes an infinite
train with no `count`. It is a pure function of transport time, so it derives
rather than retains: an edit costs it nothing, and a hard reset re-enters at the
correct phase for free — which is precisely what the rejected proposal was
reaching for, obtained by not introducing state in the first place.

The existing algebra survives untouched. `prove_range` bounds one period on the
exact piecewise-linear path. `activity()` needs no new case: a periodic curve
never settles, so it reports `GateBounded`, and a transport curve therefore
cannot extend a note — which is the answer you want anyway.

Note the shape of the argument, because it recurs. The question "how do I write
a strobe" has an imperative answer and a declarative one, and the declarative one
is smaller only because the surrounding machinery was already built to reward it.
Adding `lt` would have been three days of work that made four existing analyses
weaker; adding a clock variant is an afternoon that makes none of them weaker.

### Blocked on tempo, and that is the correct order

A periodic curve immediately asks whether its period is seconds or bars. That is
the persistent-graph half of the `beats(0.75)` ambiguity already recorded — a
send outlives any particular tempo, so a tempo-relative duration inside a
persistent graph has no single answer. A bar-synced strobe is the first thing
anyone will ask for, so tempo/timeline integration goes first; building transport
curves before it would mean defaulting to seconds and inheriting the classic
sequencer bug in the one place the docs already warn about it.
