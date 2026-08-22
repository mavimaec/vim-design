# VIM Design — Architecture

This document records the architectural decisions guiding the implementation of VIM Design.
It complements [PROJECT_REQUIREMENTS.md](PROJECT_REQUIREMENTS.md); where the two disagree,
this document is more specific and more recent. Open questions are collected at the end.

## 1. Design tenets

1. **Never crash.** The library must never abort, segfault, or unwind across the FFI
   boundary. "Never crash" is implemented as "every operation is a command that can be
   *rejected* with a status code." Degenerate geometry, kernel failures, and invalid input
   all surface as rejected commands, never as panics.
2. **Result-status everywhere.** Every FFI entry point returns a `VimStatus` code.
   Every internal fallible operation returns `Result`. `unwrap`/`expect`/indexing panics
   are forbidden in library code (enforced by clippy lints); a `catch_unwind` at each FFI
   entry point is the last-resort backstop, not the error-handling strategy.
3. **Two-layer model.** The *parametric layer* (entities, parameters, dependency graph) is
   the authoritative state: commands mutate it, undo applies to it, serialization persists
   it. The *derived layer* (BREP geometry, tessellated meshes) is a cache, recomputed
   deterministically from the parametric layer and never serialized (in v1).
4. **Incremental and parallel.** An edit dirties only its downstream subgraph. Re-evaluation
   and tessellation run asynchronously and in parallel; the caller polls for completed
   meshes. Target scale: hundreds of thousands of building elements via instancing.
5. **Deterministic evaluation.** Given the same parametric state, evaluation produces the
   same geometry. This is what makes inverse-command undo sound: undo restores parameters,
   and derived geometry follows.

## 2. Workspace layout

Cargo workspace (Rust 2024 edition):

```
/Cargo.toml                  # workspace root
/crates/
  vim-design-lib/            # VimDesignLib: core library (pure Rust, no FFI)
  vim-design-ffi/            # C ABI layer over vim-design-lib (cdylib + staticlib)
  vim-design-web/            # VimDesignWeb: WASM + WebGPU interactive test app
  vim-design-test/           # VimDesignTest: integration & regression tests
/cpp/
  vim-design-cpp-test/       # VimDesignCppTest: CMake + GoogleTest consumer of the C ABI
/web-test/                   # VimDesignWebTest: Playwright tests + screenshot baselines
/devops/
  vbuild.ps1                 # build everything, fetch dependencies
  vactions.ps1               # run app / run tests
  lib/*.ps1                  # shared helpers
/docs/
```

- `vim-design-lib` has **no** FFI or WASM concerns — it is an ordinary Rust library, unit-
  tested directly. This keeps the core testable and keeps `unsafe` confined to `vim-design-ffi`.
- `vim-design-ffi` produces `libvim_design.so/.dylib/.dll` plus a generated C header
  (`cbindgen`) consumed by the C++ tests.
- `vim-design-web` uses `wasm-bindgen` + `wgpu` and calls `vim-design-lib` directly
  (Rust-to-Rust; the C ABI is not used in the browser).

## 3. Core model

### 3.1 Entities and identity

Entity kinds (from the requirements): `ControlPoint`, `Plane`, `Circle`, `Line`, `Spline`,
`Edge`, `Face`, `Solid`, `Material`, `Extrusion`, `Chamfer`, `SectionBox` — plus two kinds
implied by the instancing requirement:

- **`Element`** — a named group of entities (a construction subgraph) whose evaluation
  produces one or more solids. The reusable "definition."
- **`Instance`** — a placement of an `Element` at a transform (rigid + optional mirror).
  Hundreds of thousands of instances share one element's evaluated geometry.

> Note: `Element`/`Instance` commands (`CreateElement`, `CreateInstance`, …) extend the
> command list in the requirements; they fall out of the performance requirement (§8 of
> the discussion) and should be added there.

**`EntityId` is a per-document monotonic `u64`, never reused.** Rationale:

- Stable across serialization (no remapping on load).
- Undo of a deletion recreates the entity *with its original id*, so inverse commands and
  the redo stack stay valid.
- Safe across the FFI: a stale id fails lookup and returns `VimStatus::EntityNotFound`
  rather than aliasing a recycled slot.

Storage is a hash map keyed by `EntityId` (switch to an id→slot indirection over a dense
arena later if profiling demands it).

### 3.2 Dependency graph

