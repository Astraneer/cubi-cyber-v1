#!/usr/bin/env bash
# Every check here is required for the local M0 acceptance run.
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$root/tools/env.sh"
cd "$root"

# Software adapters are explicit so a machine without hardware still exercises
# the actual shader and readback path. Never convert absent GPU support to PASS.
if [[ -z "${VK_ICD_FILENAMES:-}" ]]; then
  for icd in "$root/.local/mesa/lvp-local.json" /usr/share/vulkan/icd.d/lvp_icd*.json /usr/lib/chromium/vk_swiftshader_icd.json; do
    if [[ -f "$icd" ]]; then export VK_ICD_FILENAMES="$icd"; break; fi
  done
fi
export WGPU_BACKEND=vulkan
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
python3 -m unittest discover -s tools -p 'test_*.py'
bash tools/native_smoke.sh
bash tools/build_web.sh
bash tools/check_determinism.sh
node crates/jobs/tests/browser_smoke.mjs
node tools/web_smoke.mjs
if ! command -v cargo-deny >/dev/null 2>&1 || [[ "$(cargo deny --version)" != 'cargo-deny 0.18.4' ]]; then
  CARGO_TARGET_DIR="$root/.local/tool-targets/cargo-deny" \
    cargo install cargo-deny --version '=0.18.4' --locked
fi
# Run this gate after functional checks so an independent license failure does
# not hide whether the native and browser shells actually render.
# No arbitrary Git repositories are fetched; the policy concerns locked crates.
cargo deny --locked check licenses bans sources
