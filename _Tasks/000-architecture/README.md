# 000 — Architecture

Current state, roadmap and open questions. Kept pruned; the reasoning trail,
including everything rejected, is in `LOG.md`.

Conclusions that have hardened live in `/CLAUDE.md` and `/songs/CLAUDE.md`.
This file is the implementation/status ledger and the boundary of what is *not*
settled.

`codex-architecture.md` is a second, independent architecture proposal, written
before reading this directory. Its major fork — construct a concrete voice once
per note, or stage a symbolic graph once per edit — was settled in session 2 in
favour of staging. `corpus/` holds the Strudel corpus audit that session 2 ran.

`codex-architecture-v2.md` is the post-corpus proposal. It incorporates the
web-language requirement, defines the intended Lua sandbox, keeps Lua out of
ordinary runtime queries, adds the corpus-driven join and music layers, and
demotes stable graph serialization from architectural center to optional
feature.

## Where it stands (2026-07-29)

| | |
|---|---|
| `crates/pattern` | **foundational and structured-control slices complete.** No dependencies. Mini-notation, cyclic algebra, finite timelines, exact event holds, continuous signals and numeric event/signal arithmetic, group/event provenance, group-aware arpeggiation, source spans, parse limits, scalar/curve control maps, validated map-producing merge inputs, stable structural order and left-closed onset sampling. Event/event arithmetic remains behind explicit select/join semantics. |
| `songs/*.eod` | **written as the specification; all seven songs run whole.** Every shipped document reaches routed persistent measured audio. `jamming.eod` additionally has host-fed acceptance through its shared analysers, transport control, external trigger, trigger-side modifiers and onset-bound voice scheduling; the native player attaches its first logical input to a compatible default device. |
| `songs/neon.eod`, `songs/jamming.eod` | session 3, covering free time + gestures and live input respectively. See below. |
| the Strudel corpus audit | **done.** 723 snippets, 249 k chars. See `LOG.md` and `corpus/`. |
| the synthesis corpus audit | **not started.** This is the one that bears on the graph model. |
| `crates/music` | **typed pitch, key and literal-chord slices written.** Scientific pitch notation, fractional MIDI, accidentals, pitch ↔ frequency, enharmonic tonic classes, major/minor/dorian context, chord-symbol parsing and deterministic literal voicing shapes are data-only. Patterned theory inputs, scale degrees and automatic voice leading remain. |
| `crates/synth` | **voice, curve-valued parameters, routing and persistent analysis slices written.** Data-only `GraphTemplate`, range-checked scalar/curve `ParamValue`, two-coordinate lifetime analysis, spans, validation/budgets, graph inputs, per-note and program controls, routed stems, `PatchTemplate`, deterministic per-voice initialization, allocation-bounded interpolating delay, shared envelope/pitch/onset analysis, flat transport sequences, diffusion/FDN, stereo width/reverb/limiter, symbolic pow/clamp/decay and fundsp lowering. |
| `crates/transport` | **pure score clock written.** Device-free constant/step/ramped tempo maps and exact cycle↔seconds conversion are shared by Lua-owned programs and live scheduling. |
| `crates/live` | **voice, persistent and external-onset runtime paths written.** Monotonic-frontier single- and multi-track schedulers consume constant/ramped tempo maps, routed main/bus stems, persistent source/whole-stem processing, shared controls, transactional generations, trigger recording, onset-sampled init bindings, native default-device input through a preallocated capture ring, and cpal frontend/backend output. Multi-track windows lower completely before any voice is pushed; persistent state survives those windows. Replacement crossfade, browser input attachment, native input selection and MIDI binding remain. |
| `crates/lua` | **the external `supersaws.eod` song and ordinary voice/finite-score paths are connected end to end.** Lua evaluates to an owned program; tempo maps, stored-but-not-yet-consumed tonal context and `timeline { at(...) }` use shared Rust types. Literal chord symbols, anchors and named voicings expand during evaluation to grouped fractional-MIDI events; root selection survives finite capture through a domain-neutral primary-member marker. Exact cycle/beat holds live in the pattern AST under a distinct host-selected query-look-back ceiling, seconds holds resolve through mapped finite placement, and `phase(...)` breakpoint curves remain live for the declared note duration. Score signals are sampled through setters at note onset. Routed voices, typed beat durations, clipped explicit-spacing arpeggiation, deterministic ply/random choice, per-track scalar sends, program-scope send returns/master finalization, fixed-frequency trigger labels, persistent patches/controls/buses, symbolic pow/clamp/decay and stereo reverb/limiter reach the live executor. `velocity` is the sole score-level amplitude setter, graph/master amplitude uses `mul`, and `n.velocity`/`n.duration` match synth exactly while declared `gate` remains distinct. |
| `crates/app` | **native and wasm voice and compatible-persistent GUI paths written.** The lexically highlighted Lua editor and explicit Run/Stop commands drive the production evaluator, tempo-mapped scheduler, persistent arena, fundsp and cpal. Native owns that work on a player thread; the reusable wasm custom element evaluates explicitly and advances lookahead on the browser event loop, with wasm-pack packaging and a GitHub Pages workflow. Program controls appear as live faders. Voice-only and persistent-compatible edits retain frontier activation and fader/DSP state; incompatible persistent/layout or tempo-map changes transactionally prepare a replacement and then hard-reset at cycle zero. Parser diagnostics while typing, files, transport controls, threaded browser evaluation, clock-map reconciliation and replacement crossfade remain. |

