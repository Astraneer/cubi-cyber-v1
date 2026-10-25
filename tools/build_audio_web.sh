#!/usr/bin/env bash
set -euo pipefail
cubi_audio_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_audio_root/tools/env.sh"
cd "$cubi_audio_root"
if [[ "$(wasm-bindgen --version)" != 'wasm-bindgen 0.2.104' ]]; then
  echo 'Run tools/build_web.sh first to prepare pinned wasm-bindgen.' >&2
  exit 1
fi
cargo build --locked -p audio --target wasm32-unknown-unknown
mkdir -p .local/web-dist/audio-pkg
wasm-bindgen --target web --out-dir .local/web-dist/audio-pkg --out-name audio target/wasm32-unknown-unknown/debug/audio.wasm
cp crates/audio/tests/browser.html .local/web-dist/audio-test.html
