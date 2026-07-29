# apteronotus-lua

This crate is the language boundary. It evaluates one edit in a fresh Piccolo
VM and returns an owned, data-only `Program`. No Lua closure or garbage
collected value can survive `Evaluator::evaluate`.

Behavioral decisions and provisional first-slice policy are recorded in
[`DESIGN.md`](DESIGN.md). In particular, it documents graph operator arity,
note-input names, seed identity, clocks, delay bounds, patch inputs, publication
validation, and the remaining source-transform restriction.

The current slice supports:

- Piccolo's safe core language and standard library, minus nondeterministic or
  host-facing entries such as `math.random`, `print`, and `load`;
- fuel, Lua-memory, source-size, graph-node, pattern-node, cycle-hold
  look-back, voice, and track budgets;
- `pattern("mini notation")`;
- construction-time structural transforms `fast`, `slow`, `shift`/`late`,
  `early`, `rev`, `every`, `off`, `sometimes`, `degrade`, `segment`, and
  `range`, deterministic `ply(count_or_choice)`, mini-notation `|`
  random-choice alternation, plus group-aware
  `arp("up"|"down"|"outside-in"|"inside-out", optional_spacing)`;
  transform values build ordinary owned AST nodes and never retain a Lua
  callback;
- tagged `secs`/`ms` absolute durations, `bars(x)` cycle durations, and
  tempo-declared `beats(x)`. Beats project exactly to cycle time; graph-time
  seconds are available only under a constant tempo, where the conversion is
  unambiguous;
- `tempo(bpm)` and step/ramped `tempo { ... }` maps, plus finite
  `timeline { at(time, pattern), ... }` placement;
- typed program tonal context through `key(tonic, mode)` for major, minor, and
  dorian. It is stored and validated but degree notation does not consume it
  yet;
- construction-time literal `chord(symbol) >> anchor(note) >>
  voicing(shape)`, grouped member provenance, `root_notes()`, and
  `octave(integer)`. Patterned symbols/anchors and automatic voice leading are
  a later runtime music boundary;
- named event controls through `pattern >> velocity(...)`,
  `pattern >> pan(...)`, and voice-scoped declared setters such as
  `pattern >> pad.pressure(...)`;
- transport-rate `sine`, `cosine`, `saw`, `perlin`, `step`, `line`, and
  `window`, numeric `+ - * /`, and `scale`; setters sample these expressions
  at each left event onset;
- note-clock event curves through
  low-level `curve { clock = "note_phase", ... }` terms or breakpoint
  `curve { { phase(0), value }, ... }` sugar, carried whole into each voice
  rather than sampled at onset; phase curves require an explicit event
  `hold(...)`;
- exact cycle/beat holds in the pure pattern AST and second-based holds
  resolved per event through the score `TempoMap` during finite timeline
  placement;
- `voice { params = ..., graph = function(n) ... end }`, with symbolic
  `n.hz`, `n.velocity`, `n.duration`, `n.pan`, and declared parameters;
- typed evaluation-only graph values and a deliberately small primitive set;
- symbolic exponentiation and fixed-range `clamp`, including per-voice
  parameter-bound `decay(n.seconds)`;
- readable graph helpers `zero()` and `soft_saw(hz)`; the latter stages an
  ordinary saw/fundamental blend and is not a new backend primitive;
- persistent `patch` graphs with explicit audio inputs, arena-scoped writable
  controls, `run`, buses, and graph-local `to(bus, level)` sends;
- program-scope `send { graph, level }` returns and `master(processor)`,
  finalized after evaluation into one full-layout persistent processor;
- stereo reverb and limiter nodes with publication-visible state/tail costs;
- score-level `pattern >> to(bus, scalar_level)` sends, stored per track and
  bound once per onset as copies of the completed voice output;
- note-clock curve bases, bounded interpolating delays, and deterministic
  `init_random`/`init_rand(n.id, ...)`;
- `play(voice, pattern)`, with mini-notation provenance derived from the
  original Lua call site's byte offset; fixed-frequency voices whose graph
  does not read `n.hz` accept trigger labels such as `bd` and `x`, while the
  same labels remain pitch diagnostics for a graph that does read `n.hz`.

The crate vendors an exact unreleased Piccolo revision. The 0.3.3 crates.io
release does not dispatch arithmetic or bitwise metamethods and can panic when
a table grows after deleting string keys; both are hard blockers for this
sandbox and are fixed in the snapshot. The single local wasm32 patch is
documented in `vendor/piccolo/APTERONOTUS.md`.

Symbolic graph signals implement `+`, `-`, `*`, `/`, `^`, and unary `-`. Typed
processors add `>>` serial connection, `|` port stacking, `&` shared-input bus
mixing, `~` independent-output branching, and compatible processor
sum/product/scaling. `mix(table)` handles dynamically built processor banks.
Processors and port bundles exist only while the edit is evaluated; applying a
processor emits ordinary `GraphBuilder` nodes, so no Lua value reaches a
`GraphTemplate`.

