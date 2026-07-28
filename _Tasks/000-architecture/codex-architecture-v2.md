# Apteronotus architecture proposal, post-corpus

This is the second independent Codex architecture proposal. It incorporates:

- the four specification songs;
- the 723-snippet Strudel corpus analysis;
- the decision that the complete Apteronotus authoring language must run in
  the browser;
- the clarification that a normal language sandbox need not expose the whole
  operating-system-facing Lua standard library;
- the decision that independently playable serialized songs are optional.

Where this document conflicts with `codex-architecture.md`, this document is
the current recommendation. The first proposal remains useful as a reasoning
record, especially for its account of graph staging and live replacement.

## Product definition

Apteronotus is a live-coded pattern and synthesis environment:

```text
sequence musical events
construct the instruments that render them
edit either while sound continues
run the same authoring environment on desktop and web
```

It is not primarily a sample sequencer, a wrapper around an external synth, or
a graph file player. It combines a Tidal-shaped temporal pattern model with a
first-class modular synthesis graph.

The same source must produce the same musical result. A syntax error, runtime
error, exhausted script budget, invalid graph, or failed update must leave the
last valid revision playing.

## Conclusions that are now strong

### The full authoring environment runs on the web

Desktop and browser expose the same language, standard library, pattern
operations, synthesis builders, and update behavior.

“Full language” means the complete Apteronotus Lua sandbox. It does not mean
all facilities distributed with PUC Lua. File access, process access, native
module loading, locale behavior, and a debug library are not expected inside a
browser music sandbox.

### Lua is evaluated once per edit

An explicit evaluation runs the top-level Lua program and all construction
helpers needed to build a new revision. A successful evaluation publishes
data-only pattern and synthesis structures:

```text
Lua source
    │ evaluate
    ▼
LiveProgram revision
    ├─ pattern trees
    ├─ instrument graph templates
    ├─ persistent patch graphs
    ├─ track and bus routing
    ├─ tempo and tonal context
    └─ source origins
```

Lua does not run during ordinary pattern queries, voice instantiation, control
processing, or audio rendering.

This restriction is now evidence-based rather than merely conservative. In
the analyzed Strudel corpus, nearly all apparent query-time closures were:

- construction helpers;
- workarounds for missing structured values;
- music-theory parsing;
- simple arithmetic;
- pattern-of-pattern joins.

Only one real song depended musically on retained mutable query state, and it
violated pure/idempotent pattern querying. Live MIDI and keyboard input are
legitimate but should enter through explicit runtime controls rather than host
closures.

An explicitly dynamic script pattern remains a possible future door, not a v1
foundation.

### Graph functions are staged once per edit

`graph = function(n)` runs with symbolic note and parameter inputs. It produces
a `GraphTemplate`; it is not retained as a per-note Lua factory.

All four specification songs support this model:

- loops iterate over literal ranges or tables and unroll during evaluation;
- every `n.*` use appears in arithmetic or as a graph port value;
- no song branches its topology using a concrete note value.

Per-note arithmetic remains necessary:

```lua
ring(n.hz * ratio, n.ring / index)
```

The template therefore contains a small `Init`-rate expression evaluated by
Rust when an event instantiates a voice. It does not require Lua at onset.

### Pattern trees remain data, not closures

The Strudel corpus supports rather than weakens the AST decision. It revealed
missing domain operations, not a general need for arbitrary callbacks.

Pattern data provides:

- exact and idempotent querying;
- deterministic randomness;
- source highlighting through transformations;
- resource and density validation;
- predictable behavior under lookahead and overlapping editor queries;
- the option of transient or future serialization.

### Stable serialized programs are optional

The runtime should use ordinary owned data rather than Lua or backend objects,
but it does not need a stable, versioned public interchange format in v1.

Internal serialization may still be useful for:

- transferring one successful program between two web modules if Emscripten is
  required;
- caches;
- debugging and snapshot tests;
- sharing revisions;
- offline rendering.

The schema may initially be private and tied to one application build.

## System topology

### Native

