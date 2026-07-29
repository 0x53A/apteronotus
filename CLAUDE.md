# Apteronotus

A code-driven synthesizer: patterns in text, real synthesis underneath, live
editing.

Named for the black ghost knifefish, whose electric organ discharge is among
the most stable biological oscillators known — sub-microsecond jitter at
roughly a kilohertz.

## Layout

- `crates/pattern` — the pattern algebra and the mini-notation. **No audio, no
  I/O, no dependencies.** Cyclic patterns and finite `Timeline`s share the same
  pure `Pattern::query` boundary; recorded timelines preserve arrival ordinals,
  and chord groups carry provenance beside values.
- `crates/music` — typed scientific-notation pitch, accidentals, pitch ↔
  frequency, owned major/minor/dorian key context, literal chord symbols,
  anchors, slash basses and the first deterministic named voicings are written.
  Patterned music-theory inputs and wider dictionaries remain. Deliberately
  *not* inside `pattern`, which has no musical domain knowledge and should keep
  it that way.
- `crates/synth` — data-only `GraphTemplate`, validation and caller-supplied
  publication budgets, per-note voices, routed stems, program-scope writable
  controls, explicit graph inputs, an allocation-bounded interpolating delay,
  and the first persistent `PatchTemplate` lowering onto fundsp. The wider
  primitive set remains.
- `crates/transport` — device-free constant/ramped `TempoMap` and exact
  cycle↔seconds conversion shared by owned programs and schedulers.
- `crates/live` — a monotonic-frontier pitch scheduler over transport clocks,
  routed voice stems, the first persistent run/control executor,
  transactional generation activation, external-trigger recording, native
  default-device input through a bounded capture ring, a smoothed `MasterGain`
  at the device edge, and cpal output over `fundsp::Sequencer::backend()`.
  Browser input attachment, native input selection, MIDI binding and persistent
  replacement remain.
- `crates/lua` — a fresh, sandboxed Piccolo VM per evaluation, producing only
  owned pattern/graph/program data. The same crate builds natively and for
  `wasm32-unknown-unknown`. Typed graph operators, voices, persistent patches,
  controls, buses/sends, structural pattern transforms and source-site
  attribution for notation/random transforms are connected. Tempo maps and
  finite timeline placement are owned `Program` data; graph-expression spans
  and the wider score API remain.
- `crates/songs` — the specification corpus from `songs/`, embedded as static
  strings for other applications to consume. No dependencies, no evaluator, no
  audio; the crate is a delivery mechanism, and it exists because
  `~/src/idiosepius` had started keeping its own copies.
- `crates/render` — the device-free half of a Run, and offline rendering to a
  WAV file. It owns the `Program` → scheduler/arena binding (`playable_channels`,
  `persistent_runtime`, `scheduled_tracks`, `scheduled_runs`) that the GUI
  player and the corpus test also use, so a rendered file cannot disagree with
  what the device would have played. With no deadline there is no lookahead:
  the whole window is scheduled before the first block, and `Sequencer` is
  driven directly instead of split frontend/backend. Renders are reproducible
  sample for sample, which is what lets a render be measured against a
  reference recording. `--stems` emits the routed bus layout rather than the
  main channels, because a per-bus comparison is a real error signal where
  mix-against-mix confounds every part at once.
- `crates/app` — the first native and wasm user paths: a lexically highlighted
  Lua editor, explicit Run/Stop commands, activation diagnostics, persistent
  patch/bus execution, a permanent master fader and live control faders over
  the production evaluator, scheduler and cpal output. It is also where the
  corpus is pinned against the backend: `src/corpus.rs` asserts that all seven
  songs still evaluate, lower and open a stereo output. Native uses a
  control-thread player; the custom
  web component evaluates explicitly and advances lookahead on the browser
  event loop, with wasm-pack packaging and GitHub Pages deployment.
  Compatible persistent edits retain the live arena and control values at the
  scheduling frontier. Incompatible persistent/layout edits use an explicit
  transactional hard reset until replacement crossfade exists; file handling
  and parser/type diagnostics while typing also remain.

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

**`graph = function(n)` runs once per edit, not once per note.** `n.hz`,
`n.velocity`, `n.duration`, `n.pan` and every declared parameter are *symbolic*
while it executes; it builds a `GraphTemplate` that the runtime instantiates per
onset. Loops and tables may generate topology, but a branch on a symbolic note
value has to become an explicit graph operation rather than an ordinary `if`.
All seven specification songs' graph functions stage cleanly under this rule —
none contains such an `if`, and every topology-building loop is over literal
constants. If a real instrument ever needs event-dependent topology the answer
is an explicitly marked dynamic factory, not making every voice dynamic.

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

