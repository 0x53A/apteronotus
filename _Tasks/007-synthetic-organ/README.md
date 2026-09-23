# Synthetic organ

Two synthesis paths now exist: a voiced additive instrument for composition and
an experimental pressure-driven physical flue. Both are sample-free. The organ
studies contain original phrases, not a transcription of the Darktide score.

- `songs/cathedral-organ.eod` (**Iron Choir**) demonstrates dry stops, a full
  registration, independent pedal, rhythmic keying and one shared cathedral.
- `songs/organ-laboratory.eod` (**Pipes Under Pressure**) compares the original
  additive flute, revised additive speech and the physical flue dry, followed
  by wind changes and repeated valve closures on one retained physical pipe.

## Additive instrument

`organ_pipe(hz, stop)` builds one mono keyed pipe voice. `organ(hz, stops)` sums
1–16 ranks. Both are readable Lua compositions in
`crates/lua/src/stdlib/organ.lua`. The generic `harmonics(hz, amplitudes)`
oscillator is the Rust boundary; it knows nothing about organ stops.

| Rank | Designed spectral character | Base unison attack / release |
| --- | --- | --- |
| `principal` | Full fundamental, progressively softer upper harmonics | 25 / 85 ms |
| `flute` | Strong fundamental and subdued even harmonics | 45 / 110 ms |
| `string` | Relatively strong upper spectrum, slower speech | 65 / 100 ms |
| `reed` | Strong, slowly falling upper partials | 18 / 60 ms |

These are designed spectra, not measurements of named historic stops. Footage
is relative to 8-foot unison: 16 halves the frequency, 4 doubles it. Fractional
footages allow mutations. Supported feet: 0.5–32; fixed tuning: ±100 cents.
`level` accepts a scalar or a graph signal. Smooth manually operated controls
with `slew`; the helper does not secretly filter deliberate audio modulation.
Key velocity does not alter the organ unless the score explicitly uses it.

Default `voicing="speech"` separates harmonics 1–2, 3–6 and 7 upwards into
three phase-coherent groups. Their attack times are 0.75, 1.35 and 0.50 times
the base attack, respectively. Upper modes overshoot their settled amplitude
and settle over 65 ms. Base times scale by sqrt(feet/8). Explicit `attack`
overrides all three attack times; `release` controls all three releases.
Each group releases from its current level, even during attack. A short upper
mode/filtered-air `chiff` remains optional and has its own keyed envelope.

The middle and upper groups are scaled by c and c², where
`c = clamp((220 / hz)^0.20, 0.55, 1.30)`. This gentle register-dependent voicing
keeps the A3 reference spectrum and reduces upper weight in the treble. It
is not a measurement or a physical scaling law. `voicing="classic"` keeps
the original fixed spectrum and common envelope for comparison or reuse.

Harmonic weights are normalized per rank before voicing. Adding a stop never
turns existing stops down. Speech overshoot and bass voicing can raise the
peak above the original static rank bound, so registration and mix gain remain
explicit. No hidden compressor, distortion or limiter is part of the organ.

`harmonics` stores at most 32 coefficients with absolute sum ≤1. It owns one
phase and a fixed array, and computes partials using a sine recurrence. Each
partial tapers between 0.40 and 0.48 times the actual device sample rate.
This prevents static ultrasonic partials from folding back; arbitrary FM or
amplitude modulation can still make sidebands. Harmonic tables now contribute
to publication's `data_entries` budget as well as the usual node budget.

## Physical flue experiment

```lua
local pipe = patch { graph = function()
  return flue_pipe(110, .85, noise(), {min_hz=40}) * .4 >> pan(0)
end }
run(pipe)
```

`flue_pipe(hz, pressure, turbulence, {min_hz})` couples a delayed nonlinear
jet to a lossy bore waveguide. Opening pressure injects energy; closing it
removes the drive while the existing air-column state decays. The pressure
input can be a note envelope, a transport signal or a writable control. Put
it inside a retained patch to reopen the *same* vibrating pipe. Inside an
ordinary voice each onset gets an independent pipe, as with other voices.