```text
editor/UI thread
    │ source revisions
    ▼
language/control thread
    ├─ Lua evaluation
    ├─ validation
    ├─ pattern scheduler
    └─ graph preparation
           │ bounded prepared commands
           ▼
audio thread
    └─ DSP rendering
```

### Browser

Preferred:

```text
editor/UI
    │
    ▼
Rust WASM control runtime
    ├─ pure-Rust Lua VM
    ├─ pattern scheduler
    └─ graph preparation
           │
           ▼
AudioWorklet
    └─ Rust/WASM DSP
```

The language/control runtime should live in a worker if browser integration
allows it cleanly. This isolates an expensive evaluation from UI rendering.
Lookahead isolates the audio renderer from both.

The application may contain more than one Wasm module. The critical property
is not “one `.wasm` file”; it is that the Lua VM never participates in audio
rendering and that program publication is transactional.

## The Apteronotus Lua sandbox

The language should be described by a positive capability contract rather than
“Lua minus whatever happens not to work.”

### Required core behavior

- lexical scoping;
- functions and closures;
- proper upvalues;
- tables and table constructors;
- numeric and generic `for`;
- `pairs`, `ipairs`, and `next`;
- multiple returns and varargs;
- metatables and required metamethods;
- arithmetic, comparison, concatenation, length, and bitwise operators;
- errors, `assert`, and `pcall`;
- ordinary string manipulation;
- deterministic numeric behavior suitable for construction.

### Required safe libraries

At least:

```text
base        selected safe functions
math        deterministic subset
string      practical parsing and formatting
table       insert/remove/sort/concat/unpack and related basics
utf8        useful text operations
coroutine   optional for v1, acceptable if the VM already supports it
```

Apteronotus supplies its own modules:

```text
pattern
music
signal
graph
instrument
transport metadata
```

### Deliberately absent or replaced

```text
io                  absent
os                  absent
loadfile/dofile     absent
package.loadlib     absent
debug               absent
filesystem require  absent
native C modules    absent
ambient clock       absent
ambient entropy     absent
```

`require` may load only host-provided, deterministic standard-library modules.

`math.random` should either be absent or explicitly construction-seeded. Music
randomness belongs in the position- and seed-derived pattern/signal operations,
where re-querying is deterministic.

`load()` is not needed by the corpus and complicates source attribution and
budgeting. Omit it initially.

Native and web builds must expose the same sandbox. Desktop must not quietly
gain filesystem or native-package capabilities merely because its VM could
provide them.

## Lua VM choice

### First candidate: Piccolo

Piccolo is the first integration spike because it is pure Rust and therefore
fits `wasm32-unknown-unknown` directly. Its stackless execution, fuel, and
memory accounting align with the requirement that bad source return control
without stopping sound.

Its incomplete OS-facing and debug libraries are not blockers. The spike must
measure the required language contract, particularly:

- all operator metamethods needed by graph values;
- table and string operations used by songs and the standard library;
- Rust userdata and callbacks;
- fuel exhaustion;
- memory reporting and limits;
- errors crossing the Rust boundary;
- native and wasm behavior;
- repeated evaluation and cleanup;
- compilation and runtime source positions.

Piccolo is pre-1.0 and should be isolated behind an Apteronotus-owned adapter.
No lower crate may depend on its public types.

If the missing behavior is small, implement it in the adapter, contribute it
upstream, or maintain a focused fork. The target is the documented
Apteronotus sandbox, not perfect PUC compatibility.

### Fallback: PUC Lua through Emscripten

`mlua` supports Lua under `wasm32-unknown-emscripten`, while the main web
application is expected to use the wasm-bindgen-oriented
`wasm32-unknown-unknown` path. Those artifacts cannot simply link together.

The fallback is a separate compiler module:

```text
source
  │
  ▼
Emscripten module
  ├─ PUC Lua
  ├─ Lua bindings
  └─ pattern/graph builders
          │ one provisional Program transfer per edit
          ▼
main Rust/WASM runtime
```

