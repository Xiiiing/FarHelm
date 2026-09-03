#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

cargo run -p farhelm-hub >"${TMPDIR:-/tmp}/farhelm-hub-smoke.log" 2>&1 &
hub_pid=$!
trap 'kill "$hub_pid" 2>/dev/null || true' EXIT

healthy=false
for _ in $(seq 1 40); do
  if cargo run -q -p farhelmctl -- health >/dev/null 2>&1; then
    healthy=true
    break
  fi
  sleep 0.25
done

if [[ "$healthy" != true ]]; then
  echo "Hub did not become healthy" >&2
  exit 1
fi

cargo run -q -p farhelmctl -- health
cargo run -q -p farhelm-agent -- worker-smoke