The LFO entry is the one the implementation has not yet earned. `Op::Sine` and
`Op::Pulse` lower to free-running fundsp oscillators, so their phase comes from
an accumulator. Inside a per-note voice that is correct — the note clock *is*
the right coordinate there — but inside a persistent `patch` it means phase
starts when the instance does, and a hard reset re-phases it. Compatible-edit
arena reuse hides this; it does not fix it. See the transport-clock curve door
below.

It is a test that can be applied per primitive rather than a slogan. A reverb's
output depends on all past input, so it retains. A pitch tracker's depends on
recent input, so it retains a *bounded* horizon it must declare. Analyser ops
now expose that as `warmup_seconds()` separately from audible response tail;
the metadata is intentionally inert until incompatible persistent replacement
can pre-roll a candidate from retained input. An LFO's depends only on `t`, so
it derives and an edit costs it nothing. The payoff is that the live-update
system has correspondingly little to reconcile: state that is derived never
needs migrating, warming or crossfading, because it was never mutable in the
first place.

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
`Group`, `Slowcat`, `Timecat`, `Fast`, `Shift`, `Rev`, `When`, `Degrade`,
`Signal`, `Segment`, `Range`, `Math`, `Timeline` — that is all of it. `*`, `/`,
`!`, `@`, `?`, `(k,n,r)` are parser sugar; euclidean rhythms become a `Timecat`
of the pattern and silence. `Math` accepts scalar/signal or event/signal
arithmetic but rejects event/event operands until temporal join semantics are
explicit. The algebra has no special cases for notation.

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

**A patch is one persistent graph instance with explicit writable controls and
audio inputs.** It may not refer to `n.*`, declared per-note parameters,
note-clock envelopes or per-voice initializers. Program-scope `ControlId`
handles lower to one shared atomic node per graph with fan-out; handles are
arena-scoped, so equal declaration ordinals in two program generations cannot
alias. In the first live executor, a zero-input `run` is an autonomous source
mixed with routed voices. An input-bearing `run` must consume every flattened
main/bus lane and processes that layout in declaration order; exact arity
prevents accidental modulo folding between buses. Instances survive scheduler
windows. A finite autonomous run inserts its gate at nonterminating graph
sources so downstream state drains before removal; finite whole-stem
processors remain deferred because their input-retention semantics differ.
Replacement, host input selection/browser attachment and clocked patch
automation remain separate concerns, not hidden inside this lifetime slice.

**The master volume is a gain between the engine and the device, not part of
the score.** The obvious alternative was a `control` the score declares and the
host drives, which is exactly how live faders work — but `master(...)` may be
declared only once, so a score that already has one cannot be given a gain stage
from outside, and every interesting score has one. A master volume that stops
working the moment a real song is pasted in is not a master volume. `MasterGain`
is therefore a `fundsp` node `crates/live` inserts downstream of everything the
language can express, on every route including the one with no persistent
processor, so it works on any program and asks nothing of it.

Three consequences worth keeping. It is smoothed — a `Shared` read straight into
a multiplier steps once per block, and a step in gain is a click, worst at
exactly the moment somebody reaches for the master. It attenuates only, because
boost at the one point with no headroom left and no meter on it is just
clipping; gain belongs in the score where it is written down. And the handle
belongs to a stream while the *level* belongs to the host, which reapplies it
before `play` — a fader that reset with the transport would be one the user has
to find again after every incompatible edit.

**Per-voice randomness derives from event provenance.** Pattern events hash
their construction origin, exact occurrence span, and group-member identity.
Recorded events additionally carry their captured ordinal. The scheduler binds
that seed into `Note`. `init_random(stream, min, max)` derives init-rate values
from it, while `noise()` and `pink()` derive one backend stream per structural
node so separate onsets do not restart the same short waveform. Runtime voice
handles remain addressing tokens and must never seed sound.

**Voice lifetime has two coordinates.** `gate_tail` is response duration after
scheduled release; `absolute_horizon` is a finite activity time measured from
onset. The scheduler ends a voice at
`max(gate + gate_tail, absolute_horizon)`. Both coordinates add through a
stateful response and take component-wise maxima at joins, so a delayed
gate-bound branch cannot lend its delay to an unrelated long envelope.
Stateful stdlib compositions may attach response metadata without becoming DSP
primitives: `ring(hz, decay)` remains multiplication plus a band-pass.

An event curve is immutable data, so instantiation may inspect its declared
horizon; this is not sampling a runtime signal. `NoteSeconds` horizons are
bounded by `ParamSpec` for publication, while `NotePhase` is clamped to the
reachable `[0, 1]` interval. Sources without a finite terminating horizon —
oscillators, noise, DC, LFOs and nonzero-settling curves — are deliberately
bounded by the note gate. Audio/control signals are never sampled to discover
lifetime.

