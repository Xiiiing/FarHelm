#!/usr/bin/env bash
set -euo pipefail

bundle_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
exec "$bundle_dir/bin/farhelm-hub" install
