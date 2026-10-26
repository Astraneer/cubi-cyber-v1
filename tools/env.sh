#!/usr/bin/env bash
# Source from any working directory. All task-specific writes stay in this repo.
_cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_HOME="$_cubi_root/.local/cargo"
export RUSTUP_HOME="$_cubi_root/.local/rustup"
export CARGO_TARGET_DIR="$_cubi_root/target"
export XDG_CACHE_HOME="$_cubi_root/.local/cache"
export XDG_CONFIG_HOME="$_cubi_root/.local/config"
export XDG_RUNTIME_DIR="$_cubi_root/.local/run"
export TMPDIR="$_cubi_root/.local/tmp"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
# Cloud disks are bounded; source checkpoints replace multi-gigabyte incremental
# histories. Callers may explicitly opt in on a larger development machine.
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
export PATH="$CARGO_HOME/bin:$_cubi_root/.local/bin:$PATH"
if [[ -f "$_cubi_root/.local/audio-deps/usr/lib/x86_64-linux-gnu/pkgconfig/alsa.pc" ]]; then
  export PKG_CONFIG_PATH="$_cubi_root/.local/audio-deps/usr/lib/x86_64-linux-gnu/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi
mkdir -p "$CARGO_HOME" "$RUSTUP_HOME" "$XDG_CACHE_HOME" "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR" "$TMPDIR"
chmod 700 "$XDG_RUNTIME_DIR"
unset _cubi_root
