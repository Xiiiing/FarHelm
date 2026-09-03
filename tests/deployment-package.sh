#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
release_dir=${1:-"$repo_root/dist/release"}
version=$(cargo pkgid -p farhelm-core | sed 's/.*#//')
hub_versioned="$release_dir/farhelm-hub-$version-linux-x86_64"
agent_versioned="$release_dir/farhelm-agent-$version-linux-x86_64"
hub_stable="$release_dir/farhelm-hub-linux-x86_64"
agent_stable="$release_dir/farhelm-agent-linux-x86_64"
test_dir=$(mktemp -d)
hub_pid=
trap 'kill "${hub_pid:-}" 2>/dev/null || true; rm -rf "$test_dir"' EXIT

(
  cd "$release_dir"
  sha256sum --check SHA256SUMS
)
for binary in "$hub_versioned" "$agent_versioned" "$hub_stable" "$agent_stable"; do
  test -x "$binary"
done
cmp "$hub_versioned" "$hub_stable"
cmp "$agent_versioned" "$agent_stable"
env -i "$hub_stable" --version | grep -Fxq "farhelm-hub $version"
env -i "$agent_stable" --version | grep -Fxq "farhelm-agent $version"

tar -C "$test_dir" -xzf "$release_dir/farhelm-hub-$version-linux-x86_64.tar.gz"
tar -C "$test_dir" -xzf "$release_dir/farhelm-agent-$version-linux-x86_64.tar.gz"
hub_compat="$test_dir/farhelm-hub-$version-linux-x86_64"
agent_compat="$test_dir/farhelm-agent-$version-linux-x86_64"
for required in \
  "$hub_compat/bin/farhelm-hub" \
  "$hub_compat/install.sh" \
  "$agent_compat/bin/farhelm-agent" \
  "$agent_compat/install.sh"; do
  test -e "$required"
done
bash -n "$hub_compat/install.sh" "$agent_compat/install.sh"
test "$(<"$hub_compat/VERSION")" = "$version"
test "$(<"$agent_compat/RELEASE_TAG")" = "V$version"

hub_config="$test_dir/hub.toml"
agent_config="$test_dir/agent.toml"
cat >"$hub_config" <<EOF
[hub]
bind = "127.0.0.1:18787"
database = "$test_dir/hub.db"
[admin]
user = "package-admin"
password = "package-password-1234"
[agents]
token = "package-agent-token-with-at-least-32-characters"
EOF
cat >"$agent_config" <<EOF
[agent]
id = "package-gpu"
hostname = "package-trainer"
hub_url = "http://127.0.0.1:18787"
token = "package-agent-token-with-at-least-32-characters"
heartbeat_seconds = 15
command_poll_seconds = 2
database = "$test_dir/agent.db"
[worker]
python = "python3"
EOF

"$hub_stable" serve --config "$hub_config" >"$test_dir/hub.log" 2>&1 &
hub_pid=$!
healthy=false
for _ in $(seq 1 40); do
  if "$hub_stable" health --hub http://127.0.0.1:18787 >/dev/null 2>&1; then
    healthy=true
    break
  fi
  sleep 0.25
done
if [[ "$healthy" != true ]]; then
  sed -n '1,120p' "$test_dir/hub.log" >&2
  exit 1
fi

"$agent_stable" heartbeat --config "$agent_config"
probe_response=$(curl --fail --silent --show-error \
  --user 'package-admin:package-password-1234' \
  --header 'Content-Type: application/json' \
  --data '{"idempotency_key":"package-probe-request-0001","ttl_secs":60}' \
  http://127.0.0.1:18787/api/v1/agents/package-gpu/probe)
command_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["command_id"])' <<<"$probe_response")
"$agent_stable" command-poll --config "$agent_config"
curl --fail --silent --show-error \
  --user 'package-admin:package-password-1234' \
  "http://127.0.0.1:18787/api/v1/commands/$command_id" | grep -q '"state":"completed"'
curl --fail --silent --show-error \
  --user 'package-admin:package-password-1234' \
  http://127.0.0.1:18787/agents | grep -q '<title>FarHelm Console</title>'

