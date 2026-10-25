#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$root/tools/env.sh"
cd "$root"

binding_version=0.2.104
if ! command -v wasm-bindgen >/dev/null 2>&1 || [[ "$(wasm-bindgen --version)" != "wasm-bindgen $binding_version" ]]; then
  CARGO_TARGET_DIR="$root/.local/tool-targets/wasm-bindgen" \
    cargo install wasm-bindgen-cli --version "=$binding_version" --locked --root "$CARGO_HOME"
fi
cargo build --locked --target wasm32-unknown-unknown -p client-web
dist="$root/.local/web-dist"
mkdir -p "$dist/pkg"
cp "$root/web/index.html" "$dist/index.html"
wasm-bindgen --target web --out-dir "$dist/pkg" --out-name client_web "$CARGO_TARGET_DIR/wasm32-unknown-unknown/debug/client_web.wasm"
printf '%s\n' "Web build ready in $dist"
