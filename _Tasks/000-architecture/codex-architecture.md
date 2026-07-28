# Apteronotus architecture proposal

This document is an independent proposal written before inspecting the other
files in this task directory. It records the architecture implied by the design
discussion, including the decisions that should remain deliberate rather than
emerge accidentally during implementation.

## Purpose

Apteronotus is a live music system with two equally important halves:

1. A temporal pattern system that describes when musical events and controls
   occur.
2. A synthesis system that describes how signals are generated, routed,
   modulated, and rendered.

It should support both conventional sequencing and modular synthesis in one
program. A user should be able to describe a melody, construct the instrument
that plays it, alter either while playback continues, and export the result as
a portable program that does not depend on the authoring language.

Tidal and Strudel are useful architectural references for live evaluation,
pattern querying, lookahead scheduling, and sample-accurate playback. The
implementation should not use their code, syntax, or internal representations.

## Central decision: the product is the program IR

Lua is an authoring frontend, not the runtime definition of a song.

Evaluating source produces an immutable, data-only `Program`:

```text
Lua source ──evaluate once per edit──▶ Program
                                         │
                                         ├─ pattern trees
                                         ├─ voice graph templates
                                         ├─ persistent patch graphs
                                         ├─ buses, sends, and master graph
                                         ├─ tempo and transport metadata
                                         ├─ assets
                                         └─ source map
```

The scheduler and audio renderer consume `Program`; they do not call back into
Lua. A different frontend—a purpose-built language, JavaScript, a visual patch
editor, or an offline compiler—can generate the same representation.

This gives the system a stable center:

- Desktop Lua and a future web editor can share one runtime.
- A program can be serialized and played without its source language.
- Validation and resource limits happen before publication.
- Playback is deterministic unless a program explicitly names live input.
- The control and audio threads never depend on garbage collection.
- Source errors do not interrupt the currently playing revision.

“Evaluate once” means once for each edit or explicit evaluation, not once for
the lifetime of a song. Live coding continually produces new immutable
revisions.

## Runtime layers

```text
editor / frontend
    │
    │ source evaluation
    ▼
immutable Program revision
    │
    │ atomic publication with an effective musical time
    ▼
control runtime and scheduler
    │
    │ timestamped graph/control operations
    ▼
audio backend
```

### Frontend

The frontend owns parsing and executing Lua, user-facing diagnostics, library
functions, and conversion to the common IR. It may allocate, use tables and
loops, and run ordinary user functions while constructing a program.

No Lua closure, userdata object, registry reference, or interpreter state may
be stored in `Program`.

### Control runtime

The control runtime owns transport, tempo conversion, pattern queries,
lookahead, voice allocation decisions, external control input, and publication
of prepared audio commands. It runs repeatedly but never evaluates user Lua.

### Audio backend

The audio backend owns sample processing and all state that changes at audio
rate. It receives prepared, bounded commands and must not allocate, lock, parse,
load assets, or invoke a language runtime in its callback.

## Pattern model

A pattern is a pure queryable tree:

```text
query(timespan) -> events
```

Time uses exact rational values in the musical domain. Each event carries its
whole occurrence, the part intersecting the query, a source origin, and a
control map. The scheduler queries non-overlapping spans and advances a
monotonic frontier:

```text
query [frontier, frontier + lookahead)
schedule returned onsets at absolute audio times
frontier = frontier + lookahead
```

Duplicate prevention belongs to this frontier, not to event deduplication.
Identical simultaneous events are valid music.

Pattern trees may be dynamic without containing language closures. Runtime
nodes can include sequence, stack, repetition, reversal, conditional cycles,
seeded degradation, Euclidean rhythm, arithmetic signals, external named
controls, and other explicitly modeled operations.

User-defined Lua helpers are construction-time macros in the ordinary sense:

```lua
function shimmer(p, amount)
    return p
        >> off(beats(0.5), gain(amount))
        >> sometimes(0.2, rev())
end
```

Calling `shimmer` leaves standard IR nodes behind. The Lua function does not
survive into the scheduler.