The source attribution pass injects byte-offset identities into direct
`pattern(...)`, `play(...)`, `degrade(...)`, `sometimes(...)`, `ply(...)`, and transport
signal constructor calls. Mini-notation callbacks use `mini::parse_at`;
randomized transforms derive their seed from the same stable
within-evaluation site identity, while signal arithmetic inherits an operand
span. Graph-node byte spans and a complete Lua diagnostic source map remain
pending.
Construction-time graph limits in this crate remain intentionally separate
from `GraphTemplate::validate_limits`, which is the caller's publication
budget.

The evaluation suite covers language semantics, sandbox limits, transactional
graph construction, typed operators, structural transforms, persistent program
data, and publication budgets. Cross-crate acceptance coverage is described
below.

## Current compatibility boundary

The binding can run finite or cyclic polyphonic, transformed multi-track scores
with scalar or note-curve event controls and mapped tempo, plus persistent
patches, controls, buses, graph sends, track-output ducking, shared analysers,
and explicitly compiled transport-clock controls. All seven shipped songs
evaluate and render through routed voices, persistent returns, master
processing and measured stereo output.

Optional `audio_input` and `control_input` declarations are now owned program
resources and may be captured by multiple graphs. The native player attaches
the first logical audio input to the default input device at an exactly
matching sample rate; missing or remaining lanes use the declared `silence`
fallback with a warning. Browser attachment remains pending. Controls use
their shared declared defaults. Program-scope `envelope_follower`,
`pitch_tracker` and `onset_detector` nodes stage once and fan out through
retained controls.
`control_signal(pattern, period)` compiles one explicitly bounded numeric
period into allocation-free persistent data; it never retains a Lua callback
or queries a pattern on the audio thread.

An onset-detector result remains a live trigger rather than pretending to be a
queryable pattern. The app observes its rising edge, schedules at a small
host-selected latency, samples voice-scoped `at_onset(...)` bindings once into
the new voice, and samples ordinary event-control patterns at the observed
transport cycle. Degradation uses the captured arrival ordinal, so it does not
pretend a live edge has a future query coordinate. This includes literal-Hertz
pitch without a MIDI/tuning round trip. The complete `jamming.eod` acceptance
also feeds deterministic host audio into the same production analyser,
external-trigger, init-binding and routed scheduling path.

A representative runnable score is:

```lua
tempo {
  { at = bars(0), bpm = 104 },
  { at = bars(8), bpm = 112, over = bars(2) },
}

local bass = voice {
  graph = function(n)
    return sine(n.hz) * n.velocity * 0.08 >> pan(-0.25)
  end,
}
local lead = voice {
  graph = function(n)
    return saw(n.hz) * n.velocity * 0.035 >> pan(0.25)
  end,
}

play(bass, timeline {
  at(bars(0), pattern("c3 e3") >> every(2, rev) >> velocity(0.8)),
  at(bars(4), pattern("g2 a2")),
})
play(lead, pattern("g4 ~") >> fast(2) >> off(0.25, rev)
                              >> degrade(0.1) >> velocity(0.55))
```

The next high-leverage gaps are:

- degree/scale notation consuming `key`, patterned music-theory inputs, and
  broader voicing dictionaries;
- browser audio-input attachment, native input selection/routing, and physical
  control-input binding;
- the remaining DSP/stdlib vocabulary and arbitrary feedback graph causality;
- timed input-bearing whole-stem processors and persistent patch replacement.

Select/join alignment and broader music-theory runtime boundaries remain
decision-blocked and are not guessed here.

The cross-crate acceptance test in `crates/live/tests/lua_program.rs` evaluates
one tiny `voice`/`play` script, publishes the owned program, resolves its
`VoiceId`, schedules `"c4 e4 g4"` through fundsp, and measures the rendered
audio. Further acceptances carry a `NotePhase` pressure curve through the same
path and compare early/late RMS to prove it remains live during the held note,
schedule a transformed two-track score and measure its stereo output, and mix a
routed voice with an autonomous persistent patch while changing a shared
control between scheduling windows. A finite-timeline acceptance also carries
a step tempo map through scheduling and measures notes on both sides of the
silent gap. Shipped-song acceptances exercise every document in `songs/`,
including continuous track ducking, sends/returns, bounded feedback delay,
long envelopes, deterministic init randomness, pole-based percussion, finite
through-composed placement, shared persistent analysers, compiled
transport-clock control, external-onset ownership, the jamming song's
declared silent-input fallback, and a separate host-fed jamming path that
proves tracked pitch, trigger-side controls and routed bell audio.
The same paths reach the native GUI and output device with:

```sh
cargo run -p apteronotus-app
```

The portability and quality gates are:

```sh
cargo test --workspace
cargo check -p apteronotus-lua --target wasm32-unknown-unknown
cargo clippy -p apteronotus-lua --all-targets --no-deps -- -D warnings
```
