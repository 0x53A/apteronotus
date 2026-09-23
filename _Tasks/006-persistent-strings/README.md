# Persistent strings and instrument construction

Design proposal, 2026-09-08. The high-level syntax below is illustrative.
The narrower working PoC described next is implemented.

## Working PoC

The first full song built on it is `songs/iron-in-the-rain.eod`: 68 bars at
128 BPM, six rhythm strings plus one lead and one bass string. Its local
evaluation-time helpers produce pick timelines and compressed transport
controls; no new runtime primitives are required.

`crates/synth/src/string.rs` implements `string_resonator(excitation, hz, mute,
{ min_hz, decay })`, exposed by the Lua graph builder. It has one bounded
fractional delay, mild brightness loss, nominal T60 feedback gain, smoothed pitch
and internal damping. It intentionally does not model actual fret contact,
energy-preserving retuning, bridge coupling or pickup geometry.

`songs/six-strings.eod` uses six separate excitation buses feeding six retained
strings in one persistent patch. Short noise voices deliver sample-scheduled
picks, so repeated picks do not depend on a boolean edge. Existing compiled
transport signals supply fret/bend/mute changes; three live controls supply
amp drive, additional high-string bend and palm pressure. One shared amp follows
the sum. This demonstrates the musical lifetime without a new command API.

Unit tests cover ten-second sustain, repick, mute/release with no resurrection,
bending without new excitation, bounded state through extreme inputs, and
tick/block/reset agreement. Production rendering tests cover the demo and
sample-identical lower strings while upper-string tracks are changed, measured
before the amp. A six-string implementation does not imply the broader gesture
and instrument-construction TODOs below are complete.

Claude review was requested through its installed CLI using
[this architecture brief](POC-REVIEW-PROMPT.md). The CLI refused the request:
“Your organization has disabled Claude subscription access for Claude Code”.
No Claude review was obtained; implementation continued with local verification.

Validation: synth and Lua native unit/integration suites pass; both retained
string production-render tests pass; the app's complete embedded corpus lowers;
Lua checks for `wasm32-unknown-unknown`; synth/Lua Clippy passes with `--no-deps
-- -D warnings` (the vendored VM has unrelated lint warnings). The 32-second
demo plus one-second tail renders at 48 kHz with 30 short excitation voices,
six persistent strings, stereo output, and peak -11.8 dBFS. Listening remains
the user's check of timbre; the automated evidence establishes behavior.

Current direction: a reusable string model with composable excitation, contact,
coupling and observation. Guitar, bass and ukulele are stdlib configurations,
not separate DSP instrument types. Earlier `guitar` examples illustrate an
optional convenience builder, not the required engine abstraction.

## Musical contract

A conventional guitar preset owns six persistent string resonators, ordered low
to high. The underlying string bank has a declared, bounded number of strings;
six is a preset choice, not an engine restriction. Pattern
events describe gestures addressed to those strings; event duration does not
release them. An absent event leaves the string ringing. The runtime retains
actual vibration history, while pattern queries remain pure.

- Fret changes a string's effective length/pitch. The first implementation
  must declare how abrupt fretting damps old vibration and smooths delay changes;
  a naive delay jump is not an adequate physical model.
- Pick adds a short excitation to the existing vibration. Repeated identical
  picks must still trigger. It does not allocate another sustained voice or
  silently erase the old vibration.
- Mute increases damping inside the resonator, with a short smooth transition.
  Closing only an output VCA would hide energy that returns when reopened.
- A chord helper emits fret/pick/mute gestures per string. A numeric fret
  means fret/pick, including `0` for an open string; `"x"` means mute;
  `"-"` means leave completely untouched: no fret change, excitation, damping
  change, or reset. Require six explicit slots rather than Lua array holes,
  so string identities never shift (N explicit slots for an N-string bank).
  Slots run from low E to high E (not
  conventional guitar string numbering). Strum direction and spread set
  gesture times; `spread` is the interval between picked strings, and skipped
  strings consume no pick interval. Explicit mutes occur at gesture onset.
  A retained same-fret setting does not itself cause fretting damping: picking
  that fret again only re-excites the string.

