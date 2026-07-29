# Composing by measurement: `drift.eod` and `outbound.eod`

A companion to `../002-audio-debugging/README.md`, written after a July 2026
session that produced two new songs. It is the same underlying constraint —
the model cannot audition audio — but a different activity. 002 is about
finding a defect. This is about authoring something that has no correct answer,
where the only failure mode is "sounds wrong" and there is no test for it.

The short version: **composition splits into a part that measurement decides
and a part it cannot touch, and the split is much cleaner than expected.**
Balance, spectral shape, stereo width, density, level stability and structural
period are all measurable, and every one of them was wrong on the first render.
Melody, harmony and whether the piece is any good are not, and no amount of
analysis substitutes for one listener saying "it's chirpy".

## What was made

| song | brief | outcome |
|---|---|---|
| `drift.eod` | "synthwave to study and relax to"; the existing `synthwave.eod` was too busy and chirpy | 74 BPM, no arrangement at all — all variation derived from 3/7/11/13-bar transport periods against an 8-bar loop |
| `outbound.eod` | upbeat, not busy; derived from the characteristics of a reference track the user linked | 140 BPM half-time; polymetric layers of 3/4/5/7/8 bars, so the *combination* is the form |

Both were registered in `crates/songs` and are covered by the corpus lowering
test, which is the only automated check that exists for them.

## The tooling correction to 002

002 concluded:

> Python with NumPy/SciPy was attempted for spectral analysis, but those
> packages were not installed. SoX covered the immediate need. A project tool
> should not depend on a workstation's optional Python environment.

The premise is out of date and the conclusion is therefore too strong. `uv`
with PEP-723 inline script metadata makes an analysis script **self-contained**
— dependencies are declared in the file, resolved and cached on first run, and
nothing is installed into a workstation environment or into this project:

```python
# /// script
# dependencies = ["numpy", "soundfile"]
# ///
import numpy as np, soundfile as sf
```

```sh
uv run -q analyse.py drift.wav
```

First run takes a few seconds; subsequent runs are instant. This session used
six such scripts and never touched `shell.nix`, a virtualenv or the system.
Everything below was done in NumPy, and most of it is not expressible in SoX
`stats` at all — Welch averaging, third-octave banding, per-bar spectral
feature vectors, cross-song delta tables.

So the durable rule is narrower than 002 stated: *a project tool* should not
depend on an ambient Python environment — a throwaway analysis script may,
provided it declares its own dependencies. The two are different artifacts with
different lifetimes.

`ffmpeg` and `yt-dlp` were likewise used through `nix-shell -p` for a one-off
and left uninstalled.

## Measurements that worked

Roughly in order of how often they changed a decision.

### 1. Per-track solo levels (used constantly)

The single highest-value measurement. Programmatically comment out every
`play(...)` block but one, render, and report RMS, peak and spectral centroid.
On `drift.eod`'s first render:

```
    kick  rms  -30.7  peak  -12.2  centroid     64 Hz
   brush  rms  -60.6  peak  -31.9  centroid   1704 Hz
    bass  rms  -22.1  peak  -13.8  centroid     52 Hz
     pad  rms  -36.1  peak  -23.2  centroid    163 Hz
    lead  rms  -40.0  peak  -24.5  centroid    414 Hz
```

The bass was the loudest thing in a piece where the pad is supposed to be the
bed; the brush was 38 dB below the bass and effectively did not exist. Neither
is visible in the source, and both are unambiguous once printed. This took the
mix from guesswork to arithmetic in one step.

The centroid column earns its place separately: `pad centroid 163 Hz` said the
pad was functioning as a sub, which turned out to be a filter Q of 0.22 plus a
sub-octave sine — a diagnosis the level columns alone would not have given.

### 2. Welch third-octave curve, in dB against a fixed anchor

The measurement that decided every tonal question. A `1<<15` FFT with 50%
overlap, averaged across the file, summed into third-octave bands, printed as
dB relative to a chosen band with an ASCII bar:

```
      500 Hz  +22.6  ###############################
      630 Hz  +19.5  #############################
     1000 Hz  +19.8  #############################
     2000 Hz  +14.3  ###########################
     4000 Hz  +11.2  #########################
     8000 Hz   +2.9  #####################
```

Twenty-seven lines of text, readable at a glance, and it answers the question a
spectrogram is usually reached for. 002 spent significant context on
spectrogram PNGs; **this session generated no images at all** and did not want
any. A spectrogram shows time-frequency structure, which matters for a
transient defect; for mix balance a time-averaged curve is strictly better and
about a thousand times cheaper in context.

### 3. Reference delta table

For `outbound.eod` the user supplied a reference recording, so the third-octave
curve became a comparison. Two refinements were necessary:

- **Re-reference to 1 kHz, not to the lowest band.** Anchoring at 31 Hz makes
  every number a function of how much sub each mix happens to have, and a
  master high-pass moving from 38 Hz to 46 Hz shifted the entire column by
  10 dB while nothing audible changed.
- **Collapse to region means.** Twenty-seven deltas is too many to act on.
  Six is actionable:

```
relative to 1 kHz, mean error by region:
           sub 39-63:   -4.7 dB
          low 79-160:   +3.2 dB
      lowmid 198-320:   +3.8 dB
          mid 400-1k:   +0.5 dB
   presence 1.2-3.2k:   +2.8 dB
            air 4-8k:   -3.0 dB
```

First render was +12 dB below 250 Hz and +6 dB above 1.2 kHz against the
reference — a smile curve where the reference is mid-focused. Three edit rounds
brought every region inside about 4 dB.

Two scalar companions mattered as much as the curve:

- **side/mid ratio.** `20·log10(rms(L−R) / rms(L+R))`. The reference measured
  −6.2 dB; `drift.eod` measures −21 dB and `outbound.eod`'s first render −18.9.
  This was invisible in every other measurement and is a large part of why a
  mix sounds small. Nothing in the source hints at it.
- **20 ms envelope percentile spread** (p95 − p5) as a proxy for pumping.
  Reference 11.8 dB, first render 23.9 dB — the sidechain ducking was roughly
  twice as deep as the idiom.

### 4. Long-window level stability

For a piece meant to run for hours, "does the level stay put" is a real
correctness property. Five-second RMS windows across a 5-minute render:

```
spread 3.7 dB   peak max -9.2
```

Cheap, and it would immediately catch a runaway feedback path or an arrangement
curve accidentally decaying to nothing.

### 5. Structural period verification

New in this session and the one genuinely compositional check. `outbound.eod`
claims specific loop lengths per layer; that claim is testable without ears.
Render each track solo, cut into bars, reduce each bar to a per-bar feature
vector (log energy in five bands × eight time slices), normalise, and report
the lag minimising mean distance between bars:

```
     ride: best repeat lag = 5 bars   (next 10, 2)   dists 5:0.7  others ~8.5
     sing: best repeat lag = 7 bars   (next 2, 5)    dists 7:2.4  others ~7
      tom: best repeat lag = 7 bars
    snare: best repeat lag = 4 bars
```

Limitation, and it is worth stating because it looks like a failure: layers
whose bars differ only in *pitch* (a bass playing the same rhythm on changing
roots) show a best lag of 1, because band energy is pitch-blind. That is the
metric's blind spot, not the pattern's. A chroma-based feature would fix it.

### 6. Click and pop detection

Two different things, and conflating them wasted time.

- **Sample discontinuity:** outliers in `diff(x)` against a MAD-based
  threshold. Finds a voice truncated mid-envelope. **High false positive rate
  on noise percussion** — a band-passed noise burst legitimately has large
  sample-to-sample jumps — so the useful output is the `level before` /
  `level after` columns around each jump, not the count. A real click has a
  step; a noise transient does not.
