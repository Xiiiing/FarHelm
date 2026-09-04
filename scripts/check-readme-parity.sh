#!/usr/bin/env bash
set -euo pipefail

for file in README.md README.en.md; do
  test -s "$file"
  grep -Fq '<div align="center">' "$file"
  grep -Fq 'farhelm-console/public/farhelm-mark.svg' "$file"
  grep -Fq 'releases/latest/download/farhelm-hub-linux-x86_64' "$file"
  grep -Fq 'releases/latest/download/farhelm-agent-linux-x86_64' "$file"
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
