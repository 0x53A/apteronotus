# 000 — Architecture

Current state and open questions. Kept pruned; the reasoning trail, including
everything rejected, is in `LOG.md`.

Conclusions that have hardened live in `/CLAUDE.md` and `/songs/CLAUDE.md`.
This file is what is *not* settled.

`codex-architecture.md` is a second, independent architecture proposal, written
before reading this directory. Its major fork — construct a concrete voice once
per note, or stage a symbolic graph once per edit — was settled in session 2 in
favour of staging. `corpus/` holds the Strudel corpus audit that session 2 ran.

`codex-architecture-v2.md` is the post-corpus proposal. It incorporates the
web-language requirement, defines the intended Lua sandbox, keeps Lua out of
ordinary runtime queries, adds the corpus-driven join and music layers, and
demotes stable graph serialization from architectural center to optional
feature.

## Where it stands (2026-07-28)

| | |
|---|---|
| `crates/pattern` | **done.** 65 tests, no dependencies. Mini-notation, pattern algebra, continuous signals, source spans, parse limits. |
| `songs/*.eod` | **written as the specification.** Four songs, unimplemented. Known too narrow — no music theory, no select/join, and (session 3) all four cyclic, all four with parameters fixed at onset. |
| `songs/neon.eod`, `songs/jamming.eod` | session 3, covering free time + gestures and live input respectively. See below. |
| the Strudel corpus audit | **done.** 723 snippets, 249 k chars. See `LOG.md` and `corpus/`. |
| the synthesis corpus audit | **not started.** This is the one that bears on the graph model. |
| `crates/music` | **first slice written.** Scientific pitch notation, fractional MIDI, accidentals, pitch ↔ frequency. Music-theory expansion remains open. |
| `crates/synth` | **first voice path written.** Data-only `GraphTemplate`, per-input and per-node spans, validation, Rust builder, note binding and fundsp lowering. 18 offline graph tests. |
| `crates/live` | **first sound path written.** Configurable-meter constant transport, monotonic-frontier pitch scheduler, cpal frontend/backend output and `examples/first_sound.rs`. 4 offline scheduler tests. Editor, generations/activation and MIDI remain. |
| the scripting language | Lua-shaped, **on the web is now a hard requirement** |

The first sound path is deliberately narrower than the score model:
mini-notation note names or MIDI values feed one staged voice. It proves the
seams without pre-empting the open `Value`/control-map decisions. The workspace
integration test renders `c4` through pattern → scheduler → synth → sequencer
and measures nonzero audio at approximately 261.6 Hz; no sound card is needed.

## Next, in order

The browser-language spike moved to the front because "the full language runs in
the browser" became a v1 requirement, and it is the only open assumption that
can invalidate the crate layout.

1. **Browser-language spike.** A disposable crate or branch, answering only
   whether piccolo is a viable foundation. It builds natively *and* as
   `wasm32-unknown-unknown` and runs the same tests on both. The contract it is
   tested against — required semantics, exposed libraries, deliberate omissions
   — is in `/CLAUDE.md`; the question is no longer "is this complete Lua 5.4?"
   but:

   - does the core language implement the required semantics (closures and
     upvalues, tables and constructors, both `for` forms, `pairs`/`ipairs`,
     multiple returns and varargs, `pcall`)?
   - are all the operator metamethods the patch algebra needs present —
     `sine(440) >> lowpass(1200) >> gain(0.2)`, and constant-topology loops
     accumulating with `mix`?
   - can the required `string`/`table` functions be filled cheaply? These are
     the real gap; the peripheral libs are irrelevant under the contract.
   - do fuel and memory limits actually enforce the sandbox — does an infinite
     loop stop?
   - can **source transformation** supply byte-accurate call sites, given there
     is no `debug` library and never will be?
   - do Rust callbacks build real `Pattern` and provisional `GraphTemplate`
     values, with **no Lua object surviving** into the returned program?
   - does repeated evaluation retain nothing from the previous program?
   - does the same sandbox pass identically on both targets?

   Fallback ladder if it does not: fill piccolo's bounded gaps; an emscripten
   Lua module transferring one encoded `Program` per edit; our own VM.
