#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
  printf 'Do not use root or sudo. Run this installer as the training user.\n' >&2
  exit 1
fi

bundle_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(<"$bundle_dir/VERSION")
user_name=$(id -un)
user_home=$(getent passwd "$user_name" | cut -d: -f6)
if [[ -z "$user_home" ]] || [[ "$user_home" != /* ]]; then
  printf 'Unable to resolve an absolute home directory for %s.\n' "$user_name" >&2
  exit 1
fi
data_home=${XDG_DATA_HOME:-"$user_home/.local/share"}
config_home=${XDG_CONFIG_HOME:-"$user_home/.config"}
install_root="$data_home/farhelm-agent"
config_file="$install_root/config/agent.env"
unit_dir="$config_home/systemd/user"
unit_file="$unit_dir/farhelm-agent.service"
hub_url=${FARHELM_HUB_URL:-}
agent_token=${FARHELM_AGENT_TOKEN:-}
agent_id=${FARHELM_AGENT_ID:-$(hostname | tr -c 'A-Za-z0-9._-' '-' | sed 's/-$//')}
agent_hostname=${FARHELM_AGENT_HOSTNAME:-$(hostname)}
no_service=${FARHELM_NO_SERVICE:-0}

for required in bin/farhelm-agent worker/src/farhelm_worker_codex farhelm-agent.service run.sh uninstall.sh; do
  if [[ ! -e "$bundle_dir/$required" ]]; then
    printf 'Bundle is incomplete: %s is missing.\n' "$required" >&2
    exit 1
  fi
done

for path in "$data_home" "$config_home" "$install_root" "$unit_dir"; do
  safe_path_pattern='^/[A-Za-z0-9._/-]+$'
  if [[ ! "$path" =~ $safe_path_pattern ]]; then
    printf 'User data/config paths must be absolute and use only safe path characters: %s\n' "$path" >&2
    exit 1
  fi
done
case "$install_root" in
  */farhelm-agent) ;;
  *) printf 'Unsafe Agent installation path: %s\n' "$install_root" >&2; exit 1 ;;
esac
if [[ "$no_service" != 0 && "$no_service" != 1 ]]; then
  printf 'FARHELM_NO_SERVICE must be 0 or 1.\n' >&2
  exit 1
fi
if [[ "$no_service" == 0 ]]; then
  command -v systemctl >/dev/null || {
    printf 'systemctl is unavailable; use FARHELM_NO_SERVICE=1 for a foreground-only installation.\n' >&2
    exit 1
  }
  systemctl --user show-environment >/dev/null 2>&1 || {
    printf 'The systemd user manager is unavailable in this session.\n' >&2
    printf 'Retry from a normal login session or use FARHELM_NO_SERVICE=1.\n' >&2
    exit 1
  }
fi

generated=false
if [[ ! -e "$config_file" ]]; then
  https_url_pattern='^https://[A-Za-z0-9._:/?&=%-]+$'
  if [[ ! "$hub_url" =~ $https_url_pattern ]]; then
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
  generated=true
fi

if [[ "$no_service" == 0 ]] && [[ -e "$unit_file" ]]; then
  systemctl --user stop farhelm-agent.service >/dev/null 2>&1 || true
fi

install -d -m 0700 "$install_root" "$install_root/bin" "$install_root/config" \
  "$install_root/worker/src/farhelm_worker_codex"
install -m 0755 "$bundle_dir/bin/farhelm-agent" "$install_root/bin/farhelm-agent"
install -m 0755 "$bundle_dir/run.sh" "$install_root/run.sh"
install -m 0755 "$bundle_dir/uninstall.sh" "$install_root/uninstall.sh"
install -m 0644 "$bundle_dir/worker/src/farhelm_worker_codex/"*.py \
  "$install_root/worker/src/farhelm_worker_codex/"
install -m 0644 "$bundle_dir/worker/pyproject.toml" "$install_root/worker/pyproject.toml"
install -m 0644 "$bundle_dir/worker/uv.lock" "$install_root/worker/uv.lock"
install -m 0644 "$bundle_dir/VERSION" "$install_root/VERSION"

if [[ "$generated" == true ]]; then
  umask 077
  {
    printf 'FARHELM_HUB_URL=%s\n' "$hub_url"
    printf 'FARHELM_AGENT_TOKEN=%s\n' "$agent_token"
    printf 'FARHELM_AGENT_ID=%s\n' "$agent_id"
    printf 'FARHELM_AGENT_HOSTNAME=%s\n' "$agent_hostname"
    printf 'FARHELM_HEARTBEAT_INTERVAL=15\n'
    printf 'RUST_LOG=farhelm_agent=info\n'
  } >"$config_file"
fi
chmod 0600 "$config_file"

if [[ "$no_service" == 0 ]]; then
  install -d -m 0700 "$unit_dir"
  escaped_root=$(printf '%s' "$install_root" | sed 's/[&|]/\\&/g')
  escaped_config=$(printf '%s' "$config_file" | sed 's/[&|]/\\&/g')
  unit_tmp=$(mktemp)
  trap 'rm -f "$unit_tmp"' EXIT
  sed \
    -e "s|__FARHELM_INSTALL_ROOT__|$escaped_root|g" \
    -e "s|__FARHELM_CONFIG_FILE__|$escaped_config|g" \
    "$bundle_dir/farhelm-agent.service" >"$unit_tmp"
  install -m 0600 "$unit_tmp" "$unit_file"
  systemctl --user daemon-reload
  systemctl --user enable --now farhelm-agent.service
fi

printf 'FarHelm Agent %s installed without root.\n' "$version"
printf '  Managed data: %s\n' "$install_root"
if [[ "$no_service" == 0 ]]; then
  printf '  User service: %s\n' "$unit_file"
  printf '  Status: systemctl --user status farhelm-agent\n'
  linger=$(loginctl show-user "$user_name" --property=Linger --value 2>/dev/null || true)
  if [[ "$linger" != yes ]]; then
    printf '\nWarning: systemd linger is not enabled for %s.\n' "$user_name" >&2
    printf 'The service is running now, but automatic operation after logout/reboot depends on host policy.\n' >&2
    printf 'Ask the server administrator to run: loginctl enable-linger %s\n' "$user_name" >&2
  fi
else
  printf '  No service was installed. Run in the foreground with: %s/run.sh\n' "$install_root"
fi
if [[ "$generated" == false ]]; then
  printf 'Existing connection settings were preserved.\n'
fi
printf 'Uninstall completely with: %s/uninstall.sh\n' "$install_root"
