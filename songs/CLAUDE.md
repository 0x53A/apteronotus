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

The first four are cyclic and fix every parameter at onset. The last two exist
because that turned out to be a property of the author rather than of music —
see session 3 in `../_Tasks/000-architecture/LOG.md`.

## Vocabulary the four of them use

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

**Signals and curves are one type.** `sine(0.22)` is an LFO inside a graph and
automation inside a score, with no conversion. Constructors: `sine` `cosine`
`perlin` `rand` `scale`, and the declarative basis `step` `line` `decay`
`window` `curve`. They take arithmetic (`0.95 * main`, `1 - outro`) and `shift`.

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

**1. Events must carry a parameter map.** `>> gain(0.8) >> pan("-0.2 0.2")`
needs a control map per event; `crates/pattern` currently carries a single
`Value`. This is the next thing to build: `Value::Map`, a `Named` node lifting
bare values into a one-key map, and a `Merge` node taking structure from the
left. Everything else in the score notation is blocked on it.

**2. The merge sampling rule is a deliberate deviation from Tidal.** Tidal
emits one output event per right-hand event, which multiplies when the right
side is faster. Proposal: sample the right side with a **zero-width query at
the left event's onset** and take the first value. One voice per note, no
combinatorial blowup, and continuous signals fall out correctly — which suits a
model where a voice is instantiated at onset with its parameters baked in.
Ratcheting is then something you write in the notation instead.

**3. `slew(n.hz, n.glide)` in `techno.eod` has no way to work.** Portamento
needs the *previous* note's pitch, and a voice instantiated per note has never
heard of one. Either the scheduler passes `n.prev_hz` and `n.legato`, or an
instrument may declare itself monophonic and get one persistent graph with
`Shared` pitch. The second is more honest — a 303 *is* monophonic — but it is a
second voice model. **Unresolved.**

**4. `duck(pattern, amount)` is not a per-note parameter.** It modulates a
whole line's output from a rhythm, which means a played line needs its own bus
and that **a pattern must be convertible into a control signal** — sample and
hold, with a shaped release. New concept, needed by two of the four songs.

**5. `to(send, x)` is used at two different levels and they are not the same.**
Inside a graph (`glass`, `cowbell`) it is a fixed patch; in a score
(`supersaw`) it is a per-note send level. Either allow both and define them
separately, or force sends to be score-level only. **Unresolved.**

**6. `arp("up")` needs chords to survive as chords.** `[a3,c4,e4]` becomes a
`Stack`, and a query returns its events unordered. The arpeggiator has to
recover "these three share a `whole`" and order them by pitch. Groupable, but
it means `arp` is not a plain pattern transform — it needs the events, not the
tree.

**7. Every voice has implicit parameters it never declares.** `n.pan` is used
in three songs and declared in none; `n.hz`, `n.vel`, `n.dur` likewise. There
is a fixed implicit set — decide it once and document it, rather than letting
it accrete.

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

**11. An event must be able to carry a signal, not just a number.** `n.pressure`
and `n.bend` in `neon.eod` are control-rate for the note's whole life, and
`ControlMap` currently holds scalars. This is the same blocker as #1 and lands
in the same decision — `Value` needs a `Curve` case with its clock attached.

**11a. But that is not enough for polyphonic expression, and no merge ever will
be.** The merge rule samples at the *left event's onset*, and a struck chord's
notes share one onset, so they receive one value. This is not specific to
curves: **no merged parameter can vary across simultaneous events** — pan,
velocity, detune and send level included. A strummed chord works, a struck one
does not. Per-voice variation is therefore built in `crates/music`, where a
voicing still knows how many notes it made and in what order:
`voicing(…):each(function(note, index, count) … end)`, a build-time closure that
disappears like `every`'s. Neither song exercises it yet.

**11b. A `Stack` should know whether it is a chord.** #6 and #11a are one gap:
the query layer cannot address members of a simultaneity because a `Stack`
records no group identity or member order. Nothing currently stops `arp`
arpeggiating across `stack(bassline, melody)` — two lines that coincide are not
a chord, and `[a3,c4,e4]` is. Record identity where it is known: in the voicing
builder, and in the parser, which sees the syntactic group and discards it.

**12. The first four songs were all cyclic, and that was an accident of the
author.** `waves.eod` and `synthwave.eod` fake linear time by gating gains with
curve windows over cyclic material, which works because their material *is*
cyclic. A film-score cue's is not. The query model is fine; the constructor set
was the narrow part.

**13. External triggers are not patterns.** `jamming.eod` fires bells from an
audio onset detector, and you cannot query the future of a live stream. Anything
that generates events from outside can only schedule at now-plus-latency, so it
must not be typed as an ordinary pattern. Recording one turns it into a finite
`timeline`, which is also what makes such a piece reproducible — one conversion,
both problems.

**14. A stateful signal read from several graphs must lower to one node.**
`their_hz` in `jamming.eod` is a pitch tracker referenced by two racks and the
score. Copying that subgraph per graph is not merely wasteful: two trackers can
disagree. But this is **identity preservation, not common-subexpression
elimination** — two separately written, structurally identical trackers must
stay distinct, because wanting two independent detector states is a legitimate
thing to write. The rule is that the *binding* carries the identity: bound once
and referenced twice is one node, written twice is two. No earlier song shared a
stateful signal, which is why this took five songs to surface.

**15. `at_onset` cannot be implicit.** The same tracked pitch feeds the rack
live and the bells frozen-at-strike, eight lines apart in one file. A struck
resonator that kept tracking would slide for its whole ring. It is a typed rate
boundary, `Signal<T> → Init<T>`, and the only legal collapse from signal to
init — enforced by the rate checker, not by documentation.

**16. Anything a voice seeds from must be derived, never counted.**
`neon.eod`'s `init_rand(n.id, …)` reproduces analogue component tolerance as
deterministic per-voice drift. A runtime counter for `n.id` would change with
query chunking, with offline versus live scheduling, and — worst — with the
order a `Stack`'s members come back in, which #6 says is unspecified. So the
implicit contract's voice identity splits: a derived `event_seed` for anything
reproducible, a `voice_handle` counter for addressing a sounding voice and
nothing else. `waves.eod` is specified as giving the same nine minutes every
time it is opened; this is what that costs.

## Rules these songs are written to

- No host-language closure ever reaches a graph. Envelopes are declarative
  curves (`decay(ms(26))`, `curve{...}`, `window(a, b)`), never
  `function(t) ... end`. See the rate hierarchy in `../CLAUDE.md`.
- Randomness is seeded and reproducible. `waves.eod` must give the same nine
  minutes every time it is opened.
- Anything expressible by composition is stdlib and lives in readable source,
  not in Rust. `ring` is the example to imitate.
