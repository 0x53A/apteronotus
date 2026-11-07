# Offline rendering and song auditioning

The renderer evaluates the same owned program and uses the same scheduler,
voices and persistent processors as the player. It needs no audio device.
From the repository, enter `nix-shell` if the host audio build dependencies
are not already available.

```sh
cargo run --release -p apteronotus-render -- --list-songs
cargo run --release -p apteronotus-render -- my-edit.eod --seconds 30
```

The curated song library is currently empty. `--song` will select a curated
entry once songs are added. A source filename defaults to a WAV beside that
source; `-o path.wav` overrides it. Existing destinations require `--force`;
creation without that flag is exclusive even when two renders finish together.

## Measuring a change

```sh
# Inspect track indices before isolating a part.
cargo run --release -p apteronotus-render -- my-edit.eod --list-tracks

# Measure a section with all of its preceding DSP history intact.
cargo run --release -p apteronotus-render -- my-edit.eod \
  --cycles 12..16 --tail 2 --spectrum --dynamics -o breakdown.wav

# Compare routed buses, or a single track through the production effects.
cargo run --release -p apteronotus-render -- my-edit.eod \
  --cycles 8 --stems -o dub-stems.wav
cargo run --release -p apteronotus-render -- my-edit.eod \
  --cycles 8 --solo-track 1 -o dub-bass.wav

# Machine-readable measurement. This renders every selected track separately.
cargo run --release -p apteronotus-render -- my-edit.eod \
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
cargo run -p apteronotus-render -- my-edit.eod --check-syntax
```

It reports syntax/compiler failures with their original source line. A successful
check does not establish that binding names, graph types or mini-notation are
valid; rendering or Run performs those checks.