A DAG over entity ids. Each entity stores its inputs as **ordered, typed slots** declared
statically per entity kind (e.g. `Extrusion`: slot 0 = profile `Face`, slot 1 = path
`Line|Spline`; order is semantic). Slot declarations are the single source of truth for
command validation, kind-checking on rewires, and reject-if-dependents. The graph
maintains the reverse `downstream` adjacency as a derived index (rebuildable from scratch
as a debug check/repair). Invariants:

- **Acyclicity** is checked at command-validation time; a command that would introduce a
  cycle is rejected (`VimStatus::WouldCreateCycle`).
- **Deletion policy: reject-if-dependents.** `Delete*` commands are rejected with
  `VimStatus::HasDependents` (and the FFI can query the dependent list so a UI can explain
  why). Composite delete commands may delete a whole subgraph leaf-first as one transaction.
- Evaluation order is a topological sort of the dirty subgraph.

### 3.3 Evaluation model

Each entity kind has an evaluator: `fn evaluate(&self, inputs: &[Evaluated]) -> Result<Evaluated, EvalError>`.
`Evaluated` values are kernel objects (points, curves, surfaces, BREP shells/solids).
Evaluation failures (e.g., a boolean that fails, a degenerate extrusion) do not fail the
command that *caused* them retroactively — see §6.4 for how stale-but-valid geometry is
retained and errors are reported per-entity.

## 4. Command system

### 4.1 Commands compile to graph deltas; inverses are derived

Commands are user-level *intents*. Applying a command compiles it into a sequence of
four **primitive deltas** — the only operations that ever mutate the parametric layer:

```rust
enum Delta {
    Insert { id: EntityId, record: EntityRecord },          // inverse: Remove
    Remove { id: EntityId, record: EntityRecord },          // inverse: Insert (same id)
    SetParams { id: EntityId, old: Params, new: Params },   // inverse: swap old/new
    Rewire { id: EntityId, slot: SlotIdx, old: Option<EntityId>, new: Option<EntityId> },
}
```

- **Inverses are mechanical:** the inverse of any command — including composites — is its
  delta list reversed with each delta inverted. There are exactly four inversion rules to
  test, not one hand-written inverse per command.
- **Atomicity via speculative apply:** a command (or composite) applies its deltas as it
  goes; if any step fails validation, the already-applied deltas roll back in reverse.
  A rejected command leaves the document untouched.
- **Validation is structural only:** ids exist, slot kinds match, no cycles, no dependents
  on delete. Geometric failures are *not* command rejections — they surface asynchronously
  as per-entity evaluation errors (§6.4).
- **Coalescing** for interactive drags: consecutive `SetParams` deltas on the same entity
  merge (first `old` wins, last `new` wins).

### 4.2 Undo/redo stack

- The stacks store `(command, Vec<Delta>)` — the command only labels the step for UI and
  history ("Undo Create Cylinder"); undo/redo applies the inverted/original deltas.
- `undo_stack: Vec<CommandGroup>`, `redo_stack: Vec<CommandGroup>`; a new command clears
  the redo stack. Stack depth is configurable (default: unbounded; revisit if memory
  profiling at scale says otherwise — `Delete` inverses hold full entity records, which
  are small parametric data, not geometry).
- **Composite commands** (e.g. `CreateCylinder`, subgraph deletes) execute as a
  `CommandGroup`: constituents apply speculatively in order (rolling back on failure,
  §4.1) and are undone/redone as a single unit in reverse order.
- Interactive dragging (e.g. `UpdateControlPoint` at 60 Hz) should not flood the stack:
  the FFI exposes a *coalescing* flag so consecutive updates to the same entity merge into
  one undo step (first old-value wins).

## 5. Geometry kernel

### 5.1 truck, behind a seam

We start with the [`truck`](https://github.com/ricosjp/truck) crate family:
`truck-modeling` (BREP construction, sweeps/extrusions), `truck-geometry` (NURBS curves/
surfaces), `truck-shapeops` (booleans), `truck-meshalgo` (tessellation), `truck-stepio`
(optional STEP interchange later).

The kernel is wrapped in a thin internal module (`vim_design_lib::kernel`) so that kernel
types do not leak into the entity/command/FFI layers. This is *not* a full abstraction
layer over "any kernel" — that would be speculative — but it keeps the blast radius small
if truck's booleans or fillets force a change of kernel (candidates to evaluate then:
the `monstertruck` fork of truck, `BrepRs`, or an OpenCascade binding, at the cost of the
pure-Rust/WASM story).