An arbitrary query-time Lua callback may be considered later as an explicitly
nonportable escape hatch, but it must not be the foundation. Such an extension
would require a separate worker, instruction and event budgets, lookahead,
failure isolation, and a clear marker that the program cannot be exported
without Lua.

## Control maps and parameter merging

Events carry named values rather than a single untyped payload:

```text
{
    note: C3,
    velocity: 0.8,
    cutoff: 1200 Hz,
    pan: -0.2
}
```

Parameter application takes timing structure from the note/event pattern and
samples the parameter pattern at each left-hand onset. It produces one voice
per left event. It must not multiply events merely because a control pattern
has greater density.

Every declared instrument parameter has:

- a stable name;
- a type and unit;
- a valid range;
- a default;
- a permitted rate.

There is also a documented fixed set of implicit note controls such as pitch,
frequency, velocity, duration, gate, pan, and stable event identity.

## Synthesis graph IR

The synthesis representation is a typed graph, not an opaque host object. At a
minimum it contains:

- versioned primitive operation identifiers;
- stable node identifiers;
- typed input and output ports;
- channel counts;
- constants and symbolic parameters;
- edges and explicit fan-out/mixing;
- state and lifetime declarations;
- source origins;
- resource estimates.

The meaning of a portable graph requires more than node names. The IR and its
primitive registry must define:

- sample-rate behavior;
- initialization, event, control, and audio-rate parameters;
- channel conversion rules;
- state initialization and reset;
- feedback legality;
- latency;
- deterministic random behavior;
- denormal and non-finite handling;
- voice completion;
- compatibility across primitive versions.

Rates should be explicit:

```rust
enum Rate {
    Init,    // fixed when an instance is prepared
    Event,   // fixed or changed at a musical event
    Control, // updated at block/control rate
    Audio,   // one value per sample
}
```

Signals and automation curves should share a compositional value model where
possible. A sine signal, envelope, line, random signal, or arithmetic
combination can drive an oscillator input, filter cutoff, mixer gain, or
score-level parameter, subject to rate checking.

Feedback cycles require an explicit delaying/stateful primitive. A bare
zero-delay graph cycle is invalid.

## Two instrument lifetimes

A per-note voice and a persistent modular patch are distinct concepts and
should remain explicit.

### Voice

```lua
voice {
    graph = function(n)
        return sine(n.hz)
            >> lowpass(n.cutoff)
            >> mul(adsr(...))
    end
}
```

A voice graph is a template. Each note onset instantiates fresh oscillator,
filter, envelope, and other state. This is naturally polyphonic. Old instances
may finish after a new program revision is published.

The Lua graph function is staged once per edit with symbolic note inputs. It
constructs a graph template. It is not run for every note. Loops and tables can
construct repeated graph structure; data-dependent topology must use explicit
IR operations such as selection rather than an ordinary Lua branch over a
symbolic value.

### Patch

```lua
patch {
    controls = { pitch, gate, velocity, cutoff },
    graph = function(cv)
        return sine(cv.pitch)
            >> lowpass(cv.cutoff)
            >> mul(envelope(cv.gate))
    end
}
```

A patch is instantiated once and runs continuously. Events update named
controls or gates rather than constructing new graphs. It supports Eurorack-like
behavior:

- free-running oscillators;
- phase continuity;
- monophonic pitch and gate;
- portamento based on prior control state;
- persistent filter and delay state;
- feedback;
- long-running modulation;
- live virtual CV.

Polyphonic note instruments normally use `voice`; monosynths, drones, modular
racks, sidechain followers, and persistent effects may use `patch`.

Both compile to the same graph primitive vocabulary but have different state
ownership and scheduling contracts.

## Buses, sends, and master processing

The program contains an explicit routing graph:

```text
voice/patch outputs
    ├─ track bus
    ├─ named sends
    └─ master graph
```

A send embedded in an instrument graph is a fixed routing edge. A score-level
send value is an event or control parameter that changes the amount routed from
that instance. These should not be represented by one ambiguous operation.

