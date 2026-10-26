#!/usr/bin/env bash
# Required local acceptance checks for the implemented stages.
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
source "$root/tools/check_one.sh"
run_check 'Rust format' cargo fmt --all -- --check
run_check 'Rust Clippy' cargo clippy --locked --workspace --all-targets -- -D warnings
run_check 'Workspace tests' cargo test --locked --workspace
run_check 'Benchmark route and HTML' bash tools/check_bench.sh
run_check 'Python helpers' python3 -m unittest discover -s tools -p 'test_*.py'
run_check 'Native window' bash tools/native_smoke.sh
run_check 'Web build' bash tools/build_web.sh
run_check 'Native/WASM determinism' bash tools/check_determinism.sh
run_check 'Web Worker SAB/transfer' node crates/jobs/tests/browser_smoke.mjs
run_check 'WebGPU sky pixels' node tools/web_smoke.mjs
run_check 'WebGPU GI, water and worker bounds' node tools/m10_graphics_smoke.mjs
run_check 'WebGPU cloud optics and sample preservation' node tools/cloud_gpu_smoke.mjs
run_check 'WebGPU sparse shadow footprint and depth preservation' node tools/shadow_footprint_smoke.mjs
run_check 'Distant shadow oracle inputs' cargo run --locked -p render --example distant_shadow_inputs
run_check 'WebGPU public hill building cutout and Unknown shadows' node tools/distant_shadow_smoke.mjs
run_check 'WebGPU exact shadow alpha lifecycle' node tools/shadow_alpha_gpu_smoke.mjs
run_check 'Model GI oracle inputs' cargo run --locked -p render --example model_shaders
run_check 'WebGPU model GI and absent-probe behavior' node tools/model_gi_gpu_smoke.mjs
run_check 'World recipe board shader inputs' cargo run --locked -p render --example recipe_board_shaders -- .local/recipe-board-shaders
run_check 'WebGPU world recipe boards and depth' node tools/recipe_boards_gpu_smoke.mjs
run_check 'WebGPU water reflections and fallback' node tools/water_reflection_gpu_smoke.mjs
run_check 'WebGPU contact occlusion and direct-light preservation' node tools/contact_ao_gpu_smoke.mjs
run_check 'WebGPU SMAA analytic coverage and phase sweeps' node tools/smaa_gpu_smoke.mjs
run_check 'Public LOD oracle inputs' cargo run --locked -p render --example lod_oracle_inputs
run_check 'WebGPU public LOD mixed cutouts and per-sample depth' node tools/lod_gpu_smoke.mjs
run_check 'Portable height JOIN inputs' cargo run --locked -p render --example height_oracle_inputs
run_check 'Independent finite height support' node tools/height_reference_test.mjs
run_check 'WebGPU production CompactUnit near/far JOIN' node tools/height_join_gpu_smoke.mjs
run_check 'WebGPU forest and movement' node tools/terrain_smoke.mjs
run_check 'Player edits, completed frames and image equality' node tools/player_smoke.mjs
run_check 'Authority binary for product scenario' cargo build --locked -p server
run_check 'Product UI, local imports, privacy and offline F8' node tools/product_ui_smoke.mjs
if ! command -v cargo-deny >/dev/null 2>&1 || [[ "$(cargo deny --version)" != 'cargo-deny 0.18.4' ]]; then
  CARGO_TARGET_DIR="$root/.local/tool-targets/cargo-deny" \
    cargo install cargo-deny --version '=0.18.4' --locked
fi
# Run this gate after functional checks so an independent license failure does
# not hide whether the native and browser shells actually render.
# No arbitrary Git repositories are fetched; the policy concerns locked crates.
run_check 'Dependency policy' cargo deny --locked check licenses bans sources
