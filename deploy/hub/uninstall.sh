#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  printf 'Run this uninstaller as root (for example: sudo ./uninstall.sh).\n' >&2
  exit 1
fi

systemctl disable --now farhelm-hub.service >/dev/null 2>&1 || true
rm -f /etc/systemd/system/farhelm-hub.service
systemctl daemon-reload
systemctl reset-failed farhelm-hub.service >/dev/null 2>&1 || true

rm -f /usr/local/bin/farhelmctl
rm -f /etc/farhelm/hub.env /etc/farhelm/Caddyfile.example
rm -rf /opt/farhelm-hub
rm -rf /var/lib/farhelm-hub
rmdir /etc/farhelm >/dev/null 2>&1 || true
if id farhelm-hub >/dev/null 2>&1; then
  userdel farhelm-hub
fi

printf 'FarHelm Hub was removed from all installer-managed paths.\n'
printf 'The shared /etc/caddy/Caddyfile was not modified; remove the FarHelm site block and reload Caddy.\n'
