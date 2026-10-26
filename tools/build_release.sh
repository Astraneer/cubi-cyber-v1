#!/usr/bin/env bash
# Build actual portable programs; never package developer caches or identities.
set -euo pipefail
cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_root/tools/env.sh"
cd "$cubi_root"
case "${1:-}" in
  linux|windows) cubi_platform="$1" ;;
  *) printf '%s\n' 'Usage: bash tools/build_release.sh linux|windows [--web]' >&2; exit 2 ;;
esac
shift
cubi_web=false
if [[ "${1:-}" == --web ]]; then cubi_web=true; shift; fi
if [[ $# -ne 0 ]]; then printf '%s\n' 'Unexpected release argument' >&2; exit 2; fi
cargo build --locked --release -p client-native -p server
cargo build --locked --release -p client --example benchmark
cargo build --locked --release -p sim --example replay
bash tools/package native --directory "$CARGO_TARGET_DIR/release" --platform "$cubi_platform" --out ".local/artifacts/dist/cubi-$cubi_platform.zip"
if [[ "$cubi_web" == true ]]; then
  bash tools/build_web.sh --release
  bash tools/package web --directory .local/web-dist --out .local/artifacts/dist/cubi-web.zip
fi
