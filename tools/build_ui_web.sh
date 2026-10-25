#!/usr/bin/env bash
set -euo pipefail
cubi_ui_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_ui_root/tools/env.sh"
cd "$cubi_ui_root"
[[ "$(wasm-bindgen --version)" == 'wasm-bindgen 0.2.104' ]]
cargo build --locked -p ui --target wasm32-unknown-unknown
mkdir -p .local/web-dist/ui-pkg
wasm-bindgen --target web --out-dir .local/web-dist/ui-pkg --out-name ui target/wasm32-unknown-unknown/debug/ui.wasm
cp crates/ui/tests/browser.html .local/web-dist/ui-test.html
