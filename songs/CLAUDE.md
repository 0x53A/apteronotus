# The songs are the specification

Written before the runtime, on purpose. These are the target: the
implementation is finished when they play, and any feature not needed by one of
them is not needed yet.

| song | what it is there to stress |
|---|---|
| `synthwave.eod` | polyphony, sends, sidechain, chords into arpeggios, curve automation spanning 64 bars, gated reverb |
| `waves.eod` | continuous signals steering everything, multi-second envelopes, sparse reproducible randomness, a voice with no oscillator in it |
| `techno.eod` | rigid grid, a 32-bar riser that must stay coherent, euclidean rhythms, ducking deep enough to be composition |
| `poles.eod` | δ at audio rate, percussion as pole placement, the stdlib-vs-primitive split, voices whose entire parameter set is two numbers |
| `neon.eod` | finite through-composed time, tempo changes and off-grid entrances, extended voicings, two-layer raw subtractive synthesis, per-voice drift, note-relative gestures, a persistent mono lead, a large modulated reverb |
| `jamming.eod` | live audio as a graph source, the audio → control crossing, `patch` as a persistent rack, the pattern algebra driving effects rather than notes, and a piece that is not reproducible until its input is recorded |
| `supersaws.eod` | an external Strudel port: stereo topology generation, weighted and polymetric event structure, per-event filter envelopes, channel-wise distortion, and probabilistic ratchets |

The first four are cyclic and fix every parameter at onset. `neon.eod` and
`jamming.eod` exist because that turned out to be a property of the author
rather than of music — see session 3 in
`../_Tasks/000-architecture/LOG.md`. `supersaws.eod` is the first port selected
by an external author, and exists to challenge the vocabulary with habits that
the preceding six could still share.

## Vocabulary the seven songs use

This is the target vocabulary inventory, not a claim that every word is already
bound. The current executable Lua subset is tracked in
`../crates/lua/README.md`; engine status and unresolved shapes are tracked in
`../_Tasks/000-architecture/README.md`.

### Executable checkpoint

The current Lua/runtime path can play multiple polyphonic tracks with
mini-notation; `fast`, `slow`, `shift`/`late`, `early`, `rev`, `every`, `off`,
`sometimes`, `degrade`, `segment`, and `range`; scalar or note-clock curve
event controls; staged graph arithmetic and filters; and persistent patches,
controls, buses, and graph sends. Those features reach both measured offline
audio and the native player.

No complete song in this directory runs yet. The dominant remaining seams are
tempo/timeline and transport-signal bindings; key/music theory and `arp`;
score-level sends and `duck`; typed non-pitch trigger events; symbolic graph
curve arguments; feedback/reverb and the rest of the DSP/stdlib vocabulary;
and live external inputs. The native player currently uses 120 BPM, and a
persistent/layout edit performs an explicit cycle-zero hard reset unless its
persistent data is compatible with the live arena. Compatible edits retain that
arena and activate at the ordinary frontier; changed persistent topology still
needs replacement crossfade.

`supersaws.eod` adds two independent pattern gaps at its final hat line:
`ply(...)` event repetition and mini-notation `|` random-choice alternation.
Implementing one does not make the other implicit. Its three weighted and
polymetric notation strings are already parse-guarded in
`crates/pattern/tests/mini.rs`.

**Top level** — `tempo`, `play`, `voice`, `send`, `master`, and from session 3
`patch` (persistent lifetime), `run` (activate an autonomous rack), `timeline`
and `at` (finite, through-composed placement).

Declarations are inert. `play(x, notes)` is the same call whether `x` is a
`voice` or a `patch` — the lifetime lives in the declaration, so moving an
instrument between polyphonic and persistent does not touch the score.
`run(patch, span)` is for a rack with no notes to drive it.

`tempo(72)` is sugar for a one-entry tempo map; missing `over` steps, explicit
`over` interpolates.

**Durations** — `bars`, `beats`, `secs`, `ms`. Always explicit; tempo-relative
and absolute are different things and guessing is the classic sequencer bug.

**Pattern transforms**, chained with `>>` — `fast` `slow` `rev` `every` `off`
`sometimes` `degrade` `arp` `shift` `duck` `to`, plus **one setter per declared
parameter**. `cutoff(...)` exists because `supersaw` declares `cutoff`; that is
the whole payoff of `params` being data.

**Signals and curves share arithmetic, not placement semantics.** A transport
signal merged through a setter is sampled at the note onset. A
`ControlValue::Curve` is carried whole into the voice and runs on its explicit
`NoteSeconds` or `NotePhase` clock. A persistent bus control remains continuous.
Pattern merge and `at_onset` are explicit signal-to-init boundaries at
different layers.

**Graph algebra** — `>>` `|` `&` `~` `+` `*` `-`, plus `mul` `mix` `zero` `dc`.