- **Pop density**, for deliberate crackle: median-relative peaks in a 5 ms
  envelope, reported per second. This is what turned "the crackle is too much"
  into a number (14/s, thinned to 3.9/s).

## Measurements that misled

Recorded because they cost real time.

**Band energy as a percentage of total.** The first spectral tool summed
`|FFT|²` into nine wide bands and printed percentages from a *single* windowed
FFT over the whole file. It reported the 500–1000 Hz band at 1.15% and
2000–4000 Hz at 22%, which reads as a hollow midrange and a harsh top. Several
edits were made on that basis. The Welch third-octave curve then showed a
smooth, slightly dark slope, closely comparable to `synthwave.eod`'s. Three
things were wrong at once: an unaveraged FFT is noisy; percentage-of-total
means one hot band compresses every other number toward zero; and there was no
reference curve, so there was nothing to be "too little" relative to.

The fix is the whole of point 2 above — average, express in dB, anchor to a
fixed band, and always compare against something.

**Onset counts from audio.** Spectral-flux onset detection systematically
missed slow-attack voices (a lead with a 550 ms attack) and double-counted
ducking releases. It was useful for gross density comparison against the
reference and useless for verifying that a pattern plays what it says. As 002
already argues under P0: the scheduler knows the exact event coordinates, and
recovering them from audio is both harder and worse.

## Findings about the engine and the idiom

Not defects, but each cost a render to discover and each is invisible in the
source.

**`x >> (pan(a) & pan(b))` is mono.** `&` fans one source into several, so both
pans receive the *identical* signal and the result is perfectly correlated —
measured side/mid of **−145.9 dB** on a noise floor written that way. Two
genuinely independent sources are required:

```lua
local hiss_left  = (pink() | dc(3200) | dc(0.5)) >> lowpass()
local hiss_right = (pink() | dc(3200) | dc(0.5)) >> lowpass()
```