2. **Control maps in `crates/pattern`.** `Value` gains a multi-value form, a
   `Named` node lifting bare values into it, a `Merge` node taking structure
   from the left. Everything in the score notation is blocked on this. **Two
   open questions must be answered before this fixes `Value`'s shape** — ordered
   versus named, and scalar versus signal. The second was found in session 3 and
   is the more expensive of the two to get wrong.
3. **Commit the merge sampling rule** (below — it is no longer a coin flip).
4. **Initial slice complete: the smallest data-only symbolic graph builder.**
   `GraphTemplate` owns an operand-list DAG and outputs, with one
   `Input { source, src }` per port. There is no versioned primitive registry,
   bundle format or cross-backend contract. No `Box<dyn AudioUnit>`, fundsp
   node, Lua closure or backend pointer sits inside it; `lower.rs` is the only
   fundsp-aware module.
5. **Initial slice complete: one path that makes sound.** Symbolic note
   frequency → sine → low-pass → duration-keyed envelope/VCA → output, through
   the exact pattern frontier and `fundsp::Sequencer`. The scheduled unit lives
   for gate duration plus graph tail.
6. **Transcribe progressively harder instruments** from the four songs against
   that builder.
7. **Buses and sends**, then pattern-to-control conversion and ducking.
8. **The persistent `patch` lifetime**, which is where portamento lives.
9. **Serialization only when it earns its place** — for caching, sharing or
   offline rendering, not as an upfront format.
10. **Lua** once the Rust builder surface has survived all of the above.

The synthesis corpus audit (below) can run in parallel with any of this; it
gates nothing but informs step 4 onward.

## Open questions

Blocking, in rough order of cost-if-wrong.

**Can an event value be a signal?** A voice needs control-rate inputs that stay
writable after onset — polyphonic pressure, a bend gesture, a brightness swell
across a held note. The graph side of this already exists (`adsr(…)` inside a
voice *is* a note-clock control-rate signal); what does not is that
`ControlMap` values are scalars, so an event can hand a voice a number but not a
curve. Proposed: `Value::Curve(BasisSum, Clock)`, which falls out of the
existing rate rules with nothing added to the graph model, keeps the merge rule
intact — a zero-width query returns the curve *as a value*, it does not sample
it. Preferred over a separate `GestureMap` beside `ControlMap`. **Decide before
step 2 fixes the shape**; this is the most expensive open question on the list,
because step 2 is the next code to be written.

It does **not** give per-note contours within a chord, and an earlier version of
this entry claimed it did. Simultaneous events share an onset, so a zero-width
query returns them all the same value; `pressure("<soft swell hard>")` alternates
per cycle and still hands one curve to every note of a struck chord. The general
form of that is worth stating once, because it is not about gestures:
**no merged parameter can vary across simultaneous events — ever, for any
parameter.** Pan, velocity, detune and send level are all affected. A strummed
chord works because its onsets differ; a struck one does not.

So per-voice variation is a *construction-time* concern, which puts it in
`crates/music` where voicings are built. **Resolved without touching `Value`:**
`voicing(…)` returns a temporary builder with stable member indices, and
`:each(function(note, index, count) … end)` attaches per-member values. The
closure runs once and disappears, exactly as `every` and `off` already do, so
nothing survives into the tree and no list enters query-time event values.

Note that this is the same blind spot as `arp`, and the builder is the same
mechanism for both — see the group-identity entry below.

Corroborated from an unexpected direction: `neon.eod` writes gestures as
`>> pressure(curve { … })` — an ordinary parameter setter taking a curve value,
identical in form to `>> gain(0.8)`. That is the `Value::Curve` shape, reached
by the *notation* even though the same session's Rust sketch proposed a separate
`GestureMap`. When the notation and the struct disagree, the notation is the
specification.