The shared amplifier processes the sum of all strings. Saturation keeps a
decaying string apparently loud until its input drops out of saturation; it
does not replenish vibration. Actual amplifier-to-string feedback is separate
future work, with explicit stability and gain constraints.

## Illustrative score

```lua
-- PROPOSED API: this does not evaluate in Apteronotus today.
local g = guitar {
  tuning = { "e2", "a2", "d3", "g3", "b3", "e4" },
  decay = secs(20), -- nominal time to lose 60 dB, not automatic note-off
}
run(g) -- own the six strings for the performance
play(g, timeline {
  at(bars(0), g:strum { frets = {0, 2, 2, 0, 0, 0}, spread = ms(12) }),
  at(bars(2), g:strum { frets = {"-", "-", "-", 0, 0, 0}, spread = ms(12) }),
  at(bars(4), g:strum { frets = {"x", 3, 2, 0, 1, 0}, spread = ms(12) }),
  at(bars(8), g:mute()),
})
```

The first chord rings during the empty bars. At bar two only the three
highest-pitched strings are picked again; the three lowest retain their pitch,
damping, and vibration with no commands sent to them. At bar four the gesture
explicitly mutes the low E and frets/picks the other five retained strings.
Methods build owned gesture data during evaluation; none is a runtime Lua
callback. Amp wiring is
omitted here. Exact builder names and timeline payload representation remain
to be designed against the existing APIs.

## Revision: two hands, continuous expression, and impossible instruments

The chord shorthand must not become the primitive interface. A player can
change a fingering without picking, pick without changing a fingering, and
bend an already ringing note without either. Separate three things:

1. Persistent string state: vibration, current fret/contact, damping settings.
2. Discrete gestures: pick, hammer, pull, touch, release contact.
3. Continuous controls: bend, vibrato, palm pressure, excitation/pickup position.

The controls describe causes of sound, rather than applying a generic envelope
to every new pitch. Conventional techniques motivate the following contracts:

| Technique | Proposed behavior / modelling requirement |
| --- | --- |
| Bend and release | Per-string signed semitone trajectory on top of the fret pitch, without another pick. Variable depth and duration; allow pre-bending before a pick. |
| Vibrato | A separate additive pitch lane with variable depth/rate and explicit phase origin. Starting a bend must not replace vibrato. |
| Slide | Move the fret/base pitch over time while retaining vibration. Contact losses and optional scrape excitation distinguish a slide from a clean electronic glide. |
| Hammer-on / tapping | Change contact and add a small, separately parameterized contact excitation; no synthetic pick attack. |
| Pull-off | Release to an explicitly supplied lower fret/open string, with a lateral excitation. No hidden fingering stack in the first version. |
| Palm mute | Continuous pressure/contact-position control over loss and brightness, which can remain engaged during repeated picking. |
| Dead note / choke | Pick against strong damping for a short percussive sound; choke an existing note through loss, not merely output gain. |
| Finger/alternate picking | Per-pick strength, direction, position, and hardness. These affect excitation spectrum as well as amplitude. |
| Natural/artificial harmonics | Explicit light contact at a string position, plus excitation; requires an internal contact model, not just transposing the fundamental. Deferred DSP feature. |
| Whammy bar | Shared pitch lane combined with independent string bends. Real bridge coupling and tension-dependent timbre are further physical refinements. |
| Volume swell / pickup switch | Output-side controls distinct from string energy; lowering volume intentionally leaves vibration alive. |