which measured −9.0 dB. This is a direct consequence of the identity rule in
`/CLAUDE.md` ("bound once and referenced twice is one node, written twice is
two") — but the rule is stated there as a *correctness* guarantee for stateful
analysers, and its stereo consequence is easy to miss. The `&` idiom appears in
`waves.eod` with a comment describing "two independent widths"; the structural
argument says that cannot be what it produces, and it is worth measuring.

**A bare `decay()` is a click.** `decay(ms(180))` starts at 1, so multiplying a
source by it steps from silence to full scale in one sample. Every struck voice
in `drift.eod` now uses `decay(t) * (1 - decay(ms(3..12)))`, which costs
nothing audible and removes the transient. This is most of the difference
between a soft kit and a sharp one, and it is a one-token change that no
measurement in the mix-balance set detects — it shows up only as excess
high-frequency energy in a solo, or as a listener saying "pops".

**A high-pass has no upper limit.** The obvious hat construction
`noise() >> highpass(8000)` contributes full-scale energy all the way to
Nyquist. `outbound.eod`'s first render measured +18 dB against the reference at
12.7 kHz almost entirely from this. Band-pass is the right primitive for a hat;
the difference between "bright" and "harsh" is the upper skirt.

**Signals are not accepted everywhere a number is.** `degrade(...)` requires a
finite scalar and rejects a transport signal, so "how much melody survives"
cannot be automated that way. It did not matter — `degrade`'s seed derives from
cycle position, so a *fixed* amount already selects a different subset on every
pass of the loop. The lesson generalises: when a derived-coordinate system
refuses a modulation, check whether the coordinate already supplies the
variation.

## The listener remains the instrument

Confirming 002's P2 from the other direction, twice in one session:

1. The user reported crackling. It was deliberate (a synthesized vinyl floor),
   but measuring it showed 14 pops/second where the source comment claimed
   "sparse". The *code's own stated intent* was the thing being violated, and
   only a listener asking the question caused it to be checked.
2. The user then reported pops with the crackle control at zero. Discontinuity
   analysis of the render found none — the artifact was not in the audio the
   engine produces. The remaining hypotheses were realtime dropouts in the
   debug-build player (untestable offline) and short noise transients reading
   as ticks. The second was addressed; the first can only be resolved by the
   listener trying `--release`.

Case 2 is the important shape: **a report that does not reproduce offline is
itself information**, and it separates "the score is wrong" from "the runtime
is struggling" in one measurement. That is worth reaching for early, because
the two have completely different fixes and sound similar.

002's request for bar-range feedback coordinates stands unchanged and would
have helped here too.

## Tool gaps

### Confirmed from 002, hit again

**Source-preserving track isolation is the top gap.** Every solo in this
session was produced by parsing `play(` blocks out of the source with balanced
paren matching and prefixing each line with `--`. This is a marginal
improvement on 002's `sed` line deletion — it survives edits and does not need
line numbers — but it shares the fatal flaw 002 identified: commenting a line
adds two bytes, so **every byte offset after the first muted track moves**, and
source offsets feed the deterministic seeds for `degrade`, `sometimes` and
`ply`. A solo render therefore does not necessarily contain the same randomized
event selection as the full render. For level balancing this is tolerable. For
anything provenance-sensitive it is silently wrong.

`--solo-track` / `--mute-track` after evaluation would remove ~60 lines of
fragile scripting from every future session of this kind.

### New requests

**`--reference <file>` on the renderer.** Given a reference audio file, print
the third-octave delta table and the region means from point 3, plus side/mid
and envelope-spread deltas. This is the single most useful thing built this
session and it is currently forty lines of throwaway NumPy. It is also useful
without a reference: comparing a song against another song in the corpus is the
same operation, and is how "is this darker than `synthwave.eod`?" gets
answered.

**Per-track summary as part of a normal render.** The solo loop exists only
because the renderer reports one aggregate voice count. Printing per-track RMS,
peak and centroid alongside it — which the scheduler can attribute directly,
per 002's P0 — would make the most-used measurement in this session free.

**A structural-period report.** Given a program, report each track's pattern
period in cycles from the AST rather than from audio. Trivial compared to the
audio version, exact where the audio version is pitch-blind, and it turns "the
polymetric claim in the header comment" into something checkable.

### Operational notes

- The renderer resolves a relative `-o` against the process working directory.
  Driving it from a script whose own cwd differs writes output into the repo;
  one stray 8 MB WAV was left in the root this session. Pass absolute paths.
- Rendering is fast enough to be used freely: 300 seconds of `drift.eod` in 34
  seconds wall, debug build. There is no reason to guess at anything.

## Protocol for authoring, as opposed to debugging

002's ten-step debugging protocol stands. This is the sibling for writing
something new.

1. Write the whole piece first and render it. It will evaluate or it will not;
   the language errors are fast and specific.
2. Solo every track. Fix the balance before touching anything else — most of
   what sounds wrong at this stage is one part 15 dB out.
3. Take the Welch third-octave curve and compare it to something: a reference
   recording, or the closest existing song in the corpus. Absolute curves mean
   nothing; deltas mean everything.
4. Check side/mid. It will be too narrow.
5. Check long-window level stability over minutes, not seconds.
6. Verify any structural claim the header comment makes. If the comment says
   five bars, measure five bars.
7. Re-read every comment against the final numbers. Comments written before
   tuning are wrong by the end — this session had a "four soft saws" that was
   five, a "quarter of the level" that was half, and a claimed 5½-hour repeat
   period that was actually 21.6 hours.
8. Ship it to the listener and ask for bar-range feedback.
9. When feedback arrives, first establish whether it reproduces in an offline
   render at all.

Step 7 is not housekeeping. In this repository the prose is the design record,
and a comment that drifted from the code is worse than no comment, because the
next session will believe it.