Lua-boundary decisions, provisional policy, and explicit non-decisions are
catalogued in [`crates/lua/DESIGN.md`](../../crates/lua/DESIGN.md). Keep that
distinction intact when promoting first-slice behavior into architecture.

The sound path now covers mini-notation note names, MIDI values or validated
fixed-frequency trigger labels, multiple
staged voices, structural transforms, scalar/curve event controls, routed
stems, and persistent processing. Acceptance coverage renders Rust-built and
Lua-built programs through pattern → scheduler → synth → sequencer, measures
pitch and stereo energy, proves held-note curves remain live, and exercises
shared persistent controls. None needs a sound card.

The native and browser GUIs are the same path with a device at the end, not
second players. Their host policies and transactional ordering are documented in
[`crates/app/README.md`](../../crates/app/README.md). Persistent sources, routed
stems, whole-layout processors and controls are connected. Candidates with the
same persistent controls, buses and activated `run` patch graphs are rebound to
the live arena and continue at the scheduling frontier with shared-control and
DSP state intact; inert patch declarations may change. Incompatible persistent
or output-layout replacement prepares a complete new stream and then performs
an explicit cycle-zero hard reset. Crossfade remains required rather than
allowing that reset policy to become a permanent substitute.

The current wasm host also evaluates explicit Runs and advances lookahead on
the browser main thread, where CPAL schedules its WebAudio buffers. The
browser-specific 2048-frame buffer mitigates ordinary scheduling jitter but
cannot guarantee an uninterrupted deadline during an expensive evaluation.
Moving evaluation behind an owned/transferable worker boundary or replacing
CPAL's main-thread scheduler with an AudioWorklet remains browser-host work,
separate from the pure evaluator and graph contracts.

## Roadmap and completed slices

The one assumption that could invalidate the crate layout is retired: the same
sandbox now builds natively and for `wasm32-unknown-unknown`, enforces fuel and
memory limits, and returns only owned Rust data. Piccolo needed a small audited
portability patch and bounded library/metamethod fixes; those are vendored and
documented. Direct `pattern(...)`, `play(...)`, `degrade(...)`, and
`sometimes(...)` source transformation now preserves original byte-offset
identities; graph-expression spans and a complete diagnostic source map remain
integration work, not a language-feasibility question.

