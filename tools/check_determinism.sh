#!/usr/bin/env bash
# Execute the same world commits and donor generator in two actual runtimes.
set -euo pipefail
cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_root/tools/env.sh"
cd "$cubi_root"
if ! command -v wasm-bindgen >/dev/null 2>&1 || [[ "$(wasm-bindgen --version)" != 'wasm-bindgen 0.2.104' ]]; then
  CARGO_TARGET_DIR="$cubi_root/.local/tool-targets/wasm-bindgen" \
    cargo install wasm-bindgen-cli --version '=0.2.104' --locked
fi
cargo build --locked -p determinism-runner --bin determinism-runner
cargo build --locked -p determinism-runner --lib --target wasm32-unknown-unknown
cubi_dist="$cubi_root/.local/determinism"
mkdir -p "$cubi_dist"
wasm-bindgen --target nodejs --out-dir "$cubi_dist" --out-name determinism \
  "$CARGO_TARGET_DIR/wasm32-unknown-unknown/debug/determinism_runner.wasm"
node tools/determinism.mjs
