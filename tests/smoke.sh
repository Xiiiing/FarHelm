#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

smoke_dir=$(mktemp -d)
hub_log="$smoke_dir/hub.log"
trap 'kill "${hub_pid:-}" 2>/dev/null || true; rm -rf "$smoke_dir"' EXIT
printf '<!doctype html><title>FarHelm smoke</title>\n' >"$smoke_dir/index.html"

export FARHELM_ADMIN_USER=smoke-admin
export FARHELM_ADMIN_PASSWORD=smoke-password-1234
export FARHELM_AGENT_TOKEN=smoke-agent-token-with-at-least-32-characters
export FARHELM_CONSOLE_DIR="$smoke_dir"
export FARHELM_HUB_URL=http://127.0.0.1:8787

cargo build -q -p farhelm-hub -p farhelmctl -p farhelm-agent

target/debug/farhelm-hub >"$hub_log" 2>&1 &
hub_pid=$!

healthy=false
for _ in $(seq 1 40); do
  if target/debug/farhelmctl health >/dev/null 2>&1; then
    healthy=true
    break
  fi
  sleep 0.25
done

if [[ "$healthy" != true ]]; then
  printf 'Hub did not become healthy. Log follows:\n' >&2
  sed -n '1,120p' "$hub_log" >&2
  exit 1
fi

target/debug/farhelmctl health
target/debug/farhelm-agent heartbeat \
  --agent-id smoke-gpu \
  --hostname smoke-trainer

agent_list=$(curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/api/v1/agents")
if ! grep -q '"agent_id":"smoke-gpu"' <<<"$agent_list"; then
  printf 'Authenticated Agent list did not contain smoke-gpu\n' >&2
  exit 1
fi

if curl --fail --silent "$FARHELM_HUB_URL/api/v1/agents" >/dev/null 2>&1; then
  printf 'Protected Agent list accepted an unauthenticated request\n' >&2
  exit 1
fi

curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/agents" | grep -q 'FarHelm smoke'

target/debug/farhelm-agent worker-smoke
printf 'Deployment smoke passed.\n'
