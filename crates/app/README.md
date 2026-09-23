# Apteronotus app

This crate is the first user-facing desktop and browser path: a lexically
highlighted Lua text editor, explicit **Run** (`Ctrl/Cmd+Enter`) and **Stop**
(`Ctrl/Cmd+Period`) commands, activation diagnostics, program-control faders,
realtime audio output, and sounding mini-notation tokens derived from the
audible transport revision. While audio is live, the status bar advances the
latency-adjusted audible cycle rather than displaying the last edit boundary.

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
declare writable controls; user-declared controls appear as live sliders beside
the editor. Engine-owned controls used to fan out analysers and trigger edges
remain hidden.

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
   the live arena; on native, an incompatible persistent-to-persistent edit
   with the same tempo and output layout prepares a second stream at a future
   exact frontier; remaining incompatible edits prepare a replacement at cycle
   zero;
6. fills the audible sequencer and, on the first Run, starts the stream.

Nothing in steps 1–4 changes the active revision or audible sequencer. A
failure therefore leaves the current program filling lookahead. On an ordinary
frontier edit, voices already submitted are not removed; their declared tails
drain while the new revision begins. Native persistent replacement overlaps
the old tails and new stream for a bounded 80 ms. A hard reset instead replaces
the stream and cuts those voices by definition. There is no implicit
keystroke-to-sound path and no audio-thread Lua callback.

Multi-track scheduling is all-or-nothing per window. If a later window of a new
revision nevertheless fails (for example, a latent non-pitch pattern value),
the window has pushed no voices, so the player restores the previous program at
that same frontier and reports the failure.

## Deliberately narrow first slice

The GUI schedules each owned program through its constant or ramped `TempoMap`.
Ordinary note voices, autonomous zero-input runs, whole-stem processors, graph
sends, buses, and program controls share one realtime path. Zero-input runs mix
with routed voices; a run with explicit inputs must consume the complete
flattened main/bus layout and processes that layout in declaration order.

It still reports and rejects, rather than ignores:

- mixed track channel layouts in a main-only program.

A routed persistent program has an authoritative main layout. Mono voice tracks
are broadcast into that layout before buses and whole-stem processors, so a
mono pad may coexist with stereo voices in `synthwave.eod`. Wider incompatible
track layouts remain diagnostics. Program-scope onset detectors publish a
short retained control pulse; the player observes rising edges and schedules
their voices with a 30 ms minimum-latency margin while sampling any
`at_onset(...)` bindings. On native systems the first declared logical
`audio_input` is attached to the default input device when its sample rate
matches the output device. Missing devices, mismatched rates and unbound lanes
use the declaration's silence fallback and produce a visible warning; the host
does not guess at resampling. Browser permission/device attachment remains
pending.

When controls, buses and the patch templates selected by `run` compare
compatible after resolving their arena-scoped handles, a later Run keeps the
existing persistent runtime and `ControlStore`. Inert patch declarations may
change. New voice graphs and patterns are rebound to that live arena and
activate at the ordinary monotonic frontier, so score editing neither rewinds
transport nor resets faders.

On native, changing one persistent program into another while retaining the
tempo map and output layout uses a bounded two-stream replacement. The player
fills the incumbent to a future exact frontier, initializes coordinate-derived
candidate controls from that absolute transport position, proves and opens the
candidate while the incumbent remains active, then starts it silent at the
frontier and overlaps the two device-edge gain stages for 80 ms. Failures before
the candidate starts leave every active handle untouched. Stateful candidate
DSP starts with empty history; retained-input pre-roll remains separate work.

Introducing or removing persistent state, changing the output layout or tempo
map, and all incompatible browser edits still use the transactional hard reset.
That path fully evaluates, validates, lowers, fills, and opens the replacement
before pausing the incumbent and restarting transport at cycle zero. It resets
DSP and control state and may click. Ordinary and persistent-compatible edits
continue to activate at the monotonic frontier without opening another stream.

Choosing a library document retains the last edited buffer in one restore slot.
Browsing further untouched library documents keeps that slot; editing a loaded
document makes it the next buffer to preserve. Native recovery also restores
its saved-file association. This is a single recovery slot, not file history.

The editor's base colouring is deliberately lexical and presentation-only.
Sounding-source highlights appear only while the buffer is byte-identical to
the revision actually audible at the device clock; editing hides them until the
next successful Run. The layouter checks that equality again after same-frame
text edits, so old offsets cannot split new Unicode characters. Not present yet:
graph/type diagnostics while typing, recent documents, autosave, transport
controls, MIDI/device selection and
input-lane attachment, threaded browser evaluation, browser persistent
replacement, retained-input pre-roll, or tempo/layout reconciliation.
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