**Multi-value events: ordered or named?** A control map solves
`{note, gain, pan}`. It does not obviously solve `[string, fret, articulation]`,
where position is the meaning. The corpus splits three ways — genuinely ordered
(guitar strings, `(magnitude, unit)`), ordered only because the notation could
not produce names (one author's `as` helper does nothing but name positions),
and values that actually want to be typed musical objects (chord symbols). The
evidence leans towards `Value::List` at notation level plus an explicit naming
step, but it is one author's corpus. **Decide before step 2 fixes the shape.**
Less urgent since session 3: the voicing builder removed the one requirement
that would have forced `List` in from a different direction, so this can now be
decided on the corpus evidence alone rather than under pressure.

**The select/join node's temporal modes.** Strudel exports `pick`, `pickF`,
`pickOut`, `pickRestart`, `pickReset`, `squeeze`, each with a wrapping `mod`
variant — two orthogonal dimensions, alignment and out-of-range policy. `reset`
and `restart` differ exactly when branches vary between cycles. The mode cannot
be implicit. Unresolved: what `density()` and source-span propagation mean for
each mode, `squeeze` especially.

**Where the music layer's boundary falls.** `crates/music` (typed pitches,
scales and modes, chord symbols, voicing dictionaries, anchors, inversions,
pitch ↔ frequency) is pure and deterministic, so most of it should expand at
construction time. But real songs pattern its inputs — `.transpose("<~@16
-12@24 …>")`, patterned anchors — so some of it has to survive as runtime
nodes. Getting this wrong puts music theory inside the query loop.

**`key()`'s semantics.** No longer "define or delete" — it is required. The
intended meaning is a program-level tonal context that degree notation resolves
against. It must be neither decorative nor hidden mutable scheduler state.

**Portamento has no home.** `slew(n.hz, n.glide)` in `techno.eod` needs the
previous note's pitch and a per-note voice has never heard of one. The resolution
is the persistent `patch` lifetime — a 303 *is* monophonic — which also covers
drones, modular racks, feedback networks and bus processors. It is a second
voice model, so it comes after the first one sounds, at step 8. Session 3
narrowed the trigger for it: a bend *inside* one sustained note is an ordinary
note-relative curve in a per-note voice and needs nothing new. The rule is
**`voice` unless the persistence is musically observable** — legato pitch
sliding through a note boundary, oscillator phase continuity, or a live
controller steering whichever voice is currently sounding.

**Voice-addressed control routing.** Live polyphonic aftertouch on note 64 must
reach the voice currently sounding note 64. That cannot be written as a signal
in a score, because voice identity does not exist at score level, so it needs a
mechanism — and it should bind to the *same* graph port an authored gesture
curve does. Keep it narrow: this is note-expression routing, not a general
automation system. The wider `map { during, target = path, combine }` proposal
was rejected (see `LOG.md`) because `during` is `window`, `combine = "add"` is
`+`, and a global `ParameterPath` namespace is action at a distance in a
language that is otherwise lexical and dataflow.

**Shared signals have identity, and the graph builders must respect it.**
`audio_input` and `control_input` return *program-scope resource handles*, not
nodes owned by one graph-builder arena — `jamming.eod` references one input from
its sensing block, two patches and the score. Each graph lowers the handle to
its own input node while the program keeps one external stream identity.

The consequence nobody has written down is one level up. Derived signals are
shared too: `their_hz = them >> pitch_tracker{…}` is used by a patch, another
patch and the score. A pitch tracker is **stateful**, so duplicating that
subgraph per referencing graph is not a wasted-cycles question — two instances
can disagree, and the piece stops being one system listening to one thing. So
**a shared derived signal must lower to one node with fan-out.**

**This is identity preservation, not common-subexpression elimination**, and the
distinction is load-bearing: two separately written but structurally identical
trackers must stay *distinct*, because wanting two independent detector or
smoothing histories is a legitimate thing to write. So identity comes from the
**binding site, not the expression shape** — bound once and referenced twice is
one node; written out twice is two. That is simpler than CSE as well as more
correct, since build-time arena allocation supplies it with no hashing.

