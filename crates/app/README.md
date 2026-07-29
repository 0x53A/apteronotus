# Apteronotus app

This crate is the first user-facing desktop and browser path: a lexically
highlighted Lua text editor, explicit **Run** (`Ctrl/Cmd+Enter`) and **Stop**
(`Ctrl/Cmd+Period`) commands, activation diagnostics, program-control faders,
and realtime audio output.

Run the native app from the repository's Nix development shell:

```text
cargo run -p apteronotus-app
```

Build and serve the wasm app with:

```text
./tools/run-web.sh
```

That command uses `wasm-pack` directly—there is no Trunk project—and serves
`web/` on `http://127.0.0.1:8000`. The wasm library registers a reusable
`<apteronotus-app>` custom element whose shadow root owns the egui canvas. The
static `web/index.html` is only one host for that element.

`.github/workflows/pages.yml` builds the release package on every push to
`main` and deploys the complete `web/` directory through GitHub Pages. The
workflow can also be run manually.

Audio is opened lazily on the first successful Run. Stop closes the stream,
drops the persistent arena, and returns transport to cycle zero; it is not a
pause/resume operation. The starter document is a
real `voice { ... }` plus `play(v, "c4 e4 g4")` program and goes through the
same owned `Program`, revision, scheduler, `GraphTemplate`, fundsp sequencer and
cpal output path as the offline acceptance tests. A program may instead start
autonomous `run(patch)` graphs, route voices and patches through buses, and
declare writable controls; those controls appear as live sliders beside the
editor.

## Activation and failure semantics

Typing only edits text. Run is the evaluation boundary; this app does not
evaluate on each keystroke.

On native systems, the window thread never evaluates Lua or constructs graphs.
A bounded evaluation job builds each candidate while the player worker
continues filling the active program's lookahead. The player owns revision
state, scheduling and the cpal sequencer. The audio callback owns only fundsp's
realtime backend.

The browser build cannot use `std::thread::spawn` without committing the site
to shared-memory headers and cross-origin isolation. It evaluates only at the
explicit Run boundary and advances lookahead once per UI frame instead. Run is
pumped during the trusted click/key event so CPAL can create and resume its
WebAudio context under browser autoplay policy. Lua still never reaches the
audio callback: CPAL renders the same lowered fundsp backend used natively.

For a Run, the player:

1. evaluates a fresh sandbox into an owned `Program`;
2. applies the GUI host's playable-shape checks;
3. constructs any persistent control/patch arena and atomically lowers every
   track in the first scheduling window into a throwaway sequencer;
4. opens audio if necessary;
5. publishes an ordinary revision at the live scheduler's next unscheduled
   exact cycle coordinate when it is voice-only or its persistent data matches
   the live arena; otherwise it prepares a complete replacement stream at cycle
   zero;
6. fills the audible sequencer and, on the first Run, starts the stream.

Nothing in steps 1–4 changes the active revision or audible sequencer. A
failure therefore leaves the current program filling lookahead. On an ordinary
frontier edit, voices already submitted are not removed; their declared tails
drain while the new revision begins. A hard reset instead replaces the stream
and cuts those voices by definition. There is no implicit keystroke-to-sound
path and no audio-thread Lua callback.

Multi-track scheduling is all-or-nothing per window. If a later window of a new
revision nevertheless fails (for example, a latent non-pitch pattern value),
the window has pushed no voices, so the player restores the previous program at
that same frontier and reports the failure.

## Deliberately narrow first slice

The GUI currently uses a fixed 120 BPM transport. Ordinary note voices,
autonomous zero-input runs, whole-stem processors, graph sends, buses, and
program controls share one realtime path. Zero-input runs mix with routed
voices; a run with explicit inputs must consume the complete flattened
main/bus layout and processes that layout in declaration order.

It still reports and rejects, rather than ignores:

- live-input graphs;
- mixed track channel layouts.

