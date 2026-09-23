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
  `timeline { at(time, pattern[, capture_duration]), ... }` placement;
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
- a band-limited graph-rate `triangle(hz)` oscillator, also available as
  `hz >> triangle()`. Frequency accepts symbolic/modulated graph inputs in
  voices and persistent patches; this spelling is not a transport pattern
  signal. Like `saw`, it uses shared generated waveform tables, not recordings;
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

`excitation >> pluck(hz, gain_per_second, damping)` builds a Karplus–Strong
string from a synthesized excitation; it does not load a sample. The gain is
strictly between zero and one, and damping is a coefficient in `[0, 1]`, not a
filter cutoff in Hertz. Pitch and damping currently accept a literal or a
direct note parameter (for example `n.hz`), but not graph arithmetic, even when
its operands are fixed at onset. Apply modulatable filtering after the string.
`songs/rust-and-voltage.eod` combines this primitive with oscillator fifths and
distortion to build its sample-free rhythm instruments.

For sounding-token highlighting in generated arrangements, store patterns:
`local riffs = { pattern("e2 g2"), pattern("a2 e2") }`, then pass `riffs[i]`
through transforms and timeline placement. Storing plain strings and later
calling `pattern(riffs[i])` currently loses their document source offsets.
Loops and tables preserve attribution once the literal has become a pattern.

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

### Retained-string proof of concept

`string_resonator(excitation, hz, mute, { min_hz = 40, decay = secs(35) })`
is a graph-only mono primitive. All three signal inputs accept signals or
scalars. `min_hz` (1–20000 Hz) bounds allocation; `decay` (positive, at most
120 seconds) is nominal T60, fixed at staging. Interpolation and brightness
losses shorten actual decay. Pitch is clamped to `min_hz..sample_rate/4`
(the sample-rate ceiling wins if those bounds conflict), and smoothed over
approximately 2 ms. This is a delay-loop approximation, not physical fretting.

In a persistent patch, excitation adds energy to the same string on every
pick. Zero excitation leaves it ringing. `mute` is clamped to 0–1 and increases
loss **inside** the loop: full pressure gives approximately 25 ms nominal T60.
Releasing pressure does not restore old vibration. Leaving pressure engaged
also damps subsequent picks. Input and internal state are bounded; extreme
excitation clips at an internal magnitude of 16. There is no implicit oscillator,
new voice, or reset when frequency changes. Memory is one delay of at most
`1/min_hz` seconds plus up to eight padding samples per string (including
room for the sample-rate-dependent pitch ceiling).

[`six-strings.eod`](../../songs/six-strings.eod) is a runnable 32-second electric
guitar demonstration. Six tiny noise-pick voices feed six mono buses; one
persistent patch contains six strings and one shared amp. It demonstrates all
six strings, an upper-three-only strum, a two-semitone bend, repicking while
bent, release, a hand mute, a new chord and palm-muted picks. Faders provide amp
drive, additional high-string bend, and palm pressure. Open **Six Strings, One
Amplifier** in the app, or render it with:

```sh
cargo run --release -p apteronotus-render -- --song six-strings --seconds 32 --tail 1 -o /tmp/six-strings.wav
```

The score uses existing `control_signal` transport patterns and short scheduled
audio excitations. The proposed addressed fret/pick/mute API is still a TODO;
there is no `guitar` or `strings.bank` builder yet. Scheduled curves repeat after
the demo's 32-second control period, while its excitation timeline is finite.
Source highlights identify pick events, not the full subsequent vibration.
Harmonics, realistic fret contact, bowed excitation and string coupling remain
in the [design ledger](../../_Tasks/006-persistent-strings/README.md).

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

## Compile-only feedback

`check_syntax(source)` and `check_syntax_with_limit(source, source_bytes)` parse
and compile the original Lua text with Piccolo's compiler and a temporary string
interner. They create no VM, install no bindings and execute no code. Returned
`SyntaxDiagnostic`s contain a message, an optional one-based Lua line, and that
whole line's original UTF-8 byte range. CR, LF, CRLF and LFCR follow the lexer’s
own line-counting rules. Token columns are deliberately absent. This is grammar
and compiler validation, not graph type checking or mini-notation validation.

## Finite capture windows

`at(start, pattern)` captures onsets in one local cycle, as before. Its optional
third argument is an explicit positive `bars`, `beats`, `secs` or `ms` duration:

```lua
tempo(112)
local phrase = pattern("c4 d4 e4 f4 g4 a4 b4") >> slow(7 / 8)
local score = timeline {
  at(bars(0), phrase, bars(7 / 8)),
  at(bars(7 / 8), phrase, bars(7 / 8)),
}
```

The child starts at local cycle zero for each placement. Only onsets within the
half-open capture window are copied; held notes and response tails can extend
beyond it. Longer windows advance cyclic alternation normally. A duration in
seconds is projected from the placement's start through the owned tempo map,
including any tempo changes crossed, and requires `tempo(...)` earlier in the
evaluation. It is not an absolute wall-clock endpoint.

`Limits::max_timeline_capture_cycles` defaults to 4096. Capture length and
estimated event count are checked before querying; the existing pattern-node
budget also limits the result. Unrepresentable shifted onset/release coordinates
produce diagnostics instead of overflowing rational arithmetic.

### Synthetic pipe organ

`organ_pipe(hz, stop)` produces a mono keyed pipe; `organ(hz, stops)` sums
1–16 ranks. They are shipped as readable [Lua source](src/stdlib/organ.lua).
Use them inside a `voice.graph`, then pan or route the result as usual:

```lua
local cathedral = voice { graph = function(n)
  return organ(n.hz, {
    {kind="flute", feet=16, level=.25},
    {kind="principal", feet=8, level=.65},
    {kind="principal", feet=4, level=.30, cents=1.2},
    {kind="reed", feet=8, level=.25, cents=-1},
  }) * .2 >> pan(n.pan)
end }
play(cathedral, pattern("[d3,a3,d4,f4] ~") >> hold(beats(3.5)))
```

Kinds are `principal` (default), `flute`, `string`, `reed`. Stop fields are
`feet` (0.5–32, default 8), `cents` (±100, default 0), `level` (scalar or graph
signal, default 1), optional `attack`/`release` durations, and `chiff` (0–0.2,
rank-specific default), and `voicing` (`"speech"` by default, or `"classic"`). `hz` is modulatable. Voice sustain stays constant until
key release; velocity response and room acoustics are explicit score choices.
Stop gains are added without registration normalization. Use sensible gain
for chords; no limiter is hidden inside the instrument.

The underlying reusable graph source `harmonics(hz, {a1, a2, ...})` accepts
1–32 finite amplitudes with absolute sum ≤1, fundamental first. It fades
ultrasonic partials at the device's actual sample rate. The organ is a designed
additive approximation, not a wind/pipe physical model. See the
[design and acceptance contract](../../_Tasks/007-synthetic-organ/README.md)
and the Library's **Iron Choir** (`cathedral-organ`) for dry stops, full
registration, independent pedals and a shared hall.

The default organ `voicing="speech"` gives foundation, middle and upper
harmonics different attacks, a brief upper-mode overshoot, and gentle voicing
across pitch. Use `voicing="classic"` on a stop for the original common-envelope,
fixed-spectrum variant. Both sustain while held and release cleanly; explicit
`attack` overrides each group's attack, and `release` controls their release.
The speech variant can have higher transient/bass peaks than the classic rank.

`flue_pipe(hz, pressure, turbulence, {min_hz=40})` is an **experimental physical
flue** available in both voice and patch graphs. It has retained bore/jet state,
normalized pressure (0–1), and explicit inlet turbulence (±1; use `noise()` or
`0`). Start around pressure 0.85; low pressure can fall below its speaking
threshold. Closing pressure lets vibration drain. In a voice, put the note
envelope on **pressure**, rather than merely fading the final audio:

```lua
local flue = voice {graph=function(n)
  local pressure = adsr(ms(20), ms(1), 1, ms(35)) * .85
  return flue_pipe(n.hz, pressure, noise(), {min_hz=40}) * .4 >> pan(n.pan)
end}
```

Put the node in a persistent `patch` when repeated keying must act on the same
vibration. `min_hz` is a staged allocation bound in 20–1000 Hz; pitch is limited
to min(1200 Hz, device rate/32). It uses 4× internal processing and has bounded
state, but is a reduced open-flue model with approximate tuning, not a physical
replacement for every named stop. Its pressure response is nonlinear and its
output deliberately dark. **Pipes Under Pressure** (`organ-laboratory`) gives a
dry comparison and a retained-pipe demonstration. The design document above
specifies smoothing, lifetime, memory budgets and the tested tuning region.
