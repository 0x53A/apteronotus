# Audio debugging: synthwave/techno retrospective

This records the July 2026 investigation into two reports:

- `synthwave.eod` was much busier than intended;
- both `synthwave.eod` and `techno.eod` had a fast, high chirping quality.

`../003-composing-by-measurement/README.md` is the sibling record for
*authoring* rather than diagnosing, and corrects one conclusion below: the
NumPy blocker is solved by `uv` with inline script metadata.

It is partly a bug record and partly a tooling design note. The important
lesson is that a reproducible renderer is necessary but not sufficient:
diagnosis also needs source-preserving track isolation, structured audio
measurements, and compact visualizations.

## Outcome

There were three different problems layered together.

1. Four synthwave graphs did not apply `n.velocity`, although the score used
   velocity curves to arrange their entrances. Zero-velocity kick, snare and
   supersaw events still made full-level sound, and the pad ignored its intended
   `0.22` level.
2. Resonant filter envelopes reached the upper audible band repeatedly. The
   original techno acid expression could request a cutoff of
   `3400 * (1 + 6) = 23.8 kHz` on accented notes, at resonance `0.93`.
3. The engine instantiated every `noise()` and `pink()` node with the same
   fundsp seed. Every short kick click, hat and clap therefore restarted the
   exact same waveform. Repetition made a noise transient acquire a stable,
   chirp-like identity. This was the implementation bug that the first two
   score-editing passes could not remove.

The engine fix derives noise streams from event provenance, structural node
index and noise kind. The same event remains reproducible sample for sample;
different events no longer replay one identical burst. A regression test pins
both halves of that contract.

The timing implementation was audited independently and was not the problem.
At 120 BPM and 48 kHz, `x*4` produces impulses at samples `0`, `24000`, `48000`
and `72000`: four quarter-note beats in one two-second 4/4 cycle.

## Investigation sequence

### 1. Read the score before rendering

`rg`, `sed` and `nl` were used to inspect the two songs and locate their voice
graphs and score transforms.

The text immediately suggested likely contributors:

- synthwave: seven saws, `arp("up") >> fast(4)`, high sends, bright hats and
  multiple velocity-based arrangement curves;
- techno: `fast(2)`, periodic additional `fast(2)`, an 11-of-16 hat pattern,
  and a resonant acid filter whose cutoff and resonance rose for 32 bars.

Reading also exposed the missing `n.velocity` multipliers, but did not establish
which bright source the listener perceived as a chirp.

### 2. Render through the production path

The existing offline renderer was the essential tool:

```sh
cargo build -p apteronotus-render
target/debug/apteronotus-render songs/synthwave.eod \
  --seconds 48 --tail 3 --stems -o /tmp/synthwave-stems.wav
target/debug/apteronotus-render songs/techno.eod \
  --seconds 58 --tail 3 --stems -o /tmp/techno-stems.wav
```

Long enough windows were chosen to reach synthwave's bar-16 entrance and
techno's bar-32 drop. Float WAV preserved overloads rather than flattening
them. Neither render clipped.

This was useful, but rendering whole arrangements first was more expensive than
necessary. A program-aware analysis command should be able to render a named
bar range and selected tracks directly.

### 3. Measure before looking

SoX supplied peak, RMS, DC and band-limited RMS without requiring a Python
scientific stack:

```sh
sox mix.wav -n trim 37 11 remix 1,2 stats
sox mix.wav -n trim 37 11 remix 1,2 sinc 8000-20000 stats
```

Measurements established that:

- synthwave developed a roughly `+0.23` full-scale DC offset when the wet
  supersaw entered;
- techno's above-8-kHz energy rose by roughly 13 dB across the build;
- removing the acid line dropped late above-8-kHz energy by roughly 13 dB.

The DC measurement was more diagnostic than a waveform screenshot would have
been. Text metrics should therefore be the default output of future tooling,
with an image requested only when temporal/frequency shape is still ambiguous.

### 4. Generate spectrograms

SoX generated time/frequency overviews:

```sh
sox mix.wav -n spectrogram -x 1600 -Y 900 -z 100 -o spectrum.png
```

They made the repeated descending filter sweeps visible and showed full-band
vertical transients at rapid onsets.

This step wasted substantial model context. The synthwave stem WAV had six
channels but only the first stereo pair was active after persistent processing;
SoX still reserved panels for all six. Four of six panels were black. Techno
reserved four panels for two active channels. The PNGs therefore spent
approximately 50–67% of their area on known-empty data, then were inspected at
original resolution.