**Graph sends and event sends are different data.** A graph send taps named
internal `Source`s and may use a symbolic signal as its level; an event send
copies the completed voice outputs with a scalar bound once per onset. Both
target opaque program-scope `BusId` handles, never strings. Routed lowering
flattens main and bus stems into one channel layout and sums collisions before
anything reaches fundsp. A graph containing sends is rejected by the
main-output-only lowering path rather than silently losing its wet path. Bus
effects remain persistent processors over those stems; routing does not make
them part of a voice.

**Every primitive is exposed in its modulatable form.** Bind `lowpass()` (cutoff
and Q as inputs), never `lowpass_hz()`, and auto-lift scalars to `dc()`. One
function, and any parameter of anything accepts any signal. That single rule is
what makes it a rack rather than a preset browser, and it is why a curve needs
no special support — it is just another signal into the same input.

**The same sandbox runs on desktop and web.** The language-feasibility risk is
retired: `crates/lua` builds natively and for `wasm32-unknown-unknown`, with no
alternate browser VM or language boundary. The native integration suite verifies
the behavioral contract; executing it in a browser remains shell/harness work,
not a crate-layout assumption. The contract is the Apteronotus Lua sandbox with
full relevant language semantics, not a complete PUC Lua distribution. A
sandbox is expected to be a subset — nobody expects `io` in a browser — so
peripheral stdlib omissions are policy, not defects. Core language behaviour
remains non-negotiable: tables, closures, varargs, loops and metamethods behave
like Lua's.

Lua-shaped because Lua 5.3+ has `__shr`, `__bor`, `__band`, so it hosts fundsp's
operator algebra almost verbatim (only `^` → `~` for branch, since `^` is
exponentiation).

### What the sandbox exposes

Required language semantics, now covered by the integration suite:

functions, closures and lexical scope; tables and table constructors; numeric
and generic `for`; `pairs`/`ipairs`; multiple returns and varargs; metatables
and every operator the patch algebra needs; errors and `pcall`; ordinary string
and numeric operations.

| library | what is exposed |
|---|---|
| `base` | selected safe functions |
| `math` | deterministic subset — **no `math.random`**, use the pattern algebra's seeded randomness |
| `string` | Piccolo's practical text operations |
| `table` | Piccolo plus sandbox implementations of `insert`/`remove`/`sort`/`concat` |
| `coroutine` | Piccolo's core coroutine library |
| `utf8` | not exposed yet; no specification song requires it |
| `apteronotus` | current pattern/synthesis builders plus tempo maps and finite timelines; music theory remains |

Deliberately absent: `io`, `os`, `loadfile`, `dofile`, `package.loadlib`,
`debug`, filesystem-backed `require` (host-controlled stdlib modules only). Also
not promised: PUC bytecode compatibility, `__gc` finalizers, locale-dependent
behaviour, exact error wording, exact table iteration order, C API compatibility.

`load()` is a policy choice, and the answer is **omit it initially** — the corpus
contains zero dynamic evaluation, and it complicates source attribution and
resource accounting for no demonstrated musical benefit.

### The implemented route, and why it was not the obvious one

mlua does support `wasm32-unknown-emscripten`, but **wasm-bindgen does not
support that target** (`rustwasm/wasm-bindgen#2722`) and the two targets' WASM is
ABI-incompatible. So mlua cannot simply become another dependency of the
existing `unknown-unknown` application.

`crates/lua` uses a vendored **Piccolo** revision: a pure-Rust Lua VM that
targets `wasm32-unknown-unknown` directly, with no Emscripten toolchain. Its
stackless executor provides enforceable fuel, and `gc-arena` provides measured
memory accounting. Apteronotus fills the table-library operations its topology
builders need, removes host access and nondeterministic randomness, and carries
an audited wasm portability patch. The crates.io 0.3.3 release also lacked the
arithmetic/bitwise metamethod dispatch required by the graph algebra and had a
table-growth panic; the pinned revision contains those fixes. Exact revision and
patch provenance live in `vendor/piccolo/APTERONOTUS.md`.

The missing `debug` library means exact call sites come from source
transformation rather than stack walking. The first pass is implemented:
direct `pattern(...)` and `play(...)` calls receive original byte offsets and
feed `mini::parse_at`; direct `degrade(...)` and `sometimes(...)` use the same
site identity for deterministic seeds. Graph-expression spans and a complete
diagnostic source map are the remaining attribution work.