### 5.2 Known kernel gaps and plans

- **Chamfer:** truck does not provide fillet/chamfer. Initial scope is deliberately
  building-shaped: chamfers on **straight edges between planar faces** (the dominant case
  in AEC), implemented in-house as: split adjacent faces along offset lines, insert the
  ruled chamfer face, re-stitch the shell. True fillets (rolling-ball blends on curved
  faces) are out of scope for v1.
- **Section box (modeling boolean):** implemented as `solid ∖ section-volume` via
  `truck-shapeops`. At scale this cannot mean "boolean every solid":
  - Maintain a spatial index (AABB tree) over instance bounds.
  - Instances fully outside the box: dropped from output. Fully inside: untouched, stay
    instanced. Only *straddling* instances get a boolean, and a cut instance becomes a
    unique (de-instanced) mesh for as long as it straddles.
  - Boolean failures on individual solids degrade gracefully (per-entity error + uncut
    geometry retained), never crash.

## 6. Evaluation & tessellation pipeline

### 6.1 Dirty propagation

A committed command marks its target entities dirty; dirtiness propagates downstream
through the DAG. Dirty roots are batched per "generation" (one generation per committed
command or command group).

### 6.2 Async, parallel evaluation

- The parametric layer is single-writer (see §9 threading contract). On commit, the dirty
  subgraph's *parametric inputs are snapshotted* and handed to a background evaluation job.
- Within a generation, entities at the same topological depth evaluate in parallel
  (`rayon` on native). Tessellation of finished solids is embarrassingly parallel.
- A newer generation cancels obsolete pending work for the same entities (check a
  generation counter between pipeline stages).
- **WASM (de-risked 2026-08-22):** rayon-on-wasm is verified working — 8 worker threads,
  ~4.3× speedup on an 8-core browser, truck compiling and running in both threaded and
  single-threaded wasm builds. Requirements: SharedArrayBuffer via COOP/COEP headers (the
  dev server sets them), nightly rustc + `-Z build-std` with an explicit set of
  atomics/shared-memory `RUSTFLAGS` — current nightlies no longer pass the threading
  linker args automatically; the full set lives in `devops/lib/build.ps1` and
  `crates/vim-design-web/README.md`. Without cross-origin isolation, the single-threaded
  fallback bundle (stable toolchain, no atomics) is served instead.

### 6.3 Mesh facade

The caller-facing output contract:

- Meshes are delivered per **element**, instanced via per-instance transforms:
  `MeshUpdate { element_id, generation, vertices (pos+normal), indices, submeshes: [(material_id, index_range)] }`
  plus `InstanceUpdate { instance_id, element_id, transform_4x3 }` lists.
- The caller polls: `vim_poll_updates(handle)` returns a **changed-set delta, coalesced,
  keyed by stable ids** — never a full-scene report. Each handle keeps a poll cursor; a
  poll returns only ids whose renderer-visible state changed since the previous poll, and
  for each, the *latest* state (an element re-tessellated five times between polls appears
  once, with the newest mesh). Deletions arrive as explicit tombstone lists. The caller's
  renderer bookkeeping is two upsert maps: `element_id → GPU mesh` and
  `instance_id → (element_id, transform)`. Polling suits both game-loop C++ hosts and the
  requestAnimationFrame loop in the browser; no callbacks across FFI.
- **Settledness:** each poll reports `committed_generation` (latest command commit),
  `evaluated_generation` (everything ≤ it is fully meshed), and a pending-entity count.
  `evaluated == committed` ⇒ quiescent — the predicate test harnesses and progress UIs
  bind to. Stale meshes are retained while re-evaluation is in flight, so the scene never
  flickers to empty.
- **Evaluation starts eagerly on every commit** (decision 2026-08-22). Callers batching
  many commands may defer kickoff with `vim_hold_evaluation(handle, true)` and release it
  when done; default-eager means naive callers get correct behavior with no extra calls.
- Buffers returned by a poll are owned by the library and remain valid until the next
  poll on that handle (double-buffered internally); the caller copies or uploads to GPU
  within the frame.
- Tessellation quality: chordal deviation tolerance, default **1 mm** (0.001 m), and
  angular tolerance default ~20°; both configurable per document.

### 6.4 Per-entity evaluation errors

