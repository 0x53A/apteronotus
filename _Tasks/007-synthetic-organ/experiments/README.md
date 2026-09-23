# Organ trials and isolated-pipe references

[Open the audition page](../../../target/organ-experiments/index.html), with five
RMS-matched players, editable standalone scores, spectra and waterfalls. The
[sequential reel](../../../target/organ-experiments/all-variants-matched.wav)
plays baseline → bright → spatial → driven → trumpet. Starts: **0:00, 0:16.75,
0:33.50, 0:50.25, 1:07.00**. Each version lasts 16 seconds, separated by 750 ms.

Every version plays the same original 12-second phrase: held G♯ minor chord,
two chord changes, then rhythmic keys. Four seconds of room follow. Playback
files use constant gain to match the first 12 seconds to −20 dBFS stereo RMS;
this is not perceptual/LUFS matching. The original production renders remain
beside them. No compression or peak normalization is used for the trials.

## Five versions

| Version | Change | Centroid, held chord | 2–12 kHz power | Side/mid |
| --- | --- | ---: | ---: | ---: |
| Baseline | Existing seven-rank registration and quiet hall; manual only | 421 Hz | 1.12% | −23.0 dB |
| Bright | More 8′/4′ reed, upper principals and mutations; reduced 16′ foundation | 1159 Hz | 10.55% | −25.0 dB |
| Spatial | Bright ranks distributed across stereo; stronger, shorter, brighter room | 1146 Hz | 10.35% | −7.8 dB |
| Driven | Spatial plus gentle per-rank tanh, DC removal and low-pass | 1129 Hz | 10.17% | −7.6 dB |
| Trumpet | Spatial with a newly designed 32-partial reed replacing its reed ranks | 1288 Hz | 14.74% | −8.1 dB |

Measurements use seconds 1–3 of the same held chord. The existing soundtrack
excerpt is included on the spectral graph, explicitly labeled as a full mix.
The organ-only performance uses different music and is separate from this
matched experiment.

**Spatial and Trumpet are the most useful next listening candidates.** Stop
balance accounts for most of the increased brightness; rank placement and room
return supply width. At this gentle setting, saturation makes little difference
to aggregate spectral balance. It may change texture, which the numbers do not
judge. The new reed is deliberately more assertive, and its isolated upper-band
fraction exceeds the reference trumpet; it is a creative candidate, not a fitted
replica. No auditory preference is claimed.

The shaper experiment is not oversampled. Its post-filter limits bandwidth but
does not undo aliasing. These remain experimental scores; no synth defaults or
existing songs changed. The trumpet uses existing band-limited `harmonics`
oscillators, three keyed harmonic groups and a gentle register-dependent taper.
All topology and coefficients are staged by Lua; runtime remains native DSP.

## Real organ references

Downloaded from their original publishers:

- [Jeux d’orgues 2 — Stiehr-Mockers, Romanswiller](https://www.jeuxdorgues.com/jeux-d-orgues-2-stiehr-mockers/):
  Montre 8′, Bourdon 8′, Salicional 8′ and Trompette 8′, each nominal A3
  (`057-A.wav`). Recorded by Joseph Basquin; the GrandOrgue package identifies
  its remastering contributors as Graham Goode, Joseph Basquin and Martin
  (M.XY), including noise reduction. These are processed sample-library
  recordings of real pipes, not raw microphones. The files were obtained from
  the publisher's linked GrandOrgue archive.
- [Bureå Funeral Chapel, Lars Palo](https://familjenpalo.se/vpo/burea-funeral-chapel/):
  Gedackt 8′ and Salicional 8′, nominal A3. CC BY-SA 2.5; the local normalized
  reference copies retain that license and attribution. The publisher's archive
  MD5 `1d1a44aae37911f1f81f781b64eb668e` was verified. `bsdtar` failed on some
  files; extraction was repeated successfully using `unar` before analysis.
- [Lars Palo's organ-only Allegro example](https://familjenpalo.se/vpo/examples/):
  part of C. P. E. Bach's F-major sonata, Bureå Gedackt, Rörflöjt and Principal,
  played through GrandOrgue with JConvolver room convolution. This is a
  sample-based performance with added room, not a live unprocessed pipe-organ
  recording. Original MP3, decoded WAV, overview and 10–18 s zoom/waterfall are
  available locally. The publisher explicitly provides the audio for download.

The six single-pipe players retain the entire source sample, including attack
and release. Audition gain is based on sustain RMS with a peak headroom cap;
not every source can reach the same sustain level without clipping. The synth
itself uses none of this audio.

## What the isolated pipes changed in the assessment

At A3 our principal's centroid is 318 Hz versus 275 Hz for the recorded Montre;
our string is 562 Hz versus 395 Hz and 249 Hz for the two Salicionals. **The
soundtrack comparison did not mean that every individual rank needed brightening.**
Our stopped-flute shape broadly resembles the Bourdon's odd-harmonic emphasis;
the Bureå Gedackt is even closer to a sine. This variety is a reason to keep soft
stops and adjust registration for the desired music.

The recorded Trompette retains stronger higher partials than our original reed:
its sustain has 5.58% power above 2 kHz, versus 1.16% for ours. That motivated
`trumpet-voice.lua`: a hand-designed upper-middle plateau, a dip around the third
harmonic and a tail extending to 32 harmonics. The trial has 9.90% upper power at
A3. It explores a stronger color without replacing the softer original reed.
One note per rank does not establish the whole keyboard's voicing or speech.

## Measurement and validation

Real-pipe sustain windows come from each WAV's first RIFF `smpl` loop, with an
inclusive end sample. This matters: several files are only 1.4–2 seconds long,
and measuring a fixed second would include their releases. Loops are analyzed
once at their native duration, not repeated to invent frequency resolution.
Measured fundamentals are near 220 Hz; no retuning is applied.

Harmonic charts use mean stereo channel power, a Hann window bounded by the
available loop length, and integration within ±2.5 bins of each estimated
harmonic (capped at 70 Hz). Values are relative to the fundamental. Window
resolution differs between loops; recording noise contributes to weak upper
bins, and no noise-floor subtraction is performed. Room and microphone response
remain part of the reference. Whole-spectrum metrics and source hashes are in
`measurements.json`. The previous analysis's waterfall implementation is reused.

All five scores evaluate and render through the production path, each with
22 voices; the five dry-pipe probes also render. Raw and normalized files are
finite and unclipped. Matched audition peaks range from about −5.8 to −3.0 dBFS.
Analysis checks input PCM format, normalization headroom and generated output;
plots were inspected. No backend behavior changed, so no new DSP regression
suite was needed.

## Reproduce

From the repository root, after downloading/extracting the reference archives
under `target/organ-experiments/references`:

```sh
python3 _Tasks/007-synthetic-organ/experiments/prepare.py
nix-shell --run 'for name in baseline bright spatial driven trumpet; do cargo +1.97.1 run -p apteronotus-render -- target/organ-experiments/$name.eod --seconds 12 --tail 4 --force -o target/organ-experiments/$name.wav || exit; done'
nix-shell --run 'cargo +1.97.1 run -p apteronotus-render -- target/organ-experiments/single-pipes.eod --seconds 20 --tail 0 --force -o target/organ-experiments/single-pipes.wav'
nix-shell -p 'python3.withPackages (p: [ p.numpy p.scipy p.matplotlib ])' --run 'PYTHONDONTWRITEBYTECODE=1 python3 _Tasks/007-synthetic-organ/experiments/analyze.py'
```

`prepare.py` combines the experiment-local trumpet helper with the score and
writes complete standalone `.eod` files. These can be opened/imported into the
app directly. Edit the maintained source here and regenerate to keep experiments
comparable. Large downloaded libraries and generated audio/figures remain in
ignored `target/`; source, measurements and this interpretation remain here.
