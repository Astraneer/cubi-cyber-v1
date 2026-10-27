This standalone diagnostic compiles the production Rust `ClientView` into a
separate WASM module. It does not change product entry points or `.local/web-dist`.

From the repository root:

```sh
source tools/env.sh
python3 tools/build_temporal_view_probe.py
node tools/temporal_view_gpu_smoke.mjs
```

Builds fence the actual dependency-input bytes and mtimes. The probe reads real
canvas pixels, motion/identity textures, TAA counters and complete encoder GPU
intervals. Histories commit only after queue completion and the canvas
presentation API plus a browser animation-frame boundary. Abandoned and submitted
but unpresented candidates target an offscreen attachment: WebGPU canvas output
can present implicitly, so dropping its Rust wrapper would not prove cancellation.

CPU jobs run synchronously through their existing production codecs to prepare
the controlled replica. This is not a Worker/performance claim. Source LIGHT is
an explicitly authored constant fixture. The full-effects case prepares distant
motion banks but has no committed far front; actual Height/Proxy image acceptance
remains a separate requirement. The moving Human uses a stable full UUID and a
fixed camera, with a separate background control and ordinary MSAA4 image
reference. Static warmup must reach eight real phases. The ghost witnesses use
the flat floor, keeping unrelated floor/wall and Chest AA boundaries separate.

TAA mode 2 remains hidden in product settings until all source, whole-graph and
platform requirements pass. No default-product or hardware-FPS acceptance follows
from this diagnostic.

The separate `run_temporal_far_probe` / `tools/temporal_far_gpu_smoke.mjs` route
commits 25 public Height tiles and eight exact Geometry pages, pans the camera,
and replaces the source to remove its pillar. Both Native and actual Dawn passed
family16/17 motion and the strict vacated-interior comparison. Accepted opaque
bundles and raw results are retained under `.local/artifacts/temporal-far-probe`.
A partial-alpha roof additionally checks current-only color and zero inherited
surface identities before any product mode2 acceptance.

To keep the standalone crate's `ui` rlib metadata independent of root-workspace
builds, set `CARGO_TARGET_DIR` to a dedicated directory after `tools/env.sh`.
The build script records and uses that directory; default product dist is untouched.
