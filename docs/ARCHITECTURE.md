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
`Edge` (a curve trimmed to a parameter interval, oriented), `Wire` (an ordered, closed
loop of edges), `Face` (one outer wire + optional hole wires + an optional `Plane`
surface — when absent, the evaluator infers a plane by least-squares fit of the wire, and
a wire non-planar beyond tolerance becomes a per-entity evaluation error, §6.4; curved
faces are never authored — they are *generated* by operations and addressed via `SubRef`,
§3.4), `Solid`, `Material`,
`Extrusion`, `Revolve` (profile face about an axis line), `Chamfer`, `SectionBox` — plus
two kinds implied by the instancing requirement:

- **`Element`** — a named group of entities (a construction subgraph) whose evaluation
  produces one or more solids. The reusable "definition."
- **`Instance`** — a placement of an `Element` at a transform (rigid + optional mirror).
  Hundreds of thousands of instances share one element's evaluated geometry.
- **`Selection`** — a query entity whose evaluation produces an ordered set of references
  to entities and/or subelements matching a predicate, wireable into other entities'
  slots (§3.5).

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

### 3.4 Subelement references — provenance naming (mandatory evaluator rule)

Commands like `CreateChamfer` and `UpdateFaceMaterial` must reference faces/edges that
are *generated* by upstream operations (an extrusion's lateral faces, a boolean's cut
faces) — topology that has no `EntityId`. Referencing generated topology by kernel
output index recreates FreeCAD's decade-long **topological naming problem**: indices
shuffle when upstream parameters change, and downstream references silently attach to
the wrong face. The rule here, mandatory from the first evaluator onward:

- Evaluators emit **provenance-named topology**:
  `SubRef { owner: EntityId, path: ProvenancePath }`, where the path is derived from the
  *stable ids of the inputs that gave rise to the subelement* — e.g. for extrusion `E12`:
  `Side(profile_edge: E7)`, `Cap(Start)`, `Cap(End)`; for a boolean: names derived from
  the provenance of the input faces that produced each output face.
- Operations **propagate** provenance through their outputs (a chamfer's blend face is
  named by the edge it blends; a cut face by the cutting plane).
- Commands and slots reference subelements **only via `SubRef`, never by index**.

Deterministic evaluation (tenet 5) plus stable input ids make these names stable across
parameter edits: "the face swept from profile edge E7" survives adding a fifth control
point, because `E7` is still `E7`. A `SubRef` whose path no longer resolves after an
upstream change (e.g. the source edge was deleted) is a *per-entity evaluation error* on
the referencing entity (§6.4), never a crash or a silent re-bind.

### 3.5 Selections — declarative targeting of entities and subelements

A `Selection` is an entity kind whose evaluation produces an ordered reference set:

```rust
enum Ref { Entity(EntityId), Sub(SubRef) }
// Evaluated::RefSet(Vec<Ref>)  — sorted by (id, path) for determinism
```

Selections let commands target entities *by criteria* and wire the result into other
entities' inputs — e.g. "all edges whose start and end points lie on plane z = 3 m" fed
into a chamfer's edges slot. Consuming slots that accept multiple refs (chamfer edges,
face-material assignments) accept either an explicit ref list or a `Selection` input.

- **Predicate = closed, serializable AST** — kind filters, geometric tests (endpoint
  position, length, direction, bounding volume, on-plane-within-tolerance), provenance
  tests (owner entity, path role), combinators (`and`/`or`/`not`), and set operations
  between selections. No user-provided code: every predicate is total, panic-free,
  serde-serializable, and expressible over the C ABI.
- **Scope is an explicit input**: a set of entities, an `Element` subtree, or —
  explicitly opted into — document-global filtered by kind. Scoped selections are
  preferred; a global selection is the one sanctioned form of implicit dependency and
  carries its cost visibly (see dirtiness below).
- **Live by default**: a selection is a graph node, so its membership re-evaluates when
  the model changes — create a new edge on the z = 3 m plane and the chamfer grows to
  include it. The substrate maintains per-kind indexes so `Insert`/`Remove`/`SetParams`
  deltas dirty exactly the selections whose scope could contain the touched entity;
  global selections re-evaluate on any change to a matching kind (the documented price).
- **Frozen when desired**: a command may *bake* a selection — resolve it once and store
  the explicit `Vec<Ref>` in the consumer's params (snapshot semantics, no tracking).
  Both workflows are first-class; live is the default.
- **Cycle safety**: scope wiring participates in the normal structural cycle check;
  additionally, candidates downstream of the selection node itself are excluded at
  evaluation time (deterministically, with a per-entity warning), so dynamic membership
  can never create an evaluation cycle.

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

### 5.1 monstertruck, behind a seam

