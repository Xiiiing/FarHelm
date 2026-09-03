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
export FARHELM_HUB_DATABASE="$smoke_dir/hub.db"
export FARHELM_HUB_URL=http://127.0.0.1:8787
export FARHELM_AGENT_DATABASE="$smoke_dir/agent.db"

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

probe_response=$(curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  --header 'Content-Type: application/json' \
  --data '{"idempotency_key":"smoke-probe-request-0001","ttl_secs":60}' \
  "$FARHELM_HUB_URL/api/v1/agents/smoke-gpu/probe")
command_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["command_id"])' <<<"$probe_response")

target/debug/farhelm-agent command-poll \
  --agent-id smoke-gpu \
  --hostname smoke-trainer

command_status=$(curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/api/v1/commands/$command_id")
if ! grep -q '"state":"completed"' <<<"$command_status"; then
  printf 'Probe command did not reach completed state\n' >&2
  exit 1
fi

duplicate_response=$(curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  --header 'Content-Type: application/json' \
  --data '{"idempotency_key":"smoke-probe-request-0001","ttl_secs":60}' \
  "$FARHELM_HUB_URL/api/v1/agents/smoke-gpu/probe")
duplicate_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["command_id"])' <<<"$duplicate_response")
if [[ "$duplicate_id" != "$command_id" ]]; then
  printf 'Idempotent probe retry created a different command\n' >&2
  exit 1
fi

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

kill "$hub_pid"
wait "$hub_pid"
target/debug/farhelm-hub >>"$hub_log" 2>&1 &
hub_pid=$!
for _ in $(seq 1 40); do
  if target/debug/farhelmctl health >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
persisted_status=$(curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/api/v1/commands/$command_id")
if ! grep -q '"state":"completed"' <<<"$persisted_status"; then
  printf 'Completed command did not survive Hub restart\n' >&2
  exit 1
fi

target/debug/farhelm-agent worker-smoke
printf 'Deployment smoke passed.\n'
