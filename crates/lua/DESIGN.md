# Lua boundary decisions

This note records behavior that is important to authored programs but is not
obvious from the Rust types. It distinguishes settled semantics from first-slice
policy and from work that is deliberately blocked. A provisional choice is not
an invitation for the runtime to infer more behavior from it.

## Settled semantics

### Source attribution

Mini-notation events created by direct `pattern(...)` and `play(...)` calls
carry document-absolute byte spans when the lexical source pass can locate an
unescaped literal exactly. Provenance remains derived from string-local spans,
so moving a call in the document does not perturb event or voice randomness.
Escaped quoted strings, CR-normalised long strings and nonliteral arguments
carry no newly invented document coordinate.

One attribution gap remains deliberate: `numeric_pattern_operand` can coerce an
inline string used in pattern arithmetic without a call-site argument. Those
events carry no source span until arithmetic coercion receives its own `_at`
entry path. Timeline placement is not a gap; it preserves spans already carried
by its `LuaPattern` values.

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

`zero()` is the neutral mono source used for loop-built graph accumulators.
`soft_saw(hz)` is scripting vocabulary implemented as a staged blend of the
existing saw and its sine fundamental; it consumes ordinary graph nodes and
adds no backend operation or retained callback.

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
`ply(n)` repeats an event inside its existing extent. A count choice such as
`ply("2 | 6")` is deterministic per cycle; `|` is independently part of
mini-notation and selects one stored branch per cycle from exact time and
source-site seed. Both remain pure under sliced and overlapping queries.

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
`beats(x)` requires a prior `tempo(...)` and captures both available
projections from that declaration. Beats-to-cycles is always
`value / beats_per_cycle`, including under ramps; it is position arithmetic
and needs no tempo integral. Beats-to-seconds is available only for a constant
tempo. A graph or note-clock consumer under a changing map receives a
diagnostic telling the author to use seconds rather than a guessed duration.

### Tempo and finite placement

`Program` owns the pure `apteronotus_transport::TempoMap`; Lua does not retain
a callback and does not duplicate the scheduler's clock representation.
`tempo(104)` creates a constant four-beat cycle, while `tempo { ... }` accepts
strictly increasing `{ at = bars(...), bpm = ..., over = bars(...) }` points.
`over` means “arrive at this BPM at `at`”, matching the Rust map. One evaluation
may declare tempo once. A separate meter API can replace the provisional
`beats_per_cycle` table field when music-theory context is settled.

`timeline { at(time, pattern), ... }` captures one cycle of each child pattern
and produces the existing finite `Pattern::Timeline`. Table order followed by
stable structural query order defines captured event ordinals; that order is
deterministic but is not musical chord order. Values, source spans and chord
member provenance survive capture. Empty timelines are silence. The resulting
pattern never repeats beyond its finite extent.

Cycle placements accept `bars(...)`. Absolute `at(secs(...), ...)` converts
through the score map, and `beats(...)` captures that map's meter and possible
constant seconds projection. Both therefore require an explicit `tempo(...)`
earlier in the same evaluation; silently using the default before a later
declaration would make textual declaration order change an already-constructed
value. Timeline IDs are evaluation-local counters because cross-edit
continuity is a revision concern, not call-site identity.

This prior-`tempo` requirement is one of two provisional top-level ordering
rules, rather than ordinary lexical dataflow. The other is activated
input-bearing `run(...)` patches: they process the flattened stems serially in
their `run` declaration order. Ordinary declarations remain inert. The
intended endpoints are a hoisted program clock declaration and explicit patch
inputs, respectively; neither rule should quietly spread to new APIs.

`key(tonic, mode)` similarly creates owned program data, but does not rewrite
literal scientific pitches. The current typed slice accepts major/ionian,
minor/aeolian, and dorian plus enharmonic tonic spellings. The value is
currently stored and validated but not consumed by scheduling, synthesis, or
literal pitch/chord expansion. It exists so future degree notation consumes
one explicit tonal context; pretending the declaration already changes tuning
would be a no-op disguised as semantics.

### Clocks, curves, and delay allocation

`ms` and `secs` return tagged absolute durations. Graph, delay and note-clock
curve consumers unwrap them as seconds; pattern timing rejects them. Conversely,
`bars` is accepted by pattern timing and rejected by graph consumers. Bare
finite numbers remain context-dependent for compatibility with scalar graph
code, so explicit units are the spelling that receives cross-domain checking.
`fast` and `slow` accept only dimensionless numbers.

