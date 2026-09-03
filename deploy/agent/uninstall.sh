#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
  printf 'Do not use root or sudo. Run this uninstaller as the training user.\n' >&2
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
install_root="$data_home/farhelm-agent"
unit_file="$config_home/systemd/user/farhelm-agent.service"

safe_path_pattern='^/[A-Za-z0-9._/-]+$'
if [[ ! "$install_root" =~ $safe_path_pattern ]] || [[ ! "$unit_file" =~ $safe_path_pattern ]]; then
  printf 'Refusing unsafe Agent paths.\n' >&2
  exit 1
fi

case "$install_root" in
  /*/farhelm-agent) ;;
  *) printf 'Refusing unsafe uninstall path: %s\n' "$install_root" >&2; exit 1 ;;
esac
if [[ "$unit_file" != /*/systemd/user/farhelm-agent.service ]]; then
  printf 'Refusing unsafe unit path: %s\n' "$unit_file" >&2
  exit 1
fi

if command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; then
  systemctl --user disable --now farhelm-agent.service >/dev/null 2>&1 || true
fi
rm -f "$unit_file"
if command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; then
  systemctl --user daemon-reload
  systemctl --user reset-failed farhelm-agent.service >/dev/null 2>&1 || true
fi
rm -rf "$install_root"

printf 'FarHelm Agent was removed.\n'
printf 'Removed: %s\n' "$install_root"
printf 'Removed: %s\n' "$unit_file"
