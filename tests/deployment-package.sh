#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
release_dir=${1:-"$repo_root/dist/release"}
version=$(cargo pkgid -p farhelm-core | sed 's/.*#//')
hub_name="farhelm-hub-$version-linux-x86_64"
agent_name="farhelm-agent-$version-linux-x86_64"
test_dir=$(mktemp -d)
hub_pid=
trap 'kill "${hub_pid:-}" 2>/dev/null || true; rm -rf "$test_dir"' EXIT

(
  cd "$release_dir"
  sha256sum --check SHA256SUMS
)
tar -C "$test_dir" -xzf "$release_dir/$hub_name.tar.gz"
tar -C "$test_dir" -xzf "$release_dir/$agent_name.tar.gz"

hub_dir="$test_dir/$hub_name"
agent_dir="$test_dir/$agent_name"
for required in \
  "$hub_dir/bin/farhelm-hub" \
  "$hub_dir/bin/farhelmctl" \
  "$hub_dir/console/index.html" \
  "$hub_dir/install.sh" \
  "$agent_dir/bin/farhelm-agent" \
  "$agent_dir/worker/src/farhelm_worker_codex" \
  "$agent_dir/install.sh"; do
  test -e "$required"
done
bash -n "$hub_dir/install.sh" "$agent_dir/install.sh"

export FARHELM_HUB_BIND=127.0.0.1:18787
export FARHELM_HUB_URL=http://127.0.0.1:18787
export FARHELM_ADMIN_USER=package-admin
export FARHELM_ADMIN_PASSWORD=package-password-1234
export FARHELM_AGENT_TOKEN=package-agent-token-with-at-least-32-characters
export FARHELM_CONSOLE_DIR="$hub_dir/console"

"$hub_dir/bin/farhelm-hub" >"$test_dir/hub.log" 2>&1 &
hub_pid=$!
healthy=false
for _ in $(seq 1 40); do
  if "$hub_dir/bin/farhelmctl" health >/dev/null 2>&1; then
    healthy=true
    break
  fi
  sleep 0.25
done
if [[ "$healthy" != true ]]; then
  sed -n '1,120p' "$test_dir/hub.log" >&2
  exit 1
fi

"$agent_dir/bin/farhelm-agent" heartbeat --agent-id package-gpu --hostname package-trainer
curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/api/v1/agents" | grep -q '"agent_id":"package-gpu"'
curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/agents" | grep -q '<title>FarHelm Console</title>'
"$agent_dir/bin/farhelm-agent" worker-smoke --worker-root "$agent_dir/worker"

printf 'Release package smoke passed.\n'
