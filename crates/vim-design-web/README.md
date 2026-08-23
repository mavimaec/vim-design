# vim-design-web

WASM browser test app. Two pages, served by
`pwsh devops/vactions.ps1 -VimDesignWeb`:

- **`www/index.html` — the interactive demo** (`src/demo/`): a wgpu
  renderer (WebGPU with automatic WebGL2 fallback; right-handed Z-up,
  orbit/zoom camera, lambert shading, per-submesh material colors, and an
  alpha-blended tessellation-wireframe overlay toggled from the panel).
  The scene — floor plate with a hole, cube, cylinder, cone — is authored
  at startup through the real command API, each object wrapped in an
  `Element` and placed with an `Instance`. Seven sliders submit coalesced
  `Update*` commands (the cube-chamfer slider also creates/deletes a
  `Chamfer` on the cube's two opposite top-rim edges — the largest set
  monstertruck-fillet supports in one operation — swapping the cube
  element's member between extrusion and chamfer) and drive the
  `eval::Engine` facade
  (`evaluate_pending` → `poll_updates` → GPU upload); the status line
  shows generations, commit→mesh latency, triangle count, and eval
  errors. Undo/Redo buttons revert whole slider gestures.
  Authoring phase B (docs/AUTHORING.md): the document seeds a Site
  singleton (Montreal defaults live in the app) and two levels; every
  object's profile control points are attached to "Ground" and every
  element associated with it — dragging Ground's elevation moves the
  whole scene. The right-hand panel hosts project settings (lat/long/
  elevation) and the level manager (derived elevation sort, add/rename/
  re-elevate/story/color, cascade delete with an honest confirmation,
  one-undo restore); level overlays are translucent squares drawn from
  Level params (never meshes); the active level is session state with
  nearest-elevation fallback.
  Phase C adds the interactive floor-plate tool: "draw floor plate"
  (gated on `can_author()`) sketches a view-only outline on the active
  level's plane (unprojected clicks, rubber-band preview via the overlay
  pipelines; Esc discards, closing commits ONE gesture group: attached
  outline → face → downward extrusion → element + instance), and "add
  hole" appends outlines to the most recent tool-authored plate's face
  (v1 limitation: no plate picking). Translation factoring is enabled —
  level-elevation drags over the fully-attached scene are transform-only
  (`Updates.base_transforms`, `world = instance ∘ base`).
- **`www/probe.html` — the wasm threading probe**: measures whether
  rayon-on-wasm achieves true parallelism in the browser.

Note for headless CI (Playwright, `web-test/`): headless Chromium's
WebGPU never reaches the compositor here (transparent canvas even though
wgpu renders without errors), so tests run without WebGPU flags and
exercise the WebGL2 fallback; real desktop browsers use WebGPU. Geometry
evaluation is sequential on wasm in both build configurations in this
milestone.

## Two build configurations

### Threaded (`--features threads`) — requires nightly

wasm-bindgen-rayon needs wasm atomics, which means rebuilding std:

```sh
RUSTUP_TOOLCHAIN=nightly \
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
  -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=1073741824 \
  -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size \
  -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base" \
cargo build -p vim-design-web --release \
    --target wasm32-unknown-unknown \
    -Z build-std=std,panic_abort \
    --features threads
wasm-bindgen --target web --out-dir crates/vim-design-web/www/pkg \
    <target-dir>/wasm32-unknown-unknown/release/vim_design_web.wasm
```

Toolchain requirements:
- nightly toolchain with the `rust-src` component (for `-Z build-std`)
- ALL the explicit linker args above: recent nightlies (verified on
  1.100.0-nightly, 2026-08-21) do NOT add them automatically when atomics
  are enabled. Without `--shared-memory` the module links non-shared memory
  and `initThreadPool` fails with a DataCloneError; wasm-bindgen's threads
  transform then requires `--import-memory` and the exported TLS symbols
  (`__wasm_init_tls`, `__tls_size`, `__tls_align`, `__tls_base`)
- `wasm-bindgen-cli` matching the pinned `wasm-bindgen` crate version
- the page must be served with `Cross-Origin-Opener-Policy: same-origin`
  and `Cross-Origin-Embedder-Policy: require-corp` (the dev server in
  `devops/lib/dev-server.mjs` does this), otherwise `SharedArrayBuffer` is
  unavailable and the pool cannot start.

### Single-threaded fallback — stable

```sh
cargo build -p vim-design-web --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir crates/vim-design-web/www/pkg-st ...
```

`sum_parallel` degrades to the sequential path; the page detects the
missing `initThreadPool` export and reports FAIL for parallelism (expected).

`devops/vbuild.ps1` builds both configurations (threaded → `www/pkg`,
fallback → `www/pkg-st`). Run the app with
`pwsh devops/vactions.ps1 -VimDesignWeb`.
