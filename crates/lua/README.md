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
`pattern(...)` and `play(...)` calls and those callbacks use `mini::parse_at`.
Graph-node byte spans and a complete Lua diagnostic source map remain pending.
Construction-time graph limits in this crate remain intentionally separate
from `GraphTemplate::validate_limits`, which is the caller's publication
budget.

The crate has 23 tests: three source-transformation unit tests and twenty
evaluation/integration tests covering language semantics, sandbox limits,
transactional graph construction, typed operators, persistent program data and
publication budgets. The additional live acceptance test is described below.

Tempo maps and finite timeline constructors are the next settled integration
slice. They are intentionally not represented by Lua-only lookalike types:
`TempoMap` first needs its device-free part made available without pulling
`cpal` through the language crate. Structured event controls, curve-valued
event parameters, select/join alignment, and music-theory runtime boundaries
remain decision-blocked and are not guessed here.

The cross-crate acceptance test in `crates/live/tests/lua_program.rs` evaluates
one tiny `voice`/`play` script, publishes the owned program, resolves its
`VoiceId`, schedules `"c4 e4 g4"` through fundsp, and measures the rendered
audio. The same path reaches a native output device with:

```sh
cargo run -p apteronotus-live --example lua_first_sound
```

The portability and quality gates are:

```sh
cargo test --workspace
cargo check -p apteronotus-lua --target wasm32-unknown-unknown
cargo clippy -p apteronotus-lua --all-targets --no-deps -- -D warnings
```
