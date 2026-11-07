#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
# This local build targets the current CPU; do not redistribute its binary to
# machines with different instruction sets. Portable release builds remain
# available through plain cargo build --release.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/native-tuned}"
export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=native"
exec cargo run -p apteronotus-app --release "$@"