Implementation: `crates/synth/src/flue.rs`. The independent reduced model uses
fractional bore and jet delays, negative reflected bore pressure, a smoothed
saturating cubic jet characteristic, and a jet DC-loss state. Pitch compensation
is empirical: 1.512 bore-delay periods plus a correction for loss-filter phase.
The fixed jet/bore delay ratio is 0.32. These coefficients do not describe a
measured pipe's dimensions. It is an open-flue approximation, not the stopped
flute spectrum in the additive helper and not a reed-pipe model.

| Input / property | Contract |
| --- | --- |
| `min_hz` | Finite 20–1000 Hz; fixes both delay allocations at publication/lowering |
| `hz` | Smoothed over 20 ms, clamped to declared minimum and device maximum min(1200 Hz, sample rate/32); at unusually low device rates the device maximum wins |
| `pressure` | Normalized 0–1, smoothed over 3 ms; nonfinite pressure or invalid Hz closes the supply |
| `turbulence` | Explicit signal clamped to ±1, entering as 0.2% relative inlet noise; zero is allowed |
| Useful blowing range | Approximately 0.7–0.95; the model has an oscillation threshold, so pressure is not a linear gain |
| Internal rate | Fixed 4× oversampling with interpolated turbulence; four positive low-pass poles before decimation |
| Output bandwidth | Intentionally dark: output poles at min(6000 Hz, 0.15×device rate); no claim of alias-free arbitrary modulation |
| Response tail | 80 ms + 64/min_hz seconds after the pressure drive's own lifetime |

No tick/process allocation, callbacks or unbounded iteration. Convex delay
interpolation and loss filtering, a jet flow bounded by ±1, its DC estimate
bounded by ±1, and a 0.5 reflection bound bore state by ±4. Output filtering
is convex and output scale is 0.22, giving an absolute output bound of 0.88.
This bound also holds while retuning. Output attenuation does not substitute
for dissipating stored energy.

The two f64 buffers run at 4× rate. Publication charges their f32-equivalent
storage as `16.16/min_hz` delay seconds, plus fixed interpolation guards in
the implementation. Only pressure carries activity lifetime through the pipe:
frequency and turbulence cannot keep a windless pipe sounding indefinitely.
Finite persistent runs gate its pressure input before the resonator, preserving
its response tail; gating the output would have truncated stored vibration.

## What the experiment establishes

Tests verify tuning within 15 cents at nominal 55, 110, 220, 440 and 660 Hz,
pressure 0.85, at 24, 44.1, 48 and 96 kHz. This is a tested operating region,
not a guarantee over every pitch/pressure combination. Larger pressure changes
can affect frequency and spectrum; no exact equal-tempered tuning is promised.

The retained model demonstrably differs from a fresh instance after a short
valve closure. A long closure dissipates the old state, and reopening speaks
again without allocating another pipe. Tests also cover bounded output under
extreme/nonfinite controls, tick/block/reset equivalence, publication bounds,
pressure-only lifetime, and finite-run wind closure.

For the additive path, tests measure a changing attack spectrum, different
relative harmonic balance across the keyboard, steady ten-second sustain,
key release during attack, and live stop changes. Production tests cover both
studies, unclipped stereo output, dry closure and shared room decay, approximate
A3 comparison level matching, and sample-identical repeated renders.

The useful design distinction is now concrete: additive synthesis specifies
speech, while the physical model evolves coupled jet/bore state. We retain
both. Automated measurements cannot establish which sounds better or whether
either captures the desired Darktide character. No listening judgment is
claimed by this implementation report.

## Still outside this slice

There is no shared windchest/reservoir solver, pipe-to-pipe coupling, reed
mechanics, calibrated geometry, nonlinear radiation, or physical keyboard bank
with coupler/retrigger arbitration. Stop changes on held additive keys still
unmute an existing rank rather than start a fresh valve transient. More accurate
pipe geometry and a shared supply should be evaluated separately; the new
pressure input leaves that route open.

