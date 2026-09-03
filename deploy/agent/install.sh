#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  printf 'Run this installer as root (for example: sudo ./install.sh).\n' >&2
  exit 1
fi

bundle_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(<"$bundle_dir/VERSION")
release_dir="/opt/farhelm-agent/releases/$version"
config_dir=/etc/farhelm
config_file="$config_dir/agent.env"
run_user=${FARHELM_RUN_USER:-${SUDO_USER:-}}
hub_url=${FARHELM_HUB_URL:-}
agent_token=${FARHELM_AGENT_TOKEN:-}
agent_id=${FARHELM_AGENT_ID:-$(hostname | tr -c 'A-Za-z0-9._-' '-' | sed 's/-$//')}
agent_hostname=${FARHELM_AGENT_HOSTNAME:-$(hostname)}

for required in bin/farhelm-agent worker/src/farhelm_worker_codex farhelm-agent.service; do
  if [[ ! -e "$bundle_dir/$required" ]]; then
    printf 'Bundle is incomplete: %s is missing.\n' "$required" >&2
    exit 1
  fi
done

command -v systemctl >/dev/null || { printf 'systemd is required.\n' >&2; exit 1; }
command -v python3 >/dev/null || { printf 'Python 3.12 is required.\n' >&2; exit 1; }
python3 -c 'import sys; raise SystemExit(0 if sys.version_info[:2] == (3, 12) else 1)' || {
  printf 'Python 3.12 is required; found %s.\n' "$(python3 --version 2>&1)" >&2
  exit 1
}
if [[ -z "$run_user" ]] || ! id "$run_user" >/dev/null 2>&1; then
  printf 'Set FARHELM_RUN_USER to the existing Unix user that owns training projects.\n' >&2
  exit 1
fi
if [[ ! "$run_user" =~ ^[a-z_][a-z0-9_-]*[$]?$ ]]; then
  printf 'FARHELM_RUN_USER is not a safe systemd user name.\n' >&2
  exit 1
fi
if ! getent group farhelm-agent >/dev/null 2>&1; then
  groupadd --system farhelm-agent
fi
run_group=farhelm-agent

install -d -m 0755 /opt/farhelm-agent/releases "$release_dir/bin" "$release_dir/worker"
install -m 0755 "$bundle_dir/bin/farhelm-agent" "$release_dir/bin/farhelm-agent"
cp -a "$bundle_dir/worker/." "$release_dir/worker/"
ln -sfn "$release_dir" /opt/farhelm-agent/current
install -d -m 0755 "$config_dir"

generated=false
if [[ ! -e "$config_file" ]]; then
  if [[ ! "$hub_url" =~ ^https://[A-Za-z0-9._:/?&=%-]+$ ]]; then
    printf 'Set FARHELM_HUB_URL to the public HTTPS Hub URL.\n' >&2
    exit 1
  fi
  if [[ ! "$agent_token" =~ ^[A-Za-z0-9._-]{32,}$ ]]; then
    printf 'Set FARHELM_AGENT_TOKEN to the 32+ character token from the Hub installer.\n' >&2
    exit 1
  fi
  if [[ ! "$agent_id" =~ ^[A-Za-z0-9._-]{1,64}$ ]]; then
    printf 'FARHELM_AGENT_ID must use 1-64 safe characters.\n' >&2
    exit 1
  fi
  if [[ ! "$agent_hostname" =~ ^[A-Za-z0-9._-]{1,255}$ ]]; then
    printf 'FARHELM_AGENT_HOSTNAME is invalid.\n' >&2
    exit 1
  fi
  umask 077
  {
    printf 'FARHELM_HUB_URL=%s\n' "$hub_url"
    printf 'FARHELM_AGENT_TOKEN=%s\n' "$agent_token"
    printf 'FARHELM_AGENT_ID=%s\n' "$agent_id"
    printf 'FARHELM_AGENT_HOSTNAME=%s\n' "$agent_hostname"
    printf 'FARHELM_HEARTBEAT_INTERVAL=15\n'
    printf 'RUST_LOG=farhelm_agent=info\n'
  } >"$config_file"
  generated=true
fi
chown root:"$run_group" "$config_file"
chmod 0640 "$config_file"

unit_tmp=$(mktemp)
trap 'rm -f "$unit_tmp"' EXIT
sed \
  -e "s/__FARHELM_RUN_USER__/$run_user/g" \
  -e "s/__FARHELM_RUN_GROUP__/$run_group/g" \
  "$bundle_dir/farhelm-agent.service" >"$unit_tmp"
install -m 0644 "$unit_tmp" /etc/systemd/system/farhelm-agent.service
systemctl daemon-reload
systemctl enable farhelm-agent.service
systemctl restart farhelm-agent.service

printf 'FarHelm Agent %s installed as %s.\n' "$version" "$run_user"
if [[ "$generated" == true ]]; then
  printf 'Configured Hub: %s\n' "$hub_url"
else
  printf 'Existing connection settings were preserved in %s.\n' "$config_file"
fi
printf 'Check status with: systemctl status farhelm-agent\n'
