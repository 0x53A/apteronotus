# Apteronotus

A code-driven synthesizer: patterns in text, real synthesis underneath, live
editing.

Named for the black ghost knifefish, whose electric organ discharge is among
the most stable biological oscillators known — sub-microsecond jitter at
roughly a kilohertz.

## Layout

- `crates/pattern` — the pattern algebra and the mini-notation. **No audio, no
  I/O, no dependencies.** The only way in is `mini::parse`, the only way out is
  `Pattern::query`. Everything here is testable without a sound card, which is
  why it is a crate of its own and why it must stay that way.
- `crates/music` — the first pure slice is written: typed scientific-notation
  pitch, accidentals and pitch ↔ frequency. Scales and modes, chord symbols,
  voicing dictionaries, anchors and inversions remain. Deliberately *not*
  inside `pattern`, which has no musical domain knowledge and should keep it
  that way.
- `crates/synth` — the first voice path is written: data-only `GraphTemplate`,
  validation, a Rust builder and lowering onto fundsp. `InstrumentSpec` and
  the wider primitive set remain.
- `crates/live` — the first output path is written: constant-tempo transport
  with configurable beats per cycle, a monotonic-frontier pitch scheduler,
  and cpal output over `fundsp::Sequencer::backend()`. Editor, live activation
  and MIDI in remain.

`pattern` never learns that fundsp exists; `synth` never learns there is a
language. That seam is the point: it is what keeps the engine liftable behind
an ABI later without disturbing anything above it.

## The rate hierarchy

This is the constraint everything else falls out of. Five rates, and a
different answer at each — note that the scripting language appears in exactly
one row, and it is the topmost:

| rate | frequency | what lives there | who may write it |
|---|---|---|---|
| edit / evaluation | once per live edit | building patterns and staging graphs | the scripting language, freely |
| pattern query | ~1–10 Hz | which note, when | Rust over the pattern AST |
| voice instantiation | once per note | binding event values into a staged graph | Rust over `GraphTemplate` |
| control | 500 Hz (fundsp `envelope`/`lfo` sample at 2 ms and interpolate) | envelopes, sweeps, LFOs | compiled expressions only |
| audio | 48 kHz, blocks of 64 | oscillators, filters, shapers | compiled expressions or built-in nodes |

**`graph = function(n)` runs once per edit, not once per note.** `n.hz`, `n.vel`
and every declared parameter are *symbolic* while it executes; it builds a
`GraphTemplate` that the runtime instantiates per onset. Loops and tables may
generate topology, but a branch on a symbolic note value has to become an
explicit graph operation rather than an ordinary `if`. All four songs stage
cleanly under this rule — none of them contains an `if`, and every loop is over
literal constants. If a real instrument ever needs event-dependent topology the
answer is an explicitly marked dynamic factory, not making every voice dynamic.

**No host-language closure may survive into the graph.** Once
`Sequencer::push` hands a voice to the backend it is rendered *on the audio
thread*; an interpreter callback there can allocate, which can collect, which
is a dropout. Not slow — audibly broken, nondeterministically, under load.
Whatever a user writes as an envelope has to be lowered to something
realtime-safe before it is pushed.

## Derive from coordinates; retain only real history

**If state is a pure function of stable coordinates, derive it. Retain history
only where the output genuinely depends on history.**

This was the founding constraint of `crates/pattern` — a query is a pure
function of its timespan, which is why `tests/algebra.rs` can slice a window at
1/7 and demand the same onsets, and why the crate is testable without a sound
card. It was not recognised as general until it turned up five more times:
seeded randomness derived from position rather than drawn from a generator;
group keys from AST node plus occurrence span; event seeds from event provenance
rather than an allocation counter; a fixed-rate LFO's phase from transport time
rather than an accumulator; and tempo as a signal whose integral is position.

It is a test that can be applied per primitive rather than a slogan. A reverb's
output depends on all past input, so it retains. A pitch tracker's depends on
recent input, so it retains a *bounded* horizon it must declare. An LFO's
depends only on `t`, so it derives and an edit costs it nothing. The payoff is
that the live-update system has correspondingly little to reconcile: state that
is derived never needs migrating, warming or crossfading, because it was never
mutable in the first place.

Corollary, and the reason it matters beyond elegance: **evaluation proposes a
runtime configuration; it never authorises destruction of user material.**
Acquiring a durable resource is evaluation's business, mutating its schema is
not. Growing a buffer extends it; shrinking it, or changing its channel layout
or format, is incompatible and produces a diagnostic while the previous program
keeps playing. Truncation and replacement belong to an explicit command path,
and deleting a declaration merely orphans a resource rather than discarding it.
G1 keeps the sound alive through any failure; this is the same rule protecting
the material.

