#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

if [[ $(uname -s) != Linux ]] || [[ $(uname -m) != x86_64 ]]; then
  printf 'This release profile currently supports Linux x86_64 only.\n' >&2
  exit 1
fi

for command_name in cargo corepack sha256sum; do
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
install -m 0644 farhelm-console/public/third-party-notices.txt "$output_dir/farhelm-third-party-notices.txt"

(
  cd "$output_dir"
  sha256sum \
    "$hub_versioned" \
    "$agent_versioned" \
    "$hub_stable" \
    "$agent_stable" \
    farhelm-third-party-notices.txt >SHA256SUMS
)

printf 'FarHelm V%s native role programs built in %s\n' "$version" "$output_dir"
printf '  %s (stable alias: %s)\n' "$hub_versioned" "$hub_stable"
printf '  %s (stable alias: %s)\n' "$agent_versioned" "$agent_stable"