If an entity fails to evaluate (kernel error), it is marked `EvalState::Error` with a
diagnostic; its last successful geometry is retained and flagged stale. Downstream
entities evaluate against the stale value where possible, or inherit the error state.
The FFI exposes a query for entities in error state. The *command* that introduced the
bad parameters still succeeded (it validly changed parameters) and is still undoable —
undo is the escape hatch.

## 7. Units, tolerance, coordinates

- **Units:** meters, everywhere, no exceptions (angles in radians).
- **Coordinate system:** right-handed, **Z-up**. +X east, +Y north by convention.
- **Kernel tolerance:** point-coincidence / topology tolerance **1e-6 m** (1 µm).
  Building-scale coordinates (spans up to ~1e3–1e4 m) keep f64 comfortably accurate at
  this tolerance. Needs empirical validation against truck's internal `TOLERANCE`
  constants (truck uses its own fixed tolerances — see Open Questions).
- **Display/merge tolerance** (e.g. snapping, mesh dedup): 1e-5 m, separate from kernel
  tolerance.

## 8. Error handling — the "never crash" contract

- `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]`
  in `vim-design-lib` and `vim-design-ffi`.
- Every FFI entry point: `catch_unwind` → `VimStatus::InternalPanic` (plus the panic
  message retrievable via `vim_last_error`). A panic that reaches this backstop is a bug
  and gets a regression test; the document is thereafter treated as suspect but the
  process lives.
- truck calls are treated as hostile: all `Result`s handled, and kernel entry points that
  are known to be panic-prone are additionally wrapped in `catch_unwind` *inside* the
  kernel seam (documented case-by-case), because a third-party panic must not poison the
  whole document.
- Allocation failure and stack overflow are explicitly out of scope of the guarantee.

## 9. C ABI (`vim-design-ffi`)

- **Style:** plain C ABI, header generated by `cbindgen`. No C++ types in the interface;
  the C++ test builds a thin RAII wrapper on top.
- **Handles:** `VimDesignHandle` (opaque pointer) from `vim_create()` / `vim_destroy()`.
  Multiple documents = multiple handles.
- **Commands:** one `extern "C"` function per command with a plain-C params struct, e.g.
  `VimStatus vim_create_control_point(VimDesignHandle, const VimControlPointParams*, VimEntityId* out_id);`
  Composite commands likewise. (A batched binary command buffer is deferred — open question.)
- **Errors:** every function returns `VimStatus` (0 = Ok). `vim_last_error(handle)` returns
  a UTF-8 message for the most recent failure on that handle (stored per-handle, valid
  until the next call on that handle).
- **Undo/redo:** `vim_undo(handle)`, `vim_redo(handle)`, `vim_can_undo/can_redo`.
- **Mesh polling:** `vim_poll_updates(handle, VimUpdates* out)` per §6.3 — changed-set
  upserts + tombstones + generation counters; all buffer pointers are library-owned and
  valid until the next poll on that handle (double-buffered internally).
  `vim_hold_evaluation(handle, bool)` defers eager evaluation kickoff for batched
  submissions.
- **Threading contract:** a handle is **not** thread-safe; the caller must externally
  synchronize all calls on one handle (calls on *different* handles are safe
  concurrently). Internally the library still parallelizes evaluation on worker threads.
- **Serialization:** `vim_save(handle, VimBuffer* out)` / `vim_load(const uint8_t*, size_t, VimDesignHandle* out)`;
  buffers freed with `vim_buffer_free`.

## 10. Serialization

- **What is persisted:** the parametric layer — entity records, dependency edges, next-id
  counter, document settings (tolerances, tessellation quality). **Not** persisted in v1:
  derived geometry/mesh caches (recomputed on load) and the undo/redo stacks (open
  question below).
- **Format:** `serde` + [`postcard`](https://docs.rs/postcard) (compact, `no_std`-friendly,
  well-maintained) wrapped in a small envelope: magic `VIMD`, `u32` format version,
  payload. Loaders reject newer major versions with `VimStatus::UnsupportedVersion`.
- Schema evolution: additive fields with serde defaults; breaking changes bump the
  envelope version with an explicit migration step.
- A JSON debug export (`vim_save_json`, dev builds) for diffing documents in tests.
- Round-trip property test in `VimDesignTest`: save → load → save must be byte-identical,
  and re-evaluation must produce identical meshes.

## 11. VimDesignWeb (WASM + WebGPU)