Note the basis is the propose/destroy separation, not the evaluation cadence.
**Evaluation is explicit — a hotkey, not a keystroke.** Typing continuously
updates parsing, highlighting and diagnostics; only the command evaluates,
validates and activates at a musical boundary, leaving the current program
playing if any of that fails. Two cadences, and the span machinery serves the
fast one. That removes transiently valid intermediate edits as a hazard —
`secs(30)` on its way to `secs(300)` never passing through a live `secs(3)` —
but the rule stands without that argument, because a deliberate evaluation of a
mistaken edit must not eat a recording either. Automatic evaluation stays
available as a later preference, never the architectural default.

## `crates/pattern`

**Time is rational, not float.** A triplet divides a cycle into thirds; in
`f64` those boundaries stop meeting after a few operations and events that
should abut start overlapping. `Frac` is `i64/i64`, normalised, with `i128`
intermediates so comparison and arithmetic cannot wrap. `f64` appears only at
the very edge, where the scheduler turns cycles into seconds.

**Queries are pure and idempotent over overlapping windows.** The scheduler
runs ahead of the audio clock to fill buffers; the editor asks about *now* to
know what to highlight. Both must get the same answer, so nothing may advance
a cursor or draw from a global generator. `tests/algebra.rs` pins this down
directly — a window sliced at 1/7 of a cycle must yield exactly the onsets the
whole window did.

**`whole` and `part` are different things.** `part` is the fragment the query
saw, `whole` is the event's real extent. A voice triggers on `has_onset()`
(`whole.begin == part.begin`), so a buffer boundary bisecting a note continues
it instead of restriking it. Getting this wrong shows up much later as
double-triggering at certain buffer sizes.

**The pattern is an AST, not a tree of closures.** Other tools like Tidal and Strudel both use
closures. The tree costs a little indirection and buys three things a closure
cannot: source spans survive every combinator (so the editor can highlight what
is sounding), the tree is inspectable and serialisable, and reproducibility is
structural rather than promised.

**Randomness is a pure function of position and seed.** `rand.rs` is spelled
out — splitmix64's finaliser over the rational's numerator and denominator —
rather than taken from `rand`, so a dependency improving its generator can
never silently reshuffle everybody's patterns. Hashing the exact rational, not
a float, means a third is a third however it was arrived at. Each `?` in a
source string derives its seed from its byte offset, so two degrades in one
line do not choose identically and the same text always sounds the same.

**Continuous signals have no `whole`.** `sine`, `perlin`, `saw` are sampled at
the midpoint of the query window and produce one event with `whole: None`, so
they can never trigger a voice. They are for steering a parameter. `segment(n)`
is what gives one onsets when you do want to play it.

**Notation desugars into a small node set.** `Silence`, `Pure`, `Stack`,
`Slowcat`, `Timecat`, `Fast`, `Shift`, `Rev`, `When`, `Degrade`, `Signal`,
`Segment`, `Range` — that is all of it. `*`, `/`, `!`, `@`, `?`, `(k,n,r)` are
parser sugar; euclidean rhythms become a `Timecat` of the pattern and silence.
The algebra has no special cases for notation.

**`every` holds branches, not functions.** `When { modulo, offset, then,
otherwise }` — the transform is applied while the tree is built, so the tree
itself stays free of function values and stays serialisable. Same trick for
`off` and `sometimes_by`, which decompose into `Stack` + `Degrade`.

**A sequence is `Timecat`, not `fastcat`.** They differ for nested alternation:
`"<a b> c"` must give `a c` then `b c`, stepping once per bar rather than once
per slot. `Timecat` compresses one cycle of the child into each slot, which is
the behaviour that produces this.

## Decisions taken, with reasons

**fundsp is the DSP layer.** Pure Rust to the bottom (`libm`, `wide`,
`hashbrown`, `tinyvec`, `numeric-array`, `microfft`, `funutd`), `no_std`
capable; the only C-adjacent dependency is `symphonia`, behind the default
`files` feature, so a wasm build turns default features off. Its operator
algebra *is* a patchbay: `>>` is a cable, `|` stacks modules, `&` buses one
source into several, `+`/`*` are a mixer and a VCA.

**`fundsp::Sequencer` is the scheduling architecture, already built.**
`push(start, end, fade, fade_in, fade_out, unit)` in absolute seconds;
`backend()` splits into a frontend that allocates and a backend that renders,
communicating over bounded `thingbuf` channels. That is the control-thread /
audio-thread discipline, realtime-safe by construction. The bridge from this
crate is: query a window in cycles → convert to seconds → push.

