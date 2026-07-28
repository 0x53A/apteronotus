# Vendored Piccolo snapshot

This is Piccolo revision `ce709eb1dae5c543cbc78e7e12bb80249d88c55f`.
It is vendored because the crates.io 0.3.3 release lacks arithmetic and bitwise
metamethod dispatch and contains a panic when tables grow after string-key
deletion.

Apteronotus carries one source change:

- `src/stdlib/math.rs` uses `SmallRng::seed_from_u64` for the two-word
  `math.randomseed` case. Upstream hardcodes `[u8; 32]`, but `SmallRng::Seed`
  is 16 bytes on wasm32, making the otherwise portable VM fail to compile.
  Apteronotus removes `math.random` and `math.randomseed` from its sandbox, so
  this only makes unreachable stdlib code portable and does not affect authored
  programs.

The upstream MIT and CC0 license files are included beside this note.

This directory is a pinned upstream snapshot, not first-party Apteronotus
source. It participates in workspace builds and tests, but is excluded from the
first-party warnings-as-errors Clippy command; current Clippy emits numerous
style lints against the upstream code.