mock_bin="$test_dir/mock-bin"
install -d -m 0755 "$mock_bin"
cat >"$mock_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
for argument in "$@"; do
  if [[ -n ${FARHELM_TEST_SYSTEMCTL_FAIL:-} ]] && [[ "$argument" == "$FARHELM_TEST_SYSTEMCTL_FAIL" ]]; then
    printf 'injected systemctl failure for %s\n' "$argument" >&2
    exit 1
  fi
done
exit 0
EOF
cat >"$mock_bin/loginctl" <<'EOF'
#!/usr/bin/env bash
printf 'yes\n'
EOF
chmod 0755 "$mock_bin/systemctl" "$mock_bin/loginctl"

(
  export PATH="$mock_bin:$PATH"
  export HOME="$test_dir/home"
  export XDG_DATA_HOME="$test_dir/user-data"
  export XDG_CONFIG_HOME="$test_dir/user-config"
  export XDG_BIN_HOME="$test_dir/user-bin"
  export USER=package-user
  export FARHELM_HUB_URL=http://127.0.0.1:18787
  export FARHELM_AGENT_TOKEN=package-agent-token-with-at-least-32-characters
  export FARHELM_AGENT_ID=package-installed
  export FARHELM_AGENT_HOSTNAME=package-installed-host
  mkdir -p "$HOME"

  "$agent_stable" install
  installed_binary="$XDG_BIN_HOME/farhelm-agent"
  installed_config="$XDG_CONFIG_HOME/farhelm/agent.toml"
  installed_data="$XDG_DATA_HOME/farhelm"
  unit_file="$XDG_CONFIG_HOME/systemd/user/farhelm-agent.service"
  test -x "$installed_binary"
  test -f "$installed_config"
  test "$(stat -c '%a' "$installed_config")" = 600
  test -f "$installed_data/runtime/codex-worker/$version/src/farhelm_worker_codex/__main__.py"
  grep -q "$installed_binary" "$unit_file"
  grep -q "$installed_config" "$unit_file"
  ! grep -q 'current/run.sh' "$unit_file"

  "$installed_binary" doctor
  "$installed_binary" status
  "$installed_binary" restart
  set +e
  timeout 1 "$installed_binary" run --config "$installed_config" >"$test_dir/installed-agent.log" 2>&1
  installed_status=$?
  set -e
  if [[ "$installed_status" -ne 124 ]]; then
    sed -n '1,80p' "$test_dir/installed-agent.log" >&2
    printf 'Installed Agent did not remain running.\n' >&2
    exit 1
  fi

  config_hash=$(sha256sum "$installed_config" | cut -d' ' -f1)
  printf 'persistent-state\n' >"$installed_data/state/preserved.txt"
  "$agent_stable" install
  test -f "$XDG_BIN_HOME/farhelm-agent.previous"
  "$installed_binary" rollback
  test "$(sha256sum "$installed_config" | cut -d' ' -f1)" = "$config_hash"
  test "$(<"$installed_data/state/preserved.txt")" = persistent-state

  "$installed_binary" uninstall
  test ! -e "$installed_binary"
  test ! -e "$installed_config"
  test ! -e "$installed_data"
  test ! -e "$unit_file"
)

(
  export PATH="$mock_bin:$PATH"
  export HOME="$test_dir/failure-home"
  export XDG_DATA_HOME="$test_dir/failure-data"
  export XDG_CONFIG_HOME="$test_dir/failure-config"
  export XDG_BIN_HOME="$test_dir/failure-bin"
  export USER=failure-user
  export FARHELM_HUB_URL=http://127.0.0.1:18787
  export FARHELM_AGENT_TOKEN=package-agent-token-with-at-least-32-characters
  export FARHELM_AGENT_ID=failure-agent
  mkdir -p "$HOME"

  "$agent_stable" install
  installed_binary="$XDG_BIN_HOME/farhelm-agent"
  installed_config="$XDG_CONFIG_HOME/farhelm/agent.toml"
  installed_data="$XDG_DATA_HOME/farhelm"
  unit_file="$XDG_CONFIG_HOME/systemd/user/farhelm-agent.service"
  printf 'must-survive\n' >"$installed_data/state/preserved.txt"
  binary_hash=$(sha256sum "$installed_binary" | cut -d' ' -f1)
  config_hash=$(sha256sum "$installed_config" | cut -d' ' -f1)
  unit_hash=$(sha256sum "$unit_file" | cut -d' ' -f1)

  set +e
  FARHELM_TEST_SYSTEMCTL_FAIL=enable "$agent_stable" install >"$test_dir/failure-install.log" 2>&1
  failure_status=$?
  set -e
  test "$failure_status" -ne 0
  grep -q 'previous service restored' "$test_dir/failure-install.log"
  test "$(sha256sum "$installed_binary" | cut -d' ' -f1)" = "$binary_hash"
  test "$(sha256sum "$installed_config" | cut -d' ' -f1)" = "$config_hash"
  test "$(sha256sum "$unit_file" | cut -d' ' -f1)" = "$unit_hash"
  test "$(<"$installed_data/state/preserved.txt")" = must-survive

  "$installed_binary" uninstall
)

