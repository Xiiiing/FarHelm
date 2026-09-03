#!/usr/bin/env bash
set -euo pipefail

install_root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
config_file="$install_root/config/agent.env"
if [[ ! -r "$config_file" ]]; then
  printf 'Agent configuration is missing or unreadable: %s\n' "$config_file" >&2
  exit 1
fi

while IFS='=' read -r key value; do
  [[ -z "$key" || "$key" == \#* ]] && continue
  if [[ ! "$key" =~ ^[A-Z][A-Z0-9_]*$ ]]; then
    printf 'Invalid setting name in %s: %s\n' "$config_file" "$key" >&2
    exit 1
  fi
  export "$key=$value"
done <"$config_file"

exec "$install_root/bin/farhelm-agent" run "$@"
