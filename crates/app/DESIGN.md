# The Apteronotus visual language

The interface is a laboratory instrument for a fish that makes electricity: a
dark tank, one discharge, and hard edges everywhere. It is deliberately not
the default look of a framework demo.

Every token lives in `src/style.rs`. `lib.rs` composes widgets and never
names a colour, a size or a margin of its own. If a screen needs something the
style module does not offer, the answer is a new helper there, not a literal
in the layout code.

## Three rules

**Rectangles.** `CornerRadius::ZERO` on every widget state, every panel, every
frame, every chip. No shadows: `Shadow::NONE` on windows and popups. Depth is
expressed by fill value and a one-pixel hairline, never by a bevel or a blur.

**One accent.** `DISCHARGE` (`#00c8ff`) is the only saturated colour in the
chrome, and it means *live or actionable*: the wordmark, the Run button, the
sounding indicator, the cursor, a fader handle, the current line number.
Everything else is a desaturated blue-grey, so the accent never has to compete.
`CAUTION` (amber) and `ALERT` (red) are states, not decoration, and appear only
while the state holds.

**Monospace throughout.** Roboto Mono is the interface face as well as the
editor face. A code-driven instrument has no reason to switch metrics between
the document and the labels around it, and a fixed advance width is what lets
the gutter, the readouts and the status fields line up without a grid widget.

## Type

| style | face | size | used for |
|---|---|---|---|
| `Heading` | Big Shoulders Display Black | 22 | wordmark, panel titles |
| `Monospace` | Roboto Mono | 14 | the document |
| `Body` | Roboto Mono | 13 | status lines, values |
| `Button` | Roboto Mono | 13 | actions |
| `Small` | Roboto Mono | 11 | field labels, key caps |

The display face is the closest freely licensed stand-in for ITC Machine:
uppercase-driven, ultra-heavy, flat-sided, squared counters, very tight. It is
extremely condensed, so it is set *larger* than the text beside it rather than
merely heavier, and it is reserved for the wordmark and section titles — never
for anything a user has to read carefully. Face provenance, licences and the
alternatives that were rejected are in `assets/fonts/PROVENANCE.md`.

## Surfaces

Five fills, darkest to lightest, and each one means a specific thing:

| token | role |
|---|---|
| `VOID` | the document well and other recessed fields |
| `DEEP` | the window body behind panels |
| `PANEL` | chrome: masthead, status bar, diagnostics, controls |
| `RAISED` | a widget at rest |
| `RAISED_HOT` | a widget under the pointer |

`LINE` is the only hairline colour. Chrome closes with a one-pixel outer margin
on the edge that faces the document — `style::chrome(Edge)` — so the seam is
always on the document side and panels never draw a box around themselves.

Interaction is where the accent enters: hovering strokes a widget's edge in
`DISCHARGE`, holding it fills with `DISCHARGE_DIM`. Nothing grows on hover
(`expansion = 0.0`); a control that moves under the pointer breaks the
alignment the monospace grid exists to provide.

## Code colour

The editor is coloured by a purely lexical pass (`src/highlight.rs`). It is
presentation only — it never parses and never reports, because **evaluation
remains the one explicit boundary** at which a document becomes a program.

Two things get to be loud, because they are what carries musical meaning:
the sandbox vocabulary (`CODE_BUILTIN`, the accent itself) and string literals
(`CODE_STRING`), which is where mini-notation lives. Mini-notation is given the
single warm colour in the palette so a rhythm reads as a distinct object inside
an otherwise blue document. Lua's own keywords sit a step back in a muted blue,
identifiers are plain `TEXT`, and comments recede almost into the well.

The layouter never wraps. A code editor scrolls sideways; wrapping would also
desynchronise the gutter, which counts newlines.

### Sounding source

Mini-notation tokens receive the discharge wash only while their events overlap
the latency-adjusted transport window. The window is derived on every frame by
querying the audible program; there is no cursor, timer or retained flash
history. Held events therefore stay lit and very short events remain visible
for a 60 ms retrospective window, while continuous pattern signals are ignored.

“Audible” is deliberately not “last accepted.” Compatible edits are staged at
the scheduler frontier and promoted only when the device clock reaches their
exact `effective_at`; multiple accepted revisions remain queued in order. A
new transport origin, Stop, or runtime uncertainty clears the mirror. Finally,
the editor buffer must be byte-identical to the source that produced the
audible program. The first typed byte after Run turns every highlight off rather
than attempting an unsafe span remap.

## Motion

The transport indicator is a row of bars whose heights are a function of
`input.time` and bar index. It holds no state, so it cannot drift out of step
with anything, and it settles flat the moment nothing is sounding — the same
*derive from coordinates* rule the engine is built on, applied to a decoration.

That is the only animation. Nothing else fades, slides or eases.

## What the layout says

- **Masthead**: identity, then the transport actions, then the document
  actions, then the transport readout. Run is the only *filled* control in the
  application; Stop, Library and File are outlined, and Stop greys out when nothing
  is sounding, so the accent always points at the thing that starts sound
  rather than at the thing that ends it. In the Library menu the document
  currently loaded is named in the accent and marked `open`. Choosing a
  document retains the last edited buffer while browsing; recovery is an
  explicit menu item rather than a confirmation dialog on every choice.
- **Document**: a recessed well with a gutter. The current line number is
  accented; that is the only place the editor comments on your cursor.
- **Controls**: program-declared writable controls as faders, right of the
  document, present only when a program declares them.
- **Diagnostics**: current-buffer syntax problems and last-Run failures are
  separate labeled messages above the status bar, in `ALERT`, selectable.
  A source-line action moves the editor cursor without evaluating the text.
- **Status bar**: what is sounding on the left, what the document is on the
  right. Its cycle readout is derived each frame from the audible program's
  tempo map and device transport origin; an accepted future revision boundary
  is not mislabeled as the current playhead.

A panel that has nothing to say is not drawn. There are no empty states and no
placeholder chrome.

The lexical colour pass still does no parsing. A separate compile-only checker
runs after a typing pause and supplies the first original-source line diagnostic.
Its red gutter number takes precedence over the cursor accent. Syntax feedback
never changes the transport or the active program.
