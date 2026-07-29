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
- fuel, Lua-memory, source-size, graph-node, pattern-node, voice, and track
  budgets;
- `pattern("mini notation")`;
- construction-time structural transforms `fast`, `slow`, `shift`/`late`,
  `early`, `rev`, `every`, `off`, `sometimes`, `degrade`, `segment`, and
  `range`; transform values build ordinary owned AST nodes and never retain a
  Lua callback;
- tagged `secs`/`ms` absolute durations and `bars(x)` cycle durations, checked
  at graph/note-clock versus pattern-time boundaries; a tempo-aware `beats(x)`
  is intentionally not faked as fixed seconds;
- named event controls through `pattern >> velocity(...)`,
  `pattern >> pan(...)`, and voice-scoped declared setters such as
  `pattern >> pad.pressure(...)`;
- note-clock event curves through
  `curve { clock = "note_phase", ... }`, carried whole into each voice rather
  than sampled at onset;
- `voice { params = ..., graph = function(n) ... end }`, with symbolic
  `n.hz`, `n.velocity`, `n.duration`, `n.pan`, and declared parameters;
- typed evaluation-only graph values and a deliberately small primitive set;
- persistent `patch` graphs with explicit audio inputs, arena-scoped writable
  controls, `run`, buses, and graph-local `to(bus, level)` sends;
- note-clock curve bases, bounded interpolating delays, and deterministic
  `init_random`/`init_rand(n.id, ...)`;
- `play(voice, pattern)`, with mini-notation provenance derived from the
  original Lua call site's byte offset.

The crate vendors an exact unreleased Piccolo revision. The 0.3.3 crates.io
release does not dispatch arithmetic or bitwise metamethods and can panic when
a table grows after deleting string keys; both are hard blockers for this
sandbox and are fixed in the snapshot. The single local wasm32 patch is
documented in `vendor/piccolo/APTERONOTUS.md`.

Symbolic graph signals implement `+`, `-`, `*`, `/`, and unary `-`. Typed
processors add `>>` serial connection, `|` port stacking, `&` shared-input bus
mixing, `~` independent-output branching, and compatible processor
sum/product/scaling. `mix(table)` handles dynamically built processor banks.
Processors and port bundles exist only while the edit is evaluated; applying a
processor emits ordinary `GraphBuilder` nodes, so no Lua value reaches a
`GraphTemplate`.

The source attribution pass injects byte-offset identities into direct
`pattern(...)`, `play(...)`, `degrade(...)`, and `sometimes(...)` calls.
Mini-notation callbacks use `mini::parse_at`; randomized transforms derive
their seed from the same stable within-evaluation site identity. Graph-node
byte spans and a complete Lua diagnostic source map remain pending.
Construction-time graph limits in this crate remain intentionally separate
from `GraphTemplate::validate_limits`, which is the caller's publication
budget.

The evaluation suite covers language semantics, sandbox limits, transactional
graph construction, typed operators, structural transforms, persistent program
data, and publication budgets. Cross-crate acceptance coverage is described
below.

## Current compatibility boundary

The binding can run polyphonic, transformed multi-track scores with scalar or
note-curve event controls, plus persistent patches, controls, buses and graph
sends. It does not yet run any complete specification song.

A representative runnable score is:

```lua
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

play(bass, pattern("c3 e3") >> every(2, rev) >> velocity(0.8))
play(lead, pattern("g4 ~") >> fast(2) >> off(0.25, rev)
                              >> degrade(0.1) >> velocity(0.55))
```

The next high-leverage gaps are:

- constant and mapped tempo plus finite timeline constructors;
- transport-clock signal arithmetic and sampling through named setters;
- music-theory/key and grouped-event arpeggiation;
- score-level sends and pattern-to-continuous-control operations such as
  `duck`;
- `ply` event repetition and mini-notation `|` random-choice alternation,
  which are separate operations;
- symbolic arguments to graph curves and the remaining DSP/stdlib vocabulary;
- typed non-pitched trigger events for synthesized drums.

Tempo and timeline bindings must reuse the existing Rust framework types rather
than introduce Lua-only lookalikes. `TempoMap` first needs a device-free
dependency boundary so the browser language build does not pull in `cpal`.
Select/join alignment and broader music-theory runtime boundaries remain
decision-blocked and are not guessed here.

The cross-crate acceptance test in `crates/live/tests/lua_program.rs` evaluates
one tiny `voice`/`play` script, publishes the owned program, resolves its
`VoiceId`, schedules `"c4 e4 g4"` through fundsp, and measures the rendered
audio. Further acceptances carry a `NotePhase` pressure curve through the same
path and compare early/late RMS to prove it remains live during the held note,
schedule a transformed two-track score and measure its stereo output, and mix a
routed voice with an autonomous persistent patch while changing a shared
control between scheduling windows. The same paths reach the native GUI and
output device with:

```sh
cargo run -p apteronotus-app
```

The portability and quality gates are:

```sh
cargo test --workspace
cargo check -p apteronotus-lua --target wasm32-unknown-unknown
cargo clippy -p apteronotus-lua --all-targets --no-deps -- -D warnings
```