**A voice is a function from note to graph, not a graph with knobs.** `n.hz` is
baked in when the unit is instantiated, which is what gives real polyphony. A
persistent graph with `Shared` control values is a monosynth. `Shared` keeps
its place for what it is good at — global, continuously varying controls that
outlive any note.

**Every primitive is exposed in its modulatable form.** Bind `lowpass()` (cutoff
and Q as inputs), never `lowpass_hz()`, and auto-lift scalars to `dc()`. One
function, and any parameter of anything accepts any signal. That single rule is
what makes it a rack rather than a preset browser, and it is why a curve needs
no special support — it is just another signal into the same input.

**The same sandbox runs on desktop and web.** That is the v1 requirement, and it
is the only open assumption that can still invalidate the crate layout. Note the
phrasing: *the same Apteronotus Lua sandbox with full relevant language
semantics*, not a complete PUC Lua distribution. A sandbox is expected to be a
subset — nobody expects `io` in a browser — so peripheral stdlib gaps are a
property of the sandbox, not a defect in it. What may **not** be missing is core
language behaviour: if it looks like Lua, tables and closures have to behave like
Lua's.

Lua-shaped because Lua 5.3+ has `__shr`, `__bor`, `__band`, so it hosts fundsp's
operator algebra almost verbatim (only `^` → `~` for branch, since `^` is
exponentiation).

### What the sandbox exposes

Required language semantics — the spike passes or fails on these:

functions, closures and lexical scope; tables and table constructors; numeric
and generic `for`; `pairs`/`ipairs`; multiple returns and varargs; metatables
and every operator the patch algebra needs; errors and `pcall`; ordinary string
and numeric operations.

| library | what is exposed |
|---|---|
| `base` | selected safe functions |
| `math` | deterministic subset — **no `math.random`**, use the pattern algebra's seeded randomness |
| `string` | practical text manipulation |
| `table` | `insert`/`remove`/`sort`/`concat` and friends |
| `utf8` | probably useful |
| `coroutine` | optional; piccolo already has it |
| `apteronotus` | patterns, graphs, music theory, transport |

Deliberately absent: `io`, `os`, `loadfile`, `dofile`, `package.loadlib`,
`debug`, filesystem-backed `require` (host-controlled stdlib modules only). Also
not promised: PUC bytecode compatibility, `__gc` finalizers, locale-dependent
behaviour, exact error wording, exact table iteration order, C API compatibility.

`load()` is a policy choice, and the answer is **omit it initially** — the corpus
contains zero dynamic evaluation, and it complicates source attribution and
resource accounting for no demonstrated musical benefit.

### The route, and why it is not the obvious one

mlua does support `wasm32-unknown-emscripten`, but **wasm-bindgen does not
support that target** (`rustwasm/wasm-bindgen#2722`) and the two targets' WASM is
ABI-incompatible. So mlua cannot simply become another dependency of the
existing `unknown-unknown` application.

Try **piccolo** first: a Lua VM in pure Rust, `wasm32-unknown-unknown`, one
module, direct interop, no emscripten toolchain. Verified against 0.3.3 (June
2026), it already has closures with proper upvalues, proper tail calls, varargs,
coroutines that yield transparently through Rust callbacks, `_ENV`, fully
recursive metamethods, safe-downcasting userdata, an incremental cycle-detecting
GC in the style of PUC 5.3/5.4, **execution fuel**, and accurate memory
accounting inside its `gc-arena`. The stackless design is *for* sandboxing and
resilience against untrusted-script DoS, so the instruction budget and the
interrupt-a-runaway-loop requirement are design goals rather than a debug hook
bolted on afterwards. That serves G1 better than mlua does.

Its real gaps, from the same check: `io`, `file`, `os`, `package`, `string`,
`table` and `utf8` are missing or sparse; no stack traces; no debugger, probably
ever; poor error messages; frequent pre-1.0 API breakage. Against the contract
above, most of that is irrelevant — but `string` and `table` are *not*
peripheral and are the thing to measure. They are also a bounded, contributable
amount of work under piccolo's MIT/CC0 licensing.

The missing `debug` library has one consequence worth stating separately:
**exact call sites must come from source transformation**, not from a stack
walk. That is the better answer anyway — it is what gives byte-accurate spans
rather than mlua's line-level attribution — but it has to be built, not assumed.

