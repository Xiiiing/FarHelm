#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  printf 'Run this installer as root (for example: sudo ./install.sh).\n' >&2
  exit 1
fi

bundle_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(<"$bundle_dir/VERSION")
release_tag=$(<"$bundle_dir/RELEASE_TAG")
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || [[ "$release_tag" != "V$version" ]]; then
  printf 'Bundle version metadata is invalid.\n' >&2
  exit 1
fi

install_root=/opt/farhelm-hub
releases_root="$install_root/releases"
release_dir="$releases_root/$version"
stage_dir="$releases_root/.$version.stage.$$"
config_dir=/etc/farhelm
config_file="$config_dir/hub.env"
stage_created=false

cleanup() {
  if [[ "$stage_created" == true ]]; then
    rm -rf "$stage_dir"
  fi
}
trap cleanup EXIT

for required in bin/farhelm-hub bin/farhelmctl console/index.html farhelm-hub.service Caddyfile.example install.sh rollback.sh uninstall.sh VERSION RELEASE_TAG; do
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

install -d -m 0755 "$releases_root"
if [[ -e "$release_dir" ]]; then
  if [[ ! -f "$release_dir/RELEASE_TAG" ]] || [[ "$(<"$release_dir/RELEASE_TAG")" != "$release_tag" ]]; then
    printf 'Release directory %s belongs to a legacy or different build; remove the old installation before establishing the V0.1.0 baseline.\n' "$release_dir" >&2
    exit 1
  fi
else
  install -d -m 0755 "$stage_dir/bin" "$stage_dir/console"
  stage_created=true
  install -m 0755 "$bundle_dir/bin/farhelm-hub" "$stage_dir/bin/farhelm-hub"
  install -m 0755 "$bundle_dir/bin/farhelmctl" "$stage_dir/bin/farhelmctl"
  cp -a "$bundle_dir/console/." "$stage_dir/console/"
  install -m 0755 "$bundle_dir/install.sh" "$stage_dir/install.sh"
  install -m 0755 "$bundle_dir/rollback.sh" "$stage_dir/rollback.sh"
  install -m 0755 "$bundle_dir/uninstall.sh" "$stage_dir/uninstall.sh"
  install -m 0644 "$bundle_dir/farhelm-hub.service" "$stage_dir/farhelm-hub.service"
  install -m 0644 "$bundle_dir/Caddyfile.example" "$stage_dir/Caddyfile.example"
  install -m 0644 "$bundle_dir/VERSION" "$stage_dir/VERSION"
  install -m 0644 "$bundle_dir/RELEASE_TAG" "$stage_dir/RELEASE_TAG"
  "$stage_dir/bin/farhelm-hub" --version | grep -Fxq "farhelm-hub $version"
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

install -m 0755 "$bundle_dir/rollback.sh" "$install_root/rollback.sh"
install -m 0755 "$bundle_dir/uninstall.sh" "$install_root/uninstall.sh"
install -d -m 0755 "$config_dir"
install -d -m 0750 -o farhelm-hub -g farhelm-hub /var/lib/farhelm-hub

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
    printf 'FARHELM_ADMIN_PASSWORD must be 12+ safe characters.\n' >&2
    exit 1
  fi
  if [[ ! "$agent_token" =~ ^[A-Za-z0-9._-]{32,}$ ]]; then
    printf 'FARHELM_AGENT_TOKEN must be 32+ safe characters.\n' >&2
    exit 1
  fi
  umask 077
  {
    printf 'FARHELM_HUB_BIND=127.0.0.1:8787\n'
    printf 'FARHELM_ADMIN_USER=%s\n' "$admin_user"
    printf 'FARHELM_ADMIN_PASSWORD=%s\n' "$admin_password"
    printf 'FARHELM_AGENT_TOKEN=%s\n' "$agent_token"
    printf 'FARHELM_CONSOLE_DIR=/opt/farhelm-hub/current/console\n'
    printf 'FARHELM_HUB_DATABASE=/var/lib/farhelm-hub/farhelm.db\n'
    printf 'RUST_LOG=farhelm_hub=info\n'
  } >"$config_file"
  generated=true
fi
if ! grep -q '^FARHELM_HUB_DATABASE=' "$config_file"; then
  printf 'FARHELM_HUB_DATABASE=/var/lib/farhelm-hub/farhelm.db\n' >>"$config_file"
fi
chown root:farhelm-hub "$config_file"
chmod 0640 "$config_file"

install -m 0644 "$bundle_dir/farhelm-hub.service" /etc/systemd/system/farhelm-hub.service
install -m 0644 "$bundle_dir/Caddyfile.example" /etc/farhelm/Caddyfile.example
if [[ -n "$previous_target" ]] && [[ "$previous_target" != "$release_dir" ]]; then
  atomic_release_link previous "$previous_target"
fi
atomic_release_link current "$release_dir"
ln -sfn /opt/farhelm-hub/current/bin/farhelmctl /usr/local/bin/farhelmctl
systemctl daemon-reload
systemctl enable farhelm-hub.service
systemctl restart farhelm-hub.service

healthy=false
for _ in $(seq 1 40); do
  if /usr/local/bin/farhelmctl health --hub http://127.0.0.1:8787 >/dev/null 2>&1; then
    healthy=true
    break
  fi
  sleep 0.25
done
if [[ "$healthy" != true ]]; then
  if [[ -n "$previous_target" ]] && [[ "$previous_target" != "$release_dir" ]]; then
    atomic_release_link current "$previous_target"
    atomic_release_link previous "$release_dir"
    systemctl restart farhelm-hub.service || true
    printf 'Hub %s failed its health check; current was rolled back to %s.\n' "$version" "$(basename "$previous_target")" >&2
  else
    printf 'Hub %s failed its health check and no previous release is available.\n' "$version" >&2
  fi
  exit 1
fi

printf 'FarHelm Hub %s installed and healthy on 127.0.0.1:8787.\n' "$version"
printf 'Configure Caddy using /etc/farhelm/Caddyfile.example before public access.\n'
if [[ "$generated" == true ]]; then
  printf '\nSave these values now; the Agent needs the token:\n'
  printf '  Admin user: %s\n' "$admin_user"
  printf '  Admin password: %s\n' "$admin_password"
  printf '  Agent token: %s\n' "$agent_token"
else
  printf 'Existing credentials and state were preserved.\n'
fi
printf 'Upgrade later with: sudo farhelmctl upgrade\n'
printf 'Rollback with: sudo farhelmctl rollback\n'
printf 'Uninstall completely with: sudo /opt/farhelm-hub/uninstall.sh\n'