**Primitives used** — `saw` `soft_saw` `sine` `pulse` `noise` `pink` `impulse`;
`moog` `lowpass` `highpass` `bandpass` `peak`; `shape` `dcblock` `chorus`
`reverb` `delay` `feedback` `limiter` `pan` `pluck`; `adsr`.

**Stdlib, composed rather than primitive** — `ring(hz, t60)` solves a
resonator's bandwidth from a decay time via `t_se ≈ 3/(ζω₀)`; `slew`; `mix`.
Session 3 adds the reverb network itself: `predelay`, `early`, `diffuse` and
`fdn` compose out of delays, allpasses and matrix mixing, so only the
interpolating delay line is a Rust primitive.

**External sources** (session 3) — declared lexically and bound by the host:
`audio_input { name, channels, fallback }`, `control_input { name, range,
default }`. Never a global namespace with string accessors. Audio → control
crossings are always named nodes, never implicit coercion: `envelope_follower`,
`rms`, `pitch_tracker`, `onset_detector`. An `onset_detector` produces a live
*event stream*, which is a distinct type from a pattern — it has no lookahead
contract, and recording one turns it into an ordinary finite `timeline`.

## What writing them exposed

Ordered by how much it costs to be wrong.

**1. Events carry a named parameter map. Implemented.**
`ControlValue` has scalar and curve leaves; `ControlMap` is sorted and unique,
with `"value"` reserved for the primary note. `Named` creates a map-producing
control pattern and `Merge` preserves left timing, spans and provenance.
Nested maps and ordered `List` values remain deliberately absent.

**2. The merge sampling rule is a deliberate deviation from Tidal.
Implemented.** Tidal
emits one output event per right-hand event, which multiplies when the right
side is faster. Rule: sample the right side with a **zero-width query at
the left event's onset** and take the first value. One voice per note, no
combinatorial blowup, and continuous signals fall out correctly — which suits a
model where a voice is instantiated at onset with its parameters baked in.
Ratcheting is then something you write in the notation instead.

**3. `slew(n.hz, n.glide)` in `techno.eod` has no way to work as an ordinary
polyphonic voice.** Portamento
needs the *previous* note's pitch, and a voice instantiated per note has never
heard of one. The persistent `PatchTemplate` and program controls now provide
the honest monosynth lifetime, but the score-to-control driver that turns note
events into pitch/gate changes is still unwritten. Passing `n.prev_hz` and
`n.legato` remains the alternative; the song-level choice is unresolved.

**4. `duck(pattern, amount)` is not a per-note parameter.** It modulates a
whole line's output from a rhythm. Bus/stem routing now exists, but **a pattern
must still be convertible into a control signal** — sample and hold, with a
shaped release. New score-level concept, needed by two of the original four
songs.

**5. `to(send, x)` is used at two different levels and they are not the same.**
Inside a graph (`glass`, `cowbell`) it is a fixed patch; in a score
(`supersaw`) it is a per-note send level. **Resolved as two operations:** a
graph send taps internal graph channels and its level may itself be a signal;
an event send copies the completed voice channels with a scalar bound at the
onset. Both resolve a lexical bus binding to an opaque `BusId`, and routed
lowering sums them into the same program-wide stem layout. Graph sends are
bound in Lua; the score-level event-send constructor remains to be connected.

**6. `arp("up")` needs chords to survive as chords. Initial identity slice
implemented.** `[a3,c4,e4]` now becomes a `Group`, not a plain `Stack`, and its
events carry immutable group key, original member index and original count.
Unrelated coincident layers remain a `Stack`. The arpeggiator itself is still
unwritten: it must consume grouped events and choose an explicit pitch/order
policy rather than infer grouping from coincidence.

**7. Every voice has implicit parameters it never declares. Resolved and
implemented in synth/Lua.** The fixed symbolic set is `n.hz`, `n.velocity`,
`n.duration`, and `n.pan`, with documented defaults. Reproducible event seed
and runtime voice handle are separate identities: `init_random` consumes the
former, and nothing may seed sound from the latter.

**8. Voice-build code needs real loops and tables.** `for i = -3, 3`,
`ipairs({2810, 3730, ...})`, table literals. That is at voice-build rate, so it
is allowed by the rate hierarchy — but it rules out a minimal expression DSL
for this layer and confirms the Lua-shaped direction.

**9. `+` and `&` were used interchangeably for parallel banks** in `poles.eod`.
**Resolved:** `+` is mixing/summing, `&` is fan-out routing — Apteronotus
semantics, lowered to fundsp rather than inherited from how one backend happens
to overload an operator. `poles.eod` is fixed: the cowbell's two rings are
summed, so they take `+`.

The same pass caught a real arity bug rather than a mere inconsistency. The
resonator banks accumulated onto `zero()`, which has no inputs, while `ring()`
takes one — so `zero() + ring(...)` never type-checked. Collections now build a
table and pass it to `mix(...)`, which is what an explicit mix helper is for.
(`synthwave.eod`'s supersaw accumulator is *not* affected: it sums `saw()`
sources, which have no inputs either, so its arity always matched.)

