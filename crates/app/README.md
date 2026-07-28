# Apteronotus native app

This crate is the first user-facing desktop path: a Lua text editor, an
explicit **Run** button (`Ctrl+Enter` / `Cmd+Enter`), diagnostics and native
audio output.

Run it from the repository's Nix development shell:

```text
cargo run -p apteronotus-app
```

Audio is opened lazily on the first successful Run. The starter document is a
real `voice { ... }` plus `play(v, "c4 e4 g4")` program and goes through the
same owned `Program`, revision, scheduler, `GraphTemplate`, fundsp sequencer and
cpal output path as the offline acceptance test.

## Activation and failure semantics

Typing only edits text. Run is the evaluation boundary; this app does not
evaluate on each keystroke.

The window thread never evaluates Lua or constructs graphs. A bounded
evaluation job builds each candidate while the player worker continues filling
the active program's lookahead. The player owns revision state, scheduling and
the cpal sequencer. The audio callback owns only fundsp's realtime backend.

For a Run, the player:

1. evaluates a fresh sandbox into an owned `Program`;
2. applies the GUI host's playable-shape checks;
3. atomically lowers every track in the first scheduling window into a
   throwaway sequencer;
4. opens audio if necessary;
5. publishes the revision at the live scheduler's next unscheduled exact cycle
   coordinate;
6. fills the audible sequencer and, on the first Run, starts the stream.

Nothing in steps 1–4 changes the active revision or audible sequencer. A
failure therefore leaves the current program filling lookahead. Voices already
submitted before a successful edit are not removed; their declared tails drain
while the new revision starts at the frontier. There is no implicit
keystroke-to-sound path and no audio-thread Lua callback.

Multi-track scheduling is all-or-nothing per window. If a later window of a new
revision nevertheless fails (for example, a latent non-pitch pattern value),
the window has pushed no voices, so the player restores the previous program at
that same frontier and reports the failure.

## Deliberately narrow first slice

The GUI currently uses a fixed 120 BPM transport and supports ordinary
zero-input note voices with one shared output channel layout. It reports and
rejects, rather than ignores:

- persistent `run(patch)` processors;
- writable program controls;
- live-input graphs;
- graph bus sends;
- mixed track channel layouts; and
- output channel-count changes after cpal has opened the stream.

The cpal stream configuration is fixed for its lifetime, which is why a channel
layout change asks for an app restart. Rebuilding or crossfading the device
stream is separate from revision publication.

Not present yet: continuous parse/highlight diagnostics while typing, sounding
source highlighting, files/recent documents, transport controls, MIDI/device
selection, browser packaging, or persistent patch/bus execution. Those are UI
and host integrations over framework types that already exist; they are not
new Lua syntax.

## Realtime headroom

The native callback renders through fundsp's allocation-free, SIMD block path
in chunks of 64 frames. It requests a 512-frame device buffer when the reported
device range permits that size, clamps the request to narrower known ranges,
and retains the platform default when the range is unknown. At 48 kHz this is
about 10.7 ms of device-buffer latency: enough desktop scheduling headroom
without committing a future MIDI instrument to a large delay.

The workspace development profile optimizes `fundsp`, `apteronotus-synth` and
`apteronotus-live` while leaving the GUI and Lua boundary debuggable. A normal
`cargo run -p apteronotus-app` therefore no longer asks audio-rate DSP to meet
device deadlines using unoptimized code.
