#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -eq 0 ]]; then
  printf 'Do not use root or sudo for Agent rollback.\n' >&2
  exit 1
fi

install_root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
releases_root="$install_root/releases"
current=$(readlink -f "$install_root/current" 2>/dev/null || true)
previous=$(readlink -f "$install_root/previous" 2>/dev/null || true)
for target in "$current" "$previous"; do
  case "$target" in
    "$releases_root"/*) [[ -d "$target" ]] || { printf 'Installed release is missing: %s\n' "$target" >&2; exit 1; } ;;
    *) printf 'Agent rollback target is missing or unsafe.\n' >&2; exit 1 ;;
  esac
done
if [[ "$current" == "$previous" ]]; then
  printf 'Agent current and previous releases are identical.\n' >&2
  exit 1
fi

atomic_release_link() {
  local link_name=$1
  local target=$2
  local link_tmp="$install_root/.$link_name.$$"
  rm -f "$link_tmp"
  ln -s "releases/$(basename "$target")" "$link_tmp"
  mv -Tf "$link_tmp" "$install_root/$link_name"
}

atomic_release_link current "$previous"
atomic_release_link previous "$current"
if [[ "$(<"$install_root/INSTALL_MODE")" == service ]]; then
  systemctl --user restart farhelm-agent.service
  for _ in $(seq 1 20); do
    if systemctl --user is-active --quiet farhelm-agent.service; then
      printf 'FarHelm Agent rolled back from %s to %s.\n' "$(basename "$current")" "$(basename "$previous")"
      exit 0
    fi
    sleep 0.25
  done
  atomic_release_link current "$current"
  atomic_release_link previous "$previous"
  systemctl --user restart farhelm-agent.service || true
  printf 'Previous Agent release failed its health check; the original current release was restored.\n' >&2
  exit 1
fi
printf 'FarHelm Agent rolled back from %s to %s.\n' "$(basename "$current")" "$(basename "$previous")"
