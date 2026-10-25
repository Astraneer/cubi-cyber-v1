#!/usr/bin/env bash
# Retain actual native failure context even when the cloud cannot read API logs.
set -euo pipefail
cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_root/tools/env.sh"
cd "$cubi_root"
mkdir -p .local/artifacts
export RUST_BACKTRACE=1
if cargo run --locked -p client-native -- --smoke-frames 3 2>&1 | tee .local/artifacts/windows-native.log; then
  exit 0
else
  cubi_codes=("${PIPESTATUS[@]}")
  cubi_status="${cubi_codes[0]}"
  if [[ "$cubi_status" == 0 ]]; then cubi_status="${cubi_codes[1]}"; fi
  python tools/check_diagnostic.py .local/artifacts/windows-native.log
  exit "$cubi_status"
fi
