# The songs are the specification

The first seven were written before the runtime, on purpose. Those are the
target: the implementation is finished when they play, and any feature not
needed by one of them is not needed yet.

| song | what it is there to stress |
|---|---|
| `synthwave.eod` | polyphony, sends, sidechain, chords into arpeggios, curve automation spanning 64 bars, gated reverb |
| `waves.eod` | continuous signals steering everything, multi-second envelopes, sparse reproducible randomness, a voice with no oscillator in it |
| `techno.eod` | rigid grid, a 32-bar riser that must stay coherent, euclidean rhythms, ducking deep enough to be composition |
| `poles.eod` | δ at audio rate, percussion as pole placement, the stdlib-vs-primitive split, voices whose entire parameter set is two numbers |
| `neon.eod` | finite through-composed time, tempo changes and off-grid entrances, extended voicings, two-layer raw subtractive synthesis, per-voice drift, note-relative gestures, a persistent mono lead, a large modulated reverb |
| `jamming.eod` | live audio as a graph source, the audio → control crossing, `patch` as a persistent rack, the pattern algebra driving effects rather than notes, and a piece that is not reproducible until its input is recorded |
| `supersaws.eod` | an external Strudel port: stereo topology generation, weighted and polymetric event structure, per-event filter envelopes, channel-wise distortion, and probabilistic ratchets |
| `drift.eod` | written *after* the engine: a stationary loop with no arrangement, variation derived from mutually prime transport periods, a stateless persistent noise floor, and live controls over it |
| `outbound.eod` | also after the engine: polymetric loop lengths and offsets *as* the form, a fast pulse against a low event count, and pentatonic cells that stay consonant wherever the 8-bar harmony has got to |

The first four are cyclic and fix every parameter at onset. `neon.eod` and
`jamming.eod` exist because that turned out to be a property of the author
rather than of music — see session 3 in
`../_Tasks/000-architecture/LOG.md`. `supersaws.eod` is the first port selected
by an external author, and exists to challenge the vocabulary with habits that
the preceding six could still share.

`drift.eod` and `outbound.eod` are written in the other direction — against a
working backend rather than ahead of one — against a working
backend rather than ahead of one — and so they specify nothing. They are here
because the corpus is the only place the songs live, and because between them
they test one thing the seven do not: all seven have an arrangement, and a
curve over 64 bars is a thing that runs out. A piece meant to be left on has
to get its variation from periods that do not divide the loop. `drift.eod`
does that with parameters and `outbound.eod` does it with whole layers, and
both are the same derive-from-coordinates rule the pattern algebra is built
on, applied to form instead of to a query.

## These files are the only copy

`crates/songs` embeds them with `include_str!`, so any application that wants
the corpus — the editor, the study app in `~/src/idiosepius`, a renderer —
links `apteronotus-songs` rather than copying a `.eod` into its own assets.
Editing a file here changes every consumer at the next build, which is the
point. `crates/app/src/corpus.rs` then asserts that every one of them still
evaluates, lowers and opens a stereo output, so a song cannot quietly stop
being a specification the engine meets.

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
controls, buses, graph sends, mapped tempo, finite `timeline { at(...) }`
placement, and transport-signal arithmetic sampled through numeric setters.
Literal chord symbols, anchors and the first named voicings now expand at
evaluation into grouped pitch events; `root_notes`, integer `octave`, and
derived or typed-beat `arp` spacing operate on that owned data.
Those features reach both measured offline audio and the native player.

All seven songs now evaluate as complete owned programs and reach routed
persistent measured audio. This
includes score ducking, bounded filtered-feedback delay, gated reverb,
graph-rate cosine and pink noise, init-rate Karplus–Strong strings, peak
filtering, deterministic per-voice `rand`, previous-onset pitch glide,
graph/event sends, breakpoint gestures, finite mapped-tempo timelines, shared
analysers, compiled transport controls, external-trigger ownership, the
modulated diffusion/FDN hall, and master processing. Send handles remain lexical: a send
used while staging a voice is declared before that voice rather than being
secretly hoisted.

