#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
  printf 'Do not use root or sudo. Run this installer as the training user.\n' >&2
  exit 1
fi

bundle_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(<"$bundle_dir/VERSION")
release_tag=$(<"$bundle_dir/RELEASE_TAG")
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || [[ "$release_tag" != "V$version" ]]; then
  printf 'Bundle version metadata is invalid.\n' >&2
  exit 1
fi

user_name=$(id -un)
user_home=$(getent passwd "$user_name" | cut -d: -f6)
if [[ -z "$user_home" ]] || [[ "$user_home" != /* ]]; then
  printf 'Unable to resolve an absolute home directory for %s.\n' "$user_name" >&2
  exit 1
fi
data_home=${XDG_DATA_HOME:-"$user_home/.local/share"}
config_home=${XDG_CONFIG_HOME:-"$user_home/.config"}
install_root=${FARHELM_INSTALL_ROOT:-"$data_home/farhelm-agent"}
releases_root="$install_root/releases"
release_dir="$releases_root/$version"
stage_dir="$releases_root/.$version.stage.$$"
config_file="$install_root/config/agent.env"
mode_file="$install_root/INSTALL_MODE"
unit_path_file="$install_root/UNIT_PATH"
unit_dir="$config_home/systemd/user"
unit_file="$unit_dir/farhelm-agent.service"
if [[ -r "$unit_path_file" ]]; then
  unit_file=$(<"$unit_path_file")
  unit_dir=$(dirname "$unit_file")
fi

hub_url=${FARHELM_HUB_URL:-}
agent_token=${FARHELM_AGENT_TOKEN:-}
agent_id=${FARHELM_AGENT_ID:-$(hostname | tr -c 'A-Za-z0-9._-' '-' | sed 's/-$//')}
agent_hostname=${FARHELM_AGENT_HOSTNAME:-$(hostname)}
no_service=${FARHELM_NO_SERVICE:-0}
if [[ ${FARHELM_UPGRADE:-0} == 1 ]] && [[ -r "$mode_file" ]]; then
  case "$(<"$mode_file")" in
    service) no_service=0 ;;
    foreground) no_service=1 ;;
    *) printf 'Installed Agent mode is invalid.\n' >&2; exit 1 ;;
  esac
fi

for required in bin/farhelm-agent worker/src/farhelm_worker_codex farhelm-agent.service install.sh run.sh rollback.sh uninstall.sh VERSION RELEASE_TAG; do
  if [[ ! -e "$bundle_dir/$required" ]]; then
    printf 'Bundle is incomplete: %s is missing.\n' "$required" >&2
    exit 1
  fi
done

for path in "$data_home" "$config_home" "$install_root" "$unit_dir" "$unit_file"; do
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
if [[ -e "$install_root/bin" ]] && [[ ! -L "$install_root/current" ]]; then
  printf 'A legacy Agent installation exists at %s; remove it before establishing the V0.1.0 baseline.\n' "$install_root" >&2
  exit 1
fi
if [[ "$no_service" != 0 && "$no_service" != 1 ]]; then
  printf 'FARHELM_NO_SERVICE must be 0 or 1.\n' >&2
  exit 1
fi
if [[ "$no_service" == 0 ]]; then
  command -v systemctl >/dev/null || { printf 'systemctl is required for service mode.\n' >&2; exit 1; }
  systemctl --user show-environment >/dev/null 2>&1 || {
    printf 'The systemd user manager is unavailable in this session.\n' >&2
    exit 1
  }
fi

generated=false
if [[ ! -e "$config_file" ]]; then
  https_url_pattern='^https://[A-Za-z0-9._:/?&=%-]+$'
  [[ "$hub_url" =~ $https_url_pattern ]] || { printf 'Set FARHELM_HUB_URL to the public HTTPS Hub URL.\n' >&2; exit 1; }
  [[ "$agent_token" =~ ^[A-Za-z0-9._-]{32,}$ ]] || { printf 'Set FARHELM_AGENT_TOKEN to the 32+ character Hub token.\n' >&2; exit 1; }
  [[ "$agent_id" =~ ^[A-Za-z0-9._-]{1,64}$ ]] || { printf 'FARHELM_AGENT_ID must use 1-64 safe characters.\n' >&2; exit 1; }
  [[ "$agent_hostname" =~ ^[A-Za-z0-9._-]{1,255}$ ]] || { printf 'FARHELM_AGENT_HOSTNAME is invalid.\n' >&2; exit 1; }
  generated=true
fi

if [[ "$no_service" == 0 ]] && [[ -e "$unit_file" ]]; then
  systemctl --user stop farhelm-agent.service >/dev/null 2>&1 || true
fi

install -d -m 0700 "$install_root" "$install_root/config" "$install_root/state" "$releases_root"
stage_created=false
cleanup() {
  if [[ "$stage_created" == true ]]; then
    rm -rf "$stage_dir"
  fi
}
trap cleanup EXIT
if [[ -e "$release_dir" ]]; then
  if [[ ! -f "$release_dir/RELEASE_TAG" ]] || [[ "$(<"$release_dir/RELEASE_TAG")" != "$release_tag" ]]; then
    printf 'Release directory %s belongs to a different build.\n' "$release_dir" >&2
    exit 1
  fi
else
  install -d -m 0700 "$stage_dir/bin" "$stage_dir/worker/src/farhelm_worker_codex"
  stage_created=true
  install -m 0755 "$bundle_dir/bin/farhelm-agent" "$stage_dir/bin/farhelm-agent"
  install -m 0755 "$bundle_dir/install.sh" "$stage_dir/install.sh"
  install -m 0755 "$bundle_dir/run.sh" "$stage_dir/run.sh"
  install -m 0755 "$bundle_dir/rollback.sh" "$stage_dir/rollback.sh"
  install -m 0755 "$bundle_dir/uninstall.sh" "$stage_dir/uninstall.sh"
  install -m 0644 "$bundle_dir/farhelm-agent.service" "$stage_dir/farhelm-agent.service"
  install -m 0644 "$bundle_dir/worker/src/farhelm_worker_codex/"*.py "$stage_dir/worker/src/farhelm_worker_codex/"
  install -m 0644 "$bundle_dir/worker/pyproject.toml" "$stage_dir/worker/pyproject.toml"
  install -m 0644 "$bundle_dir/worker/uv.lock" "$stage_dir/worker/uv.lock"
  install -m 0644 "$bundle_dir/VERSION" "$stage_dir/VERSION"
  install -m 0644 "$bundle_dir/RELEASE_TAG" "$stage_dir/RELEASE_TAG"
  "$stage_dir/bin/farhelm-agent" --version | grep -Fxq "farhelm-agent $version"
  mv "$stage_dir" "$release_dir"
  stage_created=false
fi

atomic_release_link() {
  local link_name=$1
  local target=$2
  case "$target" in
    "$releases_root"/*) ;;
    *) printf 'Refusing release path outside %s: %s\n' "$releases_root" "$target" >&2; return 1 ;;
  esac
  [[ -d "$target" ]] || { printf 'Release directory is missing: %s\n' "$target" >&2; return 1; }
  local link_tmp="$install_root/.$link_name.$$"
  rm -f "$link_tmp"
  ln -s "releases/$(basename "$target")" "$link_tmp"
  mv -Tf "$link_tmp" "$install_root/$link_name"
}

previous_target=
if [[ -L "$install_root/current" ]]; then
  previous_target=$(readlink -f "$install_root/current")
  case "$previous_target" in
    "$releases_root"/*) [[ -d "$previous_target" ]] || previous_target= ;;
    *) previous_target= ;;
  esac
fi

if [[ "$generated" == true ]]; then
  umask 077
  {
    printf 'FARHELM_HUB_URL=%s\n' "$hub_url"
    printf 'FARHELM_AGENT_TOKEN=%s\n' "$agent_token"
    printf 'FARHELM_AGENT_ID=%s\n' "$agent_id"
    printf 'FARHELM_AGENT_HOSTNAME=%s\n' "$agent_hostname"
    printf 'FARHELM_HEARTBEAT_INTERVAL=15\n'
    printf 'FARHELM_COMMAND_POLL_INTERVAL=2\n'
    printf 'FARHELM_AGENT_DATABASE=%s/state/agent.db\n' "$install_root"
    printf 'RUST_LOG=farhelm_agent=info\n'
  } >"$config_file"
fi
if ! grep -q '^FARHELM_COMMAND_POLL_INTERVAL=' "$config_file"; then
  printf 'FARHELM_COMMAND_POLL_INTERVAL=2\n' >>"$config_file"
fi
if ! grep -q '^FARHELM_AGENT_DATABASE=' "$config_file"; then
  printf 'FARHELM_AGENT_DATABASE=%s/state/agent.db\n' "$install_root" >>"$config_file"
fi
chmod 0600 "$config_file"
install -m 0755 "$bundle_dir/rollback.sh" "$install_root/rollback.sh"
install -m 0755 "$bundle_dir/uninstall.sh" "$install_root/uninstall.sh"
if [[ "$no_service" == 0 ]]; then
  printf 'service\n' >"$mode_file"
  install -d -m 0700 "$unit_dir"
  printf '%s\n' "$unit_file" >"$unit_path_file"
  chmod 0600 "$unit_path_file"
  escaped_root=$(printf '%s' "$install_root" | sed 's/[&|]/\\&/g')
  escaped_config=$(printf '%s' "$config_file" | sed 's/[&|]/\\&/g')
  unit_tmp=$(mktemp)
  sed -e "s|__FARHELM_INSTALL_ROOT__|$escaped_root|g" -e "s|__FARHELM_CONFIG_FILE__|$escaped_config|g" "$bundle_dir/farhelm-agent.service" >"$unit_tmp"
  install -m 0600 "$unit_tmp" "$unit_file"
  rm -f "$unit_tmp"
else
  printf 'foreground\n' >"$mode_file"
  rm -f "$unit_path_file"
fi
chmod 0600 "$mode_file"

if [[ -n "$previous_target" ]] && [[ "$previous_target" != "$release_dir" ]]; then
  atomic_release_link previous "$previous_target"
fi
atomic_release_link current "$release_dir"

healthy=true
if [[ "$no_service" == 0 ]]; then
  systemctl --user daemon-reload
  systemctl --user enable --now farhelm-agent.service
  systemctl --user restart farhelm-agent.service
  healthy=false
  for _ in $(seq 1 20); do
    if systemctl --user is-active --quiet farhelm-agent.service; then
      healthy=true
      break
    fi
    sleep 0.25
  done
fi
if [[ "$healthy" != true ]]; then
  if [[ -n "$previous_target" ]] && [[ "$previous_target" != "$release_dir" ]]; then
    atomic_release_link current "$previous_target"
    atomic_release_link previous "$release_dir"
    systemctl --user restart farhelm-agent.service || true
    printf 'Agent %s failed its health check; current was rolled back to %s.\n' "$version" "$(basename "$previous_target")" >&2
  else
    printf 'Agent %s failed its health check and no previous release is available.\n' "$version" >&2
  fi
  exit 1
fi

printf 'FarHelm Agent %s installed without root.\n' "$version"
printf '  Managed data: %s\n' "$install_root"
printf '  Current release: %s\n' "$release_dir"
if [[ "$no_service" == 0 ]]; then
  printf '  User service: %s\n' "$unit_file"
  linger=$(loginctl show-user "$user_name" --property=Linger --value 2>/dev/null || true)
  if [[ "$linger" != yes ]]; then
    printf '\nWarning: systemd linger is not enabled for %s.\n' "$user_name" >&2
    printf 'Ask the server administrator to run: loginctl enable-linger %s\n' "$user_name" >&2
  fi
else
  printf '  No service was installed. Run: %s/current/run.sh\n' "$install_root"
fi
if [[ "$generated" == false ]]; then
  printf 'Existing connection settings and state were preserved.\n'
fi
printf 'Upgrade later with: %s/current/bin/farhelm-agent upgrade\n' "$install_root"
printf 'Rollback with: %s/current/bin/farhelm-agent rollback\n' "$install_root"
printf 'Uninstall completely with: %s/uninstall.sh\n' "$install_root"