The model: a program-scope arena owns persistent inputs, analysers and control
signals; lexical bindings hold stable handles into it; voice and patch templates
reference the handles; anything depending on `n.*` stays template-local and
instantiates per voice. Pure expressions may be duplicated freely, stateful ones
never. This needs no monolithic backend graph — a persistent analyser publishes
to a shared control bus that independently instantiated graphs read, which is
precisely the role `CLAUDE.md` already reserves for fundsp's `Shared`: "global,
continuously varying controls that outlive any note." The slot existed; nothing
had been connected to it.

**Cross-edit continuity is a reconciliation relation, not a fifth identity.** An
earlier version of this entry guessed that source-transformation call-site IDs
could supply stable identity across generations. They cannot: byte offsets move
when text is inserted, AST ordinals move when a node is added, and generated IDs
are regenerated. A call-site ID names a construction site *within one program*.
Matching two programs is a separate relation computed between them, and the
existing rule stands unchanged — new program state starts clean, old and new
crossfade with old tails draining, and reuse happens only where a stable key and
a compatibility fingerprint agree. Migration improves continuity; it may not
define correctness.

Which dissolves the worry that motivated the guess, and the question it left
open — *is there state that needs migrating rather than warming or draining?* —
now has an answer: **no, and state migration is out of v1.** See the settled
entry below.

**A tempo map makes tempo-relative durations ambiguous inside persistent
graphs.** `beats(0.75)` currently resolves once, because tempo is a scalar.
Under a tempo map, cycles → seconds becomes a piecewise integral and
`synthwave.eod`'s `send { graph = delay(beats(0.75)) … }` has no single answer,
because the send outlives any particular tempo. Absolute `secs(…)` is
unaffected, so the seam holds — but tempo-relative durations in persistent
graphs need either resolution at instantiation or prohibition. Note that a tempo
map and rubato are different mechanisms — the first is composed and
reproducible, the second is `late` fed a seeded signal.

**Which lifetime does `audio_input` belong to?** Conceptually a live input suits
a persistent `patch` or bus because it exists continuously, and the shared-state
argument above says the same thing independently. fundsp 0.23 does **not**
settle this through `Sequencer::push`: `Sequencer::new(inputs, outputs, …)`
accepts an input count and `push` asserts `unit.inputs() == self.inputs()`, not
that either side is zero. The current Apteronotus voice lowering chooses
zero-input generator units, but that is its present scope rather than a backend
law. The realtime `SequencerBackend` input route needs its own test before it is
used as an architectural argument; persistent lifetime remains the sensible
model on musical and shared-state grounds.

**`duck(pattern, amount)` is not a per-note parameter.** It modulates a line's
whole output from a rhythm, so a played line needs its own bus and **patterns
must be convertible into control signals** — sample and hold with a shaped
release. Two of the four songs need it.

**`to(send, x)` is used at two levels.** Inside a graph it is a fixed routing
edge; in a score it is a per-note send level. Define both separately or forbid
one. They should not be one ambiguous operation.

**`arp("up")` operates on events, not on the tree.** `[a3,c4,e4]` becomes a
`Stack` and queries return it unordered; the arpeggiator has to recover "these
share a `whole`" and sort by pitch. That makes `arp` unlike every other
transform, which is worth knowing before it is written. Largely absorbed by the
group-identity entry above — with identity recorded, `arp` reads the grouping
instead of inferring it, and the residue is only its ordering policy.

**Whether the songs can be expressed with symbolic `NoteInput`s alone.** All four
stage cleanly today (no `if` in any of them, every loop over constants). If a
real instrument later needs event-dependent topology, the answer is an
explicitly marked dynamic factory, not silently making every voice dynamic.

## Settled in session 2

Recorded here so they stop being re-litigated; reasoning in `LOG.md`.

- **Staging wins.** `graph = function(n)` runs once per edit with symbolic note
  parameters. Portability is no longer the justification — validation before
  publication, predictable resource use, no per-onset topology work, source
  locations on graph nodes and clean replacement are.
- **The merge sampling rule.** Sample the right-hand pattern with a zero-width
  query at the left event's onset, take the first value. Forced, not chosen: if
  a voice is instantiated at onset with its parameters baked in, Tidal's
  one-output-per-right-hand-event rule would instantiate N voices for one note.
