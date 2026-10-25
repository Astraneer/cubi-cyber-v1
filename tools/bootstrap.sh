#!/usr/bin/env bash
# Reproduce the pinned toolchain without changing the user's shell profile.
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$root/tools/env.sh"
cd "$root"

if [[ "$(uname -s)" == Linux ]] && ! pkg-config --exists alsa; then
  # This helper preserves Debian signature/index/archive verification and writes
  # only .local; other Linux hosts should install trusted libasound2-dev packages.
  python3 tools/install_audio_deps.py
  source "$root/tools/env.sh"
fi

if ! command -v rustup >/dev/null 2>&1; then
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64) triple=x86_64-unknown-linux-gnu ;;
    Linux:aarch64) triple=aarch64-unknown-linux-gnu ;;
    Darwin:x86_64) triple=x86_64-apple-darwin ;;
    Darwin:arm64) triple=aarch64-apple-darwin ;;
    *) printf '%s\n' 'Install rustup for this host, then rerun tools/bootstrap.sh.' >&2; exit 1 ;;
  esac
  mkdir -p "$root/.local/downloads"
  installer="$root/.local/downloads/rustup-init"
  url="https://static.rust-lang.org/rustup/dist/$triple/rustup-init"
  curl --fail --location --proto '=https' --tlsv1.2 "$url" --output "$installer"
  curl --fail --location --proto '=https' --tlsv1.2 "$url.sha256" --output "$installer.sha256"
  python3 - "$installer" <<'PY'
import hashlib
from pathlib import Path
import sys
p = Path(sys.argv[1])
expected = p.with_suffix('.sha256').read_text().split()[0]
actual = hashlib.sha256(p.read_bytes()).hexdigest()
if actual != expected:
    raise SystemExit('rustup-init SHA-256 verification failed')
PY
  chmod +x "$installer"
  "$installer" -y --no-modify-path --default-toolchain none
fi

# rust-toolchain.toml is the source of truth for components and WASM target.
rustup toolchain install 1.90.0 --profile minimal --component clippy --component rustfmt --target wasm32-unknown-unknown
cargo fetch --locked
printf '%s\n' 'Toolchain and locked dependencies ready. Use: source tools/env.sh'
