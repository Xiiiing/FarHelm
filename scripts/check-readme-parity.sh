#!/usr/bin/env bash
set -euo pipefail

for file in README.md README.en.md; do
  test -s "$file"
  for component in farhelm-hub farhelm-agent farhelm-console farhelm-worker-codex farhelmctl; do
    grep -q "$component" "$file"
  done
done

grep -q 'README.en.md' README.md
grep -q 'README.md' README.en.md
echo "README language parity check passed."
