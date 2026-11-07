#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
port="${APTERONOTUS_WEB_PORT:-8000}"

# The shared build command defaults to release; --dev remains an explicit opt-in.
"$repo_root/tools/build-web.sh" "$@"

echo "Serving Apteronotus at http://127.0.0.1:${port}/"
cd "$repo_root/web"
exec python3 -m http.server "$port" --bind 127.0.0.1
