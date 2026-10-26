#!/usr/bin/env bash
set -euo pipefail
cubi_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$cubi_root/tools/env.sh"
cd "$cubi_root"
cargo run --locked -p client --example benchmark -- --software --width 320 --height 224 --out .local/artifacts/bench/acceptance.json
cargo run --locked -p sim --example replay -- .local/artifacts/bench/acceptance.json
bash tools/bench report .local/artifacts/bench/acceptance.json --out .local/artifacts/bench/acceptance.html
python3 - <<'PY'
from tools.bench_report import load
r = load('.local/artifacts/bench/acceptance.json')
assert len(r['frames']) == len(r['inputs']) == 1800
assert r['benchmark']['routeComplete']
assert r['completeWindow'] and r['lostFrames'] == r['lostInputs'] == 0
assert sum(f['gpuNs'] is not None for f in r['frames']) > 1000
assert sum(f['work']['uploads'] for f in r['frames']) > 0
assert all(f['work']['pipelines'] == f['work']['syncGeneration'] == 0 for f in r['frames'])
print('BENCH_ACCEPTANCE_OK full_route=true actual_gpu=true')
PY
