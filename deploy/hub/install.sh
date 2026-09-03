#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  printf 'Run this installer as root (for example: sudo ./install.sh).\n' >&2
  exit 1
fi

bundle_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(<"$bundle_dir/VERSION")
release_dir="/opt/farhelm-hub/releases/$version"
config_dir=/etc/farhelm
config_file="$config_dir/hub.env"

for required in bin/farhelm-hub bin/farhelmctl console/index.html farhelm-hub.service Caddyfile.example uninstall.sh; do
  if [[ ! -e "$bundle_dir/$required" ]]; then
    printf 'Bundle is incomplete: %s is missing.\n' "$required" >&2
    exit 1
  fi
done

command -v systemctl >/dev/null || { printf 'systemd is required.\n' >&2; exit 1; }
command -v openssl >/dev/null || { printf 'openssl is required to generate credentials.\n' >&2; exit 1; }

if ! id farhelm-hub >/dev/null 2>&1; then
  nologin=$(command -v nologin || true)
  [[ -n "$nologin" ]] || nologin=/usr/sbin/nologin
  useradd --system --home-dir /nonexistent --no-create-home --shell "$nologin" farhelm-hub
fi

install -d -m 0755 /opt/farhelm-hub/releases "$release_dir/bin" "$release_dir/console"
install -m 0755 "$bundle_dir/bin/farhelm-hub" "$release_dir/bin/farhelm-hub"
install -m 0755 "$bundle_dir/bin/farhelmctl" "$release_dir/bin/farhelmctl"
cp -a "$bundle_dir/console/." "$release_dir/console/"
ln -sfn "$release_dir" /opt/farhelm-hub/current
install -m 0755 "$bundle_dir/bin/farhelmctl" /usr/local/bin/farhelmctl
install -m 0755 "$bundle_dir/uninstall.sh" /opt/farhelm-hub/uninstall.sh
install -d -m 0755 "$config_dir"

generated=false
if [[ ! -e "$config_file" ]]; then
  admin_user=${FARHELM_ADMIN_USER:-admin}
  admin_password=${FARHELM_ADMIN_PASSWORD:-$(openssl rand -hex 16)}
  agent_token=${FARHELM_AGENT_TOKEN:-$(openssl rand -hex 32)}
  if [[ ! "$admin_user" =~ ^[A-Za-z0-9._-]+$ ]]; then
    printf 'FARHELM_ADMIN_USER contains unsupported characters.\n' >&2
    exit 1
  fi
  if [[ ! "$admin_password" =~ ^[A-Za-z0-9._-]{12,}$ ]]; then
    printf 'FARHELM_ADMIN_PASSWORD must be 12+ characters from A-Z, a-z, 0-9, dot, underscore or hyphen.\n' >&2
    exit 1
  fi
  if [[ ! "$agent_token" =~ ^[A-Za-z0-9._-]{32,}$ ]]; then
    printf 'FARHELM_AGENT_TOKEN must be 32+ characters from A-Z, a-z, 0-9, dot, underscore or hyphen.\n' >&2
    exit 1
  fi
  umask 077
  {
    printf 'FARHELM_HUB_BIND=127.0.0.1:8787\n'
    printf 'FARHELM_ADMIN_USER=%s\n' "$admin_user"
    printf 'FARHELM_ADMIN_PASSWORD=%s\n' "$admin_password"
    printf 'FARHELM_AGENT_TOKEN=%s\n' "$agent_token"
    printf 'FARHELM_CONSOLE_DIR=/opt/farhelm-hub/current/console\n'
    printf 'RUST_LOG=farhelm_hub=info\n'
  } >"$config_file"
  generated=true
fi
chown root:farhelm-hub "$config_file"
chmod 0640 "$config_file"

install -m 0644 "$bundle_dir/farhelm-hub.service" /etc/systemd/system/farhelm-hub.service
install -m 0644 "$bundle_dir/Caddyfile.example" /etc/farhelm/Caddyfile.example
systemctl daemon-reload
systemctl enable farhelm-hub.service
systemctl restart farhelm-hub.service

printf 'FarHelm Hub %s installed and listening on 127.0.0.1:8787.\n' "$version"
printf 'Configure Caddy using /etc/farhelm/Caddyfile.example before public access.\n'
if [[ "$generated" == true ]]; then
  printf '\nSave these values now; the Agent needs the token:\n'
  printf '  Admin user: %s\n' "$admin_user"
  printf '  Admin password: %s\n' "$admin_password"
  printf '  Agent token: %s\n' "$agent_token"
else
  printf 'Existing credentials were preserved in %s.\n' "$config_file"
fi
printf 'Uninstall completely with: sudo /opt/farhelm-hub/uninstall.sh\n'
