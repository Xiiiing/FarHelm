#!/usr/bin/env bash
set -euo pipefail

tracked_files=$(git ls-files)
staged_files=$(git diff --cached --name-only --diff-filter=ACMR)
candidate_files=$(printf '%s\n%s\n' "$tracked_files" "$staged_files" | sed '/^$/d' | sort -u)

forbidden_pattern='(^|/)Docs/|(^|/)AGENTS(\.override)?\.md$|\.(db|sqlite)(-|$)|\.(pem|key)$|config\.local\.toml$'

if printf '%s\n' "$candidate_files" | grep -E "$forbidden_pattern"; then
  echo "Refusing to publish private documentation, instructions, state, or credentials." >&2
  exit 1
fi

if printf '%s\n' "$candidate_files" | grep -E '(^|/)\.env($|\.)' | grep -v -E '(^|/)\.env\.example$'; then
  echo "Refusing to publish environment files other than .env.example." >&2
  exit 1
fi

sensitive_pattern='-----BEGIN ([A-Z ]+ )?PRIVATE KEY-----|github_pat_[A-Za-z0-9_]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}|sk-[A-Za-z0-9]{20,}'
if git grep -I -n -E -e "$sensitive_pattern" -- . || \
  git diff --cached --no-ext-diff -U0 | grep -E -e "$sensitive_pattern"; then
  echo "Refusing to publish content that resembles a credential or private key." >&2
  exit 1
fi

echo "Public tree privacy check passed."