The old fallback ladder—further Piccolo fixes, an Emscripten module transferring
one encoded `Program` per edit, or a purpose-built VM—is retained only in the
historical architecture proposals. None is currently needed.

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
gate is literally `step(0) - step(T_end)`. Event breakpoint syntax is only
construction-time sugar: successive value differences desugar to held ramp
terms before the curve enters event data.

**A curve has three placements and they use different clocks**: a graph-local
curve and a curve carried in an event both run on a note clock; a pattern signal
runs on the transport clock and `Merge` samples it at the left event's onset; a
future bus curve is persistent and continuous, and its shape is a **periodic
transport clock** rather than a new signal algebra. Placement stays
syntactically visible. Pattern merge, external-trigger setter sampling and
`at_onset` are the explicit signal-to-init boundaries at their respective
layers. The external-trigger case samples at the observed transport cycle
because a live edge has no queryable future. A sampled pattern signal freezes
for a held note; an event-carried curve does not.

**An interpolating delay declares its allocation range when staged.** Its delay
time remains an ordinary modulatable signal, but a signal cannot be sampled to
discover a safe maximum. `DelayRange` therefore supplies construction-time
minimum/maximum seconds, lowering allocates once from the maximum, and graph
publication budgets count the sum of those maxima. A delay node retains
history; it does not by itself authorize an arbitrary graph cycle. Feedback
gets an explicit representation before cycles become legal.

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
  the same need. `crates/render` now does, which removes the last practical
  argument for an interchange format. Do not design a versioned one before
  hearing anything.
- **Runtime-loadable DSP.** Three routes, cheapest first: fundsp `Net`/`realnet`
  (dynamic graphs with a realtime-safe swap), a patch matrix, plain core wasm
  modules through the host's *real* engine (`WebAssembly.instantiate` JITs;
  wasmi does not, and that is the whole difference for audio rate).
- **An FPGA backend.** `~/src/UAS/fpga/pisound` has the module ABI and an N×N
  patch matrix. Unlikely, but it is why `schedule` should be a `Backend` trait
  and why `InstrumentSpec` must be declarative data rather than names hardcoded
  in a parser.
- **The persistent transport-clock periodic control.** Score-level `Signal`
  patterns already derive periodic values from rational cycle position and are
  sampled by `Merge`. `control_signal(pattern, period)` is the explicit
  continuously running bus/patch placement: evaluation queries one bounded,
  gap-free numeric period and compiles it to a flat `TransportSequence`.
  Changing tempo is rejected because there is no single repeating seconds
  projection. No Lua or pattern query reaches the audio thread.

  The current `TransportSequenceUnit` advances a sample counter from its
  instance origin. Compatible edits reuse that instance and hard resets begin
  at cycle zero, so current output is coherent, but this is deliberately
  recorded clock debt: before replacement crossfade may start a patch at a
  nonzero transport coordinate, lowering must receive that absolute coordinate
  and derive phase from it. A replacement must never mistake its own age for
  transport position.

## Development

`.envrc` is `use nix`. The shell carries alsa and the GL/windowing libraries
that cpal and eframe dlopen at runtime; `crates/pattern` needs none of it and
builds anywhere.

`cargo run` with no arguments launches the application. Keeping that true has
one rule: exactly one default member may carry a binary, because cargo has no
way to break a tie. A new crate belongs in `default-members`, unless it has a
`[[bin]]`. The audio-free subset is still reachable by `-p` selection, which is
the only reason the default list was ever short.

```
cargo run
cargo test --workspace
cargo check -p apteronotus-lua --target wasm32-unknown-unknown
cargo clippy -p apteronotus-pattern -p apteronotus-music \
  -p apteronotus-synth -p apteronotus-live -p apteronotus-lua \
  -p apteronotus-songs -p apteronotus-render -p apteronotus-app \
  --all-targets --no-deps -- -D warnings
cargo fmt
```

Hearing a change is `cargo run`; *looking* at one is

```
cargo run -p apteronotus-render -- songs/techno.eod --seconds 8 -o /tmp/techno.wav
```

which reports voice count and peak level, and defaults to 32-bit float so an
overloaded mix arrives diagnosable rather than already flattened. Renders are
reproducible, so two of them are directly comparable — that is the intended
use, not export.

Piccolo is a pinned upstream snapshot and is intentionally not subjected to
the first-party warnings-as-errors Clippy gate.

Reference prototypes, all outside this repo: `~/src/da-beat` (cpal + fundsp +
MIDI, monophonic `Channel` trait — the approach is worth lifting, the code is
not), `~/src/wasm-in-wasm` (component model under wasmi, plus a `wit-derive`
host-binding macro), `~/src/idiosepius` (the study app this may end up
embedded in, and the source of the visual language).