Inside a staged graph, `step`, `ramp`, `decay`, and `window` create note-clock
`Curve` nodes. Outside a graph, `step`, three-argument `line`, and `window`
create pattern-rate transport signals in cycle units. `PatchTemplate` rejects
note-clock curves because persistent transport-clock automation has not been
defined. Context chooses an explicit rate boundary—graph staging or score
construction—not a clock hidden inside one stored value.

A breakpoint table constructed inside a graph uses fixed note-seconds points:
`curve { {0, 1}, {ms(10), 0.5}, ... }`. It desugars to the same offset plus
successive ramp differences as the score form below. Fixed times are required
in this slice; a symbolic point such as `{n.duration, value}` is diagnosed
rather than silently sampled. Supporting that spelling needs dynamic
breakpoint timing in the synth IR and remains part of the neon song work.

Score curves use an explicit constructor:

```lua
local rise = curve {
  { phase(0), 0 },
  { phase(1), 1 },
}
play(pad, pattern("c4") >> hold(bars(1)) >> pad.pressure(rise))
```

The breakpoint form is sugar for the same basis sum: the first value becomes
the offset and each adjacent pair becomes one held ramp difference. It carries
the whole curve into the event; it is not sampled during `Merge`. Point times
must increase strictly. `phase(x)` is limited to `[0, 1]` and requires an
explicit `hold(...)` on the event structure before the curve setter is applied.
This makes the duration dependency visible at construction rather than letting
the synthesizer guess it later. The explicit low-level
`clock`/`basis`/`terms` spelling remains available for generated curves and is
subject to the same duration rule when its clock is `note_phase`.

`hold(bars(...))` and `hold(beats(...))` become a domain-neutral
`Pattern::Hold` in exact rational cycle time. Its query looks back by the held
duration, so a slice beginning midway through a long note receives a
continuation without retriggering it. Because that look-back makes query work
proportional to hold duration times inner density, `Limits::max_hold_cycles`
is a separate host-policy ceiling. It is not folded into `density()`, which
continues to mean onsets per cycle, and an oversized hold is rejected rather
than truncated. `hold(secs(...))` cannot enter the pure pattern crate: inside
`at(...)` and `timeline {...}` evaluation resolves each event end through the
owned `TempoMap`, including its inverse under ramps. A captured seconds hold is
a finite timeline extent and does not widen later pattern queries, so it does
not consume the cycle-look-back budget. Using an unresolved seconds hold
directly in `play` is a diagnostic, not an approximate fixed-cycle conversion.

`control_signal(pattern, period)` is the explicit pattern-to-persistent-control
bridge. It is legal only while staging a persistent patch, requires a plain
numeric pattern and a constant tempo, and queries exactly one declared repeat
period during evaluation. That period must tile without gaps or overlaps and
is bounded independently by `Limits::max_control_signal_cycles` before the
query begins. The result is a flat `TransportSequence` table evaluated from
transport seconds by realtime-safe Rust; neither Lua nor the pattern AST
survives into the graph. A changing tempo has no single periodic-seconds
projection and is therefore rejected rather than approximated.

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

Program-scope analysers are a separate live-rate boundary. A lexical
`audio_input` may feed one retained `envelope_follower`, `pitch_tracker` or
`onset_detector`, and every later reference shares that stateful node by
identity rather than duplicating it structurally. An onset detector produces a
live edge handle, not a `Pattern`; `play(voice, trigger)` carries that handle
in the owned track while its pure pattern is silence. Voice-scoped
`voice.param(at_onset(signal))` records an init binding that the live scheduler
samples exactly once at the observed edge. `voice.hz(at_onset(signal))` retains
literal Hertz and deliberately bypasses MIDI/tuning conversion. Ordinary
setter patterns attached to the trigger are sampled with a left-closed
zero-width query at the observed transport cycle. `degrade` is instead derived
from structural track/voice identity plus the captured global arrival ordinal;
arrival interleaving is real retained history, while unrelated tracks can no
longer masquerade as the same event provenance. Transforms that require a
cyclic query coordinate are rejected rather than silently applied to the
trigger's vacuously silent pattern. The app polls retained detector controls
and schedules rising edges with a 30 ms minimum-latency margin.

`arp(order[, spacing])` is an owned pattern transform over immutable group provenance. It
consumes a simultaneous group's stable member indices and serializes those
members across the group's existing whole span, then removes the group marker.
The supported orders are `up`, `down`, `outside-in`, and `inside-out`.
Without spacing, the group span is divided evenly. Explicit spacing sets both
the onset interval and nominal slot length; the source group extent remains
authoritative, so slots are clipped at its end and members beginning outside it
are omitted. This is deliberately a clipping policy, not a hidden extension of
an `at(...)` placement. `beats(...)` supplies typed beat spacing after
`tempo(...)`; it projects to cycle time under any tempo map, but graph-time beat
durations require constant tempo because a changing map has no single seconds
value.