The optional-input base is implemented: declarations have owned lexical
identity, can feed multiple staged graphs, survive compatible persistent edits,
and use silence/shared-default fallbacks for unavailable lanes. The native
player binds the first logical input to a same-rate default input device; the
browser still falls back pending permission/device integration. The shared
analyser graph, transport-pattern control compiler, realtime onset edge,
transport-sampled trigger controls, ordinal-derived degradation and `at_onset`
init bindings are connected. The jamming acceptance feeds a deterministic host
signal through that complete path; real microphone capture uses the same native
processor path. Pattern-varying send levels and arbitrary
feedback graph causality remain separate from the fixed filtered-delay loop
now implemented. A
tempo-map edit or incompatible persistent/layout edit performs an explicit
cycle-zero hard reset. Compatible edits retain the live arena and activate at
the ordinary frontier; changed persistent topology still needs replacement
crossfade, and tempo edits need clock-map reconciliation.

`supersaws.eod` established `ply(...)` event repetition and mini-notation `|`
random-choice alternation as separate pattern operations. Both are now
implemented and query-idempotent; its weighted and polymetric notation remains
parse-guarded in `crates/pattern/tests/mini.rs`.

**Top level** — `tempo`, `play`, `voice`, `send`, `master`, and from session 3
`patch` (persistent lifetime), `run` (activate an autonomous rack), `timeline`
and `at` (finite, through-composed placement).

Declarations are inert. `play(x, notes)` is the same call whether `x` is a
`voice` or a `patch` — the lifetime lives in the declaration, so moving an
instrument between polyphonic and persistent does not touch the score.
`run(patch, span)` is for a rack with no notes to drive it.

There are currently two explicit exceptions to declaration-order inertness.
`at(secs(...), ...)` and `beats(...)` require `tempo(...)` earlier because
they resolve through that map during evaluation, and input-bearing `run(...)`
patches process stems serially in run order. Both are provisional and
load-bearing: the intended replacements are a hoisted program clock and
explicit patch inputs.

`tempo(72)` is sugar for a one-entry tempo map; missing `over` steps, explicit
`over` interpolates.

**Durations** — `bars`, `beats`, `secs`, `ms`. Always explicit; tempo-relative
and absolute are different things and guessing is the classic sequencer bug.

**Pattern transforms**, chained with `>>` — `fast` `slow` `rev` `every` `off`
`sometimes` `degrade` `arp` `shift` `duck` `to`, plus **one setter per declared
parameter**. `cutoff(...)` exists because `supersaw` declares `cutoff`; that is
the whole payoff of `params` being data. `velocity(...)` is the sole implicit
event-amplitude setter; graph and master amplitude use `mul(...)`.

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

**3. `slew(n.hz, n.glide)` needs event-to-event state. Resolved for scheduled
voices.** The scheduler retains the preceding pitch independently per track and
hands it to voice instantiation. The graph lowers this exact spelling to a
finite smoothstep from that pitch to the current `n.hz`; the first onset starts
at its target. Simultaneous chord members share the same preceding pitch, and
stable structural order selects the member remembered for the next onset.
History commits transactionally with the scheduling window. It is deliberately
cleared at an edit boundary because track reconciliation is not implemented;
silently borrowing the pitch of a newly reordered track would be worse than one
unglided onset.

This does not claim oscillator phase continuity. A graph that must preserve
oscillator/filter state across note boundaries remains a persistent patch.
`control_signal(pattern, period)` now supplies the explicit, bounded
transport-pattern driver used by those racks: it compiles one complete numeric
period at evaluation and retains no pattern callback. For other live signals,
`signal >> slew(seconds)` is an ordinary fixed-response follower and does not
consult score history.

**4. `duck(pattern, amount)` is not a per-note parameter. Resolved.** The
scheduler queries the trigger rhythm on the control thread and compiles its
numeric onsets into transport-aligned, allocation-free smoothstep release
segments spanning each trigger event. That envelope multiplies every completed
routed lane of the line, including sends. No Lua callback or pattern query runs
on the audio thread, and tempo projection remains the scheduler's job.