- `wgpu` renderer: instanced draw per element mesh, camera orbit/pan/zoom, basic material
  color/roughness from `Material` entities.
- Minimal command UI (buttons/panel) sufficient to exercise every command interactively,
  plus undo/redo keybindings — this is a test bed, not a product UI.
- Served by `vactions.ps1 -VimDesignWeb` via a small dev server that sets COOP/COEP
  headers (for wasm threads).

## 12. Testing strategy

- **Unit tests** inline in `vim-design-lib` (graph invariants, command validate/apply/
  inverse symmetry, tolerance math).
- **`VimDesignTest`** (integration/regression): command-sequence scenarios with
  golden-mesh comparisons (vertex/index counts + hashed buffers with tolerance);
  property tests: *for any command sequence, undo-all restores a state that serializes
  byte-identically to the initial state*; serialization round-trips; every fixed bug gets
  a named regression test.
- **`VimDesignCppTest`**: GoogleTest against the generated header — lifecycle, every
  command reachable, status codes on invalid input, poll contract, save/load.
- **`VimDesignWebTest`**: Playwright drives the browser app, executes scripted command
  sequences, screenshots against baselines (with a pixel-diff threshold to absorb GPU
  rasterization differences).
- A benchmark harness (criterion) from early on: entity-count scaling of commit→mesh
  latency, to turn "high-performance" into measured numbers.

## 13. Build & devops

Per the requirements: `devops/vbuild.ps1` (with `-Clean`, `-Debug`, `-Release`) and
`devops/vactions.ps1` (with `-VimDesignWeb`, `-Test`), shared helpers in `devops/lib/`.
PowerShell 7 (`pwsh`, verified 7.6.4 on this Linux environment) is the scripting runtime;
scripts must stay cross-platform (no Windows-only cmdlets). `vbuild.ps1` bootstraps:
rustup toolchain + `wasm32-unknown-unknown` target, `wasm-bindgen-cli`, `cbindgen`,
CMake + a C++ toolchain check, and Node/Playwright for web tests.

## 14. Open questions & things to explore

Ordered roughly by risk:

1. **truck boolean robustness & performance** — the load-bearing unknown. Prototype
   early: extrude building-like footprints, run `truck-shapeops` difference ops against
   section volumes, measure failure rate and timing at realistic complexity. This
   go/no-go gates the section-box feature as specified.
2. **truck vs. its `monstertruck` fork vs. alternatives** — truck was forked and heavily
   reworked as `monstertruck-*` (renamed crates, API changes, active in 2026); `BrepRs`
   advertises booleans *and* fillet/chamfer. When we hit truck's limits, evaluate these
   before considering OpenCascade bindings (which would sacrifice pure-Rust WASM).
   *(`monstertruck` evaluation in progress, 2026-08-22.)*
3. **truck's tolerance model** — truck historically uses fixed internal tolerance
   constants. Verify they're compatible with meter-unit building geometry (1 µm target)
   or whether coordinates need internal scaling.
4. **Chamfer scope** — is planar-face/straight-edge chamfering sufficient for v1?
   In-house implementation effort is non-trivial even for that case.
5. **WASM threading** — **RESOLVED (2026-08-22):** true parallelism verified in headless
   Chromium (8 rayon workers, ~4.3× speedup); truck compiles and runs in both threaded
   (nightly + explicit RUSTFLAGS, §6.2) and single-threaded fallback builds. Remaining
   follow-up: measure how gracefully the fallback degrades at target scale.
6. **Section box × instancing interaction** — straddling instances de-instance while cut;
   with a box crossing a large building, how many uniques is that in practice, and is
   incremental re-cut on box drag feasible, or does box-drag need a cheaper preview mode
   (display-clip) with the boolean applied on release?
7. **Undo stack persistence** — v1 does not serialize undo/redo stacks. Acceptable?
   (Saving them is straightforward later since commands are serde-serializable.)
8. **Batched command buffer over FFI** — per-command functions are fine for correctness;
   dragging interactions from C++ hosts may eventually want a binary command batch per
   frame. Defer until profiling shows FFI overhead matters.
9. **Performance targets** — unknown by design; the criterion benchmark harness plus the
   web test bed exist to discover the real numbers (commit→mesh latency vs. entity count,
   memory per 100k instances) before we commit to targets.
10. **Element/Instance command surface** — **RESOLVED (2026-08-22):** approved; commands
    added to PROJECT_REQUIREMENTS.md.