The browser still sends every incompatible persistent edit through the
transactional cycle-zero reset described above; its main-thread event-loop host
cannot block for the native two-stream handoff. Changed layout or tempo also
hard-resets on every host. The native same-clock/same-layout path now performs
the bounded overlap after preparing the candidate, but device-buffer latency
still bounds how precisely two independent cpal streams meet. Increasing the
buffer does not improve that transition.

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

## Song library

The **Library** menu contains the four short teaching examples followed by the
complete embedded song corpus, including the three variations `nightshift-dub`,
`undertow-skipping-stones` and `small-light-afterimage`, and the newer finite
pieces `seven-lanterns` and `paper-orbits`. The list scrolls on
small windows. Opening a document preserves the displaced editor buffer under
**Restore previous buffer**, and does not evaluate or activate it until Run.
The same sources can be auditioned without an audio device through the
[renderer](../render/README.md), using `--list-songs` and `--song <name>`.

Browsing further untouched library documents retains that recovery buffer.
If you edit a loaded document, its edited text becomes the next buffer saved
when you choose another document. There is one recovery slot.

## Syntax feedback

After 250 ms without a source change, the editor parses and compiles Lua without
executing it. Native checks run on a separate worker with a bounded request
queue; browser checks run after the same debounce on the event loop. The first
problem appears as **Syntax — current buffer**, independently of **Last Run**
errors and of the program still playing. A new edit immediately invalidates the
old result, and results for different source text are discarded.

The red gutter line and **Go to line** / **F8** use original-source coordinates.
The compiler supplies a line, not a token column, so the app does not pretend to
know a narrower location. This checks Lua grammar and compiler rules, including
invalid jumps and const assignments. Unknown binding names, graph types and
mini-notation inside strings still need Run. No evaluation happens while typing.

## Find in the score

**Find** or **Ctrl/Cmd+F** opens a literal, case-sensitive search. **F3** /
**Shift+F3** navigate forwards/backwards and wrap at either end; **Enter** /
**Shift+Enter** do the same while the search field is focused. The counter shows
the selected occurrence and total matches; an amber highlight keeps that occurrence
visible while the search field has focus. Reopening Find selects the previous
query for replacement and searches from the current editor position.
**Escape** closes Find and returns
focus to the selected source text, ready for editing. Matches are nonoverlapping
and recomputed when the query or document changes. Unicode selections use editor
character coordinates; no source text is normalized or evaluated by searching.

## Source files

On desktop, **File** or **Ctrl/Cmd+O** opens a path dialog. Relative paths use the
launch directory. **Save As New File** requires a new destination; **Save** or
**Ctrl/Cmd+S** writes the currently opened/saved path. The `*` on the File button
means the current source is unsaved or differs from that saved snapshot.
You can also open a file without evaluating it from the command line:

```sh
cargo run -- songs/seven-lanterns.eod
```

Writes finish in a temporary file beside the destination before publication.
Save As claims a new name without replacing existing material. Save preserves
ordinary file permissions and refuses an on-disk copy that differs from the
last opened/saved snapshot. That conflict check detects changes already present
at Save; it is not an interprocess lock. Failed reads or writes keep the editor
contents and the current audio program. Reading is limited to the default
one-MiB source budget and requires UTF-8.

In the browser, **File → Import Source** / **Ctrl/Cmd+O** uses the browser's
picker. **Download Source** / **Ctrl/Cmd+S** exports the exact UTF-8 source under
an editable filename. The app cannot silently overwrite a local browser file.
An import which completes after a newer editor change is refused, and invalid
UTF-8 or oversized imports leave the current source intact. Importing, opening
or saving never activates a program; Run remains explicit.

## Browser smoke test

After `./tools/build-web.sh --dev`, run:

```sh
nix-shell -p chromium --run 'node tools/test-web.mjs'
```

Node 22+ and Chromium are the only harness requirements. Set
`APTERONOTUS_CHROMIUM` to an executable path if Chromium is not on `PATH`.
The script serves the local web build, uses a fresh headless browser profile,
and checks UTF-8 search selections, CRLF round trips, stale/oversized import protection, source-line
navigation, zero audio contexts before Run, and continued audio-context lifetime
while an invalid replacement is typed. Screenshots and logs go under
`target/browser-smoke/`. It does not contact a deployed site.