**5. `to(send, x)` is used at two different levels and they are not the same.**
Inside a graph (`glass`, `cowbell`) it is a fixed patch; in a score
(`supersaw`) it is a per-note send level. **Resolved as two operations:** a
graph send taps internal graph channels and its level may itself be a signal;
an event send copies the completed voice channels with a scalar bound at the
onset. Both resolve a lexical bus binding to an opaque `BusId`, and routed
lowering sums them into the same program-wide stem layout. Both forms are
bound in Lua; pattern-varying event-send levels remain separate work.

**6. `arp("up")` needs chords to survive as chords. Resolved and implemented
for literal construction.** `[a3,c4,e4]` and literal voicings become `Group`s,
not plain `Stack`s, and their events carry immutable group key, original member
index and original count. Literal chord groups additionally mark their root as
a domain-neutral primary member. Unrelated coincident layers remain a `Stack`.
`arp` consumes the group and uses its stable indices; explicit spacing clips
slots to the group's existing extent rather than silently extending a timeline
placement.

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
author. Implemented.** `waves.eod` and `synthwave.eod`
fake linear time by gating gains with curve windows over cyclic material, which
works because their material *is* cyclic. `Pattern::Timeline` now supplies
finite, non-repeating material and `transport::TempoMap` supplies step/ramped tempo
conversion. Lua `timeline { at(...) }`, typed durations and mapped seconds
placement own those shared types rather than maintaining a language-only clock.

**13. External triggers are not patterns. Native live boundary implemented.**
`jamming.eod` fires bells from an audio onset detector, and you cannot query the
future of a live stream. `live::ExternalTrigger` deliberately has no query
contract; `TriggerRecorder` captures arrival ordinals and converts recordings
to a finite `Timeline`, recovering queryability and reproducibility. The Lua
binding retains an `onset_detector` control identity in the owned track; the
app detects rising edges, assigns arrival ordinals and schedules the voice
without pattern lookahead. Ordinary setter patterns are sampled at the observed
transport cycle, while `degrade` derives from the arrival ordinal. The detector
exists and is allocation-free. The native host maps the first logical input to
the same-rate default device through a bounded lock-free capture ring; missing
lanes and the browser host retain the explicit silence fallback.

**14. A stateful signal read from several graphs must lower to one node.
Implemented.**
`their_hz` in `jamming.eod` is a pitch tracker referenced by two racks and the
score. Copying that subgraph per graph is not merely wasteful: two trackers can
disagree. But this is **identity preservation, not common-subexpression
elimination** — two separately written, structurally identical trackers must
stay distinct, because wanting two independent detector states is a legitimate
thing to write. The rule is that the *binding* carries the identity: bound once
and referenced twice is one node, written twice is two. No earlier song shared a
stateful signal, which is why this took five songs to surface.

**15. `at_onset` cannot be implicit at the graph/live-signal boundary.
Implemented.** The same tracked pitch feeds the rack
live and the bells frozen-at-strike, eight lines apart in one file. A struck
resonator that kept tracking would slide for its whole ring. It is a typed rate
boundary, `Signal<T> → Init<T>`, and the only legal collapse from a live graph
signal to a voice init value — enforced by the rate checker, not by
documentation. Voice-scoped setters own `InitControlBinding`s, and
`voice.hz(at_onset(signal))` samples literal Hertz without MIDI
canonicalisation. Pattern `Merge` is the separate pattern-layer signal-to-init
boundary: a setter explicitly asks for its RHS to be sampled at each left
onset.

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
`init_random(stream, min, max)` deterministically consumes it. Stateful
`noise()` and `pink()` nodes derive distinct structural streams from the same
seed instead of replaying one identical burst at every onset.

## Rules these songs are written to

- No host-language closure ever reaches a graph. Envelopes are declarative
  curves (`decay(ms(26))`, `curve{...}`, `window(a, b)`), never
  `function(t) ... end`. See the rate hierarchy in `../CLAUDE.md`.
- Randomness is seeded and reproducible. `waves.eod` must give the same nine
  minutes every time it is opened.
- Anything expressible by composition is stdlib and lives in readable source,
  not in Rust. `ring` is the example to imitate.
