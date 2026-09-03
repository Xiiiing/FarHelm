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
  "$hub_dir/rollback.sh" \
  "$hub_dir/uninstall.sh" \
  "$agent_dir/bin/farhelm-agent" \
  "$agent_dir/worker/src/farhelm_worker_codex" \
  "$agent_dir/install.sh" \
  "$agent_dir/run.sh" \
  "$agent_dir/rollback.sh" \
  "$agent_dir/uninstall.sh"; do
  test -e "$required"
done
bash -n \
  "$hub_dir/install.sh" \
  "$hub_dir/rollback.sh" \
  "$hub_dir/uninstall.sh" \
  "$agent_dir/install.sh" \
  "$agent_dir/run.sh" \
  "$agent_dir/rollback.sh" \
  "$agent_dir/uninstall.sh"
test "$(<"$hub_dir/RELEASE_TAG")" = "V$version"
test "$(<"$agent_dir/RELEASE_TAG")" = "V$version"
if grep -Eq '^(User|Group)=' "$agent_dir/farhelm-agent.service"; then
  printf 'Agent user unit must not declare a system User or Group.\n' >&2
  exit 1
fi

export FARHELM_HUB_BIND=127.0.0.1:18787
export FARHELM_HUB_URL=http://127.0.0.1:18787
export FARHELM_ADMIN_USER=package-admin
export FARHELM_ADMIN_PASSWORD=package-password-1234
export FARHELM_AGENT_TOKEN=package-agent-token-with-at-least-32-characters
export FARHELM_CONSOLE_DIR="$hub_dir/console"
export FARHELM_HUB_DATABASE="$test_dir/hub.db"
export FARHELM_AGENT_DATABASE="$test_dir/agent.db"

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
probe_response=$(curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  --header 'Content-Type: application/json' \
  --data '{"idempotency_key":"package-probe-request-0001","ttl_secs":60}' \
  "$FARHELM_HUB_URL/api/v1/agents/package-gpu/probe")
command_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["command_id"])' <<<"$probe_response")
"$agent_dir/bin/farhelm-agent" command-poll \
  --agent-id package-gpu \
  --hostname package-trainer
curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/api/v1/commands/$command_id" | grep -q '"state":"completed"'
curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/api/v1/agents" | grep -q '"agent_id":"package-gpu"'
curl --fail --silent --show-error \
  --user "$FARHELM_ADMIN_USER:$FARHELM_ADMIN_PASSWORD" \
  "$FARHELM_HUB_URL/agents" | grep -q '<title>FarHelm Console</title>'
"$agent_dir/bin/farhelm-agent" worker-smoke --worker-root "$agent_dir/worker"

