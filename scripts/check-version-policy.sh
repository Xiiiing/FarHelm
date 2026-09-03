#!/usr/bin/env bash
set -euo pipefail

version=$(<VERSION)
if [[ ! "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  printf 'VERSION must be MAJOR.MINOR.PATCH without a prefix.\n' >&2
  exit 1
fi
if [[ "${BASH_REMATCH[1]}" != 0 ]]; then
  printf 'MAJOR is locked to 0 until the user explicitly approves a change.\n' >&2
  exit 1
fi

grep -Fxq "version = \"$version\"" <(sed -n '/\[workspace.package\]/,/^$/p' Cargo.toml)
grep -Fq "\"version\": \"$version\"" farhelm-console/package.json
grep -Fxq "version = \"$version\"" farhelm-worker-codex/pyproject.toml
grep -Fxq "__version__ = \"$version\"" farhelm-worker-codex/src/farhelm_worker_codex/__init__.py
grep -Fq "V$version" README.md
grep -Fq "V$version" README.en.md
grep -Fq "V$version" deploy/README.md
grep -Fq "V$version" deploy/README.en.md
grep -Fq "name: farhelm-V$version-linux-x86_64" .github/workflows/ci.yml
if grep -q '"farhelmctl"\|"crates/farhelm-bootstrap"' Cargo.toml; then
  printf 'V0.3+ must expose only the Hub and Agent role programs.\n' >&2
  exit 1
fi

if [[ ${GITHUB_REF_TYPE:-} == tag ]] && [[ ${GITHUB_REF_NAME:-} != "V$version" ]]; then
  printf 'Formal release tag must exactly match V%s.\n' "$version" >&2
  exit 1
fi

printf 'Version policy passed for V%s.\n' "$version"
