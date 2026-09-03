#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  printf 'Run Hub rollback as root (for example: sudo farhelmctl rollback).\n' >&2
  exit 1
fi

install_root=/opt/farhelm-hub
releases_root="$install_root/releases"
current=$(readlink -f "$install_root/current" 2>/dev/null || true)
previous=$(readlink -f "$install_root/previous" 2>/dev/null || true)
for target in "$current" "$previous"; do
  case "$target" in
    "$releases_root"/*) [[ -d "$target" ]] || { printf 'Installed release is missing: %s\n' "$target" >&2; exit 1; } ;;
    *) printf 'Hub rollback target is missing or unsafe.\n' >&2; exit 1 ;;
  esac
done
if [[ "$current" == "$previous" ]]; then
  printf 'Hub current and previous releases are identical.\n' >&2
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
systemctl restart farhelm-hub.service
for _ in $(seq 1 40); do
  if /usr/local/bin/farhelmctl health --hub http://127.0.0.1:8787 >/dev/null 2>&1; then
    printf 'FarHelm Hub rolled back from %s to %s.\n' "$(basename "$current")" "$(basename "$previous")"
    exit 0
  fi
  sleep 0.25
done

atomic_release_link current "$current"
atomic_release_link previous "$previous"
systemctl restart farhelm-hub.service || true
printf 'Previous Hub release failed its health check; the original current release was restored.\n' >&2
exit 1
