# The songs are the specification

Written before the runtime, on purpose. These four are the target: the
implementation is finished when they play, and any feature not needed by one of
them is not needed yet.

| song | what it is there to stress |
|---|---|
| `synthwave.eod` | polyphony, sends, sidechain, chords into arpeggios, curve automation spanning 64 bars, gated reverb |
| `waves.eod` | continuous signals steering everything, multi-second envelopes, sparse reproducible randomness, a voice with no oscillator in it |
| `techno.eod` | rigid grid, a 32-bar riser that must stay coherent, euclidean rhythms, ducking deep enough to be composition |
| `poles.eod` | δ at audio rate, percussion as pole placement, the stdlib-vs-primitive split, voices whose entire parameter set is two numbers |

## Vocabulary the four of them use

**Top level** — `tempo`, `play`, `voice`, `send`, `master`.

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

**9. `+` and `&` are used interchangeably for parallel banks** in `poles.eod`
and they should not be. In fundsp both feed one input to several nodes and sum
the outputs, `+` additionally requiring matching arity. Pick one for the songs
and make the other an error or an alias.

**10. `key("f#", "minor")` in `synthwave.eod` currently does nothing.** Either
define it — scale-degree notation resolving against it — or delete the line.
Leaving it as decoration is the worst option.

## Rules these songs are written to

- No host-language closure ever reaches a graph. Envelopes are declarative
  curves (`decay(ms(26))`, `curve{...}`, `window(a, b)`), never
  `function(t) ... end`. See the rate hierarchy in `../CLAUDE.md`.
- Randomness is seeded and reproducible. `waves.eod` must give the same nine
  minutes every time it is opened.
- Anything expressible by composition is stdlib and lives in readable source,
  not in Rust. `ring` is the example to imitate.