Sidechain and ducking operate on buses or control signals, not as ordinary
per-note parameters. A pattern may be converted to a shaped control signal
through an explicit sample-and-hold/trigger operation.

## Live updates

Evaluation is transactional:

```rust
evaluate(source) -> Result<ProgramRevision, Diagnostics>
```

Only a fully validated successful revision is published. On error, the old
revision continues playing.

A publication contains:

```rust
struct ProgramUpdate {
    generation: u64,
    effective_at: MusicalTime,
    program: Arc<Program>,
}
```

Supported effective times should include:

- the next unscheduled frontier;
- the next beat;
- the next cycle;
- an explicit musical time.

Program replacement has different consequences at each level:

1. The scheduler begins querying the new pattern revision at the effective
   frontier.
2. Newly triggered voices use the new templates.
3. Already sounding voices normally retain their old graph revision until
   release.
4. Persistent patches, sends, and the master graph are prepared away from the
   audio thread and swapped at a block boundary.

The reliable default for a persistent graph replacement is a short crossfade.
State migration is an optimization for compatible nodes with matching stable
identities. It may later preserve oscillator phase, filter history, envelopes,
or delay buffers, but correctness must not depend on it.

The scheduler will normally already have submitted a small amount of future
work. The initial policy should accept that bounded update latency. A stricter
mode can track scheduled event IDs by program generation and cancel or fade
events that have not started.

## Source locations

Lua can expose the caller's source file and line through debug information, but
not its exact call-expression column. Tail calls and library wrappers also make
stack attribution imperfect.

Bindings may walk upward to the first non-library Lua frame and attach a
line-level origin during construction. This work is permitted only in the
frontend.

Locations must therefore admit different precision:

```rust
enum SourceLoc {
    Span {
        file: FileId,
        start: u32,
        end: u32,
    },
    Line {
        file: FileId,
        line: u32,
    },
    Generated {
        callsite: Box<SourceLoc>,
        expansion: Vec<SourceLoc>,
    },
}
```

Exact spans require parsing or source transformation. A future Lua frontend
can rewrite library calls to include hidden call-site IDs. The editor must not
assume that every diagnostic has a byte-accurate span.

Generated graph and pattern nodes should retain both their primary user
callsite and, where useful, an expansion stack through standard-library
helpers.

## Serialization

The portable artifact is a versioned program bundle, not Lua bytecode:

```text
Program bundle
    ├─ schema and capability versions
    ├─ patterns
    ├─ voice templates
    ├─ persistent patch graphs
    ├─ routing and tempo
    ├─ assets or content-addressed asset references
    ├─ deterministic seeds
    └─ optional source map and source text
```

Lua bytecode, closures, native pointers, userdata, and backend-specific graph
objects are forbidden.

Deserialization performs the same validation as source evaluation. Unsupported
primitives or versions produce a diagnostic before playback.

## Native and web rendering

The first native backend may lower graphs to fundsp or use fundsp primitives
behind the graph registry. Backend objects must not leak into the portable IR.

On the web there are two plausible implementations:

1. Lower portable graphs into Web Audio nodes.
2. Run the same Rust DSP engine as WASM inside an `AudioWorklet`.

The second gives native and web the closest semantics and keeps all user
languages outside the audio path. JavaScript remains a shell that loads the
worklet, transfers program updates and assets, and handles browser integration.

The Web Audio platform permits JavaScript `AudioWorkletProcessor` code on the
rendering thread, but ordinary editor or pattern JavaScript does not need to be
there. Apteronotus should keep Lua and any future user language entirely out of
both the scheduler query loop and audio callback.

## Real-time safety

All unbounded work happens before publication:

- parsing and source evaluation;
- graph validation;
- resource estimation;
- asset lookup and decoding;
- graph allocation;
- backend preparation;
- compilation or lowering.

Communication to the audio thread uses bounded lock-free queues or equivalent
prepared frontend/backend handles. The audio thread performs bounded command
application and graph processing only.

Resource validation includes:

- parser depth and node budgets;
- pattern event-density limits;
- graph node, edge, channel, and state-memory limits;
- maximum polyphony;
- delay and asset-memory limits;
- prohibition of unbroken feedback cycles;
- finite numeric ranges;
- a bounded number of commands per render block.

Programs received from the web or another language are untrusted data even
when open source.

## Suggested crate boundaries

```text
pattern/
    exact time, events, control maps, pattern AST, querying

graph/
    typed portable graph IR, rates, ports, validation, primitives

program/
    instruments, routing, tempo, assets, source maps, serialization

script-lua/
    Lua runtime and bindings; Program construction only

runtime/
    transport, lookahead scheduler, generations, external controls

audio/
    backend-independent audio commands and lifecycle

audio-fundsp/
    native lowering/rendering

web/
    WASM bindings, AudioWorklet shell, browser asset transport

live/
    editor integration, evaluation transactions, diagnostics
```

The lower crates must not name Lua. The portable graph must not name fundsp,
Web Audio, egui, or a specific serialization library in its semantic API.

## Deliberate non-goals for the first implementation

- Arbitrary Lua execution during pattern queries.
- Loading arbitrary native DSP plugins.
- Perfect state migration across graph edits.
- Identical floating-point samples across every backend.
- A general optimizer for every graph.
- Zero-latency source updates after events have already been scheduled.
- An unlimited primitive catalog.

The four specification songs should define the initial surface. New machinery
should be justified by a concrete musical requirement in one of them.

## Recommended implementation order

1. Add control maps and onset-sampled parameter merging to the pattern crate.
2. Define the graph IR, type/rate rules, primitive registry, validation, and
   serialization without an audio backend.
3. Transcribe the specification instruments into graph IR using Rust builders.
4. Implement per-note voice lowering and sample-accurate scheduling.
5. Implement persistent patches and named control updates.
6. Add buses, sends, master processing, and crossfaded graph replacement.
7. Define `Program`, its portable bundle, and complete validation.
8. Add Lua strictly as a `Program` builder.
9. Add transactional live evaluation and generation-aware publication.
10. Add the web renderer using the same serialized `Program`.

Writing the instruments against Rust builders before Lua remains important:
if the underlying typed API cannot express the songs cleanly, a scripting
language will only conceal the problem.

## Decisions to settle with prototypes

The following deserve small executable experiments rather than speculative
abstraction:

- Whether fundsp graph replacement and crossfading cover persistent patches
  cleanly or require a small custom renderer.
- How stable node identities are assigned through Lua helper expansion.
- Whether one signal representation can cleanly serve score automation,
  control-rate modulation, and audio-rate modulation without hiding expensive
  rate conversion.
- The exact contract for pattern-to-control conversion and sidechain release.
- Whether voice graph topology may depend on event values, and if so whether
  explicit graph selection is sufficient.
- Which persistent node states are worth migrating during live updates.
- Whether web playback uses lowered Web Audio graphs or one WASM worklet
  renderer.

The architecture should preserve both choices where possible, but its public
semantics must not be postponed to whichever backend happens to be written
first.

---

## Reconciliation after reading the existing notes

Everything above was written before reading this task's `README.md`, `LOG.md`,
the repository `CLAUDE.md`, or `songs/CLAUDE.md`. This section records what
changed—or deliberately did not change—after comparing them.

### Strong agreement

The two proposals independently converge on:

- exact rational musical time;
- pure, queryable, serializable pattern trees rather than closures;
- `whole`/`part` event extents;
- deterministic position-derived randomness;
- a monotonic scheduler frontier rather than deduplication;
- parameter maps and left-structured, onset-sampled merging;
- declarative instrument and parameter metadata;
- a separate language binding crate;
- fundsp and its `Sequencer` as the first native implementation;
- no host-language closure at control or audio rate;
- curves/signals as declarative compositional data;
- explicit clocks for note-relative, transport-relative, and bus-relative
  automation;
- one primitive registry feeding bindings, completion, validation, and
  backends;