Technique references: Fender on [hammer-ons and
pull-offs](https://www.fender.com/articles/techniques/master-hammer-ons-and-pull-offs),
[bends, vibrato and solo
articulation](https://www.fender.com/articles/techniques/how-to-guitar-solo), and
[palm muting](https://www.fender.com//articles/techniques/3-keys-to-ace-your-palm-muting).
The DSP contracts above are our proposed approximations, not claims of physical
equivalence. A variable delay alone will not reproduce every technique.

### Proposed lower-level score vocabulary

```lua
-- Illustrative only. Each method returns owned scheduled data.
-- String slots remain low-to-high: slot 6 is the highest string here.
g:fret { string = 6, fret = 7 }       -- no pick
g:pick { strings = {6}, strength = 0.8, position = 0.18 }
g:bend { string = 6, to = 2, over = ms(180) } -- +2 semitones
g:pick { strings = {6}, strength = 0.6 }      -- pick while still bent
g:bend { string = 6, to = 0, over = ms(300) } -- release bend
g:slide { string = 6, to = 10, over = ms(220) }
g:hammer { string = 6, fret = 12, strength = 0.35 }
g:pull { string = 6, to = 10, strength = 0.25 }
g:palm { strings = {1, 2, 3}, pressure = 0.65, over = ms(30) }
g:pick { strings = {4, 5, 6}, spread = ms(12) } -- current frets/bends
```

These expressions belong at explicit timeline positions, just like the earlier
strums; their textual sequence alone does not specify timing. `pick` makes
selection independent of fret assignment. `strum { frets = ... }` remains a
convenience that lowers into addressed operations. Setting an unchanged fret
does not reapply contact losses. Picking does not implicitly reset bend,
vibrato, palm pressure, or any other modifier.

Mute semantics need to be explicit: `mute`/`"x"` denotes a bounded choke
gesture that dissipates existing vibration and then releases its added contact;
it preserves the independently held palm-pressure setting. Persistent contact
uses the palm/touch controls. Thus a later ordinary pick can sound again, but
releasing a mute without picking must not resurrect the old note.

### Control clocks and interruption

Effective frequency starts from tuning plus fret, then adds bend, vibrato and
shared whammy offsets in semitones before conversion to Hz. An open string can
be bent electronically; a realistic preset may restrict available gestures.
Wide negative bends and fractional frets are valid within declared pitch bounds.
Do not promise physically accurate tension/length coupling from this formula.

`bend(to, over)` starts at the lane's value at that scheduled instant, moves to
an absolute offset from the current base pitch, and holds its endpoint. A new
bend replaces that string's bend trajectory continuously, including when it
interrupts an unfinished bend. It does not stack another permanent offset.
Fretting changes base pitch and preserves bend; a physical convenience helper
may explicitly emit both a new fret and a bend release.

General trajectories should reuse the owned curve basis, with an explicit
gesture-relative seconds or beat clock and an explicit final held value.
Recurring transport modulation keeps absolute transport phase. No per-pick
restart of vibrato unless requested. Existing note-clock curves cannot simply
be attached unchanged to a persistent string: this placement needs lowering.
Resolve beat timing through the tempo map, not the tempo at onset alone.

Use one writer per named control lane, with explicit replacement, plus named
additive lanes for intentional superposition. Simultaneous commands have stable
provenance/arrival ordering; chord helpers emit contact/control changes before
their picks. Query-window splitting must not change ordering or random seeds.
Mid-ramp replacement uses the preceding compiled trajectory's value at that
coordinate, not a UI readback or audio-thread Lua call.

### Beyond a physical guitar

These are proposed creative extensions, not capabilities currently available:

| Instrument idea | What it needs |
| --- | --- |
| A chord whose strings bend in six different directions | Independent bounded pitch lanes, including fractional and negative offsets; no new string allocation. |
| A 24-string guitar played by a hundred fingers | A staged 24-string bank and dense addressed gestures, within publication budgets; fingering reach is optional policy. |
| A string turning from nylon-like to bell-like while ringing | Modulated loss and dispersion. Dispersion needs an explicit bounded stateful implementation, not a timbre label. |
| A pickup sweeping through a ringing string | Movable observation taps; extend the waveguide representation if a single loop cannot expose the required position. |
| A string played by a drum machine, another string, or a continuous driver | An explicit audio excitation input alongside pick gestures. Bow friction is a further nonlinear model, not just a sustained pick. |
| A struck string that awakens other strings | Declared bounded coupling paths; feedback delay and stability contracts, separate from merely mixing outputs. |
| A string that holds its energy indefinitely | An explicit sustain driver or carefully defined lossless mode, with finite-state energy control. Never implement as unrestricted loop gain above one. |
| Bass strings in one amp and treble strings in another | Per-string output taps and normal persistent routing; the default remains one shared amp. |

The interesting abstraction is an addressable resonator bank with gestures,
controls, excitation inputs and output taps. A guitar stdlib supplies six-string
tuning, fingering and techniques. It must remain possible to build a different
instrument from the same nodes without adopting guitar-specific hand limits.
Do not add one monolithic Rust operation per named technique when existing
gestures and controls can express it.

Explicit coupling changes the meaning of “untouched”: no command goes to that
string, but another string may physically excite it. The sample-identical
untouched-string guarantee applies to the default uncoupled bank. Shared output
distortion alone never feeds energy back into the strings.

### Revised implementation order

First earn one string's sound and gesture semantics: pick, variable bend,
release, repick while bent, continuous palm damping, and choke. Then six
independent strings, partial strumming, slides/contact excitation and a shared
amp. Only then extend toward harmonics, movable pickups, dispersion, coupling,
and continuous excitation. None of the exploratory extensions should block the
basic expressive instrument or silently become an expensive default.

## Engine boundary

### Revision: a string is the reusable unit

Instrument names must not select independent Rust synthesis engines. Build
instruments from these layers, preserving the same persistent identities and
gesture semantics established above:

| Layer | Responsibility | Initial implementation / extension boundary |
| --- | --- | --- |
| String | Retain travelling vibration, pitch/length, frequency-dependent loss, optional stiffness/dispersion. | New bounded retunable resonator; current instantiation-bound `Pluck` is insufficient. |
| Excitation | Inject energy at a position: pick, strike, external signal. | Shaped noise/impulse and envelopes can compose approximate one-way excitation from existing nodes. Interaction with a hammer or bow needs a contact solver. |
| Contact/boundary | Fret, slide, damp, touch a harmonic node, collide with a bridge or preparation. | Stateful interaction inside the string; cannot generally be reproduced by filtering the final output. |
| Coupling/body | Transfer vibration between strings and bridge/body modes. | Existing filters can approximate one-way body coloration. Physical two-way coupling needs an explicit bounded junction/network. |
| Observation | Read string motion at a pickup or radiate body motion, then mix/process it. | Output taps and existing buses/effects; position-sensitive observation requires appropriate internal wave access. |
| Playing interface | Tuning, courses, fret maps, strums, key/pedal mappings and named techniques. | Owned data and stdlib compositions; no instrument names in the pattern algebra. |

For example, a bass configuration uses fewer/lower strings and different loss,
dispersion, excitation and pickup parameters than a guitar. A ukulele changes
tuning, string properties and body response. These are model configurations;
merely changing pitch or a `material` label cannot promise their real timbres.
Do not require tuning to increase with slot number: re-entrant tunings and
arbitrary layouts are valid. The low-to-high convention belongs only to the
earlier guitar example. Identity comes from handles, never pitch sorting.

Avoid a global Lua `string(...)` builder because `string` is already the text
library. Illustrative generic construction could instead use `strings.new` for
one persistent string and `strings.bank` for a group. Numeric frequencies should
be valid without equal-tempered frets. Instrument helpers can translate fret
numbers or named pitches into these generic controls.

```lua
-- DESIGN SKETCH ONLY; method names and graph ownership remain to be finalized.
local low = strings.new { tuning = "e2", decay = secs(20) }
local high = strings.new { tuning = "e4", decay = secs(12) }
local pair = strings.bank { low, high }
-- A bank groups existing handles; it neither copies nor resets their state.
-- The same string cannot accidentally be instantiated twice through aliases.
-- Gestures select handles; guitar fret-table syntax is optional sugar.
```

Each string needs declared pitch/state bounds at publication; defaults in a
helper may supply them. A course groups multiple physical strings for gestures
without merging their histories. Played, drone and sympathetic groups are
roles/routes, not distinct string node types. Numeric addressing is convenient
for tablature; explicit handles remain stable through selection and grouping.

### Instruments that expose useful missing capabilities

The mechanisms in the second column come from the cited instrument sources;
the third column is our proposed engineering decomposition, not a verified
recipe for an acoustically faithful replica.

| Instrument | Distinctive mechanism | What our construction needs |
| --- | --- | --- |
| Koto | Individually bridged strings; left-hand pressure beyond the bridges changes pitch. | Per-string tuning and continuous tension/bend gestures, pluck position and body response. More faithful pressure/bridge interaction would require multiple string segments. |
| Sitar | Played strings plus sympathetic strings; broad bridge contact contributes to its characteristic sound. | Bendable strings, a separately tuned sympathetic bank, bridge/body coupling and nonlinear distributed bridge contact. Output distortion alone is not the bridge model. |
| Viola d'amore | Bowed strings plus a separate sympathetic set. | Bow friction contact driven by speed/pressure/position; unpicked strings excited through coupling. |
| Hurdy-gurdy | A cranked wheel excites strings while keys select pitches. | A shared wheel-motion control driving friction contacts on retained strings and keyed stopping. Detailed buzzing-bridge behavior is further contact-model work. |
| Yangqin | Hammered dulcimer. | Strike gestures, hammer hardness/velocity/position, grouped strings where appropriate and bridge/body response; contact dynamics for a more faithful attack. |
| Piano | Hammer excitation and audibly stiff, inharmonic strings. | Nonlinear hammer contact, dispersion, grouped strings, individual dampers plus shared pedal gestures and coupling. A useful demanding test beyond plucked instruments. |

Sources:

- [Asian Art Museum: koto](https://searchcollection.asianart.org/objects/9857/koto)
  and [Met: movable koto bridges](https://www.metmuseum.org/art/collection/search/502981).
- [Grinnell College instrument collection: sitar](https://omeka-s.grinnell.edu/s/MusicalInstruments/item/631).
- [Met: viola d'amore](https://blog.metmuseum.org/guitarheroes/viola-damore-18th-century/).
- [Met: hurdy-gurdy](https://www.metmuseum.org/art/collection/search/902422).
- [North America Chinese Youth Orchestra: yangqin](https://www.nacyo.org/instruments/yangqin).
- Julius O. Smith: [bowed strings](https://dsprelated.com/freebooks/pasp/Bowed_Strings.html),
  [piano synthesis](https://dsprelated.com/freebooks/pasp/Piano_Synthesis.html),
  and [hammer interaction](https://dsprelated.com/freebooks/pasp/String_Excitation.html).

Do not assume every excitation can be a one-way audio cable. Bow force depends
on relative bow/string velocity, and hammer force on contact deformation;
both alter the string that determines the next force. Reserve a typed contact
interface with explicit physical quantities and solver ownership. A first
implementation may use specialized bounded Rust contact nodes rather than
allowing arbitrary zero-delay graph feedback. The user-facing composition can
still be a string with a bow/hammer contact.

Likewise, a cheap single-loop string may serve plucks but cannot promise arbitrary
interior contacts, independent bridge segments, or displacement/velocity taps.
Document each model's supported capabilities; reject unsupported constructions
instead of silently substituting output effects. Sharing the string abstraction
does not require every quality level to use identical internal algorithms.

### Impossible instruments from the same construction

- Bow a bass-like string while intermittently hammering it; let both contacts
  act on its one vibration state instead of mixing independent instrument voices.
- Build a koto-like bank whose strings each follow different bend curves, with
  no hand-reach limit, feeding a shared distorted amp.
- Let a small plucked bank excite a much larger microtonal sympathetic bank;
  keep coupling sparse and budgeted rather than allocating a dense matrix by default.
- Morph loss and dispersion during a ringing note, moving from string-like to
  bell-like spectra; scan its observation position independently.
- Use synthetic percussion as external excitation, or a regulated sustainer
  to keep selected strings ringing while others decay naturally.

These combinations remain sample-free. Their cost is set by declared strings,
contacts, junctions and gestures, not by an instrument name. Changing topology
still follows normal staged graph publication; modifiers do not authorize
unbounded runtime growth or an implicit new string whenever pitch changes.

Reuse persistent patch lifetime and routing rather than a new unbounded pool
of mutable note voices. A string is a stateful Rust DSP node, likely an extended
Karplus–Strong/digital waveguide loop: bounded interpolating delay, loss filter,
and short generated excitation. Pitch bounds determine allocated delay memory;
decay in seconds must remain comparable across pitches. Frequency-dependent
loss lets upper harmonics die faster. Pick/pickup position can follow later.

Reference: Julius O. Smith, [Electric Guitars, Physical Audio Signal
Processing](https://dsprelated.com/freebooks/pasp/Electric_Guitars.html).

Existing `Pluck` has instantiation-bound pitch/damping. Existing patch controls
are scalar number/note/gate handles; `play(patch, ...)` requires a finite pattern
and creates a run over its extent. Neither supplies the contract above. A
retained guitar must outlive its gesture sequence and keep sounding after its
last pick, until explicitly muted/stopped.

Scheduled gestures need bounded, timestamped, addressed delivery with defined
same-time ordering. A shared boolean or changed scalar cannot represent two
successive identical picks reliably. Native playback and offline rendering must
consume the same owned commands at the same sample offsets. Excitation seeds
derive from event provenance. Source spans remain attached for highlighting.
No interpreter, allocation, or unbounded command work belongs on the audio
thread. Publication must budget both string state and gesture density.

## TODO / acceptance slices

- [ ] Implement one retained string with bounded pitch, calibrated loss,
  excitation, and physical damping. Define fret transition behavior.
- [ ] Add addressed scheduled gestures to persistent execution. Specify
  repeated picks, simultaneous gesture ordering, stop/drain, and source spans.
- [ ] Add a six-string stdlib builder and fingering/strum helpers. Keep musical
  mapping outside the domain-free pattern crate; share one downstream amp.
- [ ] Verify one pick rings beyond ten seconds, repeated same-pitch picks
  re-excite it, and mute removes energy instead of merely hiding it.
- [ ] Verify fret changes do not leave an independent old-note voice; untouched
  strings survive chord changes; rapid gestures do not grow resonator count.
- [ ] Verify all-six then upper-three strumming leaves the lower three string
  outputs sample-identical to a run without the second strum, measured before
  the shared amp (whose nonlinear output naturally changes). Validate six-slot
  arity and distinguish open (`0`), mute (`"x"`), and untouched (`"-"`).
- [ ] Verify deterministic offline/native scheduling, bounded callback work,
  and compatible-edit retention of string history and endpoint identity.
- [ ] Define sounding-token semantics separately from gesture duration: a pick
  may remain audible long after its event. Do not invent exact audibility from
  an expired pattern slot or an unbounded audio-thread analyser.
- [ ] Implement separate base-pitch, bend and vibrato lanes; test prebend,
  repicking during a held bend, interrupted ramps, independent string bends,
  held endpoints, tempo-aware beat curves, and phase across compatible edits.
- [ ] Specify and validate pitch/modulation bounds and interpolation behavior;
  rapid or extreme modulation must not allocate, alias unchecked, or destabilize
  the resonator. Unsupported ranges/rates need honest diagnostics or documented
  limiting rather than an implicit physical-realism promise.
- [ ] Implement continuous palm damping and separate transient choke; test
  mute release without repick, repick after choke, and palm pressure persisting
  across picks. Add slide/hammer/pull excitation as separately measured slices.
- [ ] Keep harmonics/contact, dispersion, movable pickups, continuous excitation,
  coupling and regulated sustain as explicit later primitives/contracts. Budget
  each one's state, processing and warmup rather than treating them as free knobs.
- [ ] Build guitar/bass/ukulele as configurations of generic strings; support
  stable handles, arbitrary/re-entrant tuning, selection groups and multi-string
  courses without copying state. Avoid shadowing Lua's `string` library.
- [ ] Extract the working song's local fret/strum/control-lane helpers into a
  reusable stdlib once the addressed gesture API is settled; preserve source
  attribution for frets and bends instead of highlighting only helper pick tokens.
- [ ] Specify one-way excitation versus two-way contact ports (including signal
  units and solver ownership). Prototype bow and hammer interaction as later
  bounded nodes, not generic unrestricted graph cycles.
- [ ] Specify body coloration versus physical coupling and supported interior
  contacts per model. Test an unpicked sympathetic string gaining energy only
  when an explicit coupling path is present.