Literal `chord(symbol) >> anchor(note) >> voicing(shape)` is an
evaluation-only builder. The music crate parses the symbol and applies a small
deterministic voicing dictionary; Lua immediately emits fractional-MIDI primary
values in an ordinary `Pattern::Group`. No chord object or Lua callback reaches
query time. The anchor is an upper bound on the voicing's top note. `close-N`
uses successive chord tones, while `open-N` and `wide-N` require successively
larger gaps; `drop-2` lowers the second-highest close-position voice.

Chord groups carry a domain-neutral optional primary-member index beside the
existing immutable group provenance. `root_notes()` selects that member and
consumes the group; it therefore survives `timeline` capture without storing a
chord symbol in `pattern`. `octave(n)` then adds `12*n` to the numeric primary
value. Literal slash basses affect voicing order but do not replace the
harmonic-root marker. Patterned symbols and anchors need a query-time music
node and are not accepted through this literal builder.

Pattern-rate numeric expressions are owned `Pattern::Math` nodes. Constants
are continuous signals, and `sine`, `cosine`, `saw`, `perlin`, `step`, `line`
and `window` derive their values from rational transport coordinates.
Arithmetic is accepted only when at least one operand is continuous. With one
event-structured operand, its timing and provenance survive and the continuous
operand is sampled at the event onset. Two event patterns are rejected rather
than receiving an implicit `select`/`join` mode. An event operand that can emit
text, a map, or a curve is rejected at construction with its available source
span; arithmetic never silently preserves a nonnumeric value.

At the zero-width query used by `Merge`, continuous components are sampled at
the left note onset even when a numeric control slot began earlier. Thus
`velocity("0.5 1" * main)` holds the mini-pattern value but samples `main` for
each note. `velocity` is the sole score spelling for the implicit event field,
and the graph reads `n.velocity`. Graph/master amplitude uses the signal
processor `mul(...)`; `gain` is not an alias spanning the two rates.
Voice-scoped setters accept the same numeric patterns and revalidate the field
against the target voice. Exact dynamic values are checked when their onsets
are instantiated; evaluation validates fixed scalars and curves immediately.

`play` distinguishes pitched and fixed-frequency trigger voices from the
staged graph contract. If any reachable graph source reads `n.hz`, text primary
values must parse as pitches. Otherwise non-pitch labels such as `bd` and `x`
are accepted as onset labels; the scheduler supplies an unobservable finite
placeholder in the shared `Note` layout. This keeps spelling mistakes loud for
pitched instruments without teaching `crates/pattern` about drums. This makes
the graph contract deliberately load-bearing: editing a formerly unpitched
voice to read `n.hz` also makes its existing text labels subject to pitch
validation. That action at a distance is preferable to a second handwritten
"pitched voice" flag that can disagree with the graph.

`slew(n.hz, response)` is the one graph spelling that consumes preceding-onset
context. The scheduler orders each track's onsets temporally, gives
simultaneous members the same prior pitch, and commits the remembered pitch
only with a successful scheduling window. Lowering produces a finite
onset-clock smoothstep from that pitch to the current note. A first onset has
no predecessor and begins at its target. Track identity is not reconciled
across evaluations yet, so live activation clears this history instead of
risking a glide from a different reordered track. `signal >> slew(response)`
is otherwise a normal live follower with init-rate response time and no score
history. Neither form promises oscillator or filter-state continuity; that
requires a score-driven persistent patch.

Direct transport-signal constructors receive source spans through the lexical
attribution pass. Arithmetic has no direct call site of its own, so a
signal–signal result inherits the first available operand span; an expression
such as `0.95 * main` therefore points at the constructor that produced
`main`. Arithmetic mini strings retain their mini-notation spans. Direct
`perlin(...)` calls additionally derive source-site seeds through the same
pass as randomized transforms.

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
At score level, `pattern >> to(bus, scalar_level)` stores an `EventRouting`
entry on the owned track. It copies the completed voice outputs at each onset;
it cannot address graph-local wires. Equal channel counts route lane-for-lane;
a mono voice is deliberately duplicated across a wider main output or send
bus. Other mismatches remain diagnostics. The first slice deliberately accepts
a scalar level only.
Pattern-varying send levels belong in event control data rather than being
silently sampled by this constructor.

`duck(trigger, amount)` is track-output routing, not an event parameter. During
voice preparation the scheduler queries the numeric trigger rhythm over that
voice's complete lifetime and converts each event into an allocation-free
smoothstep release over the trigger event's extent. The compiled envelope
multiplies every flattened output lane, so dry and send contributions pump
together. Pattern queries and tempo conversion occur before publication; the
audio thread only reads the resulting finite segment array.