1. **Browser-language foundation, typed synthesis seam and first call-site pass
   complete.** Continue graph-expression attribution and the wider song-level
   API in `crates/lua`, without moving Lua objects or callbacks below evaluation
   rate. The device-free transport split, owned tempo map and finite timeline
   placement are complete.
2. **Control maps and curve leaves complete in `crates/pattern`.** `Value` is
   either a leaf or a sorted named map; leaves are number, text, bool or a
   pattern-owned note-clock curve. `List` and nested maps remain deferred.
3. **Merge sampling complete.** A statically map-producing RHS is queried with
   a left-closed zero-width span at each left onset; the first stable structural
   result overrides matching fields without multiplying voices.
4. **Initial slice complete: the smallest data-only symbolic graph builder.**
   `GraphTemplate` owns an operand-list DAG and outputs, with one
   `Input { source, src }` per port. There is no versioned primitive registry,
   bundle format or cross-backend contract. No `Box<dyn AudioUnit>`, fundsp
   node, Lua closure or backend pointer sits inside it. `lower.rs` is the only
   assembly boundary; backend-private custom units in `analyzer.rs`, `fdn.rs`
   and `input.rs` implement primitives unavailable as fundsp compositions.
5. **Initial slice complete: one path that makes sound.** Symbolic note
   frequency → sine → low-pass → duration-keyed envelope/VCA → output, through
   the exact pattern frontier and `fundsp::Sequencer`. Scheduling uses
   `max(gate + gate_tail, absolute_horizon)` per concrete event.
6. **Underway: transcribe progressively harder instruments** from the original
   four songs against that builder. All five `poles.eod` voices are complete as
   framework fixtures:
   `ring` derives a band-pass Q from the requested settling time and attaches
   conservative tail metadata while remaining composition rather than a DSP
   primitive. Declared parameter ranges propagate through scalar arithmetic, so
   `n.ring * 0.85` and `n.ring / i` have construction-time upper bounds without
   sampling a runtime signal. The bell also proves literal topology-building
   loops, one-source fan-out, parallel-tail maxima and one template bound to
   different pitches. The tom proves a note-clock control signal can sweep a
   filter port; the cymbal proves one short noise excitation can fan out into a
   resonator bank. Timbre matching is not a gate; move on.
7. **Initial buses-and-sends slice complete.** `BusLayout` assigns opaque
   lexical `BusId` handles to a program-wide flattened channel layout.
   Graph-level sends tap internal symbolic sources; event-level sends copy the
   finished voice at a per-onset scalar level. Routed lowering sums both into
   main/bus stems, validates every channel shape before backend construction,
   includes send-only paths in the voice tail, and refuses to silently drop
   sends through the old main-only path. Persistent bus processors consume the
   same flattened stems through explicit graph input ports.
8. **Initial persistent `patch` execution complete.** Program-scope `ControlId`
   handles lower to shared atomic values; one handle becomes one graph node
   with fan-out, and handles from different program arenas cannot alias.
   `PatchTemplate` rejects per-note inputs and clocks. The live executor mixes
   zero-input runs with routed voices, applies exact-arity whole-stem runs in
   declaration order, and keeps oscillator/filter/delay state across scheduling
   windows and control changes. The GUI exposes the controls as sliders.
   Equivalent persistent program data is compared across arena-scoped handles
   and reused across edits; changed tracks and voice graphs bind into the
   retained arena. Explicit `control_signal(pattern, period)` compiles a
   bounded numeric period into persistent transport-clock data. Replacement
   crossfade and host audio-lane binding remain deliberately separate.
