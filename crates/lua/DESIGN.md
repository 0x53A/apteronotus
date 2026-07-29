# Lua boundary decisions

This note records behavior that is important to authored programs but is not
obvious from the Rust types. It distinguishes settled semantics from first-slice
policy and from work that is deliberately blocked. A provisional choice is not
an invitation for the runtime to infer more behavior from it.

## Settled semantics

### Evaluation and ownership

Each explicit evaluation gets a fresh Piccolo VM and a fresh set of arena-scoped
Rust identities. Success returns an owned `Program`; failure drops the whole
candidate. No Lua closure, table, userdata, callback, or garbage-collected value
can enter a graph, scheduler, or audio callback.

Lua fuel, Lua memory, source bytes, topology construction, and publication cost
are separate budgets. The construction node counter covers the entire
evaluation, not one graph. Work performed by a graph that later fails inside
`pcall` is not refunded. Refunding would let an authored loop evade the limit by
repeatedly constructing and catching failed graphs.

`GraphTemplate::validate_limits` remains a host-selected publication policy.
`Evaluator` applies it before returning a candidate, and `Program::validate`
applies it again at publication. The repetition is intentional: publication
must also be safe for programs that may eventually come from a cache or a
different frontend.

### Note inputs and deterministic initialization

The implicit note table exposes exactly:

- `n.hz`;
- `n.velocity`;
- `n.duration`, in seconds;
- `n.pan`;
- `n.id`, which is an opaque seed token.

There are no `n.vel` or `n.gate` aliases. A sequenced duration is a scalar known
when a voice is instantiated. A live gate would be a time-varying signal with a
different lifetime and failure model, so the two must not share a name.

`n.id` is deliberately not a graph signal and cannot participate in arithmetic.
It is accepted only by `init_rand(n.id, stream, min, max)`. The shorter
`init_random(stream, min, max)` is equivalent. The graph stores only a `u64`
stream identifier; per-onset variation comes from the event-derived seed during
Rust instantiation. String stream names use the spelled-out FNV-1a mapping in
the binding so a dependency update cannot reshuffle a song.

### Patch algebra types

Signal, port bundle, and processor are three different evaluation-only types:

- a signal owns one or more output channels;
- `a | b` builds an ordered input-port bundle;
- a processor describes topology awaiting inputs.

A stereo signal is therefore not the same thing as a two-item bundle. This
distinction is what makes `(audio | cutoff | q) >> lowpass()` type-check without
pretending cutoff and Q are audio channels.

The operators mean:

| operator | meaning |
|---|---|
| `>>` | connect compatible outputs to inputs |
| `|` | concatenate ports or stack independent processors |
| `&` | give the same input to compatible processors and mix their outputs |
| `~` | give the same input to independent processors and concatenate outputs |
| `+`, `-`, `*` | mix, subtract, multiply, or scale compatible processors |

Processor arity is checked before publication and, where both sides are known,
at composition time. Applying a processor emits ordinary `GraphBuilder` nodes
immediately. The processor object itself never enters `GraphTemplate`.

`lowpass()`, `highpass()`, `bandpass()`, and `moog()` are three-input
processors: audio, cutoff, Q. Supplying cutoff, or cutoff and Q, makes a
one-input processor awaiting audio. The one-argument form uses Q = 0.707, the
neutral Butterworth-style default. The old direct three-argument functional
form remains available.

`mix(table)` is the dynamic-bank operation. A table of signals is summed now; a
table of processors becomes a shared-input processor bank. This avoids a fake
zero-input accumulator with the wrong arity.

### Pattern transforms are construction-time data

`fast`, `slow`, `shift`/`late`, `early`, `rev`, `degrade`, `segment`, and
`range` return evaluation-only transform objects. `every`, `off`, and
`sometimes` compose those objects. Applying one with `pattern >> transform`
immediately constructs ordinary `Pattern` variants; it never stores or invokes
a Lua function during query.

`rev` is a transform value rather than a function, which is why
`every(4, rev)` is the canonical spelling. `every` chooses its prebuilt branch
by cycle. `off` overlays a shifted transformed copy. `sometimes` partitions
events into transformed and untouched branches rather than duplicating them.

Direct `degrade` and `sometimes` calls derive independent deterministic seeds
from their source byte offsets. A probability must be finite and in `[0, 1]`.
This site identity is stable only within one evaluation; cross-edit continuity
does not depend on it.

Pattern time remains exact rational cycle time. Lua numbers are approximated
with a bounded rational denominator at the binding boundary. `bars(x)` currently
means `x` cycles because the host's default meter is one bar per cycle. It
returns a tagged cycle duration: pattern-time consumers accept it, while graph
and note-clock consumers diagnose it instead of silently treating bars as
seconds.
`beats(x)` is deliberately unavailable until it can be resolved through the
program's tempo/meter rather than masquerading as fixed seconds.

### Clocks, curves, and delay allocation

`ms` and `secs` return tagged absolute durations. Graph, delay and note-clock
curve consumers unwrap them as seconds; pattern timing rejects them. Conversely,
`bars` is accepted by pattern timing and rejected by graph consumers. Bare
finite numbers remain context-dependent for compatibility with scalar graph
code, so explicit units are the spelling that receives cross-domain checking.
`fast` and `slow` accept only dimensionless numbers.

`step`, `ramp`/`line`, `decay`, and `window` currently create note-clock
`Curve` nodes and are therefore valid in a voice. `PatchTemplate` rejects them
because persistent transport-clock automation has not been defined. The binding
does not silently choose another clock.

Score curves use an explicit constructor:

```lua
local rise = curve {
  clock = "note_phase",
  { basis = "ramp", coefficient = 1, delay = 0, length = 1 },
}
play(pad, pattern("c4") >> pad.pressure(rise))
```

`NotePhase` evaluates only on `[0, 1]`; a term beginning after 1 is an
unreachable-expression diagnostic, while one beginning exactly at 1 is
reachable. `NoteSeconds` event curves that terminate after the gate require the
target `ParamSpec` to declare `max_curve_seconds`.

Pattern `Merge` is an explicit transport-signal-to-init boundary: it queries
its map-producing RHS at each left onset. The zero-width convention is
left-closed, so an onset exactly on a control boundary selects the slot
beginning there. The first simultaneous RHS event is chosen in stable
structural query order; that order is deterministic but is not musical chord
order.

Curve ranges are never silently clamped. Step/ramp sums have exact
piecewise-linear bounds; decay/sine terms receive sound enclosures. A range
that is unsafe or cannot be proven safe rejects the candidate while the
previous program keeps playing.

A numeric delay without separate bounds gets a fixed allocation range. A
symbolic delay time must provide explicit minimum/maximum allocation bounds.
The maximum contributes to both graph publication cost and conservative tail
metadata; the runtime never samples a signal to decide how much memory to
allocate.

### Persistent graphs, controls, and routing

`patch` stages a `PatchTemplate`, so per-note inputs, note-clock curves, ADSRs,
and per-voice initializers are rejected by the synth layer. Patch audio inputs
are explicit:

- `cv.input` is the complete possibly-multichannel signal;
- `cv.inputs[i]` is one channel, with Lua's one-based indexing.

If a patch graph returns a processor, it is applied to all declared input
channels before the template is finished. Returning a signal remains valid.

Explicit program controls and buses must be declared outside a graph. Their
opaque `ControlId` and `BusId` values belong to one evaluated program arena and
cannot alias handles from another evaluation. `to(bus, level)` is a pass-through
processor: it taps the current channels into a graph send and returns those same
channels for continued processing. Its channel count must match the bus.
Score/event sends remain a separate operation. The control-map representation
now exists, but the Lua score constructor and live execution contract for
per-event send levels are not connected yet.

`run(patch)` records persistent activation intent as a `PatchId`; it does not
instantiate DSP during evaluation. In the first live executor, zero-input runs
are autonomous sources mixed with routed voices. An input-bearing run must
consume the complete flattened main/bus layout and processes that layout in
declaration order. Exact arity is required; lanes are never folded modulo.
The processor remains instantiated across scheduler windows. On a later Run,
the native app compares the candidate's controls, buses and activated `run`
patch graphs with the live persistent data while normalising arena-scoped
handles. Inert patch declarations may change. A compatible candidate is rebound
to the live arena, retains its DSP state and `ControlStore`, and publishes
changed tracks/voices at the ordinary scheduling frontier.
Introducing, removing or changing persistent topology still builds a complete
replacement off to the side and hard-resets at cycle zero. A bounded crossfade
remains the intended replacement semantics.

### Source identity and publication

Direct `pattern(...)` and string-valued `play(...)` calls are transformed to
carry their original Lua byte offset into `mini::parse_at`. Direct `degrade`
and `sometimes` calls carry the same identity into deterministic pattern
randomness. Different sites therefore have distinct provenance/seeds, while
repeated execution of one site inside a loop keeps one identity. These IDs are
meaningful only within one evaluation. Cross-edit continuity belongs to
revision reconciliation.

The complete candidate is validated before `Evaluator` returns and again when
submitted through `RevisionSlot`. The end-to-end acceptance test deliberately
uses both boundaries, then resolves `Track::voice`, schedules the captured
pattern, lowers the graph, and measures the resulting fundsp sequencer output.
The `live` crate depends on `lua` only as a development dependency for this test
and the native example; the production synthesis/runtime crates do not learn
about the language.

## Provisional first-slice policy

These choices make the current path concrete but are not settled architecture:

- `Program::default` has a two-channel main `BusLayout`. It exists primarily as
  the silent initial value for `RevisionSlot`; a future host/program declaration
  should choose the main layout explicitly.
- Patch parameters lower to program controls named
  `patch<declaration-index>.<parameter>`. Qualification prevents two patches'
  common names such as `gain` from colliding. Declaration indices and arena IDs
  are evaluation-local and must not be used for cross-edit reconciliation. A
  later lexical declaration identity may replace the display name.
- `run(patch)` currently means the whole active program lifetime. The optional
  span used by `neon.eod` waits for finite-time integration.
- The direct-call source transformer treats `pattern`, `play`, `degrade`, and
  `sometimes` as reserved when used in call position. Strings, comments,
  method calls, and function declarations are skipped, but an authored local
  that shadows one of those names must not yet be called directly. An
  AST/scope-aware transform should remove this restriction.
- Default publication ceilings in `Limits::default` are conservative host
  policy for tests and the first application, not universal IR limits.

## Deliberately deferred

The binding does not invent representations for:

- ordered/list event values or nested control maps;
- select/join temporal alignment;
- music-theory nodes that must survive to query time;
- persistent transport-clock automation;
- patch replacement/crossfade or state migration;
- external audio/control resource identity and shared derived analysers;
- score-level event sends, ducking, or pattern-to-control conversion;
- feedback graph causality;
- tempo maps in `Program` until the device-free `TempoMap` can be reused without
  pulling `cpal` into the browser language build;
- complete graph-expression byte spans and a diagnostic source map.

These are omitted because their contracts are open or their existing Rust type
is behind the wrong dependency edge, not because the Lua layer should grow a
parallel representation.
