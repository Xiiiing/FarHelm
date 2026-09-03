#!/usr/bin/env bash
set -euo pipefail

for file in README.md README.en.md; do
  test -s "$file"
  for component in farhelm-hub farhelm-agent farhelm-console farhelm-worker-codex; do
    grep -q "$component" "$file"
  done
done

grep -q 'README.en.md' README.md
grep -q 'README.md' README.en.md
test -s deploy/README.md
test -s deploy/README.en.md
grep -q 'README.en.md' deploy/README.md
grep -q 'README.md' deploy/README.en.md
echo "README language parity check passed."