**10. `key("f#", "minor")` in `synthwave.eod` currently does nothing.** Either
define it — scale-degree notation resolving against it — or delete the line.
Leaving it as decoration is the worst option.

**11. An event must be able to carry a note-clock curve, not just a number.
Implemented.** `n.pressure` and `n.bend` in `neon.eod` are control-rate for the
note's whole life. `ControlValue::Curve` now carries the pattern-owned basis sum
with an explicit `NoteSeconds` or `NotePhase` clock, and synth instantiation
lowers it without onset-sampling. Transport-clock signals remain a distinct
placement: a setter samples them at onset once that Lua binding exists.

**11a. But that is not enough for polyphonic expression, and no merge ever will
be.** The merge rule samples at the *left event's onset*, and a struck chord's
notes share one onset, so they receive one value. This is not specific to
curves: **no merged parameter can vary across simultaneous events** — pan,
velocity, detune and send level included. A strummed chord works, a struck one
does not. Per-voice variation is therefore built in `crates/music`, where a
voicing still knows how many notes it made and in what order:
`voicing(…):each(function(note, index, count) … end)`, a build-time closure that
disappears like `every`'s. Neither song exercises it yet.

**11b. A `Stack` should know whether it is a chord. Resolved in the pattern
layer.** #6 and #11a were one gap. Mini-notation now records `[a3,c4,e4]` as a
`Group` with member provenance, while `stack(bassline, melody)` remains an
unrelated `Stack`. `degrade`, `rev` and branch transforms preserve the original
member coordinates. The future music-layer voicing builder must create the same
metadata; `arp` will consume it.

**12. The first four songs were all cyclic, and that was an accident of the
author. Initial engine slice implemented.** `waves.eod` and `synthwave.eod`
fake linear time by gating gains with curve windows over cyclic material, which
works because their material *is* cyclic. `Pattern::Timeline` now supplies
finite, non-repeating material and `live::TempoMap` supplies step/ramped tempo
conversion. Their Lua song-level constructors remain to be bound.

**13. External triggers are not patterns. Initial live boundary implemented.**
`jamming.eod` fires bells from an audio onset detector, and you cannot query the
future of a live stream. `live::ExternalTrigger` deliberately has no query
contract; `TriggerRecorder` captures arrival ordinals and converts recordings
to a finite `Timeline`, recovering queryability and reproducibility. The audio
detector/device binding is still absent.

**14. A stateful signal read from several graphs must lower to one node.**
`their_hz` in `jamming.eod` is a pitch tracker referenced by two racks and the
score. Copying that subgraph per graph is not merely wasteful: two trackers can
disagree. But this is **identity preservation, not common-subexpression
elimination** — two separately written, structurally identical trackers must
stay distinct, because wanting two independent detector states is a legitimate
thing to write. The rule is that the *binding* carries the identity: bound once
and referenced twice is one node, written twice is two. No earlier song shared a
stateful signal, which is why this took five songs to surface.

**15. `at_onset` cannot be implicit at the graph/live-signal boundary.** The same tracked pitch feeds the rack
live and the bells frozen-at-strike, eight lines apart in one file. A struck
resonator that kept tracking would slide for its whole ring. It is a typed rate
boundary, `Signal<T> → Init<T>`, and the only legal collapse from signal to
init — enforced by the rate checker, not by documentation. Pattern `Merge` is
the separate pattern-layer signal-to-init boundary: a setter explicitly asks
for its RHS to be sampled at each left onset.

**16. Anything a voice seeds from must be derived, never counted. Implemented
through the first graph initializer.**
`neon.eod`'s `init_rand(n.id, …)` reproduces analogue component tolerance as
deterministic per-voice drift. A runtime counter for `n.id` would change with
query chunking, offline versus live scheduling, and structural edits that
insert an unrelated earlier event. Stable structural query order is a sampling
contract, not semantic event identity. So the implicit contract's voice
identity splits: a derived `event_seed` for anything reproducible, a
`voice_handle` counter for addressing a sounding voice and nothing else.
`waves.eod` is specified as giving the same nine minutes every time it is
opened; this is what that costs. Pattern provenance now derives the event seed,
the scheduler binds it into `Note`, and
`init_random(stream, min, max)` deterministically consumes it.

## Rules these songs are written to

- No host-language closure ever reaches a graph. Envelopes are declarative
  curves (`decay(ms(26))`, `curve{...}`, `window(a, b)`), never
  `function(t) ... end`. See the rate hierarchy in `../CLAUDE.md`.
- Randomness is seeded and reproducible. `waves.eod` must give the same nine
  minutes every time it is opened.
- Anything expressible by composition is stdlib and lives in readable source,
  not in Rust. `ring` is the example to imitate.