This does not require one cross-module call per combinator. The Emscripten
module can construct the whole revision internally and transfer it only after
successful evaluation. Both modules ship from one build, so their internal
encoding can be version-locked rather than public.

### Last fallback: own the VM

If Piccolo is close but insufficient, extending or forking it is preferable to
starting over. A fresh Rust interpreter remains possible because the required
sandbox is bounded, but it includes difficult semantic machinery—closures,
upvalues, metatables, coroutines or deliberate omission thereof, errors, and
garbage collection—and should not be treated as a trivial parser project.

## Pattern runtime

### Existing contract

A pattern is a pure query:

```text
query(timespan) -> events
```

Musical time is exact rational time. Events distinguish their complete
occurrence (`whole`) from the queried intersection (`part`). Continuous signals
have no onset. Randomness is a pure function of exact position and an explicit
seed.

The scheduler owns a monotonically advancing frontier:

```text
query [frontier, frontier + lookahead)
schedule onsets with absolute audio timestamps
advance frontier
```

Nothing deduplicates returned events because identical simultaneous onsets are
valid.

### Structured values

Control maps are the immediate missing foundation:

```text
{
    note: C3,
    gain: 0.7,
    cutoff: 1200 Hz,
    pan: -0.2
}
```

However, named maps and ordered multi-values are distinct requirements. The
corpus contains tuple/array selection idioms. The value system should likely
support:

```rust
enum Value {
    Number(f64),
    String(String),
    Bool(bool),
    List(Vec<Value>),
    Map(ControlMap),
    // typed pitch/chord values may be separate or represented above
}
```

Before adding `List`, inspect the corpus cases to determine whether they are
genuine ordered musical values or workarounds for the absence of names.

### Merge semantics

Applying a parameter pattern to a note pattern:

- preserves the structure of the note/left pattern;
- samples the parameter/right pattern at each left onset;
- merges one value into the event's control map;
- instantiates exactly one voice per left onset.

It must not multiply notes merely because a parameter pattern is denser.
Ratcheting remains an explicit rhythmic operation.

### Pattern-of-pattern selection

The corpus makes join/alignment a core feature. A simple undifferentiated
`Pick` node is insufficient because selection modes have different temporal
semantics.

A likely shape is:

```rust
Pattern::Select {
    selector: Box<Pattern>,
    branches: Vec<Pattern>,
    mode: SelectMode,
}

enum SelectMode {
    Pick,
    PickOut,
    Squeeze,
    Reset,
    Restart,
}
```

Names and exact decomposition remain subject to tests. The implementation must
define:

- which side supplies event structure;
- whether a selected branch is compressed to the selector event;
- whether it is truncated;
- whether it restarts from its first cycle or aligns with current transport
  time;
- how `whole` and `part` are derived;
- source-origin propagation;
- density bounds.

The corpus analyzer should contribute semantic fixtures for each observed mode,
not only occurrence counts.

### Value expressions

Simple per-event closures in Strudel frequently perform arithmetic or field
selection. Apteronotus should represent these operations as a small typed
expression tree:

```rust
Expr::Field(name)
Expr::Index(index)
Expr::Add(a, b)
Expr::Mul(a, b)
Expr::Mod(a, b)
Expr::PitchToHz(x)
```

This expression language may serve both pattern value mapping and graph
`Init`-rate parameter binding, provided their type/rate requirements genuinely
align.

### External controls

Live input is explicit:

```rust
Pattern::External {
    control: ControlId,
    sampling: SamplingMode,
}
```

The control registry receives MIDI, keyboard, UI, or future sensor values.
Sampling behavior—at onset, sample-and-hold, continuous control, trigger—must
be named. External controls make a performance depend on input, but pattern
query behavior remains defined rather than depending on an arbitrary closure
over DOM or MIDI objects.

### Stateful generative algorithms

Order-dependent mutable pattern callbacks are deliberately excluded from v1.
A Markov generator can later be represented honestly through one of:

- a pure position-keyed construction with an explicit seed;
- a stateful transport process with declared reset/checkpoint semantics;
- a dynamic-language escape hatch clearly outside pure pattern queries.

It must not masquerade as a pure pattern while depending on query order.

## Music theory layer

The corpus shows that scales, modes, chords, roots, anchors, voicings, and
arpeggiation are central rather than optional polish.

They deserve a `music` crate or equivalent domain layer rather than ad hoc
string manipulation in the script bindings.

Responsibilities include:

- pitch spelling and accidentals;
- octave and pitch-class representation;
- scale and mode definitions;
- degree-to-pitch resolution;
- chord-symbol parsing;
- chord interval construction;
- inversions;
- roots and slash chords;
- voicing dictionaries;
- anchor/range selection;
- deterministic voice leading, if offered;
- pitch-to-frequency conversion;
- arpeggiation of grouped chord events.

### Tonal context

`key("f#", "minor")` is a real declaration. It should establish explicit
program or track tonal context consumed by degree notation and relevant music
operations.

It must not be inert decoration or hidden scheduler mutation. Open design
questions:

- one program-global default versus lexical/track scope;
- whether key changes may themselves be patterned;
- whether notes preserve spelling or collapse immediately to pitch numbers;
- how microtonal scales enter later.

### Runtime versus construction time

Literal chord and scale definitions can expand during evaluation. Patterned
roots, modes, degrees, voicing choices, and anchors require runtime data
operations.

Do not automatically add one `Pattern` variant for every music function.
Explore a typed transformation/expression layer that operates over pattern
values while preserving timing and origins.

`arp` is event-aware: it groups simultaneous notes sharing an occurrence and
orders/re-times them. It cannot be implemented solely as a value expression.

## Synthesis graph

### Backend-neutral source of truth

The first graph representation should be small and provisional, but it must be
owned data:

```rust
struct GraphTemplate {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    outputs: Vec<PortRef>,
}
```

It must not contain:

- Lua functions or values;
- fundsp trait objects;
- Web Audio objects;
- native pointers;
- arbitrary callbacks.

The initial backend lowers this representation directly to fundsp. There is no
need to design a permanent interchange standard before making sound.

### Rates

At least four runtime placements matter:

```text
Init      evaluated once when binding an event to a voice template
Event     changed at explicit note/control events
Control   evaluated at block/control rate
Audio     evaluated per sample
```

Authoring/evaluation sits above these and is not a DSP rate.

Parameters and expressions carry a permitted rate. Constants lift safely.
Lower-rate values may feed higher-rate inputs through explicit or defined
lifting. Higher-rate signals cannot be silently collapsed to an onset value
unless the operation explicitly samples them.

Signals and curves should share compositional arithmetic where their clocks
are explicit. The existing basis-sum proposal for curves remains attractive:

```text
Σ cᵢ · basisᵢ(t - Tᵢ)
```

### Graph algebra

The public semantics belong to Apteronotus even when the first lowering follows
fundsp:

```text
>>  serial connection
|   parallel port stacking
&   fan-out/bus one input into parallel processors
+   sum compatible signals
*   multiply/VCA compatible signals
~   branch into independent paths, if retained
```

Use `mix(collection)` when summing a dynamically constructed bank. Do not rely
on a fake zero-input `zero()` having the right arity.

The current uses of `&` in `poles.eod` that intend summation should become `+`
or `mix`.

### Primitive versus standard library

A primitive is Rust DSP when it requires:

- internal sample state;
- a per-sample feedback path;
- backend-specific optimized processing;
- anti-aliasing behavior;
- a bounded implementation not expressible by graph composition.

Composite instruments and effects remain readable sandbox library source and
expand to standard graph nodes during evaluation.

The primitive set is not expected to reproduce every SuperCollider UGen. A
SynthDef corpus should classify which behaviors compose, which require one new
primitive, and which depend on SuperCollider host facilities outside a graph.

## Two synthesis lifetimes

### Voice

A `voice` template creates fresh DSP state for every note onset. This is the
ordinary polyphonic model.