## Prior art consulted

- Fons Adriaensen's [Aeolus design presentation](https://kokkinizita.linuxaudio.org/papers/aeolus-pres.pdf): per-harmonic attacks and voicing across note number. Our three groups are a smaller approximation of that idea.
- [STK Flute](https://github.com/thestk/stk/blob/master/include/Flute.h) and [its tuning implementation](https://github.com/thestk/stk/blob/master/src/Flute.cpp): delayed jet coupled to a lossy bore; our implementation, bounds, pressure shutdown and oversampling are independent.
- [Faust physical models](https://faustlibraries.grame.fr/libs/physmodels/): explicit waveguide, jet and reed elements.
- [Organteq physical modeling](https://www.modartt.com/organteq_physical_modeling): pressure as a cause of correlated pitch, level and timbre changes.
- [Hauptwerk VI documentation](https://www.hauptwerk.com/wp-content/uploads/dlm_uploads/2020/11/MIL-091-HW6-Features-Data-Book.pdf): wind-system modeling can coexist with a different tone-generation method.

## Audition

```sh
cargo run -p apteronotus-render -- --song organ-laboratory --seconds 30 --tail 1 -o /tmp/pipes-under-pressure.wav
cargo run -p apteronotus-render -- --song cathedral-organ --seconds 48 --tail 6 -o /tmp/iron-choir-refined.wav
```

Pipes Under Pressure: 0–6 seconds classic additive, 6–12 revised additive,
12–18 physical; each plays A1/A3/E5 in two-second slots. They are roughly matched
at A3, not normalized separately for every pitch. From 18 seconds one retained
A2 pipe goes through .70 and .95 pressure, a 40 ms closure at 22 seconds,
reopening at .85, closure at 24, reopening at 25, and final shutdown at 27.
All of it is dry. The three musical sections instantiate notes; only the final
section demonstrates retained physical identity.

Iron Choir uses the refined additive voicing. The original audition remains
at `target/auditions/iron-choir.wav`; the new files are
`target/auditions/iron-choir-refined.wav` and
`target/auditions/pipes-under-pressure.wav`.

## Validation results for this iteration

Native synth/Lua/song suites, both production organ studies, the complete app
song-corpus lowering test, synth/Lua Clippy (`--all-targets --no-deps -- -D warnings`)
and the Lua `wasm32-unknown-unknown` build pass. `git diff --check` is clean.
The physical unit's tested behavior includes four device rates, block sizes
1/17/64, finite-run pressure gating and release-state retention.

At 48 kHz, the dry laboratory renders 30 seconds plus one second of tail,
nine scheduled voices plus one retained physical pipe, peak −14.0 dBFS. The
refined Iron Choir renders 48 seconds plus six seconds of tail, 108 voices,
peak −3.7 dBFS (original −4.0 dBFS). Neither score has a hidden limiter, and
both pass the no-clipping production test. Whole-study spectral centroids are
299 Hz and 248 Hz, respectively; these aggregate measurements are not a
per-rank fidelity score. The dry A3 sections are within 3 dB in the production
comparison test; other pitches deliberately retain each model's own balance.

## Darktide reference measurements

The [reference analysis](reference-analysis/README.md) compares *Light of the
Imperium* and *Empires Will Fall* with the production organ renders and an
approximate pitch-matched chord probe. It includes reproducible whole-track
spectrograms, excerpt waterfalls and normalized spectra. The current demo is
substantially darker and narrower than the selected mixed-reference passages;
registration and spatial/production treatment are the next concrete experiments.
The measurements do not isolate an organ stem or establish a need for a more
complex physical solver.

The follow-up [organ trials](experiments/README.md) provide five comparable
renders: baseline, brighter registration, spatial treatment, gentle drive and
an experimental 32-partial trumpet reed. Six isolated pipe recordings and an
organ-only performance provide a second reference set. The isolated samples
support targeted reed work and registration changes, while retaining the softer
principal/flute/string options. The trials are experiment-local scores.
