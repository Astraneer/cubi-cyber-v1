# Optional temporal model motion

The model source is implemented separately from the TAA resolver and is not yet
enabled by a product graphics receipt. Default MSAA1/4 does not prepare its pose,
metadata, target or pipeline resources. Water/cloud invalidation, distant-family
motion and whole-View presentation are separate required integration gates.

`render::presented_motion::PresentedMotion<P>` retains two loading-preallocated
banks of actual64-byte Entity/Block records, exact object keys and a sorted index.
The index handles reordered uploads in O(N log N), without per-frame allocation
or quadratic pair searches. Named keys retain every PlayerId bit; their GPU tokens
are checked allocations, never hashes. Native Mob/Item/Packet IDs retain all64 bits
in separate families and Spaces. Props use exact signed block coordinates and a
checked registration lifetime. Model kind/connectivity, spawn/despawn, hidden
primary geometry, teleport, Space, topology/provenance epoch and resize revoke
previous-pose validity. A current known identity still seeds the first history
frame when the previous pose is unavailable.

The resolver's `FrameToken` converts through both exact frame and serial fields.
`stage` and GPU upload do not advance the presented bank. `validate_commit` lets
the View validate every participating owner before its first commit mutation.
Only actual GPU completion plus presentation may call `commit_presented`;
`discard` retains the last presented pose/camera. Cached, UI-only and join-held
frames do not stage or commit a candidate.

The optional loading variants factor the existing current model vertex body into
a shared pose function. Color and motion evaluate that same static catalog with
current and previous presented64-byte records, including chest lid, wind rotor,
human head, limbs, crouch and death roll. The motion pass uses the actual primary
visibility/indirect buffers, a read-only final single-sample depth attachment and
strict `Equal`. It never replaces the primary cull lists or loosens depth to hide
rounding holes. Only explicitly enabled TAA color positions become invariant;
the default shader source is unchanged.

The separate pass uses24 attachment bytes/sample (RGBA16Float motion plus exact
RGBA32Uint identity), below the portable32-byte limit even when primary color
also has ambient MRT. It adds two vertex storage buffers and no fragment storage.
Motion is previous unjittered UV minus current unjittered UV, predicted previous
reverse depth, and validity. Identity includes family, exact catalog triangle,
UID low/high and generation. Unsupported colored glass/markers write invalid
metadata instead of vectors for the surface behind them.

Both UVs use interpolated homogeneous clips for the same actual rasterized
surface point. Subtracting the fragment pixel center instead would inject the
rasterizer's vertex snapping residual into even stationary geometry. Only the
current jitter is removed here; the previous matrix is already unjittered.

`MotionTargets::required_allocation_bytes` preflights24P plus optional256-byte
queries without device calls. Each prepared `ModelMotionGpu` adds96N+80 GPU bytes;
its preflight and actual byte getter agree. These bytes are additional to the
resolver's64P storage, not another copy of the24P already included in the total
88P TAA image formula. Actual extra startup pipelines are counted: Entity color1
+motion1; Block color2+motion2. Frame pipeline compilations remain zero. Pose,
metadata and camera upload bytes are preflighted together under the shared byte
budget, including a zero-write insufficient-budget refusal.

`ViewEntities` and `ViewProps` optionally retain identities beside successful GPU
publication. A failed upload does not advance this bank while CPU interpolation,
retirement or slot reuse continues. `PropsPublication` is emitted only after
`BlockModelsGpu::update` succeeds, including an identical64-byte pose whose logical
object lifetime changed. `RemoteHumans::write_with_identities` emits fullPlayerIds
in the same validated output order. `RemoteSession::configure_temporal_identities`
must run during loading; no optional identity bank is allocated in the frame loop.
Unkeyed legacy model input stays explicitly invalid for temporal history.
The Props registration array is optional too; ordinary profiles retain zero
registration bytes and never allocate temporal lifetimes.