9. **Also complete because their contracts were settled:** finite `Timeline`
   querying with captured external-event ordinals; group provenance and
   event-derived seeds; `init_random`; piecewise step/ramped `TempoMap`
   integration in both directions; caller-supplied graph budgets;
   transactional generation activation; live-trigger recording into a
   timeline; and the allocation-bounded interpolating delay primitive. Delay
   time is modulatable, maximum buffer length is publication metadata, and
   serial delays accumulate tails. Feedback networks still need an explicit
   representation; a delay node does not silently make arbitrary graph cycles
   legal.
10. **Serialization only when it earns its place** — for caching, sharing or
   offline rendering, not as an upfront format.
11. **Shipped-song Lua integration complete.** Structural cyclic transforms, event controls,
    routed graphs and compatible persistent reuse are connected. Tagged
    absolute/cycle durations now prevent explicit `secs`/`bars` crossings.
    Tempo/timeline and transport-signal arithmetic are connected. Music
    Literal theory/arp, score sends, non-pitch triggers, `ply`/random choice,
    continuous track ducking, bounded filtered-feedback delay, Karplus–Strong
    pluck and the wider graph operations needed by four complete songs now run.
    Previous-note portamento, timed autonomous persistent activation, shared
    live-input analysers, explicit transport control and external-onset
    scheduling now run. Deeper query-time theory, physical device binding and
    general feedback causality remain.
12. **Known DSP conformance debt.** The newer fixed-configuration wrappers
    `FeedbackDelay`, `Chorus`, `GateEnv` and `EnvelopeFollower`, plus FDN
    damping/modulation coefficients, have not yet been lifted to symbolic
    signal inputs. Allocation/stability ceilings such as delay capacity and
    `Fdn::max_t60` remain construction metadata, but the other frozen values
    are not a new exception to the modulatable-primitive rule. Resolve them
    deliberately before treating this first corpus-compatible vocabulary as a
    general DSP surface.

The synthesis corpus audit (below) can run in parallel with any of this; it
gates nothing but informs step 4 onward.

## Open questions

Blocking, in rough order of cost-if-wrong.

**Resolved: an event can carry a note-clock curve, but not a live transport
signal.** A voice needs control-rate inputs that stay
writable after onset — polyphonic pressure, a bend gesture, a brightness swell
across a held note. `ControlValue::Curve` carries the pattern-owned basis sum
whole; a zero-width query never samples it. `NoteSeconds` and `NotePhase` are
explicit. Transport signals passed through `Merge` are instead sampled at
onset, and persistent bus automation remains separate.

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
identical in form to `>> velocity(0.8)`. That is the `Value::Curve` shape, reached
by the *notation* even though the same session's Rust sketch proposed a separate
`GestureMap`. When the notation and the struct disagree, the notation is the
specification.

**Multi-value events: ordered or named?** A control map solves
`{note, velocity, pan}`. It does not obviously solve `[string, fret, articulation]`,
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
against. The current slice only stores and validates it; no scheduler, synth,
literal pitch, or chord-expansion path consumes it yet. It must be neither
permanently decorative nor hidden mutable scheduler state.

**Previous-onset portamento now has a narrow home.** For the exact
`slew(n.hz, time)` spelling, the monotonic scheduler retains the preceding pitch
per track and supplies it as instantiation context. The staged graph lowers a
finite onset-relative glide, so `techno.eod` gets real previous-note pitch
motion without pretending that a per-note constant changed after startup.
Scheduling failure cannot advance this history, simultaneous events share one
preceding pitch, and live evaluation clears it until track reconciliation
exists.

That narrow answer does not replace the persistent `patch` lifetime. A 303
whose oscillator/filter state must survive note boundaries, a free-running
monosynth, or a live controller steering whichever note is active uses a
persistent patch; a cyclic numeric score may drive it through the explicit,
bounded `control_signal(pattern, period)` bridge. The rule remains **`voice` unless persistence is
musically observable**. A bend inside one held note is simply a note-relative
curve and needs neither mechanism.

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