The kernel is the [`monstertruck`](https://github.com/virtualritz/monstertruck) crate
family — **adopted 2026-08-22**, replacing upstream `truck` after a hands-on head-to-head
evaluation; pinned `=0.4.0` while pre-1.0. Crates: `monstertruck-modeling` (BREP
construction: extrude/revolve/loft), `monstertruck-geometry` (NURBS),
`monstertruck-solid` (booleans with typed `ShapeOpsError`, `plane_cut`),
`monstertruck-meshing` (tessellation), `monstertruck-fillet` (fillets),
`monstertruck-io` (STEP, later). Why it won over upstream truck:

- **Typed, recoverable errors** where truck *panics inside library code* on boolean edge
  cases — fatal in the browser (wasm panic=abort) and a direct violation of tenet 1.
- **5.5× faster booleans, ~10× faster tessellation** in our building-scale probes.
- Compiles on wasm32 stable and wasm32+atomics; no future-incompat transitive deps
  (upstream drags `nom 3.2.1`/`quick-xml 0.22`).
- Actively maintained (2026); upstream truck has published no release since 2024-09.
- Same Apache-2.0 license. Known risks: single maintainer (fork rights are the
  mitigation) and 0.x API churn (hence the exact version pin).

The kernel is wrapped in a thin internal module (`vim_design_lib::kernel`) so that kernel
types do not leak into the entity/command/FFI layers. This is *not* a full abstraction
layer over "any kernel" — that would be speculative — but it keeps the blast radius small
if the kernel must change again (fallbacks: upstream `truck`, `BrepRs`, or an OpenCascade
binding, at the cost of the pure-Rust/WASM story).

### 5.2 Known kernel gaps and plans

- **Chamfer/fillet — RESOLVED (2026-08-22): `monstertruck-fillet` adopted** (behind
  monstertruck-modeling's `fillet` feature; wasm-clean). Verdict from hands-on
  probes + the shipped evaluator: `fillet_edges` with `FilletProfile::Chamfer` +
  `RadiusSpec::Constant` handles **straight edges between planar faces** exactly
  (chamfered-cube volume matches the analytic prism to fp precision; output shell
  closes; `Solid::try_new` validates). Curved rims (a cylinder's full circular top
  rim, 4 arc edges) *fail safely*: the call succeeds per-edge but the resulting
  shell is not closed → typed `NotConnected` from `Solid::try_new` → per-entity
  eval error. That matches the v1 acceptance bar (building-shaped cases); the
  in-house split/stitch fallback stays shelved. True rolling-ball blends on curved
  faces remain out of scope for v1.
- **Section box (modeling boolean):** implemented via `monstertruck-solid` —
  `plane_cut` (which returns the clipped solid *plus* its cross-section cap faces) per
  box plane where applicable, general `difference()` otherwise. At scale this cannot
  mean "boolean every solid":
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
- **Mesh ownership / standalone solids (implemented 2026-08-22, `eval::Engine`):**
  a *mesh owner* is every `Element` (mesh = members' solids merged) **plus** every
  solid producer (`Extrusion`, `Revolve`, `Solid`, `Chamfer`) that is not *consumed*
  — wired into an element's members slot or targeted by a chamfer (a chamfer
  replaces its target as owner: the chamfered solid is the target's render shape).
  Unconsumed producers surface as implicit standalone meshes keyed by their own
  entity id; consuming one tombstones its standalone mesh and re-delivers the
  geometry under the consumer's id. Renderers draw one copy per instance; owners
  with no instances draw once at identity. Materials: a solid's default is its
  profile-face material; `UpdateSubFaceMaterial` paints individual generated faces
  by provenance path (§3.4) and submeshes split per material group — paints are
  index-free, so they survive upstream edits and chamfers.
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
  angular tolerance default ~20°; both configurable per document. Quality is a
  *presentation policy*, never a modeling parameter: it lives in document settings (a
  per-element LOD override is a possible future knob), and **never** on individual
  faces — per-face sampling would conflate model with view and invite cracks along
  shared edges (tessellation runs per shell with each shared edge discretized once).

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
  this tolerance. Needs empirical validation against the kernel's internal `TOLERANCE`
  constants (the truck lineage uses fixed tolerances — see Open Questions).
- **Display/merge tolerance** (e.g. snapping, mesh dedup): 1e-5 m, separate from kernel
  tolerance.

## 8. Error handling — the "never crash" contract

- `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]`
  in `vim-design-lib` and `vim-design-ffi`.
- Every FFI entry point: `catch_unwind` → `VimStatus::InternalPanic` (plus the panic
  message retrievable via `vim_last_error`). A panic that reaches this backstop is a bug
  and gets a regression test; the document is thereafter treated as suspect but the
  process lives.
- Kernel calls are treated as hostile: all `Result`s handled, and kernel entry points
  that are known to be panic-prone are additionally wrapped in `catch_unwind` *inside*
  the kernel seam (documented case-by-case), because a third-party panic must not poison
  the whole document. (monstertruck returned typed errors, not panics, in every probe —
  the policy stands anyway.)
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

## 14. Lessons from parametric CAD history

The dependency-graph shape here is closest to FreeCAD's document model (explicit DAG of
objects with typed links) and deliberately unlike SolidWorks' ordered feature tree.
Decades of pain in those systems inform specific rules in this design:

| Historical mistake | Their cost | Our mitigation |
|---|---|---|
| **Topological naming** — referencing generated topology by index | FreeCAD broken ~a decade until the v1.0 toponaming overhaul; SolidWorks mitigates via Parasolid persistent naming | Provenance-named `SubRef`s, mandatory from the first evaluator (§3.4) |
| **Implicit ordering** — features depend on "the model above me in the tree" | SolidWorks reorder/rollback surprises | Explicit typed slots; dirtiness flows only through real edges |
| **Failure cascades** — one failed feature reddens the whole tree | Both | Per-entity eval errors + retained stale geometry (§6.4) |
| **Full-model rebuilds**, single-threaded | SolidWorks rebuild stalls | Dirty-subgraph-only, parallel topological waves (§6.2) |
| **Hidden circular references** — in-context assembly edits | SolidWorks update loops | Cycle check at commit; selections exclude downstream candidates (§3.5) |
| **Fragile undo** | Both, in places | Delta kernel with mechanically derived inverses (§4.1) |
| **Id reuse / dangling refs** on delete | Document corruption bugs | Monotonic never-reused ids; reject-if-dependents |

Two standing guards derived from the same history:

- **Constraint solvers don't fit a DAG.** Sketch constraints are non-directional; if a
  constraint solver is ever added, it lives *inside* a single evaluator node (a "sketch"
  entity that solves internally and exposes resolved geometry), never as graph edges.
- **Section box is the one sanctioned implicit-global dependency** (it conceptually
  touches all solids). It stays *out* of the graph — handled at the instance/mesh
  integration layer via spatial indexing (§5.2), never as edges-to-everything. Global
  `Selection` scopes are the second, explicitly opted-into case (§3.5); no other feature
  may introduce implicit global dependencies.

## 15. Open questions & things to explore

Ordered roughly by risk:

1. **Boolean robustness & performance at building scale** — still the load-bearing
   unknown. 2026-08-22 probes: box∖cylinder passes (37 ms); coplanar-face and
   holed-cap cases *fail* — safely, with typed errors, but they fail. Prototype against
   real building footprints, measure failure rate at realistic complexity; this gates
   the section-box feature as specified.
2. **Kernel choice** — **RESOLVED (2026-08-22): adopted `monstertruck =0.4.0`** after a
   hands-on head-to-head (typed errors vs. library panics, 5.5×/10× faster booleans/
   tessellation, wasm-clean, dep-hygiene-clean, actively maintained — see §5.1).
   Fallbacks if it disappoints: upstream `truck`, `BrepRs`, OpenCascade bindings.
3. **The kernel's tolerance model** — the truck lineage (including monstertruck) uses
   fixed internal tolerance constants. Verify they're compatible with meter-unit building geometry (1 µm target)
   or whether coordinates need internal scaling.
4. **Chamfer scope** — **RESOLVED (2026-08-22): `monstertruck-fillet` adopted** —
   straight-edge/planar chamfers work exactly, curved-edge attempts fail with typed
   errors (see §5.2 for the full verdict); the in-house split/stitch plan stays
   shelved unless curved-edge chamfers become a requirement.
5. **WASM threading** — **RESOLVED (2026-08-22):** true parallelism verified in headless
   Chromium (8 rayon workers, ~4.3× speedup); truck compiles and runs in both threaded
   (nightly + explicit RUSTFLAGS, §6.2) and single-threaded fallback builds. Remaining
   follow-up: measure how gracefully the fallback degrades at target scale.
6. **Section box × instancing interaction** — `plane_cut` returning cap faces (§5.2)
   likely covers the per-plane cut + cap-material case; still open: straddling instances de-instance while cut;
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
11. **Topological naming / subelement references** — **RESOLVED (2026-08-22):** approved
    and promoted to a mandatory evaluator rule; see §3.4.
12. **Selection details** — the concept is approved (§3.5); still to pin down during
    implementation: the exact predicate AST surface (and its C-ABI representation),
    the cost of global-scope selections at 100k+ entities (per-kind index granularity,
    debouncing during drags), and validation of the downstream-candidate exclusion rule
    against real modeling scenarios.
13. **Command-candidate parking lot** — the command set is *demand-driven by building
    modeling scenarios, never supply-driven by kernel capability*, and command names use
    industry CAD/AEC vocabulary, never kernel-specific terminology (the kernel's own
    names change: truck's `tsweep` became monstertruck's `extrude`). Candidates awaiting
    a driving scenario: `CreateLoft` (ramps, transitions — kernel: `try_skin_wires`).
    `CreateRevolve` was promoted 2026-08-22 (driving scenario: cone primitives in the
    evaluation-layer demo; kernel: `revolve`). Kernel capabilities
    that are deliberately NOT commands: healing, shell stitching, `plane_cut` (evaluator
    internals); STEP I/O (document-level action); meshing tolerances (document settings).
