#!/usr/bin/env bash
# Exercise a real ALSA stream callback with an explicitly named virtual sink.
# This proves callback/backend integration, never audibility or hardware latency.
set -euo pipefail
cubi_audio_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_audio_root/tools/env.sh"
cd "$cubi_audio_root"
if [[ "$(uname -s)" != Linux ]]; then
  exec cargo run --locked -p audio --example native_smoke
fi
mkdir -p .local/tmp
cubi_audio_config="$(mktemp "$TMPDIR/audio-null.XXXXXX")"
trap 'rm -f -- "$cubi_audio_config"' EXIT
cat > "$cubi_audio_config" <<'EOF'
pcm.!default {
    type null
}
EOF
ALSA_CONFIG_PATH="$cubi_audio_config" CUBI_AUDIO_VIRTUAL=1 \
  cargo run --locked -p audio --example native_smoke