- **Dynamic query-time patterns are not in v1.** 1 song in 110 needs one for a
  musical reason, and it works by violating pure querying. A retained closure
  also has no source span, so G4 degrades exactly where the pattern is most
  dynamic. Keep the door; do not build it.
- **External input gets a declared node**, not an escape hatch —
  `External { source, sampling }`. MIDI and keyboard are legitimate; ambient
  state read from a callback is not the mechanism.
- **Serialization is deferred**, in three lazy milestones: data-only in
  principle now, a temporary encoding with a native round trip once several
  instruments play, a versioned schema only if it is ever needed.
- **`+` is mix, `&` is fan-out.** Apteronotus semantics, lowered to fundsp — not
  inherited from how one backend happens to overload an operator. Consequence:
  `poles.eod` wants both rings summed, so its `&` becomes `+`, and the loops
  should use an explicit `mix(...)` rather than accumulating onto a `zero()`
  whose arity does not match.
- **Ordinals**: prefer names. Where a user-visible ordinal must exist it is
  1-based; IR vector indices stay ordinary 0-based implementation details.

## Settled in session 3

From the Blade Runner test case, run in parallel by both threads. Reasoning in
`LOG.md`.

- **The synthesis graph is already sufficient.** The CS-80 is textbook
  subtractive with no wavetable in it; the voice stages cleanly under the
  symbolic-`n` rule. Everything the test exposed is in the score, gesture and
  control-routing models.
- **The query model was never the problem — the constructors are cyclic.**
  `query(timespan) -> events` says nothing about repetition. A finite
  `Timeline { events, extent }` node queries purely, is idempotent over
  overlapping windows, and has a trivial `density()`. Free time needs a
  constructor, not a replacement, plus a `TempoMap` and absolute-second
  placement.
- **Curve placement #1 already covers composed expression.** A note-relative
  curve read continuously by a per-note voice is how a pressure swell or a bend
  inside a held note works. What is missing is only the ability to put such a
  curve *in an event* — see the open question above.
- **`InputSpec` and logical-name binding are the right external-input model.**
  `knob("filter")` names a logical control; the shell binds it to CC 74 or a
  slider; device identity never enters the song; a missing device degrades to
  its default with a diagnostic rather than stopping playback. That last part is
  G1 and is not negotiable.
- **Audio → control crossings are named nodes**, never implicit port coercion —
  `envelope_follower`, `rms`, `pitch_tracker`, `onset_detector`. Same rule as
  "higher-rate signals cannot be silently collapsed."
- **Reproducibility of live input is recovered by recording at the boundary.** A
  recorded lane is data, so it turns a non-reproducible performance back into a
  reproducible program, and offline rendering comes free.
- **The reverb is stdlib, not a primitive.** Predelay → early reflections →
  diffusion allpasses → modulated feedback matrix → damping is composition of
  delays, allpasses and matrix mixing. Only the interpolating delay lines are
  irreducibly stateful. It is also the highest-leverage single item for this
  class of music — more consequential than any oscillator decision.

### Notation settled in session 3

The two songs disagreed; `jamming.eod` lost four of five and has been converted.

- **External sources are lexical.** `local x = audio_input { name = "…", … }`,
  not a global block with string accessors — the local is the internal identity,
  `name` is the external one the host binds, and device identity never enters
  the song. The block form was proposed by the same thread that then rejected
  `ParameterPath` for being a string-keyed global namespace; the objection
  applies to both.
- **`velocity` everywhere**, including `n.velocity`. Not two spellings.
- **The implicit voice contract is fixed**: pitch, velocity, gate/duration, pan,
  and voice identity, each with a documented default. Identity is load-bearing
  rather than decorative, and it is *two* things — a derived `event_seed` that
  anything reproducible must use, and a `voice_handle` counter that addresses a
  sounding voice and may never be seeded from. This closes finding #7.
- **`hz(…)` is a unitful pitch setter**, not evidence that every derived graph
  port needs a matching setter. It supplies the canonical pitch input in hertz;
  `n.hz` is the resolved control-rate signal the graph sees, and `note("d4")` is
  the same input at different units.
