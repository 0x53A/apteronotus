# Persistent replacement without transport reset

This slice removes the cycle-zero reset for one deliberately bounded class of
native edits: both programs own persistent state, their persistent arenas are
incompatible, and their tempo map and output channel layout are unchanged.

## Invariants earned

- The incumbent is filled to a future exact scheduler frontier before the
  candidate is constructed. It therefore remains a complete playable program
  throughout preparation.
- Candidate lowering receives that frontier's absolute transport seconds.
  `TransportSequenceUnit` adds its local sample time to this origin before
  taking periodic phase, so replacement age is never mistaken for musical
  position.
- Voice and finite-run timestamps use the same explicit mapping: absolute
  transport time minus the replacement sequencer's origin. Later lookahead and
  external onsets retain that origin rather than silently returning to a
  cycle-zero backend clock.
- The complete candidate window is lowered into a throwaway sequencer, then a
  paused device stream, before any incumbent handle changes.
- The candidate starts at silence. After it starts, the old and new
  device-edge gain stages move in opposite directions for a bounded 80 ms and
  the incumbent stream is dropped. There are no fallible publication steps
  after this point. The stream-edge smoother captures its initial value when
  the graph is built, so publishing the fade target before the first device
  callback cannot make that callback jump directly to full gain.
- A preparation, lowering, device-open, missed-boundary, or stream-start error
  leaves the incumbent program and transport active.

The player's transport `Instant` is retained. The activation event therefore
keeps the original origin and reports the nonzero exact `effective_at`, which
also keeps audible-revision source highlighting on the same coordinate system.

## Intentionally outside this slice

- A changed tempo map has no shared seconds projection and still hard-resets.
- A changed output layout still requires a different device graph and
  hard-resets.
- Introducing or removing persistent state still hard-resets.
- The browser event-loop player cannot block for a two-stream handoff and keeps
  the transactional reset.
- Stateful candidate DSP begins with empty history. Analyzer warmup metadata is
  not permission to invent input history; retained-input pre-roll remains its
  own resource and replacement contract.
- Free-running persistent oscillators retain accumulator phase semantics.
  Only controls explicitly represented as transport sequences derive phase
  from the absolute coordinate.

Two independent cpal streams also meet only as precisely as the device and its
buffering permit. The overlap removes the discontinuous gain cut; a future
single-stream swappable graph could improve sample alignment without changing
the transport-origin contract established here.

## Acceptance coverage

- Synth lowering starts a two-slot 100 ms transport sequence at 75 ms and
  125 ms, proving direct and wrapped nonzero phase. Non-finite origins are
  rejected.
- Live scheduling starts a replacement backend at cycle 2, proves the cycle-2
  onset lands at local sample zero, then fills a later window and proves the
  next onset retains the same local clock.
- Program binding lowers a Lua-owned two-slot transport control at 1.5 seconds
  and measures the second slot directly from the persistent processor.
- App policy tests admit only persistent-to-persistent, incompatible,
  unchanged-tempo, unchanged-layout candidates to the crossfade path.
- Native workspace tests and the `wasm32-unknown-unknown` app check cover both
  the implemented handoff and the browser reset fallback.