`send { graph = processor, level = scalar }` is the program-scope return form.
It allocates an opaque stereo bus immediately so later voice declarations can
target it. The concrete flattened lane offsets are deliberately unavailable to
Lua. After evaluation has seen the complete bus layout, all such returns and
the optional `master(processor)` are staged as one full-layout input patch:
each return consumes its own bus lanes, mixes its scaled wet output into main,
and routed lowering clears the consumed send lanes. This finalization is why a
send declaration is data during evaluation rather than an eagerly instantiated
patch.

The currently bound `reverb(room_size, time, damping)` lowers to fundsp's
bounded native stereo FDN. That is an explicit compatibility bridge for the
first whole shipped song, not a quiet decision that authored reverb networks
belong in the Rust primitive list. The intended readable stdlib network still
depends on settled feedback IR; replacing this node must preserve the declared
state budget, response horizon, and persistent-return lifetime.

The narrower `delay(fixed) >> [lowpass(fixed) >>] feedback(amount)` form has an
explicit acyclic IR node with a declared delay allocation and a response tail
derived to the 5% repeat threshold. `feedback` rejects every other preceding
processor shape. This supports the specification tape delay without pretending
that an arbitrary graph cycle has defined causality, gain or publication cost.

`pluck` similarly makes its rate boundary explicit: excitation remains audio
rate, while frequency and damping are constants or note-bound init values.
They choose fundsp's Karplus–Strong state once per voice and cannot be curves
or arbitrary graph signals. A one-hertz lower allocation bound is publication
metadata and instantiation rejects values below it.

`run(patch)` records program-lifetime activation intent; it does not instantiate
DSP during evaluation. `run(patch, span(begin, end))` records an exact finite
transport span. A finite autonomous patch is instantiated once at its onset.
The runtime inserts a short smooth lifecycle gate immediately after every
nonterminating audio source, not at the output: downstream filters, delays and
reverbs therefore receive silence at the boundary, drain their declared
response tails, and are then removed by the sequencer. This is also why a
finite input-bearing whole-stem processor is currently rejected—its warm-up,
retained input and removal semantics need the explicit patch-input model rather
than an output gain.

Program-lifetime zero-input runs are autonomous sources mixed with routed
voices. An input-bearing run must consume the complete flattened main/bus
layout and processes that layout in declaration order. Exact arity is required;
lanes are never folded modulo.
This textual-order signal flow is the second deliberate exception listed under
“Tempo and finite placement”; a future patch that consumes another patch must
name that input rather than rely on adjacency.
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

## Optional program inputs

`audio_input { name, channels, fallback = "silence" }` and
`control_input { name, range, default, unit? }` create owned, program-scope
resources. Their userdata handles are lexical capabilities: the user-facing
name is host metadata, not a global string lookup, and one handle can be
captured by more than one staged graph. Graph templates retain an opaque input
identity and channel rather than replacing an absent input with a literal
during evaluation.

The native host binds the first declared logical audio input to the default
input device, mapping its leading channels when input and output sample rates
match exactly. The callbacks communicate through a bounded preallocated ring;
the DSP graph reads it with fixed safety latency and never invokes host or Lua
code on the audio thread. No implicit resampler hides independently clocked
devices. Missing devices, rate mismatches, additional logical inputs and
unavailable channels lower through the required `silence` fallback with a
visible host warning. The browser host still uses that fallback until
permission/device attachment is implemented. An unbound control input uses the
ordinary shared `ControlStore` initialized to its declared default. These
fallbacks are part of the owned candidate and keep an unplugged optional source
from invalidating or muting unrelated authored graph branches. Cross-edit
persistent reuse compares logical input specifications and rebinds arena-local
handles just as it does for controls and buses.

Derived program signals retain lexical identity too. A binding such as
`their_hz = input >> pitch_tracker { ... }` stages one persistent stateful
analyser and publishes one hidden control handle for every consumer. Writing
the same expression twice still creates two analysers: this is binding-identity
fan-out, not structural common-subexpression elimination. The hidden controls
are engine plumbing and the app deliberately excludes them from user faders.

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
- The direct-call source transformer treats its provenance-bearing calls
  (`pattern`, `play`, randomness and transport-signal constructors, among
  others) as reserved when used in call position. Strings, comments,
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
- host device attachment and shared derived input analysers;
- pattern-varying score-send levels and general pattern-to-control conversion;
- arbitrary feedback graph causality beyond the bounded filtered-delay form;
- complete graph-expression byte spans and a diagnostic source map.

These are omitted because their contracts are open, not because the Lua layer
should grow a parallel representation.