- buses as the right home for sends, ducking, and persistent processing;
- building the four songs against Rust APIs before binding a scripting
  language;
- keeping the last valid revision sounding through every frontend failure.

The prior notes' basis-sum representation for curves is more specific than the
independent proposal and should be retained as a promising implementation:

```text
Σ cᵢ · basisᵢ(t - Tᵢ)
```

It satisfies the proposed graph IR's requirements for a bounded,
realtime-safe signal representation and gives addition, scaling, and shifting
useful algebraic meaning.

The earlier distinction between primitives implemented in Rust and composite
instruments/effects implemented in readable standard-library source also fits
the proposal, with one qualification: standard-library source must finish by
expanding to portable graph nodes.

### The major disagreement: when `graph = function(n)` runs

The existing architecture says:

```text
pattern rate       scripting language
voice-build rate   scripting language, once per note
control/audio rate compiled expressions or built-in nodes
```

That allows `graph = function(n)` to inspect a concrete note and directly
construct a fundsp graph for every onset. It is expressive and initially
simple, but the function remains required during playback. Consequently:

- a serialized song cannot instantiate new voices without its language
  runtime;
- desktop Lua output cannot be played indefinitely on the web without Lua;
- graph-building allocation and Lua GC occur in the scheduling path;
- arbitrary note-dependent Lua control flow has no portable representation;
- the native backend's graph type tends to leak into the language binding.

The independent proposal instead says:

```text
edit/evaluation    scripting language, once per revision
pattern query      Rust over Pattern IR
voice instantiate Rust over GraphTemplate
control/audio      Rust DSP
```

Here `n.hz`, `n.vel`, and declared parameters are symbolic inputs while the
graph function executes. Lua loops may generate topology, but a branch on a
symbolic note value must become an explicit graph operation. The resulting
`GraphTemplate` is serializable and can be instantiated for every onset by any
backend.

This is a real restriction:

```lua
if n.vel > 0.8 then
    return bright_graph
else
    return soft_graph
end
```

cannot be an ordinary Lua `if` when `n.vel` is symbolic. It must be expressed
as a portable selection, two voices selected by the score, or another explicit
IR construct. In exchange, the same compiled revision can run on desktop,
web, or a future backend with no Lua present.

The discussion leading to this document established portable graph playback
and language-independent graph generation as desired properties. For that
reason, this proposal intentionally chooses staging once per edit. The old
per-note builder remains a useful prototype path, but it should not become a
public semantic contract accidentally.

A small prototype should settle whether the four songs can be expressed with
symbolic `NoteInput`s and graph selection. If they cannot, the fallback should
be an explicitly marked dynamic builder, not an invisible Lua dependency in
every `voice`.

### The rate hierarchy should be revised

The existing four rates describe the DSP correctly but mix authoring with
runtime. With staged programs, the complete hierarchy becomes:

| rate | executor | responsibility |
|---|---|---|
| edit/evaluation | frontend language | construct a `Program` revision |
| pattern query | Rust control runtime | query time spans and create events |
| event/voice instantiation | Rust control runtime | bind event values to graph templates |
| control | prepared DSP | envelopes, smoothing, modulation |
| audio | prepared DSP | oscillators, filters, feedback, mixing |

This does not weaken the existing realtime rule; it strengthens it by removing
the frontend interpreter from one additional runtime tier.

### Persistent `patch` resolves the portamento question

The existing notes correctly identify that a per-note voice cannot honestly
implement a 303-style glide. Passing `prev_hz` into a new voice approximates
the sound, but it does not model the instrument's lifetime.

The explicit `patch` lifetime is the proposed resolution:

- `voice` owns fresh state per event and is naturally polyphonic;
- `patch` owns persistent state and receives pitch, gate, velocity, and other
  named control changes.

This is not merely a portamento special case. It also supplies the correct
model for drones, modular racks, feedback networks, long-running LFOs, bus
processors, and instruments whose oscillator phase survives notes.

The current song syntax contains only `voice`; `techno.eod` should eventually
use `patch` for the acid instrument, after the runtime model is proven.

