#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

if [[ $(uname -s) != Linux ]] || [[ $(uname -m) != x86_64 ]]; then
  printf 'This release profile currently supports Linux x86_64 only.\n' >&2
  exit 1
fi

for command_name in cargo corepack tar sha256sum; do
  command -v "$command_name" >/dev/null || {
    printf 'Required build command is missing: %s\n' "$command_name" >&2
    exit 1
  }
done

version=$(cargo pkgid -p farhelm-core | sed 's/.*#//')
test "$(<VERSION)" = "$version"
platform=linux-x86_64
output_dir="$repo_root/dist/release"
case "$output_dir" in
  */dist/release) ;;
  *) printf 'Unsafe release output path: %s\n' "$output_dir" >&2; exit 1 ;;
esac
rm -rf "$output_dir"
install -d -m 0755 "$output_dir"

corepack pnpm@10.17.1 --dir farhelm-console install --frozen-lockfile
corepack pnpm@10.17.1 --dir farhelm-console build
cargo build --release --locked -p farhelm-hub -p farhelm-agent -p farhelmctl

stage_dir=$(mktemp -d)
trap 'rm -rf "$stage_dir"' EXIT
hub_name="farhelm-hub-$version-$platform"
agent_name="farhelm-agent-$version-$platform"
hub_stage="$stage_dir/$hub_name"
agent_stage="$stage_dir/$agent_name"

install -d -m 0755 "$hub_stage/bin" "$hub_stage/console"
install -m 0755 target/release/farhelm-hub "$hub_stage/bin/farhelm-hub"
install -m 0755 target/release/farhelmctl "$hub_stage/bin/farhelmctl"
cp -a farhelm-console/dist/. "$hub_stage/console/"
install -m 0755 deploy/hub/install.sh "$hub_stage/install.sh"
install -m 0755 deploy/hub/rollback.sh "$hub_stage/rollback.sh"
install -m 0755 deploy/hub/uninstall.sh "$hub_stage/uninstall.sh"
install -m 0644 deploy/hub/farhelm-hub.service "$hub_stage/farhelm-hub.service"
install -m 0644 deploy/hub/Caddyfile.example "$hub_stage/Caddyfile.example"
install -m 0644 deploy/README.md "$hub_stage/README.md"
install -m 0644 deploy/README.en.md "$hub_stage/README.en.md"
printf '%s\n' "$version" >"$hub_stage/VERSION"
printf 'V%s\n' "$version" >"$hub_stage/RELEASE_TAG"

install -d -m 0755 "$agent_stage/bin" "$agent_stage/worker/src"
install -m 0755 target/release/farhelm-agent "$agent_stage/bin/farhelm-agent"
install -d -m 0755 "$agent_stage/worker/src/farhelm_worker_codex"
install -m 0644 farhelm-worker-codex/src/farhelm_worker_codex/*.py \
  "$agent_stage/worker/src/farhelm_worker_codex/"
install -m 0644 farhelm-worker-codex/pyproject.toml "$agent_stage/worker/pyproject.toml"
install -m 0644 farhelm-worker-codex/uv.lock "$agent_stage/worker/uv.lock"
install -m 0755 deploy/agent/install.sh "$agent_stage/install.sh"
install -m 0755 deploy/agent/run.sh "$agent_stage/run.sh"
install -m 0755 deploy/agent/rollback.sh "$agent_stage/rollback.sh"
install -m 0755 deploy/agent/uninstall.sh "$agent_stage/uninstall.sh"
install -m 0644 deploy/agent/farhelm-agent.service "$agent_stage/farhelm-agent.service"
install -m 0644 deploy/README.md "$agent_stage/README.md"
install -m 0644 deploy/README.en.md "$agent_stage/README.en.md"
printf '%s\n' "$version" >"$agent_stage/VERSION"
printf 'V%s\n' "$version" >"$agent_stage/RELEASE_TAG"

tar -C "$stage_dir" -czf "$output_dir/$hub_name.tar.gz" "$hub_name"
tar -C "$stage_dir" -czf "$output_dir/$agent_name.tar.gz" "$agent_name"
(
  cd "$output_dir"
  sha256sum "$hub_name.tar.gz" "$agent_name.tar.gz" >SHA256SUMS
)

printf 'Release bundles built in %s\n' "$output_dir"
printf '  %s.tar.gz\n' "$hub_name"
printf '  %s.tar.gz\n' "$agent_name"
