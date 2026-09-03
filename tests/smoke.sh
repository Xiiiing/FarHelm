#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

smoke_dir=$(mktemp -d)
hub_log="$smoke_dir/hub.log"
trap 'kill "${hub_pid:-}" 2>/dev/null || true; rm -rf "$smoke_dir"' EXIT
console_dir="$smoke_dir/console"
mkdir -p "$console_dir"
printf '<!doctype html><title>FarHelm smoke</title>\n' >"$console_dir/index.html"

hub_config="$smoke_dir/hub.toml"
agent_config="$smoke_dir/agent.toml"
cat >"$hub_config" <<EOF
[hub]
bind = "127.0.0.1:8787"
database = "$smoke_dir/hub.db"
console_dir = "$console_dir"
[admin]
user = "smoke-admin"
password = "smoke-password-1234"
[agents]
token = "smoke-agent-token-with-at-least-32-characters"
EOF
cat >"$agent_config" <<EOF
[agent]
id = "smoke-gpu"
hostname = "smoke-trainer"
hub_url = "http://127.0.0.1:8787"
token = "smoke-agent-token-with-at-least-32-characters"
heartbeat_seconds = 15
command_poll_seconds = 2
database = "$smoke_dir/agent.db"
[worker]
python = "python3"
EOF

export FARHELM_HUB_URL=http://127.0.0.1:8787
export FARHELM_ADMIN_USER=smoke-admin
export FARHELM_ADMIN_PASSWORD=smoke-password-1234

cargo build -q -p farhelm-hub -p farhelm-agent

target/debug/farhelm-hub serve --config "$hub_config" >"$hub_log" 2>&1 &
hub_pid=$!

healthy=false
for _ in $(seq 1 40); do
  if target/debug/farhelm-hub health >/dev/null 2>&1; then
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

target/debug/farhelm-hub health
target/debug/farhelm-agent heartbeat --config "$agent_config"

probe_response=$(curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  --header 'Content-Type: application/json' \
  --data '{"idempotency_key":"smoke-probe-request-0001","ttl_secs":60}' \
  "$FARHELM_HUB_URL/api/v1/agents/smoke-gpu/probe")
command_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["command_id"])' <<<"$probe_response")

target/debug/farhelm-agent command-poll \
  --config "$agent_config"

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
target/debug/farhelm-hub serve --config "$hub_config" >>"$hub_log" 2>&1 &
hub_pid=$!
for _ in $(seq 1 40); do
  if target/debug/farhelm-hub health >/dev/null 2>&1; then
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
