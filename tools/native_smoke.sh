#!/usr/bin/env bash
# Start only this smoke's display when the cloud has Xorg/dummy but no Xvfb.
set -euo pipefail
cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_root/tools/env.sh"
cd "$cubi_root"
cubi_native_args=("$@")
if [[ ${#cubi_native_args[@]} -eq 0 ]]; then cubi_native_args=(--smoke-frames 3); fi
if [[ -z "${VK_ICD_FILENAMES:-}" ]]; then
  for cubi_icd in "$cubi_root/.local/mesa/lvp-local.json" /usr/share/vulkan/icd.d/lvp_icd*.json; do
    if [[ -f "$cubi_icd" ]]; then export VK_ICD_FILENAMES="$cubi_icd"; break; fi
  done
fi
if [[ -n "${DISPLAY:-}" ]]; then
  exec cargo run --locked -p client-native -- "${cubi_native_args[@]}"
fi
if command -v xvfb-run >/dev/null 2>&1; then
  exec xvfb-run -a cargo run --locked -p client-native -- "${cubi_native_args[@]}"
fi
if ! command -v Xorg >/dev/null 2>&1 || ! command -v xdpyinfo >/dev/null 2>&1 || [[ ! -f /usr/lib/xorg/modules/drivers/dummy_drv.so ]]; then
  echo 'Native smoke needs an X11 display, Xvfb, or Xorg with the dummy driver.' >&2
  exit 1
fi
cubi_display=''
for cubi_number in {94..120}; do
  if [[ ! -e "/tmp/.X11-unix/X$cubi_number" && ! -e "/tmp/.X$cubi_number-lock" ]]; then
    cubi_display=":$cubi_number"
    break
  fi
done
[[ -n "$cubi_display" ]] || { echo 'No unused smoke display found.' >&2; exit 1; }
cubi_xdir="$(mktemp -d "$TMPDIR/native-xorg.XXXXXX")"
cubi_xpid=''
cleanup() {
  if [[ -n "$cubi_xpid" ]]; then
    kill "$cubi_xpid" 2>/dev/null || true
    wait "$cubi_xpid" 2>/dev/null || true
  fi
  mkdir -p "$cubi_root/.local/artifacts"
  if [[ -f "$cubi_xdir/Xorg.log" ]]; then
    cp "$cubi_xdir/Xorg.log" "$cubi_root/.local/artifacts/m0-native-xorg.log"
  fi
  rm -rf -- "$cubi_xdir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cat > "$cubi_xdir/dummy.conf" <<'EOF'
Section "ServerFlags"
    Option "AutoAddDevices" "false"
EndSection
Section "Device"
    Identifier "dummy"
    Driver "dummy"
    VideoRam 256000
EndSection
Section "Monitor"
    Identifier "monitor"
    HorizSync 30.0-80.0
    VertRefresh 60.0-75.0
    Modeline "1280x720" 74.50 1280 1344 1472 1664 720 723 728 748
EndSection
Section "Screen"
    Identifier "screen"
    Device "dummy"
    Monitor "monitor"
    DefaultDepth 24
    SubSection "Display"
        Depth 24
        Modes "1280x720"
    EndSubSection
EndSection
EOF
Xorg "$cubi_display" -noreset -nolisten tcp -ac -config "$cubi_xdir/dummy.conf" \
  -logfile "$cubi_xdir/Xorg.log" > "$cubi_xdir/stderr.log" 2>&1 &
cubi_xpid=$!
export DISPLAY="$cubi_display"
cubi_ready=false
for _ in {1..100}; do
  if xdpyinfo -display "$DISPLAY" >/dev/null 2>&1; then cubi_ready=true; break; fi
  if ! kill -0 "$cubi_xpid" 2>/dev/null; then break; fi
  sleep 0.05
done
if [[ "$cubi_ready" != true ]]; then
  cat "$cubi_xdir/stderr.log" >&2
  echo 'Own Xorg smoke display failed to start.' >&2
  exit 1
fi
cargo run --locked -p client-native -- "${cubi_native_args[@]}"
