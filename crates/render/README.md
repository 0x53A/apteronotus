# Offline rendering and song auditioning

The renderer evaluates the same owned program and uses the same scheduler,
voices and persistent processors as the player. It needs no audio device.
From the repository, enter `nix-shell` if the host audio build dependencies
are not already available.

```sh
cargo run --release -p apteronotus-render -- --list-songs
cargo run --release -p apteronotus-render -- --song nightshift-dub --cycles 32 --tail 6
cargo run --release -p apteronotus-render -- songs/my-edit.eod --seconds 30
```

`--song` reads the embedded corpus, so an installed renderer works from any
working directory. It writes `<name>.wav` in that directory. A source filename
instead defaults to a WAV beside that source. `-o path.wav` overrides either
default. Existing destinations require `--force`; creation without that flag
is exclusive even when two renders finish at the same time.

The editor's **Library** menu exposes the same songs alongside the four short
teaching examples. Opening a document retains the displaced buffer in
**Restore previous buffer**. Run remains an explicit command.
Browsing further untouched library documents retains that recovery buffer.
If you edit a loaded document, its edited text becomes the next buffer saved
when you choose another document. There is one recovery slot.

## Three variations

| Embedded name | Arrangement | Complete render |
|---|---|---|
| `nightshift-dub` | E minor, 108 BPM; offbeat organ, syncopated bass and dark echoes, with two stripped sections | `--cycles 32 --tail 6` |
| `undertow-skipping-stones` | D dorian, 132 BPM; dry two-step drums, electric keys and exact swung hat offsets | `--cycles 32 --tail 5` |
| `small-light-afterimage` | 72 BPM; beatless minor-to-dorian harmony, low bells and a quiet finite ending | `--cycles 24 --tail 9` |

These are separate source documents, so each can be edited without a variation
switch or any dependency on the original song's declarations. All three use
synthesis only and join the normal corpus evaluation and audio acceptance tests.

## Measuring a change

```sh
# Inspect track indices before isolating a part.
cargo run --release -p apteronotus-render -- --song nightshift-dub --list-tracks

# Measure a section with all of its preceding DSP history intact.
cargo run --release -p apteronotus-render -- --song nightshift-dub \
  --cycles 12..16 --tail 2 --spectrum --dynamics -o breakdown.wav

# Compare routed buses, or a single track through the production effects.
cargo run --release -p apteronotus-render -- --song nightshift-dub \
  --cycles 8 --stems -o dub-stems.wav
cargo run --release -p apteronotus-render -- --song nightshift-dub \
  --cycles 8 --solo-track 1 -o dub-bass.wav

# Machine-readable measurement. This renders every selected track separately.
cargo run --release -p apteronotus-render -- --song nightshift-dub \
  --cycles 32 --tail 6 --json -o dub-analysis.wav > dub-analysis.json
```

`--seconds a..b` and `--cycles a..b` advance from transport zero and discard
the prefix. They preserve carried voices and effect history. `--tail` extends
rendering without scheduling more notes. `--json` prints its document on stdout
and diagnostics on stderr; the WAV is still written. Per-track analysis includes
autonomous persistent runs and the production processing layout, so it should
not be interpreted as an effects-free source stem. `--raw-stems` exposes those
pre-processor lanes instead.

Run `--help` for spectrum, third-octave, reference-comparison, stability,
fingerprint and sample-format options.

## Syntax without evaluation

`--check-syntax` parses and compiles a file or embedded song without evaluating
Lua, allocating audio graphs, or writing a WAV. For example:

```sh
cargo run -p apteronotus-render -- songs/seven-lanterns.eod --check-syntax
```

It reports syntax/compiler failures with their original source line. A successful
check does not establish that binding names, graph types or mini-notation are
valid; rendering or Run performs those checks.

`seven-lanterns` adds a 7/8 procession to the corpus: thirty-two measures,
28 engine cycles, and a ramped 112 → 124 → 96 BPM map. Its complete render is
`--song seven-lanterns --cycles 28 --tail 7`.

`paper-orbits` uses longer captures for a five-against-seven keyboard canon.
Its forty-eight cycles at 102 BPM use `--song paper-orbits --cycles 48 --tail 7`.
