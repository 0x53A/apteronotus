# Apteronotus

A code-driven synthesizer: patterns in text, real synthesis underneath, live
editing. Inspired by Tidal and Strudel, derived from neither.

Named for the black ghost knifefish, whose electric organ discharge is among
the most stable biological oscillators known — sub-microsecond jitter at
roughly a kilohertz.

## Layout

- `crates/pattern` — the pattern algebra and the mini-notation. **No audio, no
  I/O, no dependencies.** The only way in is `mini::parse`, the only way out is
  `Pattern::query`. Everything here is testable without a sound card, which is
  why it is a crate of its own and why it must stay that way.
- `crates/synth` — *(not yet written)* fundsp instruments, `InstrumentSpec`,
  and the bridge onto `fundsp::Sequencer`.
- `crates/live` — *(not yet written)* editor, transport clock, cpal output,
  MIDI in.

`pattern` never learns that fundsp exists; `synth` never learns there is a
language. That seam is the point: it is what keeps the engine liftable behind
an ABI later without disturbing anything above it.

## The rate hierarchy

This is the constraint everything else falls out of. Four rates, and a
different answer at each:

| rate | frequency | what lives there | who may write it |
|---|---|---|---|
| pattern | ~1–10 Hz | which note, when | the scripting language, freely |
| voice build | once per note | graph assembly | the scripting language, freely |
| control | 500 Hz (fundsp `envelope`/`lfo` sample at 2 ms and interpolate) | envelopes, sweeps, LFOs | compiled expressions only |
| audio | 48 kHz, blocks of 64 | oscillators, filters, shapers | compiled expressions or built-in nodes |

**No host-language closure may survive into the graph.** Once
`Sequencer::push` hands a voice to the backend it is rendered *on the audio
thread*; an interpreter callback there can allocate, which can collect, which
is a dropout. Not slow — audibly broken, nondeterministically, under load.
Whatever a user writes as an envelope has to be lowered to something
realtime-safe before it is pushed.

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

**The pattern is an AST, not a tree of closures.** Tidal and Strudel both use
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

- **The scripting language.** Sketched as Lua-shaped — Lua 5.3+ has `__shr`,
  `__bor`, `__band`, so it can host fundsp's operator algebra almost verbatim
  (only `^` → `~` for branch, since `^` is exponentiation). But `mlua` on
  `wasm32-unknown-unknown` is a real problem, and compiling closure bodies for
  the control rate needs the AST, which a bytecode interpreter will not hand
  over. The realistic route is a small language that *looks* like Lua.
- **The component-model boundary.** `wasm_component_layer` + `wasmi` runs the
  component model inside a wasm app without the browser engine — proven in
  `~/src/wasm-in-wasm`. If it happens, the component is the *language runtime*,
  not the song: compiling per edit kills the live loop, since there is no rustc
  in a browser. Boundary at events, never samples.
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
