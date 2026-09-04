#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

if [[ $(uname -s) != Linux ]] || [[ $(uname -m) != x86_64 ]]; then
  printf 'This release profile currently supports Linux x86_64 only.\n' >&2
  exit 1
fi

for command_name in cargo corepack sha256sum tar uv; do
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
uv python install 3.12

hub_versioned="farhelm-hub-$version-$platform"
agent_versioned="farhelm-agent-$version-$platform"
hub_stable="farhelm-hub-$platform"
agent_stable="farhelm-agent-$platform"
runtime_versioned="farhelm-codex-runtime-$version-$platform.tar.gz"
install -m 0755 target/release/farhelm-hub "$output_dir/$hub_versioned"
install -m 0755 target/release/farhelm-agent "$output_dir/$agent_versioned"
install -m 0755 target/release/farhelm-hub "$output_dir/$hub_stable"
install -m 0755 target/release/farhelm-agent "$output_dir/$agent_stable"
runtime_staging=$(mktemp -d)
trap 'rm -rf "$runtime_staging"' EXIT
managed_python=$(readlink -f "$(uv python find 3.12 --managed-python)")
managed_python_root=$(dirname "$(dirname "$managed_python")")
cp -a "$managed_python_root" "$runtime_staging/python"
cp -a \
  farhelm-worker-codex/src \
  farhelm-worker-codex/pyproject.toml \
  farhelm-worker-codex/uv.lock \
  farhelm-worker-codex/README.md \
  "$runtime_staging/"
uv venv "$runtime_staging/.venv" \
  --python "$runtime_staging/python/bin/python3.12" \
  --relocatable
UV_PROJECT_ENVIRONMENT="$runtime_staging/.venv" \
  uv sync \
    --project "$runtime_staging" \
    --frozen \
    --no-dev \
    --link-mode copy \
    --python "$runtime_staging/python/bin/python3.12"
ln -sfn ../../python/bin/python3.12 "$runtime_staging/.venv/bin/python"
PYTHONPATH="$runtime_staging/src" \
  "$runtime_staging/.venv/bin/python" -c \
  "import importlib.metadata as m,sys; assert sys.version_info[:2] == (3, 12); assert m.version('openai-codex') == '0.147.0'; assert m.version('farhelm-worker-codex') == '$version'"
tar -C "$runtime_staging" -czf "$output_dir/$runtime_versioned" .venv python src pyproject.toml uv.lock README.md

(
  cd "$output_dir"
  sha256sum \
    "$hub_versioned" \
    "$agent_versioned" \
    "$hub_stable" \
    "$agent_stable" \
    "$runtime_versioned" >SHA256SUMS
)

printf 'FarHelm V%s native role programs built in %s\n' "$version" "$output_dir"
printf '  %s (stable alias: %s)\n' "$hub_versioned" "$hub_stable"
printf '  %s (stable alias: %s)\n' "$agent_versioned" "$agent_stable"
printf '  %s (managed Python 3.12 runtime)\n' "$runtime_versioned"