New events after a program update use the new template. Existing voices retain
their old prepared graph until release unless a hard update policy says
otherwise.

### Patch

A `patch` is one persistent graph whose named controls change over time:

```lua
patch {
    controls = { pitch, gate, velocity, cutoff },
    graph = function(cv)
        ...
    end
}
```

This is the honest model for:

- a 303-style monosynth and portamento;
- free-running oscillators;
- drones;
- Eurorack-like routing;
- persistent feedback;
- long-running modulation;
- buses and effects.

Do not block the first sounding polyphonic voice on `patch`, but keep lifetime
explicit in `InstrumentSpec` so the first implementation does not close the
door.

## Routing, sends, and sidechains

Tracks require buses. Buses feed named sends and the master graph.

These are different concepts:

- a fixed graph edge from an instrument to a send;
- an event-level send amount;
- a persistent bus effect;
- a sidechain control derived from another track or pattern.

Use distinct IR operations even if the surface syntax shares `to(...)` sugar.

`duck(pattern, amount)` requires conversion from pattern events to a control
signal with explicit sample-and-hold, attack, and release behavior. It is not a
per-note setter.

## Live updates

Evaluation is transactional:

```text
source
  │ evaluate under CPU/memory limits
  ▼
candidate LiveProgram
  │ validate patterns, expressions, graphs, and resources
  ▼
publish generation N at an effective musical time
```

Any failure reports diagnostics and discards the candidate. Generation `N-1`
continues.

Publication modes should eventually include:

- next unscheduled frontier;
- next beat;
- next cycle;
- explicit musical time.

The scheduler queries only the revision active for each frontier. Already
scheduled events carry their generation. Initially they may play, producing a
bounded lookahead latency. Later they may be cancelled or faded by generation
without deduplicating intentional simultaneous events.

Persistent graph updates are prepared outside the audio thread and committed
at a block boundary. Crossfade is the default safe replacement. Compatible
stable nodes may migrate state later, but v1 must not depend on state migration.

## Source origins and diagnostics

Lua stack inspection is not a sufficient source-location design:

- common Lua debug information has lines but not columns;
- tail calls and wrappers obscure callers;
- Piccolo does not intend to provide a conventional debug library or debugger;
- exact highlighting requires byte spans.

The frontend should parse or transform Lua source before execution and assign
call-site IDs:

```lua
lowpass(1200)
```

conceptually becomes:

```lua
__at(site_42, lowpass, 1200)
```

Bindings attach `site_42` to generated pattern, expression, and graph nodes.
Standard-library expansion preserves an origin chain:

```text
primary user callsite
    └─ helper expansion callsites
```

The source transformer must preserve ordinary Lua behavior and produce useful
syntax diagnostics. It may use a pure-Rust Lua parser even if Piccolo supplies
execution.

Locations remain capable of representing line-only diagnostics for VM errors,
but exact spans are the goal for library calls and sounding-event highlights.

## Validation and budgets

### Evaluation

- VM instruction/fuel limit;
- total VM memory limit;
- source size and parse-depth limit;
- bounded host callbacks;
- no ambient I/O;
- no previous revision mutation before successful publication.

### Patterns

- AST depth and node limits;
- count limits in notation;
- density estimates after construction;
- bounded branch selection;
- bounded value/list/map sizes;
- external control registration validation.

### Graphs

- node and edge limits;
- port and channel compatibility;
- rate compatibility;
- state-memory estimates;
- maximum delay/buffer allocation;
- maximum polyphony;
- explicit feedback delay;
- finite parameter ranges;
- bounded commands per audio block.

All expensive allocation, decoding, validation, and lowering happens before
the audio callback receives a graph.

## Crate boundaries

A likely progression:

```text
pattern/
    exact time, events, structured values, AST, query, mini-notation

music/
    pitches, scales, chords, voicing, tonal context, arpeggiation

graph/
    provisional typed graph template, ports, rates, expressions, validation

program/
    tracks, instruments, voice/patch lifetime, routing, tempo, origins

script/
    language-neutral frontend traits and evaluation result

script-piccolo/
    Piccolo adapter and Apteronotus sandbox

runtime/
    transport, scheduler frontier, generations, external controls

audio/
    backend-independent prepared commands and lifetimes

audio-fundsp/
    native/fundsp lowering and renderer

web/
    browser worker, AudioWorklet, UI integration

live/
    editor, transactional evaluation, diagnostics
```

Do not create all crates before the types exist. The important dependency rule
is:

```text
pattern/music/graph/program know neither Lua nor fundsp
script adapter knows the program builders
audio backend lowers prepared program data
```

## Implementation order

### 1. Browser-language spike

Before building around an untested VM:

- run Piccolo natively and on `wasm32-unknown-unknown`;
- test the exact sandbox capability contract;
- implement one Rust-backed userdata builder;
- test all graph operators/metamethods;
- construct a tiny pattern and graph;
- exhaust fuel with an infinite loop;
- enforce or demonstrate memory accounting;
- evaluate repeatedly without retaining failed candidates;
- investigate exact source transformation.

The output is a decision and a thin adapter prototype, not production script
bindings.

### 2. Structured pattern values

Implement control maps, `Named`, and onset-sampled left-structured merge.
Classify the corpus tuple cases and add ordered values only if their semantics
are genuinely distinct.

### 3. Minimal symbolic graph to sound

Implement only enough graph data and fundsp lowering for:

```text
symbolic note frequency
    → sine
    → low-pass
    → envelope/VCA
    → output
```

Instantiate it from a pattern through the real monotonic scheduler frontier.

### 4. Corpus-driven pattern and music features

Add, with semantic fixtures:

- join/selection alignment modes;
- value expressions and field/index selection;
- pitches, keys, scales, and modes;
- chord parsing and construction;
- voicing and anchors;
- arpeggiation;
- explicit external controls.

Prioritize by song coverage rather than raw API count.

### 5. Transcribe the specification songs through Rust builders

Use them to extend graph primitives, rates, curves, routing, and validation.
The Lua bindings remain a translation of builders already proven in Rust.

### 6. Buses, sends, ducking, and persistent patches

Add track routing, pattern-to-control conversion, the second lifetime, and
crossfaded persistent graph replacement.

### 7. Live editor and complete web topology

Integrate transactional evaluation, source highlighting, runtime generations,
the browser worker, and AudioWorklet rendering.

### 8. Serialization only when demanded

Add a transient same-build encoding if the Emscripten fallback requires it.
Design a durable public format only if sharing language-free programs becomes
a real feature.

## Deliberate v1 exclusions

- arbitrary query-time Lua callbacks;
- query-order-dependent mutable patterns;
- unrestricted DOM, network, MIDI, filesystem, clock, or entropy access from
  Lua;
- native Lua modules;
- perfect PUC standard-library compatibility;
- unchanged execution of Strudel JavaScript;
- a stable public graph/program file format;
- arbitrary user-authored sample-rate code;
- every SuperCollider UGen;
- perfect state migration across graph updates.

These exclusions do not narrow the intended music significantly according to
the observed corpus. They keep impurity and realtime behavior explicit.

## Remaining high-leverage questions

1. Does Piccolo satisfy the sandbox contract and operator algebra on native and
   web targets?
2. What exact alignment algebra covers `pick`, `pickOut`, `squeeze`, `reset`,
   and `restart` without redundant AST machinery?
3. Are corpus tuples real ordered values or accidental substitutes for named
   maps?
4. What is the smallest typed expression representation shared sensibly by
   per-event mappings and graph initialization?
5. Is tonal context global, lexical, track-scoped, or patternable?
6. How are chord groups represented so arpeggiation survives querying?
7. Can fundsp lower the provisional graph and persistent patch model without
   leaking backend semantics upward?
8. Does the web DSP run as one Rust/WASM AudioWorklet processor or lower into
   browser-native Web Audio nodes?

The next code should answer question 1. Control maps follow immediately because
every later musical feature depends on structured event values.