The safe optimization is to remove **silent channels**, not to crop the
frequency axis automatically. An automatic frequency crop could have hidden
the very upper-band problem under investigation.

`tools/analyze-audio.sh` now performs the safe version:

```sh
tools/analyze-audio.sh /tmp/synthwave-stems.wav \
  --start 37 --duration 11 \
  --band 3000-8000 --band 8000-20000 \
  --spectrogram /tmp/synthwave-compact.png
```

It prints a compact tabular channel summary, selects only channels above a
configurable RMS floor, reports requested bands, and sizes the spectrogram from
the number of active channels. On the original six-channel synthwave stem it
selected channels 1 and 2 and produced a `1344 × 357` PNG instead of the
`1768 × 900` first attempt: about 70% fewer pixels, with no four empty channel
panels.

### 5. Isolate sources

Temporary source variants were streamed to the renderer with Bash process
substitution and `sed` line deletion. This separated:

- synthwave with/without supersaw;
- techno acid only/without acid.

It worked, but it is not an acceptable durable method:

- line-number deletion breaks as soon as a song is edited;
- deleting earlier text changes injected source offsets;
- source offsets participate in deterministic degradation/sometimes seeds, so
  an isolated render may not select exactly the same randomized events as the
  full render;
- it cannot produce a trustworthy manifest mapping output lanes back to score
  tracks.

The renderer needs source-preserving `--solo-track` and `--mute-track` options,
using stable program track indices or future track names after evaluation.

### 6. Edit the scores and compare reproducibly

The first score pass:

- wired synthwave velocity into the affected graphs;
- added `dcblock()` to the supersaw;
- lowered extreme cutoff/resonance ranges.

That removed the DC offset (`+0.23` to approximately `+0.001`) and reduced
continuous mix density, but listener feedback said the result was still quick
and chirpy.

The second pass reduced authored density:

- synthwave hats `x*8 → x*4`, arp `fast(4) → fast(2)`, fewer saws and quieter
  sends;
- techno `146 → 136` BPM, removed doubled acid/hat bursts, and flattened the
  acid sweep.

Voice counts over comparable windows fell from about `609 → 392` for
synthwave and `1248 → 872` for techno. This addressed real score density, but
still did not explain why noise transients had a stable pitch-like character.

### 7. Audit engine invariants after listener feedback

The user explicitly suggested an implementation bug. The audit followed the
whole path:

- `tempo()` and `bars()` binding;
- cycle-to-seconds projection;
- `Timecat`, Euclidean and `Fast` queries;
- scheduler onset placement and gate length;
- ADSR note clock;
- graph filter input order;
- fundsp Moog and noise implementations.

The timing path agreed with its tests and with a new sample-exact acceptance
test. The filter input order was correct.

Reading fundsp's `Noise` implementation exposed the defect: its generator
resets from a topology-derived hash unless explicitly seeded. Every
Apteronotus voice instantiates the same topology, so every onset restarted the
same noise sequence. A focused regression now requires two different
`Note.seed` values to render different noise while the same seed remains
sample-identical; without explicit event seeding, both notes followed the same
topology-derived reset path.

The lowering path now explicitly seeds `noise()` and the noise stage inside
`pink()` from:

```text
hash(event provenance, structural graph node index, noise stream kind)
```

The test requires same-seed equality and different-seed inequality. Two
complete renders of the same techno source were also compared with `cmp` and
remained byte-identical.

## Tools used

| tool | purpose | value | limitation encountered |
|---|---|---|---|
| `rg`, `sed`, `nl` | source navigation | fast identification of graphs and score transforms | `sed` line deletion is not safe track isolation |
| `apteronotus-render` | production-path offline audio | exact live/offline parity and reproducible WAVs | reports only total voices; cannot solo tracks or name lanes |
| SoX `stats` | peak/RMS/DC measurements | compact and highly diagnostic | output needed repeated manual parsing |
| SoX `sinc` | band-limited RMS | quantified the upper-band rise | bands were chosen ad hoc |
| SoX `spectrogram` | time/frequency shape | exposed sweeps and repeated transients | emitted large blank channel panels |
| image inspection | read spectrograms | useful after text metrics narrowed the question | original-resolution PNGs consumed too much context |
| Bash process substitution | temporary isolation | avoided repository copies | changed source offsets and was line-number brittle |
| `cmp` | reproducibility check | proved byte-identical rerenders | says nothing about musical correctness |
| Rust tests | invariant checks | converted timing/noise findings into regressions | added only after the second listening round |
| Clippy/format/corpus test | validation | caught integration/warning regressions | not diagnostic of perceived sound |