The fallback ladder, in order: use piccolo directly; fork and fill its bounded
gaps; an emscripten Lua module that builds the whole program internally and
transfers **one encoded `Program` per edit** (the same Rust pattern/graph crates
compile into both modules and ship together, so the schema need never be stable
or public — this is far cheaper than it first sounds and preserves real PUC
compatibility); implement the remaining VM functionality in Rust ourselves.
`full_moon` (StyLua's parser) supplies Lua syntax in pure Rust for that last rung.

**Primitives are two-tier.** If it needs internal state or a per-sample
feedback path it is a Rust node; if it is composition of existing nodes it
belongs in a scripting-language stdlib shipped as readable source. `formant`,
`supersaw`, `bitcrush`, ping-pong delay, mid/side width are all stdlib. This
keeps the Rust list small and stops it being the ceiling.

**Missing from fundsp, confirmed by reading it** — a real compressor (only
`Limiter`, `Declick`, `Monitor` in `dynamics.rs`; sidechain wants `follow()`
plus gain maths), a hard-sync oscillator (needs BLEP, not expressible by
composition), an ADSR keyed to note duration (`adsr_live` wants a gate signal,
but a sequencer-instantiated voice knows its length up front), and `Granular`
exists as a struct but is not in the prelude.

**Automation curves are a basis sum, not a breakpoint list.**
`Σ cᵢ·basisᵢ(t − Tᵢ)` over step / ramp / decay / sine. Curves then form a
vector space — addition, scaling and shifting are closed, so superposition
works, where two breakpoint lists cannot be added without merging time grids.
Compiles directly to a flat op array; no branching, no allocation. An intro
gate is literally `step(0) - step(T_end)`.

**A curve has three placements and they use different clocks**: inside a voice
(t = note onset), on a pattern (sampled once per onset, t = transport), on a
bus (persistent, continuous). Placement stays syntactically visible — silently
inferring a clock is charming for a week and maddening after. Known and
documented limitation: the pattern placement samples at onset, so a long held
note freezes its value.

**"Add a filter" is a mix, not a rebuild.** Graph topology is fixed once built,
so a swept effect is always present with its contribution automated. Genuine
topology change is available per note (the voice function branches on a param)
and, later, on buses via fundsp's `Net`.

**ε and δ are different primitives at different rates.** `step`/`ramp`/`decay`
are the control basis; δ has no control-rate meaning (sampled at 2 ms and
interpolated it is a 4 ms triangle) and lives only in the DSP set as
`impulse()`. δ into a resonator is the percussion design — the impulse response
of a second-order section is `e^{−ζω₀t}·sin(ω_d t)`, a struck bar, so drums are
pole placement and `t_se ≈ 3/(ζω₀)` gives the audible decay length directly.
A step at audio rate carries DC and needs `dcblock()`; an edge whose *position*
is modulated aliases, which is why hard sync must be a Rust node.

## Deliberately deferred

Kept as doors, not built:

- **The component-model boundary.** `wasm_component_layer` + `wasmi` runs the
  component model inside a wasm app without the browser engine — proven in
  `~/src/wasm-in-wasm`. If it happens, the component is the *language runtime*,
  not the song: compiling per edit kills the live loop, since there is no rustc
  in a browser. Boundary at events, never samples.
- **Dynamic query-time patterns.** A retained language closure queried during
  playback is the expressive ceiling Strudel has and we do not. The corpus says
  1 song in 110 needs it for a musical reason — and that one works by mutating
  state keyed on query position, which pure querying forbids. If it is ever
  built it needs instruction and event budgets, lookahead, failure isolation and
  a visible marker; it must not be what an ordinary pattern costs.
- **Portable graph serialization.** `GraphTemplate` is data-only from its first
  line, so this stays possible — but playing a serialized graph without the
  language is explicitly not a v1 goal, and rendering to an audio file covers
  the same need. Do not design a versioned interchange format before hearing
  anything.
- **Runtime-loadable DSP.** Three routes, cheapest first: fundsp `Net`/`realnet`
  (dynamic graphs with a realtime-safe swap), a patch matrix, plain core wasm
  modules through the host's *real* engine (`WebAssembly.instantiate` JITs;
  wasmi does not, and that is the whole difference for audio rate).
- **An FPGA backend.** `~/src/UAS/fpga/pisound` has the module ABI and an N×N
  patch matrix. Unlikely, but it is why `schedule` should be a `Backend` trait
  and why `InstrumentSpec` must be declarative data rather than names hardcoded
  in a parser.

## Development

`.envrc` is `use nix`. The shell carries alsa and the GL/windowing libraries
that cpal and eframe dlopen at runtime; `crates/pattern` needs none of it and
builds anywhere.

```
cargo test              # the whole workspace
cargo fmt
```

Reference prototypes, all outside this repo: `~/src/da-beat` (cpal + fundsp +
MIDI, monophonic `Channel` trait — the approach is worth lifting, the code is
not), `~/src/wasm-in-wasm` (component model under wasmi, plus a `wit-derive`
host-binding macro), `~/src/idiosepius` (the study app this may end up
embedded in, and the source of the visual language).
