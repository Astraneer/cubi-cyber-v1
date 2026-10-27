#!/usr/bin/env python3
"""Build only the independent optional View fixture; never product web-dist."""
import hashlib
import json
import os
import pathlib
import shlex
import subprocess
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGET = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
DEP = TARGET / "wasm32-unknown-unknown/debug/temporal_view_probe.d"
WASM = DEP.with_suffix(".wasm")
DIST = ROOT / ".local/taa-view-web"
MANIFEST = ROOT / "tools/temporal-view-probe/Cargo.toml"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inputs():
    paths = set()
    if DEP.exists():
        for item in shlex.split(DEP.read_text().splitlines()[0].split(": ", 1)[1]):
            path = pathlib.Path(item).resolve()
            if path.is_relative_to(ROOT) and not path.is_relative_to(ROOT / ".local") and not path.is_relative_to(TARGET):
                paths.add(path)
    paths.update((ROOT / "tools/temporal-view-probe").glob("src/*.rs"))
    paths.update(ROOT / path for path in [
        "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "tools/temporal-view-probe/Cargo.toml", "tools/temporal-view-probe/Cargo.lock",
        "tools/temporal_view_gpu_smoke.mjs", "tools/temporal_view_reference.mjs",
        "tools/temporal_far_gpu_smoke.mjs", "tools/temporal_far_reference.mjs",
        "tools/temporal_boards_gpu_smoke.mjs", "tools/temporal_boards_reference.mjs",
        "tools/build_temporal_view_probe.py",
    ])
    return {str(path.relative_to(ROOT)): digest(path) for path in sorted(paths)}


for attempt in range(3):
    before = inputs()
    start = time.time_ns()
    subprocess.run(["cargo", "build", "--locked", "--offline", "--manifest-path", str(MANIFEST), "--target", "wasm32-unknown-unknown"], cwd=ROOT, check=True)
    after = inputs()
    touched = [name for name in after if (ROOT / name).stat().st_mtime_ns > start]
    if before == after and not touched:
        break
    print(json.dumps({"retry": attempt + 1, "source_drift": touched, "input_set_changed": before.keys() != after.keys()}), flush=True)
else:
    raise SystemExit("View probe compiler inputs did not settle; no source-coherence claim")

(DIST / "pkg").mkdir(parents=True, exist_ok=True)
subprocess.run(["wasm-bindgen", "--target", "web", "--out-name", "temporal_view_probe", "--out-dir", str(DIST / "pkg"), str(WASM)], check=True)
(DIST / "index.html").write_text('<!doctype html><meta charset="utf-8"><title>Optional ClientView GPU fixture</title><style>body{margin:0}canvas{display:block}</style><canvas></canvas>')
result = {
    "scope": "Independent optional ClientView fixture, not product UI or server acceptance",
    "noSourceMutationDuringBuild": True,
    "inputs": after,
    "wasm": {"sha256": digest(DIST / "pkg/temporal_view_probe_bg.wasm"), "bytes": (DIST / "pkg/temporal_view_probe_bg.wasm").stat().st_size},
    "dist": str(DIST),
    "target": str(TARGET),
    "profileDebug": os.environ.get("CARGO_PROFILE_DEV_DEBUG", "default"),
}
(DIST / "manifest.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps({"dist": str(DIST), "inputs": len(after), "wasm": result["wasm"]}), flush=True)