- **`tempo(72)` desugars to a one-entry `TempoMap`.** Missing `over` steps,
  explicit `over` interpolates.
- **Declarations are inert.** `play(x, notes)` drives an instrument, `run(x)`
  activates an autonomous rack for the program's lifetime, `run(x, span)` bounds
  it. Infinity is *not* an artificial `Frac` — the unbounded case is the absence
  of a span. `play` is the same call for a `voice` and a `patch`, so moving an
  instrument between polyphonic and persistent does not touch the score.
- **An external trigger is its own type**, a live event stream with no query or
  lookahead contract. Recording one turns it into an ordinary finite `Timeline`,
  which supplies four things at once: queryability, reproducibility,
  deterministic event identity, and an offline-renderable boundary.
  **The recorded form must carry an event ordinal, not only a timestamp** —
  two live events can share an instant, and `onsets` deliberately does not
  deduplicate because identical simultaneous onsets are legal music. Keying
  recorded identity on time alone would either collapse such a pair or order it
  arbitrarily, and an arbitrary order reshuffles the derived `event_seed`. The
  same property that forbids deduplication forbids identifying by timestamp;
  the ordinal is captured, never re-derived at playback.
- **State migration is out of v1**, and the taxonomy is what retires it rather
  than a decision to tolerate glitches. Stateful things fall in three classes.
  *Finite-horizon analysis* — trackers, followers, detectors, compressors —
  warms a replacement from retained input, off the audio thread; each primitive
  **declares its own required history** rather than assuming one global figure,
  which also makes the retained buffer a computed maximum rather than a guess
  and bounds it for the resource-validation list. *Audible tails* — reverbs,
  delays, feedback networks, active voices — keep the old instance running,
  stop feeding it, and drain or crossfade. *User-owned musical memory* —
  loopers, capture buffers, recorded control and input lanes — is data in a
  persistent program resource, so replacing a graph does not replace it.
  Anything outside the three may reset until a real song proves otherwise.

  Two consequences worth carrying. The arena needs a **disposability**
  classification and not merely ownership: discarding an analyser is free, and
  discarding a looper buffer is data loss. And *derive-from-transport wherever
  possible* empties part of what would otherwise look like class 2 — a
  fixed-rate LFO's phase is a function of transport time, not an accumulator, so
  an edit costs it nothing.
- **Evaluation proposes a configuration; it never authorises destruction of user
  material.** `local loop = audio_buffer { name = "night-loop", capacity, … }` —
  lexical local, durable `name`, the same split as an external input. Growing
  the capacity preserves and extends; shrinking it, or changing channel layout
  or format, is incompatible and yields a diagnostic while the previous program
  keeps playing. Truncation and replacement belong to an explicit command path,
  and removing the declaration orphans the resource rather than discarding it.
  G1 keeps the sound alive through any failure; this is the same rule protecting
  the material.
- **Evaluation is a hotkey, not a keystroke.** Typing continuously updates
  parsing, highlighting and diagnostics; the command evaluates, validates, and
  activates atomically at a musical boundary, leaving the current program
  playing if any step fails. Automatic evaluation may return later as a
  preference, never as the architectural default. Two consequences worth
  keeping. It buys the validation pass real time — everything on the
  "unbounded work happens before publication" list (density estimates, graph
  validation, resource estimation, lowering) is affordable when publication is
  user-triggered rather than racing a typist, which matters for the piccolo fuel
  budget too. And it splits the editor's feedback into **two cadences**: parse
  and span machinery run while typing, program-level diagnostics run on command.
  The source-transformation work serves the fast one, so it is not relaxed by
  this.
- **Three scopes, not two.** Durable resources live *above* the program arena:
  name-keyed, surviving every generation, discarded only on purpose. The program
  arena is per-generation, binding-keyed, and freely disposable. Template-local
  state instantiates per voice. The earlier account of the arena had only the
  latter two.