(
  export PATH="$mock_bin:$PATH"
  export HOME="$test_dir/fresh-failure-home"
  export XDG_DATA_HOME="$test_dir/fresh-failure-data"
  export XDG_CONFIG_HOME="$test_dir/fresh-failure-config"
  export XDG_BIN_HOME="$test_dir/fresh-failure-bin"
  export USER=fresh-failure-user
  export FARHELM_HUB_URL=http://127.0.0.1:18787
  export FARHELM_AGENT_TOKEN=package-agent-token-with-at-least-32-characters
  export FARHELM_AGENT_ID=fresh-failure-agent
  mkdir -p "$HOME"

  set +e
  FARHELM_TEST_SYSTEMCTL_FAIL=enable "$agent_stable" install >"$test_dir/fresh-failure-install.log" 2>&1
  failure_status=$?
  set -e
  test "$failure_status" -ne 0
  test ! -e "$XDG_BIN_HOME/farhelm-agent"
  test ! -e "$XDG_CONFIG_HOME/farhelm/agent.toml"
  test ! -e "$XDG_CONFIG_HOME/systemd/user/farhelm-agent.service"
  test ! -e "$XDG_DATA_HOME/farhelm"
)

(
  export PATH="$mock_bin:$PATH"
  export HOME="$test_dir/legacy-home"
  export XDG_DATA_HOME="$test_dir/legacy-data"
  export XDG_CONFIG_HOME="$test_dir/legacy-config"
  export XDG_BIN_HOME="$test_dir/legacy-bin"
  export USER=legacy-user
  legacy_root="$XDG_DATA_HOME/farhelm-agent"
  mkdir -p "$HOME" "$legacy_root/config" "$legacy_root/state"
  printf 'foreground\n' >"$legacy_root/INSTALL_MODE"
  cat >"$legacy_root/config/agent.env" <<EOF
FARHELM_HUB_URL=http://127.0.0.1:18787
FARHELM_AGENT_TOKEN=package-agent-token-with-at-least-32-characters
FARHELM_AGENT_ID=legacy-agent
FARHELM_AGENT_HOSTNAME=legacy-host
FARHELM_AGENT_DATABASE=$legacy_root/state/agent.db
EOF
  python3 - "$legacy_root/state/agent.db" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("create table preserved(value text)")
db.execute("insert into preserved values ('yes')")
db.commit()
PY
  FARHELM_UPGRADE=1 FARHELM_INSTALL_ROOT="$legacy_root" "$agent_compat/install.sh"
  test -x "$XDG_BIN_HOME/farhelm-agent"
  test -f "$XDG_CONFIG_HOME/farhelm/agent.toml"
  test -f "$XDG_DATA_HOME/farhelm/state/agent.db"
  test ! -e "$XDG_CONFIG_HOME/systemd/user/farhelm-agent.service"
  test ! -e "$legacy_root"
  python3 - "$XDG_DATA_HOME/farhelm/state/agent.db" <<'PY'
import sqlite3, sys
assert sqlite3.connect(sys.argv[1]).execute("select value from preserved").fetchone() == ('yes',)
PY
  "$XDG_BIN_HOME/farhelm-agent" uninstall
)

printf 'Native role program and V0.2 migration smoke passed.\n'
