#!/usr/bin/env bash
# Mandatory release owner workload; the normal debug workspace suite skips it.
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$root/tools/env.sh"
cd "$root"
source "$root/tools/check_one.sh"
mkdir -p "$root/.local/artifacts"
git rev-parse HEAD > "$root/.local/artifacts/spec86-source-sha.txt"
run_check 'Canonical server scale release' cargo test --release --locked --offline \
  -p server --test spec86_owner \
  actual_combined_owner_preserves_results_and_recovers_durable_state \
  -- --ignored --exact --nocapture
run_check 'Canonical server scale evidence' python3 tools/verify_server_scale.py \
  "$root/.local/artifacts/check_Canonical_server_scale_release.log"