(
  mock_bin="$test_dir/mock-bin"
  install -d -m 0755 "$mock_bin"
  cat >"$mock_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
if [[ ${1:-} == --user && ${2:-} == is-active && -n ${MOCK_SYSTEMCTL_FAIL_FILE:-} && -e $MOCK_SYSTEMCTL_FAIL_FILE ]]; then
  exit 1
fi
exit 0
EOF
  printf '#!/usr/bin/env bash\nprintf "no\\n"\n' >"$mock_bin/loginctl"
  chmod 0755 "$mock_bin/systemctl" "$mock_bin/loginctl"
  export PATH="$mock_bin:$PATH"
  export XDG_DATA_HOME="$test_dir/user-data"
  export XDG_CONFIG_HOME="$test_dir/user-config"
  export FARHELM_HUB_URL=https://farhelm.example.test
  export FARHELM_AGENT_TOKEN=package-agent-token-with-at-least-32-characters
  export FARHELM_AGENT_ID=package-user-install
  export FARHELM_AGENT_HOSTNAME=package-user-host
  "$agent_dir/install.sh"
  installed_root="$XDG_DATA_HOME/farhelm-agent"
  test -L "$installed_root/current"
  test "$(basename "$(readlink -f "$installed_root/current")")" = "$version"
  test -x "$installed_root/current/bin/farhelm-agent"
  test -x "$installed_root/current/run.sh"
  test -x "$installed_root/current/rollback.sh"
  test -x "$installed_root/uninstall.sh"
  test "$(stat -c '%a' "$installed_root/config/agent.env")" = 600
  grep -q "^FARHELM_AGENT_DATABASE=$installed_root/state/agent.db$" \
    "$installed_root/config/agent.env"
  unit_file="$XDG_CONFIG_HOME/systemd/user/farhelm-agent.service"
  test -f "$unit_file"
  test "$(stat -c '%a' "$unit_file")" = 600
  ! grep -q '__FARHELM_' "$unit_file"
  grep -q "$installed_root/current/run.sh" "$unit_file"

  config_hash=$(sha256sum "$installed_root/config/agent.env" | cut -d' ' -f1)
  printf 'persistent-state\n' >"$installed_root/state/preserved.txt"
  for next_version in 0.2.0 0.2.1; do
    upgrade_dir="$test_dir/farhelm-agent-$next_version-linux-x86_64"
    cp -a "$agent_dir" "$upgrade_dir"
    printf '%s\n' "$next_version" >"$upgrade_dir/VERSION"
    printf 'V%s\n' "$next_version" >"$upgrade_dir/RELEASE_TAG"
    printf '#!/usr/bin/env bash\nif [[ ${1:-} == --version ]]; then printf "farhelm-agent %s\\n"; fi\n' "$next_version" >"$upgrade_dir/bin/farhelm-agent"
    chmod 0755 "$upgrade_dir/bin/farhelm-agent"
    FARHELM_UPGRADE=1 FARHELM_INSTALL_ROOT="$installed_root" "$upgrade_dir/install.sh"
    test "$(basename "$(readlink -f "$installed_root/current")")" = "$next_version"
    test "$(sha256sum "$installed_root/config/agent.env" | cut -d' ' -f1)" = "$config_hash"
    test "$(<"$installed_root/state/preserved.txt")" = persistent-state
  done
  test "$(basename "$(readlink -f "$installed_root/previous")")" = 0.2.0
  "$installed_root/rollback.sh"
  test "$(basename "$(readlink -f "$installed_root/current")")" = 0.2.0
  test "$(basename "$(readlink -f "$installed_root/previous")")" = 0.2.1
  test "$(sha256sum "$installed_root/config/agent.env" | cut -d' ' -f1)" = "$config_hash"
  test "$(<"$installed_root/state/preserved.txt")" = persistent-state

  failed_upgrade_dir="$test_dir/farhelm-agent-0.3.0-linux-x86_64"
  cp -a "$agent_dir" "$failed_upgrade_dir"
  printf '0.3.0\n' >"$failed_upgrade_dir/VERSION"
  printf 'V0.3.0\n' >"$failed_upgrade_dir/RELEASE_TAG"
  if FARHELM_UPGRADE=1 FARHELM_INSTALL_ROOT="$installed_root" "$failed_upgrade_dir/install.sh"; then
    printf 'Installer accepted a package whose binary version did not match VERSION.\n' >&2
    exit 1
  fi
  test "$(basename "$(readlink -f "$installed_root/current")")" = 0.2.0

  printf '#!/usr/bin/env bash\nif [[ ${1:-} == --version ]]; then printf "farhelm-agent 0.3.0\\n"; fi\n' \
    >"$failed_upgrade_dir/bin/farhelm-agent"
  chmod 0755 "$failed_upgrade_dir/bin/farhelm-agent"
  export MOCK_SYSTEMCTL_FAIL_FILE="$test_dir/systemctl-health-failure"
  touch "$MOCK_SYSTEMCTL_FAIL_FILE"
  if FARHELM_UPGRADE=1 FARHELM_INSTALL_ROOT="$installed_root" "$failed_upgrade_dir/install.sh"; then
    printf 'Installer did not fail when the upgraded service was unhealthy.\n' >&2
    exit 1
  fi
  rm -f "$MOCK_SYSTEMCTL_FAIL_FILE"
  test "$(basename "$(readlink -f "$installed_root/current")")" = 0.2.0
  test "$(basename "$(readlink -f "$installed_root/previous")")" = 0.3.0
  test "$(sha256sum "$installed_root/config/agent.env" | cut -d' ' -f1)" = "$config_hash"
  test "$(<"$installed_root/state/preserved.txt")" = persistent-state

  "$installed_root/uninstall.sh"
  test ! -e "$installed_root"
  test ! -e "$unit_file"
)

printf 'Release package smoke passed.\n'