When controls, buses and the patch templates selected by `run` compare
compatible after resolving their arena-scoped handles, a later Run keeps the
existing persistent runtime and `ControlStore`. Inert patch declarations may
change. New voice graphs and patterns are rebound to that live arena and
activate at the ordinary monotonic frontier, so score editing neither rewinds
transport nor resets faders.

Until replacement crossfade is implemented, a Run that introduces, removes, or
changes persistent state—or changes the device output layout—performs an
explicit hard reset. The player fully evaluates, validates, lowers, fills, and
opens the replacement while the old stream is still active. Only then does it
pause the old stream, start the replacement, and restart transport at cycle
zero. A failure before that switch leaves the old program sounding; if starting
the replacement fails after pausing, the player attempts to resume it.

This reset is a temporary host policy, not revision reconciliation. It discards
DSP state and resets program controls to their declared defaults, and it may
click because there is no blend. Ordinary and persistent-compatible edits with
an unchanged layout activate at the monotonic scheduling frontier. The
remaining milestone is a bounded crossfade for incompatible replacement, so
reset must not become the permanent answer.

Choosing a shipped example retains the displaced editor contents in one
in-memory restore slot exposed at the top of the Examples menu. This avoids a
confirmation dialog on the common path while making the only unsaved user state
recoverable. A later example choice replaces that slot; it is not file history.

The editor's colouring is deliberately lexical and presentation-only.
Not present yet: parser/type diagnostics while typing, sounding-source
highlighting, files/recent documents, transport controls, MIDI/device
selection, live inputs, threaded browser evaluation, or blended/continuous
persistent patch replacement.
Those are UI and host integrations over framework types that already exist;
they are not new Lua syntax.

## Known realtime limitations

In the browser, CPAL fills and schedules WebAudio buffers from main-thread
callbacks. Apteronotus currently evaluates a Run and advances scheduler
lookahead on that same thread because the Pages deployment does not opt into
shared-memory headers and cross-origin isolation. The 2048-frame WebAudio
buffer below gives ordinary playback useful headroom, but a sufficiently
expensive evaluation—or any other long browser main-thread task—can still miss
an audio deadline and produce a brief dropout. Evaluation fuel and memory
budgets bound language work and failure, but they are not a wall-clock audio
deadline.

Removing that contention is not a buffer-size tweak. It requires either a Web
Worker boundary that returns an owned/transferable program to the UI runtime,
or an AudioWorklet rendering path that no longer depends on CPAL's main-thread
buffer scheduler. Neither boundary is implemented yet.

An incompatible persistent-program or output-layout edit uses the transactional
hard reset described above. The old stream and its tails stop before the
replacement starts at cycle zero, with no gain overlap, so the transition may
click and resets persistent DSP and control state. The intended fix is a
bounded two-stream crossfade after the candidate has been completely prepared;
increasing the device buffer does not address this transition.

## Realtime headroom

The native callback renders through fundsp's allocation-free, SIMD block path
in chunks of 64 frames. It requests a 512-frame device buffer when the reported
device range permits that size, clamps the request to narrower known ranges,
and retains the platform default when the range is unknown. At 48 kHz this is
about 10.7 ms of device-buffer latency: enough desktop scheduling headroom
without committing a future MIDI instrument to a large delay.

The browser retains CPAL's 2048-frame WebAudio default. CPAL's browser backend
double-buffers scheduled `AudioBufferSourceNode`s from main-thread callbacks,
and the enormous buffer range it reports is synthetic. Forcing the native
512-frame target left too little scheduling headroom whenever egui, Lua
evaluation, or the browser occupied that same thread.

The workspace development profile optimizes `fundsp`, `apteronotus-synth` and
`apteronotus-live` while leaving the GUI and Lua boundary debuggable. A normal
`cargo run -p apteronotus-app` therefore no longer asks audio-rate DSP to meet
device deadlines using unoptimized code.
