#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
  printf 'Do not use root or sudo. Run this uninstaller as the training user.\n' >&2
  exit 1
fi

install_root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
safe_path_pattern='^/[A-Za-z0-9._/-]+/farhelm-agent$'
if [[ ! "$install_root" =~ $safe_path_pattern ]]; then
  printf 'Refusing unsafe Agent installation path: %s\n' "$install_root" >&2
  exit 1
fi

unit_file=
if [[ -r "$install_root/UNIT_PATH" ]]; then
  unit_file=$(<"$install_root/UNIT_PATH")
  if [[ ! "$unit_file" =~ ^/[A-Za-z0-9._/-]+/systemd/user/farhelm-agent\.service$ ]]; then
    printf 'Refusing unsafe Agent unit path: %s\n' "$unit_file" >&2
    exit 1
  fi
fi
if [[ -n "$unit_file" ]] && command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; then
  systemctl --user disable --now farhelm-agent.service >/dev/null 2>&1 || true
fi
if [[ -n "$unit_file" ]]; then
  rm -f "$unit_file"
fi
if command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1; then
  systemctl --user daemon-reload
  systemctl --user reset-failed farhelm-agent.service >/dev/null 2>&1 || true
fi
rm -rf "$install_root"

printf 'FarHelm Agent was removed.\n'
printf 'Removed: %s\n' "$install_root"
if [[ -n "$unit_file" ]]; then
  printf 'Removed: %s\n' "$unit_file"
fi