`view::model_motion::ViewModelMotion` is a prepared model/target transaction owner.
Its stage phase is CPU-only, `required_upload_bytes` participates in Env's global
preflight, and upload/draw/mark-encoded use the same resolver token. The View must
include Terrain/Height/Proxy draws and optical invalidators, then validate all
owners before committing. This helper is exported for integration but is not
attached to the product View or exposed as an accepted mode.

## Current verification

- Seven CPU cache tests pass: full identity/reorder, presented bones/discard,
  resolver-token transaction/ABA, spawn/topology/hidden/teleport/epochs, rigid
  origin rebase, atomic invalid input, and1000 static phases with zero allocations.
- Four native GPU tests pass on Ubuntu Mesa25.2.8. Real Chest/Wind motion has688
  independent f64 triangle witnesses (including206 articulated-bone witnesses).
  Human motion with colliding legacy display IDs has2308 f64 witnesses,1864
  articulated witnesses, fullUUID keys, reordered records and a discarded GPU
  frame. The static-camera zero-vector negative is wrong at688/2307 pixels.
  Every actual supported model depth pixel receives exact-depth metadata.
- Actual different previous/current jitter produces651 f64 witnesses and483
  matches in the previously rendered surface image, versus60 with camera-only
  zero movement. A subsequent stationary frame has exactly zero velocity.
- A one-byte-short model upload budget preserves old pixels and metadata bitwise.
  One thousand actual static model upload/cache phases have zero upload/allocation.
- Default TAA-off MSAA1/4 HDR and every depth sample remain bitwise unchanged after
  disabled preparation. The explicitly enabled invariant1-sample color variant
  has identical HDR/coverage but35 depth rounding differences, maximum6.06e-9;
  this observation is not presented as a default-path change or a depth tolerance.
- All48 actual lighting/GI/ambient source variants compile on the native device.
  Props GPU publication/lifetime and Human identity tests pass, including bounded
  capacity refusal and1000/10000 idle phases without allocations.
- Actual Dawn passes four source cases with strict depthEqual: deformed
  Chest/Wind and Humans, plus both families with different previous/current
  jitter. Every stationary supported pixel has exactly zero velocity. Physical
  previous-image surface warps are515/1667 versus69/483 with camera-only vectors.
  Independent f64 vector witnesses are683/2313 without jitter and645/2297 with
  jitter, all within the unchanged explicit half conversion bound.
- The Dawn reference first calibrates raster interpolation from three independent
  RGBA32Float triangles, including rounding ties. Exactly one of the tested grids
  and rounding modes must match. The measured SwiftShader run has73 witnesses,
  four subpixel bits, nearest-even rounding and maximum error6.92e-8. No driver
  name, subpixel constant or sample-coordinate assumption enters production.
  Actual f32 model VS clips are captured through a separate two-attachment
  point pass (32 bytes/sample), then projected/interpolated independently in
  JavaScript f64 on that measured raster grid. This avoids falsely requiring
  ideal f64 trigonometry or unsnapped vertices from a conforming GPU rasterizer.
  Surface IDs/parts, exact primary lists, bootstrap validity and the
  static-camera negative controls remain strict.

Logs: `.local/model-motion-anchor-native.log`,
`.local/model-motion-anchor-dawn-physical.log`, `.local/motion-view-identity-gpu.log`,
`.local/model-motion-first-gpu.log`, `.local/motion-presented-cache-current-tests.log`,
`.local/motion-props-publication-gpu.log`, `.local/motion-human-identities-tests.log`.
The Dawn source/witness dump is optional in `model_motion_gpu.rs`; run with
`MODEL_MOTION_DUMP=.local/model-motion-oracle`, then
`node tools/model_motion_gpu_smoke.mjs`. Its catalogs, shaders, compute cull,
visibility/indirect lists and bone records are the production inputs. Native
ideal-bone f64 witnesses remain independent; the Dawn reference uses observed
actual VS clips plus the independently calibrated f64 raster model.