Python with NumPy/SciPy was attempted for spectral analysis, but those packages
were not installed. SoX covered the immediate need. A project tool should not
depend on a workstation's optional Python environment.

## What would make the next investigation more efficient

### P0 — structured analysis in the renderer

Add `apteronotus-render analyze` or a sibling Rust binary that returns JSON and
a short human table:

```json
{
  "window": {"cycles": [16, 20], "seconds": [36.92, 46.15]},
  "tracks": [
    {
      "index": 5,
      "voice": "supersaw",
      "onsets": 24,
      "min_interval_ms": 192.3,
      "rms_dbfs": -21.4,
      "peak_dbfs": -8.1,
      "dc": 0.0003
    }
  ],
  "bands_dbfs": {
    "0-200": -18.0,
    "200-2000": -14.2,
    "2000-8000": -31.7,
    "8000-nyquist": -39.5
  }
}
```

The scheduler already knows exact event coordinates and which track produced
each voice. Reporting onset count and interval distribution from program data
is cheaper and more accurate than recovering them from audio.

### P0 — source-preserving track isolation

Support:

```text
--list-tracks
--solo-track 3
--mute-track 1,4
--cycles 16..20
--bars 16..20
```

Isolation must happen after evaluation. Rewriting source is semantically wrong
in a system whose randomness and provenance intentionally depend on source
coordinates.

### P0 — active-lane manifests

For `--stems`, emit lane metadata and measured activity:

```text
lane 0  main.L  -16.2 dBFS
lane 1  main.R  -16.4 dBFS
lane 2  verb.L  silent
lane 3  verb.R  silent
```

Visualization can then omit silent lanes without guessing. This also exposes
whether "stems" are pre-effect inputs, post-effect outputs, or cleared routing
lanes—an ambiguity encountered here.

### P1 — compact plots by construction

A useful plot request should specify:

- exact bar/cycle or second window;
- active channels only;
- one shared frequency axis;
- a maximum pixel budget;
- overview plus an optional targeted frequency zoom;
- no duplicated legends or empty panels.

Text metrics come first. A plot should answer a remaining relational question,
not act as the initial data dump.

### P1 — repeated-transient fingerprint detection

For every percussive track, compare short onset-aligned windows. Very high
cross-correlation across nominally random hits should be reported. That would
have found the fundsp noise-reset bug immediately:

```text
track 0 / kick: median onset fingerprint correlation 1.000 (suspicious)
track 2 / hat:  median onset fingerprint correlation 1.000 (suspicious)
```

After provenance seeding, identical full renders remain reproducible while
different onsets have low correlation.

### P1 — automatic A/B reports

Given two source revisions, produce:

- voice/onset count changes per track;
- peak, RMS, DC and band deltas;
- sample difference only when the sources are expected to be identical;
- compact spectrogram of the **difference**, not two full-size plots;
- warnings when source movement changed provenance-dependent random choices.

### P2 — listener feedback coordinates

The model cannot audition audio directly. The most valuable user feedback is a
timestamp or bar range plus a description:

```text
techno, bars 24–28: repeated glassy chirp on every kick
```

The app or renderer could expose the current bar/cycle while playing and place
a marker with one command. That turns “still chirpy” into a bounded render
window immediately, while keeping the human listener as the final perceptual
instrument.

## Recommended debugging protocol

1. Reproduce with the production renderer.
2. Record the exact source revision, sample rate and bar/cycle window.
3. Print program timing and per-track onset density.
4. Print peak/RMS/DC and fixed spectral bands.
5. Check engine invariants relevant to the symptom before tuning the score.
6. Solo tracks after evaluation, never by rewriting source.
7. Generate an active-channel, bounded-size plot only for the unresolved
   question.
8. Make one change at a time and emit a structured A/B report.
9. Ask the listener to verify the bounded region.
10. Turn every confirmed engine discrepancy into a focused regression test.

The broad lesson is simple: audio debugging should narrow from exact program
data, to compact scalar measurements, to a small visualization, and only then
to source edits. This investigation initially jumped from source reading to
large images and musical tuning; the provenance-seeded noise bug was found only
when the process returned to engine invariants.