### fundsp is a backend, not the portable type system

The existing notes say fundsp's operator algebra is already the patchbay. That
is valuable implementation experience and should guide the Rust builder API.
However, directly storing fundsp trait objects as the meaning of a song would
prevent serialization and make alternate backends translations from an opaque
native representation.

The reconciled position is:

- copy no fundsp code or public representation into the portable format;
- give the graph IR algebra compatible concepts—serial composition, parallel
  inputs, fan-out, mixing, arithmetic;
- validate that algebra independently;
- lower it to fundsp for the first native backend;
- use fundsp `Net`/frontend/backend facilities where they satisfy persistent
  graph updates;
- retain the option to use the same IR in a Rust/WASM AudioWorklet renderer or
  another backend.

The songs may keep concise patchbay operators. Their semantics belong to the
Apteronotus graph IR rather than being defined as “whatever fundsp does.”

### The component boundary changes shape

The prior log considered a language component whose boundary returns events,
and correctly rejects sending samples through an interpreted component
boundary.

Once `Program` is the portable artifact, a language component should ideally
return serialized pattern and graph IR, not remain involved in every pattern
query. Events are still the correct boundary for an external scheduler or MIDI
backend, but they are too late a boundary for exporting an indefinitely
playable program whose patterns remain dynamic.

There are therefore two valid liftable seams:

```text
authoring frontend → Program IR
control runtime    → timestamped events/graph commands
```

Neither seam carries sample buffers. Runtime-loadable audio DSP is a separate
advanced capability and may use compiled core Wasm on the actual audio engine,
not the language component.

### New conclusions absent from the earlier notes

The design discussion after the previous session adds:

1. **A program is revised, not compiled forever.** Source is reevaluated on
   every explicit live edit, producing an atomic replacement. This matches the
   useful live behavior of Tidal and Strudel without retaining their
   host-language closures.
2. **Publication has musical timing.** A revision should declare whether it
   becomes effective at the next frontier, beat, cycle, or explicit time.
3. **Graph replacement is lifecycle-aware.** Old per-note voices normally
   finish; persistent graphs crossfade by default; compatible stable nodes may
   migrate state later.
4. **Already queued events need a policy.** The initial policy may accept
   lookahead latency. Generation-tagged cancellation is an optional stricter
   mode, not event deduplication.
5. **Lua stack locations are line-level.** File and line can be attached by
   walking to the first user frame. Exact columns require parsing or source
   transformation, and tail calls/wrappers require an expansion-aware origin
   model.
6. **Ordinary Strudel source is outside the audio renderer but not necessarily
   executed only once.** Its top level evaluates per edit, while closures held
   by patterns can run during repeated scheduler queries. Apteronotus can make
   the stronger guarantee that no frontend-language closure survives
   evaluation.
7. **Serialized graphs are language-neutral.** Lua, JavaScript, a custom DSL,
   a GUI, or an offline tool can all generate them. Web playback requires only
   the IR runtime, not the originating language.
8. **Web Audio permits JavaScript DSP through AudioWorklet, but Apteronotus
   need not use user JavaScript there.** A Rust/WASM renderer in an
   AudioWorklet preserves one DSP implementation and keeps all authoring
   languages outside the realtime path.

### Revised next decision

Control maps remain the immediate pattern task. Before committing to
`Instrument` as a per-note language callback, the next synthesis experiment
should compare these two Rust-only shapes against all four songs:

```rust
// Existing direction: executable factory.
trait Instrument {
    fn build(&self, note: &Note) -> Box<dyn AudioUnit>;
}

// Proposed portable direction: data template.
struct InstrumentSpec {
    params: Vec<ParamSpec>,
    lifetime: InstrumentLifetime,
    graph: GraphTemplate,
}
```

The second may still lower to a factory internally. The question is whether
the public, serializable source of truth can remain `GraphTemplate`. That is
the highest-leverage uncertainty now because it decides the language boundary,
web artifact, realtime allocation model, and backend independence together.
