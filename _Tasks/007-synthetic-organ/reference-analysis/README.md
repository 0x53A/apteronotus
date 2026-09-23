# Darktide organ reference analysis — 2026-09-22

The current organ registration is substantially darker, cleaner between partials,
and narrower in stereo than the selected reference passages. An approximate
pitch-matched probe preserves that difference. This establishes a useful
production target, but **does not isolate the reference organ**: other instruments,
processing and lossy encoding contribute to every reference measurement.

[Open the illustrated report](../../../target/organ-reference-analysis/plots/index.html).
It contains whole-track overviews, 17 excerpt zooms and 17 true 3D waterfalls,
three normalized spectral comparisons, and measurement tables. Every figure has
PNG and PDF versions. The figures are generated under ignored `target/`; this
folder retains the script inputs, probe score and measured results.

## Sources and passage selection

- [Light of the Imperium](https://www.youtube.com/watch?v=Tiw1sKOJz2w), Jesper Kyd,
  official Fatshark upload, decoded duration 272.939 seconds.
- [Empires Will Fall](https://www.youtube.com/watch?v=yzQ7IBpQd0Q), Jesper Kyd,
  official artist upload, decoded duration 188.361 seconds.

Both were accessible without authentication. These are YouTube Opus deliveries,
not lossless masters or instrument stems. Titles and attribution come from the
uploads' metadata. No source separation or auditory instrument classification was
performed. “Candidate organ” below means a spectrally plausible sustained,
richly harmonic keyboard texture; a synthesizer or layered arrangement could
produce similar evidence. The windows are useful inspection points, not an
exhaustive segmentation of either song.

| Window | Spectral observation / usefulness |
| --- | --- |
| Light L1, **0:10–0:24** | Exposed pitched bands with shifting/broadened energy around them; useful for sustained texture, ambiguous instrument identity. |
| Light L2, **0:28–0:40** | Several persistent harmonic bands and strong upper-middle energy before the large rhythmic entry; useful candidate organ/keyboard layer. Low rhythmic energy increases inside this window. |
| Light L3, **0:54–1:02** | Dense rhythmic entry. Useful full-production target; poor window for extracting a stop spectrum because transients and low-frequency percussion dominate. |
| Light L4, **1:24–1:38** | Reduced bass texture exposes upper pitched energy. The higher centroid partly reflects missing bass, not necessarily a brighter instrument. |
| Empires E1, **0:18–0:28** | Upper harmonic ladders remain visible over strong repeated bass/percussive events. Candidate organ texture is mixed with other sound. |
| Empires E2, **0:28–0:40** | Bright layered pitched passage; substantial overlapping components make a single-rank interpretation unreliable. |
| Empires E3, **1:46–1:58** | Especially useful changing harmonic ladder extending several kHz, with clear rhythmic interruptions. Best selected window for studying the rich upper structure; still not a solo-organ stem. |
| Empires chord, **1:47.9–1:48.7** | Narrower probe window within E3. Peaks near 208, 247 and 311 Hz motivate an approximate G♯3/B3/D♯4 comparison. Octave assignment, additional notes and registration remain uncertain. |

The report links each reference window back to its timestamp on YouTube.

## Comparison to our organ

The existing production renders supply known sources: dry principal and reed at
D3, the full Iron Choir registration with pedal/hall, rhythmic Iron Choir, and
classic additive/refined additive/physical flue at A3 in Pipes Under Pressure.

`matched-probe.eod` additionally renders G♯3/B3/D♯4 using exactly Iron Choir's
seven-rank manual registration and gain, first dry and then with its shared hall.
It has no separate pedal, drums or other instruments. This reduces pitch/register
and pedal confounds, without claiming a transcription or a matched arrangement.
The compared probe windows are equal-length, settled 0.8-second excerpts.

| Excerpt | Spectral centroid | Power in 2–12 kHz | Side/mid energy |
| --- | ---: | ---: | ---: |
| Light 0:28–0:40 | 671 Hz | 5.54% | −2.5 dB |
| Light 0:54–1:02 | 351 Hz | 3.95% | −8.1 dB |
| Empires 1:46–1:58 | 1279 Hz | 19.07% | −3.6 dB |
| Iron Choir full registration, 0:16–0:24 | 206 Hz | 0.39% | −30.5 dB |
| Empires narrow chord window | 1212 Hz | 16.36% | −3.4 dB |
| Approximate pitch-matched organ, dry | 388 Hz | 1.01% | −∞ (mono) |
| Approximate pitch-matched organ, hall | 387 Hz | 1.00% | −21.5 dB |

These numbers describe whole excerpts, not an inferred organ stem. They are
independent of scalar gain. All comparison figures additionally match excerpts
to −20 dBFS mean-channel RMS, so mastering loudness is removed as a visual cue.

The matched probe has about **16 times less fractional energy above 2 kHz**
(about 12 dB), and the hall probe remains about **18 dB lower in side/mid ratio**.
The precise factors are not settings to copy into an EQ or stereo widener: the
reference includes sound that our probe deliberately does not synthesize.
The dry/wet probe spectra nearly coincide at the current send setting. A long
reverb time by itself has not supplied substantial width or upper spectral body.

The waterfall ridges in our sustained registration are very regular, with deep
inter-partial gaps and weak high harmonics. The reference windows contain more
filled-in and changing energy. Additional notes, modulation, saturation,
reverberation, percussion and encoding can all contribute; the waterfall cannot
assign that difference exclusively to pipe physics.

The dry A3 laboratory confirms a narrower point: classic and refined additive
flutes have essentially the same settled spectrum (both ~239 Hz centroid), while
the physical flue is somewhat richer (~304 Hz). Revised speech chiefly changes
the onset, and the physical model remains a relatively dark foundation. Neither
experiment establishes the bright upper structure of the reference mixes.

## What I would change next

1. Build a brighter reference-oriented registration, especially upper principal,
   reed and mixture weight. Test sustained spectra and attacks separately.
2. Give ranks deliberate spatial placement and make the shared room's early/wet
   contribution audible enough to measure. Recheck mono compatibility.
3. Add controlled drive and rhythmic articulation at the score/processing layer,
   measuring the organ bus separately from percussion and bass.
4. Continue physical-model work for speech and wind response, evaluated against
   isolated pipe references. These full mixes do not establish that a more
   elaborate physical simulation is the immediate missing ingredient.

No synthesizer behavior or existing score was changed during this analysis.
The only new score is the measurement probe.

## Measurement method and limits

- Decode WAV, scale integer PCM to full-scale floats, and resample every source
  to 24 kHz with SciPy's polyphase anti-aliasing filter. Analyze mean **channel
  powers**, avoiding cancellation from summing a stereo waveform to mono.
- One-sided Hann power spectral density, no detrending. Overview: 4096 samples
  (170.7 ms), hop 1024. Harmonic zoom/waterfall: 8192 samples (341.3 ms), hop 256,
  2.93 Hz bin spacing. Transient zoom: 1024 samples (42.7 ms), hop 128,
  23.44 Hz spacing. Bin spacing is not the full resolving width of a Hann window.
- Color/z limits −100 to −25 dBFS/Hz, frequency display 40–8000 Hz (transient
  panels start at 100 Hz). Long windows smear note edges; short windows blur
  low harmonics. PSD peak height also changes with window width, so compare
  equivalent panels. Very low display levels include leakage/codec residue.
- Each overview uses one whole-track normalization. Each excerpt uses one
  constant RMS normalization, never per-frame normalization. Original levels
  remain in the overview envelope and JSON. Normalization of near-silent tails
  is therefore not performed independently.
- Waterfalls plot up to 65 evenly spaced STFT frames; the heatmaps retain every
  computed frame. These are time–frequency plots, **not** measured room impulse
  responses or cumulative spectral-decay estimates. Do not infer RT60 from them.
- Comparison curves integrate linear power into disjoint 1/6-octave bands.
  Metrics integrate the 8192-point PSD over the full 0–12 kHz analysis range;
  the stated upper-band fraction covers 2–12 kHz. Side/mid uses (L−R)/2 and
  (L+R)/2. JSON uses a −160 dB numerical floor for exact mono, displayed as −∞
  in the report. Measurements cannot identify instruments by themselves.

Validation: a known 750 Hz sine passes peak-frequency and expected RMS checks;
PSD integration recovers its known power; opposite-phase stereo produces the
same spectral power as identical-phase stereo. The probe renders six voices
without clipping (−9.6 dBFS peak). Plot layouts were inspected and Python syntax
and `git diff --check` pass. Source WAV SHA-256 hashes are in `measurements.json`.

## Reproduction

Run from the repository root. Existing laboratory and Iron Choir renders can be
regenerated using the parent task's audition commands. Here, the installed Rust
toolchain requires `cargo +1.97.1`; the project nix shell supplies native libraries.

```sh
nix-shell -p yt-dlp ffmpeg --run 'yt-dlp --no-playlist -f bestaudio --write-info-json -x --audio-format wav -o "target/organ-reference-analysis/%(id)s.%(ext)s" https://www.youtube.com/watch?v=Tiw1sKOJz2w https://www.youtube.com/watch?v=yzQ7IBpQd0Q'

nix-shell --run 'cargo +1.97.1 run -p apteronotus-render -- _Tasks/007-synthetic-organ/reference-analysis/matched-probe.eod --seconds 8 --tail 3 -o target/organ-reference-analysis/matched-probe.wav'

nix-shell -p 'python3.withPackages (p: [ p.numpy p.scipy p.matplotlib ])' --run 'python3 tools/organ-waterfalls.py _Tasks/007-synthetic-organ/reference-analysis/manifest.json --output target/organ-reference-analysis/plots'
```

The plotting tool never downloads audio. Manifest paths are repository-relative;
URLs are attribution/navigation only. Retrieval may change as YouTube deliveries
change; the hashes identify exactly the files measured in this run.
