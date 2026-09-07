#!/usr/bin/env bash
# Run locally on an approved Agent project. No Python SDK is required.
set -Eeuo pipefail

report_project=${FARHELM_PROJECT:-cc08}
report_run_id=${FARHELM_RUN_ID:-batch-$(date -u +%Y%m%dT%H%M%SZ)-$$}

report_finished() {
  local training_exit=$?
  trap - EXIT
  farhelm-agent experiment report --project "$report_project" \
    --run-id "$report_run_id" --name '8 rounds of training' \
    --exit-code "$training_exit" --message 'Training batch ended' ||
    printf 'FarHelm could not save the report; retry with run ID %s and exit code %s.\n' "$report_run_id" "$training_exit" >&2
  exit "$training_exit"
}
trap report_finished EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for round in {1..8}; do
  python3 train.py --round "$round"
done
