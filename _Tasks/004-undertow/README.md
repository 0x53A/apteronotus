# Undertow: a song written through the renderer

`songs/undertow.eod` is the third piece written against the working engine
rather than ahead of it. It fills a gap in the corpus palette: a 168 BPM
broken beat whose body is heard at 84 BPM. Fast hats and syncopated bass supply
motion; one half-time backbeat supplies weight.

This note records what measurement can establish. It does not claim the model
listened to the result. A human listening pass remains authoritative.

## Form and sound

- D Dorian, with Dm9 / G6 / Cmaj9 / Am11 over eight bars.
- A returning 16-bar section-weight pattern rather than a finite arrangement.
- Four-bar drum detail, five-bar plucked counterline, and transport movements
  over 7, 9, 11 and 13 bars.
- Centred sine sub plus filtered detuned saws; the sub is not widened.
- One large backbeat, provenance-seeded noise hats, a dry resonant rim, a slow
  pad and a Karplus–Strong counterline.
- Short filtered room and three-eighth echo sends.
- Four live mix controls: bass, drums, pad and glass.

The full form is exactly 16 cycles. At 168 BPM that is 22.857 seconds, making
the repeat short enough to inspect while the mutually non-dividing detail
periods keep successive forms from having the same surface.

## Iterations

All measurements below use the production renderer over `--cycles 0..16`
with one second of response tail. Track summaries are isolated only after
evaluation, so source-derived random choices do not move.

### Version 1 — correct program, wrong hierarchy

The first complete render evaluated and scheduled 324 voices, but its spectrum
and track levels exposed a low-end pile-up:

| measurement | value |
|---|---:|
| peak | −5.1 dBFS |
| centroid | 148 Hz |
| 0–200 Hz | −18.5 dBFS |
| 200–2000 Hz | −30.0 dBFS |
| 2–8 kHz | −39.8 dBFS |
| side/mid | −24.1 dB |
| envelope spread | 21.2 dB |

Bass and kick were both about −21.5 dBFS RMS. Snare was −38.2, hats −45.0,
rim −58.2, pad −31.6 and glass −32.6. This was not an intro artefact: the
same hierarchy remained over the whole form.

### Version 2 — balance at sources

The bass and kick output gains moved down about 3 dB. Snare and hats moved up
about 5–6 dB, rim about 8 dB, and the two stereo harmonic layers about 3 dB.
No master gain compensated for those moves.

Relative to version 1, the renderer reported:

- sub region −5.4 dB after independent 1 kHz anchoring;
- low region −4.1 dB;
- side/mid +4.6 dB;
- envelope spread −4.2 dB;
- centroid 340 Hz rather than 148 Hz.

The busiest four-bar window (`--cycles 8..12`) stayed below −5.8 dBFS peak and
its one-second RMS windows spanned only 3.1 dB. That made the section-level
variation, rather than accidental overload, the source of the full form's
larger dynamic range.

### Version 3 — spatial placement

Master difference width moved from 1.65 to 2.8 and the quiet rim rose another
3.3 dB. The limiter remains after width, so the change cannot create an
unbounded output.

Final full-form measurements:

| measurement | value |
|---|---:|
| voices | 324 |
| peak | −5.5 dBFS |
| centroid | 374 Hz |
| 0–200 Hz | −20.5 dBFS |
| 200–2000 Hz | −27.3 dBFS |
| 2–8 kHz | −35.4 dBFS |
| 8 kHz–Nyquist | −40.2 dBFS |
| side/mid | −14.9 dB |
| 20 ms envelope spread | 16.7 dB |
| 2 s RMS windows | −25.1 to −17.4 dBFS |

The final isolated track peaks are deliberately much closer than their RMS
levels: kick −8.4, snare −9.2, hats −8.5, glass −10.6 and pad −13.7 dBFS.
Sparse transient parts should not be forced to match a repeating kick in RMS.

## Repetition audit

`--fingerprints` uses exact scheduler onset coordinates and 50 ms isolated
waveform windows. On the final full form:

- stochastic hats: median −0.002, maximum +0.103;
- plucked glass: median −0.039, maximum +0.281;
- bass notes: median −0.197, maximum +0.249;
- deterministic kick: median and maximum +1.000.

The kick result is expected: it is a fixed tonal graph with no random source.
The hat result is the relevant invariant and confirms that dense noise hits do
not restart one shared waveform.

## Integration

`apteronotus-songs` embeds the file and the app corpus test evaluates and
lowers it with every other shipped song. It is intentionally not in the app's
compact Examples menu: those four documents teach one idea each, while this is
a complete score. The corpus crate remains the single delivery mechanism.

## Listening handoff

The first useful human feedback should name a cycle range. In particular:

- cycles 0–4: does the sparse opening establish half-time or merely feel empty?
- cycles 8–12: does the full break stay articulate under the pad?
- cycles 12–16: does the reduced section make the return feel earned?

Those windows can be rendered verbatim with `--cycles a..b`; no source rewrite
or random reseeding is involved.