**Graph oscillator phase is an accumulator, and the transport-clock curve is
what fixes it.** `Op::Sine` and `Op::Pulse` lower to free-running fundsp
oscillators. In a per-note voice the note clock is the correct coordinate, so
nothing is wrong; in a persistent `patch` the phase starts when the instance
does and a hard reset re-phases it. Compatible-edit arena reuse masks this
rather than resolving it, which is why it has not yet been felt.

The shape of the fix is settled and recorded in `/CLAUDE.md`: a `Curve` gains a
transport clock with a period and is evaluated at `t mod period`, so a repeating
gate is one clock variant instead of `transport_seconds()`, `%` and `lt(a, b)`.
The rejected alternative and the reasons are in `LOG.md`. **The ordering
dependency is real** — a periodic curve immediately asks whether its period is
seconds or bars, which is the entry directly above. Tempo/timeline integration
therefore comes first; doing transport curves before it would mean defaulting to
seconds.

Until then a bounded note-local strobe is already expressible with the shipped
notation as a summed window train, `Σ [step(kT) − step(kT + T_on)]`. That is the
case the exact piecewise-linear range partition was built for — ±1 step
coefficients cancel, so the range proves `[0, 1]` exactly and `activity()`
returns a finite horizon at the last window. Two consequences worth knowing
before writing one: the voice lives to that last window regardless of gate, and
the count is explicit because a note-clock curve has finitely many terms.

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

**`duck(pattern, amount)` is not a per-note parameter. Resolved for track
output.** The scheduler compiles numeric trigger events into transport-aligned
smooth release segments on the control thread, then multiplies every routed
lane of each voice with the allocation-free envelope. Persistent cyclic
pattern control is separately explicit through `control_signal`; arbitrary
live pattern callbacks remain outside the runtime.

**`to(send, x)` is used at two levels. Resolved.** A graph send taps an internal
symbolic source; an event send copies the completed voice at onset. Both target
arena-scoped `BusId` handles and routed lowering sums them into the same stem.

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
- **Generic `hz(…)` is reserved; onset-bound Hertz is implemented.** A scalar
  Hz-to-MIDI conversion would retune a literal frequency under a future tuning
  context, so generic pattern `hz` remains deferred. The corpus spelling
  `voice.hz(at_onset(tracked_signal))` instead owns an `InitControlBinding` and
  samples literal Hertz directly when the external voice is instantiated.
  `note("d4")` remains the privileged primary-value setter for authored pitch.
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
  warms a replacement from retained input, off the audio thread; analyser ops
  now expose `warmup_seconds()` separately from audible `response_tail`.
  Followers with exponential support use their authored response time as the
  explicit practical warm-up convention rather than claiming exact finite
  mathematical memory. This metadata is not wired to replacement yet.
  *Audible tails* — reverbs,
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
  original member index, original count, and an optional domain-neutral primary
  member selected at construction time. `Named` and `Merge` can neither
  overwrite nor manufacture it. The chord builder marks its harmonic root so
  `root_notes()` can select it after timeline capture without putting chord
  symbols into `pattern`. A plain `Stack` remains unrelated parallel
  material; only syntax that knows it is constructing a group — `[a,c,e]` in
  mini-notation, or the voicing builder — creates the metadata. This prevents
  `arp` from treating coincident bass and melody events as one chord and keeps
  group semantics out of `Value`/`ControlMap`.
- **`at_onset` is the live-signal typed rate boundary**, `Signal<T> → Init<T>`,
  sampling at the triggering event's timestamp. It is the only legal collapse
  from a live graph signal to a triggered voice init value; pattern `Merge` is
  the separate transport-pattern boundary.
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
- **Explicit arp spacing clips to group extent.** Spacing fixes successive
  onset distance and nominal slot length, but does not extend a cyclic group or
  finite `at(...)` placement. A slot is clipped at the group's end and a member
  beginning outside it is omitted. An overflow/derived-extent policy would
  require a finite phrase boundary rather than making cyclic query slices
  disagree.

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
