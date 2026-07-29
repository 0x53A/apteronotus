# Vendored fonts

The application embeds its fonts with `include_bytes!` rather than reading
system fonts, so the interface is identical on every machine and the same
bytes can later be shipped to a browser build.

## RobotoMono-Regular.ttf, RobotoMono-Medium.ttf

Roboto Mono 3.001, copied from the nixpkgs `roboto-mono` derivation
(`share/fonts/truetype/`). Licensed under Apache-2.0; see
`LICENSE-RobotoMono.txt`.

This is the interface text face and the editor face.

## BigShouldersDisplay-Black.ttf

Big Shoulders Display, licensed under the SIL Open Font License 1.1; see
`OFL-BigShouldersDisplay.txt`.

Upstream ships only a variable font, and neither ab_glyph nor epaint can select
a variation axis, so this file is a **static instance at `wght=900`** produced
from the upstream variable font:

```sh
curl -LO 'https://raw.githubusercontent.com/google/fonts/main/ofl/bigshouldersdisplay/BigShouldersDisplay%5Bwght%5D.ttf'
uv run --with fonttools python -c '
from fontTools import ttLib
from fontTools.varLib import instancer
font = ttLib.TTFont("BigShouldersDisplay[wght].ttf")
instancer.instantiateVariableFont(font, {"wght": 900}).save("BigShouldersDisplay-Black.ttf")
'
```

Instancing is a derivative work, which the OFL permits; the OFL text travels
with the file and the family is not renamed.

It is used only for the wordmark and section headings. It was chosen as the
closest freely licensed face to ITC Machine: uppercase-driven, ultra-heavy,
flat-sided, with squared counters and very tight fitting. Anton, Archivo Black,
Bungee, Kanit, Rubik Mono One, Saira Extra Condensed, Squada One, Michroma and
Teko were rendered side by side against it; Anton and Bungee round their
counters, Saira Extra Condensed keeps a conventional grotesque skeleton, and
the rest are either too wide or too light for a toolbar.
