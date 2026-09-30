\version "2.24.3"
\header {
  title = "Licht im Gewölbe"
  subtitle = "An original chorale prelude · Light in the Vault"
  composer = "Apteronotus · 2026"
  tagline = ##f
}
\paper { #(set-paper-size "a4") }
\layout { }
global = { \key d \minor \time 4/4 }
soprano = {
  \global
  \tempo "Andante, with space" 4 = 72 \mark \markup "I · Invocation"
  a'2 d''4 c''4 | % 1
  bes'2 a'2 | % 2
  g'2 f'4 e'4 | % 3
  f'2 a'2 | % 4
  bes'2. a'4 | % 5
  a'2 f'2 | % 6
  g'2 f'4 e'4 | % 7
  e'2 g'4 e'4 | % 8
  \mark \markup "II · Answer (add principal)"
  f'2 a'4 d''4 | % 9
  c''2 a'2 | % 10
  g'4 a'4 g'4 e'4 | % 11
  f'2 d'2 | % 12
  g'2 bes'4 a'4 | % 13
  f'2 e'4 d'4 | % 14
  e'2 cis'4 e'4 | % 15
  d'2. r4 | % 16
  \mark \markup "III · Light (add 4′)"
  a'2 c''4 bes'4 | % 17
  g'2 e'2 | % 18
  f'4 a'4 d''2 | % 19
  c''2 a'2 | % 20
  bes'2 d''4 c''4 | % 21
  a'2 f'2 | % 22
  g'4 a'4 bes'4 a'4 | % 23
  g'2 e'2 | % 24
  \mark \markup "IV · Return (remove 4′ partly)"
  a'2 d''4 c''4 | % 25
  bes'2 a'4 g'4 | % 26
  g'2 bes'4 a'4 | % 27
  g'2 e'2 | % 28
  \mark \markup "Coda · soft 8′"
  f'2 a'4 f'4 | % 29
  g'2 f'4 e'4 | % 30
  e'2 cis'2 | % 31
  d'1 | % 32
  \bar "|."
}
alto = {
  \global
  f'1 | % 1
  f'2 e'2 | % 2
  d'1 | % 3
  c'2 f'2 | % 4
  d'2 g'2 | % 5
  d'1 | % 6
  d'2 bes2 | % 7
  cis'2 cis'2 | % 8
  d'1 | % 9
  f'2 e'2 | % 10
  e'2 c'2 | % 11
  d'2 bes2 | % 12
  d'2 g'2 | % 13
  d'2 a2 | % 14
  cis'2 a2 | % 15
  a2. r4 | % 16
  f'2 f'2 | % 17
  e'2 c'2 | % 18
  d'2 f'2 | % 19
  e'2 c'2 | % 20
  f'2 f'2 | % 21
  c'2 c'2 | % 22
  d'2 d'2 | % 23
  e'2 c'2 | % 24
  f'1 | % 25
  f'2 d'2 | % 26
  d'2 g'2 | % 27
  cis'2 cis'2 | % 28
  d'2 d'2 | % 29
  d'2 bes2 | % 30
  cis'2 a2 | % 31
  a1 | % 32
  \bar "|."
}
tenor = {
  \global
  a1 | % 1
  a1 | % 2
  bes2 a2 | % 3
  a1 | % 4
  bes1 | % 5
  a2 d2 | % 6
  g1 | % 7
  g2 a2 | % 8
  a2 f2 | % 9
  a1 | % 10
  g1 | % 11
  f1 | % 12
  bes1 | % 13
  f2 f2 | % 14
  g1 | % 15
  f2. r4 | % 16
  a1 | % 17
  g1 | % 18
  a1 | % 19
  a2 e2 | % 20
  bes1 | % 21
  a2 f2 | % 22
  bes1 | % 23
  g1 | % 24
  a1 | % 25
  bes2 f2 | % 26
  bes1 | % 27
  a2 g2 | % 28
  a1 | % 29
  bes2 g2 | % 30
  g2 e2 | % 31
  fis1 | % 32
  \bar "|."
}
pedal = {
  \global
  d1 | % 1
  c1 | % 2
  bes,1 | % 3
  a,1 | % 4
  g,1 | % 5
  f,1 | % 6
  e,1 | % 7
  a,1 | % 8
  d1 | % 9
  f,1 | % 10
  c1 | % 11
  bes,1 | % 12
  g,1 | % 13
  a,1 | % 14
  a,1 | % 15
  d2. r4 | % 16
  f,1 | % 17
  e,1 | % 18
  d,1 | % 19
  c1 | % 20
  bes,1 | % 21
  a,1 | % 22
  g,1 | % 23
  c1 | % 24
  d1 | % 25
  bes,1 | % 26
  g,1 | % 27
  a,1 | % 28
  f,1 | % 29
  g,1 | % 30
  a,1 | % 31
  d,1 | % 32
  \bar "|."
}

\score {
  <<
    \new PianoStaff \with { instrumentName = "Manual" } <<
      \new Staff <<
        \clef treble
        \new Voice { \voiceOne \soprano }
        \new Voice { \voiceTwo \alto }
      >>
      \new Staff { \clef bass \tenor }
    >>
    \new Staff \with { instrumentName = "Pedal" } { \clef bass \pedal }
  >>
  \layout { }
}
\markup \column {
  \line { "One manual: right hand soprano/alto, left hand tenor. Pedal: Subbass 16′ + soft 8′." }
  \line { "Begin with Gedackt 8′ and a little Salicional; add Principal 8′ at m. 9, Octave 4′ at m. 17." }
  \line { "At m. 25 reduce Octave; at m. 29 return to soft 8′. Pistons or a registrant may assist." }
  \line { "Use gentle finger legato, release repeated keys; breathe together at m. 16. No final ritard required." }
  \line { "Optional: separate flute 8′ for the tune. Let the last D-major chord and the room settle." }
}
