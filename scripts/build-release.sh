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
FARHELM_CONSOLE_EMBED_DIR="$repo_root/farhelm-console/dist" \
  cargo build --release --locked -p farhelm-hub
cargo build --release --locked -p farhelm-agent

hub_versioned="farhelm-hub-$version-$platform"
agent_versioned="farhelm-agent-$version-$platform"
hub_stable="farhelm-hub-$platform"
agent_stable="farhelm-agent-$platform"
install -m 0755 target/release/farhelm-hub "$output_dir/$hub_versioned"
install -m 0755 target/release/farhelm-agent "$output_dir/$agent_versioned"
install -m 0755 target/release/farhelm-hub "$output_dir/$hub_stable"
install -m 0755 target/release/farhelm-agent "$output_dir/$agent_stable"

# V0.3.0 keeps one compatibility archive so the V0.2.0 updater can cross to the
# native-program layout. V0.3+ clients download the versioned executables above.
stage_dir=$(mktemp -d)
trap 'rm -rf "$stage_dir"' EXIT
hub_compat="$stage_dir/$hub_versioned"
agent_compat="$stage_dir/$agent_versioned"
install -d -m 0755 "$hub_compat/bin" "$agent_compat/bin"
install -m 0755 target/release/farhelm-hub "$hub_compat/bin/farhelm-hub"
install -m 0755 target/release/farhelm-agent "$agent_compat/bin/farhelm-agent"
install -m 0755 deploy/compat/hub-install.sh "$hub_compat/install.sh"
install -m 0755 deploy/compat/agent-install.sh "$agent_compat/install.sh"
printf '%s\n' "$version" >"$hub_compat/VERSION"
printf '%s\n' "$version" >"$agent_compat/VERSION"
printf 'V%s\n' "$version" >"$hub_compat/RELEASE_TAG"
printf 'V%s\n' "$version" >"$agent_compat/RELEASE_TAG"
tar -C "$stage_dir" -czf "$output_dir/$hub_versioned.tar.gz" "$hub_versioned"
tar -C "$stage_dir" -czf "$output_dir/$agent_versioned.tar.gz" "$agent_versioned"

(
  cd "$output_dir"
  sha256sum \
    "$hub_versioned" \
    "$agent_versioned" \
    "$hub_stable" \
    "$agent_stable" \
    "$hub_versioned.tar.gz" \
    "$agent_versioned.tar.gz" >SHA256SUMS
)

printf 'FarHelm V%s native role programs built in %s\n' "$version" "$output_dir"
printf '  %s (stable alias: %s)\n' "$hub_versioned" "$hub_stable"
printf '  %s (stable alias: %s)\n' "$agent_versioned" "$agent_stable"
printf '  V0.2.0 migration archives retained for this transition\n'
