#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

mkdir -p target
exec 9>target/build-web.lock
if ! flock -n 9; then
    echo "Another web build is already running." >&2
    exit 1
fi

wasm-pack build crates/app \
    --target web \
    --out-dir ../../web/pkg \
    --out-name apteronotus_app \
    --no-pack \
    --no-typescript \
    "$@"
