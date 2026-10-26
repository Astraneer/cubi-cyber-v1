#!/usr/bin/env bash
# Shared CI runner: preserve command status and publish bounded, redacted failure
# evidence on Linux and Windows. Sourcing defines the function without running it.
run_check() {
  local cubi_label="$1"
  shift
  local cubi_root
  cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
  local cubi_log="$cubi_root/.local/artifacts/check_${cubi_label//[^[:alnum:]]/_}.log"
  mkdir -p "$cubi_root/.local/artifacts"
  if "$@" 2>&1 | tee "$cubi_log"; then
    printf 'ACCEPTANCE_OK %s\n' "$cubi_label"
  else
    local -a cubi_pipe_status=("${PIPESTATUS[@]}")
    local cubi_status="${cubi_pipe_status[0]}"
    if [[ "$cubi_status" == 0 ]]; then cubi_status="${cubi_pipe_status[1]}"; fi
    # These labels are fixed strings, never command arguments or secret values.
    # The public Actions annotation is readable even when API logs are blocked.
    printf '::error title=Acceptance gate::%s failed (exit %s)\n' "$cubi_label" "$cubi_status" >&2
    python3 "$cubi_root/tools/check_diagnostic.py" "$cubi_log"
    return "$cubi_status"
  fi
}
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  cubi_check_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
  source "$cubi_check_root/tools/env.sh"
  cd "$cubi_check_root"
  if (( $# < 2 )); then
    printf 'Usage: bash tools/check_one.sh LABEL COMMAND [ARG...]\n' >&2
    exit 2
  fi
  run_check "$@"
fi