- **The principle underneath most of the above** now lives in `/CLAUDE.md`: if
  state is a pure function of stable coordinates, derive it; retain history only
  where the output genuinely depends on history. It is the founding constraint
  of `crates/pattern` recognised as general, and it is why the live-update
  system has so little to reconcile.
- **Gesture time**: `phase(0…1)` is legal only where `hold(…)` makes duration
  known; a live note uses absolute note-clock `secs(…)` and an explicit gate.
  The illegal combination is a diagnostic, not a silent misinterpretation.
- **Per-voice construction is a builder, not a list.** `voicing(…)` returns a
  temporary with stable member indices and `:each(fn(note, index, count))`
  attaches per-member values; the closure runs once and disappears. No list
  reaches query-time event values, and the same group metadata serves `arp`.
- **Group provenance is not an event value.** A group node carries immutable
  metadata beside `whole`, `part` and the source span: its derived group key,
  original member index and original count. `Named` and `Merge` can neither
  overwrite nor manufacture it. A plain `Stack` remains unrelated parallel
  material; only syntax that knows it is constructing a group — `[a,c,e]` in
  mini-notation, or the voicing builder — creates the metadata. This prevents
  `arp` from treating coincident bass and melody events as one chord and keeps
  group semantics out of `Value`/`ControlMap`.
- **`at_onset` is a typed rate boundary**, `Signal<T> → Init<T>`, sampling at
  the triggering event's timestamp — and it is the *only* legal collapse from
  signal to init, enforced by the rate checker rather than by documentation.
  Without it a signal parameter stays live for the voice's lifetime. It is the
  mirror of the existing rule that lower-rate values lift into higher-rate
  inputs freely while higher-rate ones may never be silently collapsed;
  `at_onset` is that collapse, named.
- **Group transforms.** `degrade` preserves group identity and *original* member
  indices, leaving holes — load-bearing, because `:each`'s `count` must not
  shift and member 3 must keep contour 3. `rev` preserves indices, since it
  reverses time and not voicing order. `every` preserves whatever its chosen
  transform preserves. `arp` *consumes* the group and emits ordinary temporally
  separated events. A revoicing deliberately creates a new group.

## Constraints inherited by whatever comes next

- **Four identities, four rules, and they must not be conflated.** The
  discriminator is **not** "is it visible to a query" — an earlier version of
  this entry said that and it was wrong. It is: **does anything reproducible
  depend on it?** If yes, it must be derived from position. Only a token that is
  consumed at runtime for addressing and never observable in output may be a
  counter.

  | identity | rule |
  |---|---|
  | group key | derived from the group's AST node plus its occurrence span |
  | event seed | derived from event provenance plus occurrence and member identity |
  | stateful signal node | allocated from its lexical binding, within one program |
  | voice handle | runtime counter — addressing only, nothing may seed from it |

  Group key and event seed are both derived-from-position, which is the same
  discipline as `rand.rs` hashing the exact rational rather than drawing from a
  generator; `tests/algebra.rs` already exists to defend the property. Live
  external events carry nondeterministic seeds until recorded, at which point
  the resulting `Timeline` supplies stable ones.
- **The scheduler owes a monotonically advancing frontier.** `Pattern::onsets`
  deliberately does not deduplicate — two identical simultaneous onsets are
  legal music, so nothing at that level can tell an intended pair from a
  re-query. Fill `[frontier, frontier + lookahead)`, then advance. This is what
  Tidal and Strudel both do.
- **Live edits need a generation counter**, so events already queued from the
  previous program can be dropped without suppressing intentional duplicates.
- **The lookahead must absorb a GC pause.** Voice-build runs on the control
  thread; if the language is garbage-collected, 100–200 ms of lookahead makes a
  10 ms collection inaudible, and 20 ms does not.
- **No host-language closure may reach a graph.** See the rate hierarchy in
  `/CLAUDE.md`.
- **The binding layer gets its own crate.** `pattern`, `music` and `synth` must
  never learn what the host language is.
- **Visualisation is not an afterthought.** 42 of 110 real songs call `color`,
  `pianoroll` or `_scope`. That is G4's constituency, and it is why the pattern
  is an AST rather than a tree of closures.
