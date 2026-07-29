#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
port="${APTERONOTUS_WEB_PORT:-8000}"

build_args=("$@")
profile_set=false
for arg in "$@"; do
    case "$arg" in
        --debug|--dev|--release|--profiling|--profile|--profile=*)
            profile_set=true
            ;;
    esac
done
if [[ "$profile_set" == false ]]; then
    build_args=(--dev "${build_args[@]}")
fi

"$repo_root/tools/build-web.sh" "${build_args[@]}"

echo "Serving Apteronotus at http://127.0.0.1:${port}/"
cd "$repo_root/web"
exec python3 -m http.server "$port" --bind 127.0.0.1
