# CLAUDE.md

VIM Design is a Rust library that builds building geometry (BREP) from undoable
commands, plus a browser authoring app for modeling a simple house. The library must
never crash, and C++ can call it.

## Read first

| Document | What it holds |
|---|---|
| `docs/PROJECT_REQUIREMENTS.md` | The original goal and build requirements |
| `docs/ARCHITECTURE.md` | The library contract: entities, the delta command system, evaluation, the never-crash rules. It is more specific than the requirements. |
| `docs/AUTHORING.md` | The authoring data model and the app's UI rules, with the decisions and their dates. §8 describes the web app's behavior. |
| `crates/vim-design-web/README.md` | The wasm build configurations and toolchain notes |
| `docs/parking-lot/` | Research that is set aside |

## Layout

- `crates/vim-design-lib`: the core library. Pure Rust, with no FFI or wasm code.
- `crates/vim-design-ffi`: the C ABI and a generated header (`include/`, not committed).
- `crates/vim-design-web`: the wasm app.
  - `src/authoring/`: pure authoring logic (document operations, snapping, element model). It also compiles natively, so unit tests run there.
  - `src/author/`: the wasm-only `AuthorApp` behind `www/app.html` (camera, picking, rendering, tools).
  - `src/demo/`: the parametric demo behind `www/index.html`.
  - `www/app.js`, `app.html`, `app.css`: the authoring app's DOM. JS owns the DOM and input decoding, Rust owns the document.
- `crates/vim-design-test`: integration and regression tests, and the `.vimd` compatibility fixtures in `fixtures/`.
- `cpp/vim-design-cpp-test`: GoogleTest against the C ABI.
- `web-test`: Playwright tests. `app-*.spec.js` test the authoring app on a desktop project and an emulated-phone project.
- `devops`: the PowerShell 7 build and run scripts. Keep them cross-platform.

## Commands

All scripts run with `pwsh`.

```sh
pwsh devops/vbuild.ps1 -Debug            # native Rust, both wasm bundles, C++
pwsh devops/vbuild.ps1 -Clean -Release
pwsh devops/vactions.ps1 -Test           # cargo tests, C++ gtest, Playwright
pwsh devops/vactions.ps1 -VimDesignWeb   # serve on :8787 and open the browser
pwsh devops/vpages.ps1 -Serve            # build the Pages site and serve it on :8790

cargo test --workspace                   # the Rust tests (this is also the CI gate)
cargo test -p vim-design-test --test rooms
cargo clippy --workspace
```

The authoring app's Playwright specs serve `www/` in the Pages layout: `/` is
`app.html`, and `/pkg/` comes from `www/pkg-st`. After a Rust change, rebuild the
single-threaded bundle before you run them:

```sh
CARGO_TARGET_DIR=target-wasm-st cargo build -p vim-design-web --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir crates/vim-design-web/www/pkg-st target-wasm-st/wasm32-unknown-unknown/release/vim_design_web.wasm
cd web-test && npx playwright test tests/app-rooms.spec.js   # add --project=desktop or --project=mobile to run one project
```

Native builds and the single-threaded wasm bundle use stable Rust (1.88 or later,
edition 2024). Only the threaded wasm bundle needs nightly. `wasm-bindgen` is pinned to
`=0.2.127`, and `wasm-bindgen-cli` must match that version.

**A push to `develop` deploys** the app to GitHub Pages
(`.github/workflows/pages.yml`, gated on `cargo test --workspace --locked`). Do not
push unless the operator asks.

## Library rules (do not break)

- **Never crash.** `vim-design-lib` and `vim-design-ffi` deny `unwrap`, `expect`,
  indexing or slicing, and `panic` (clippy). Bad input is a rejected command with a
  status code. A geometry failure is a per-entity evaluation error, and the entity
  keeps its previous mesh. It is never a command rejection.
- **Commands compile to four primitive deltas** (Insert, Remove, SetParams, Rewire).
  Inverses are derived mechanically. Command validation checks structure only.
- **One user gesture is one undo step.** A drag or typing coalesces (`coalesce: true`).
  Composite edits run as one command group.
- **Session state is not document state.** The active level and plane, the tool, the
  camera, the selection, and the snap settings never enter the document or the undo
  history. The one recorded exception is `PlanSpan` (AUTHORING §13).
- **Serialization is append-only.** Add new entity kinds, commands, faces, and errors at
  the END of their serialized enums. Every fixture in `crates/vim-design-test/fixtures`
  must load, evaluate, and resave byte-identically. When you add a data-model change,
  save a new fixture version.
- **Provenance naming.** Name generated faces from stable ids (for example
  `SketchSide`, `RunFace`, `RoomWall`), never from kernel output order. A frame's basis
  must come from its stable inputs only.
- Units are meters (angles in radians). The coordinate system is right-handed and
  Z-up. The kernel tolerance is 1e-6 m. The snap and merge tolerance is 1e-5 m.
- Every fixed bug gets a named regression test.

## Authoring app UX rules

These rules are already in effect (AUTHORING §8). Keep new UI consistent with them,
and raise any inconsistency with the operator before you build it.

- Phones and desktop are both first-class: touch, mouse, and keyboard. Each feature
  needs a path for touch, and the Playwright mobile project tests it.
- There is one linear undo history, with one Undo and Redo control in the same place in
  every mode. Toasts have no Undo.
- An Edit Mode is modal. ✓ keeps it as one undo step. ✗ restores the state at entry
  exactly. Undo inside an Edit Mode stops at the entry.
- The item just created becomes the selection, so the panel's values edit it
  immediately. Those values are also the defaults for the next item (remembered
  settings).
- Controls always show the document. A refused value tells the user why and snaps
  back. A slider and its field stay in sync while either one is in use.
- A destructive cascade asks for confirmation only when real content would go, says
  what goes with it, and one Undo restores everything.
- Touch targets are 44 px or larger.
- Do not use the `vim-html-design` submodule yet.

## Working conventions

- When behavior or the data model changes, update `docs/AUTHORING.md` (or
  `ARCHITECTURE.md` for library contracts) in the same change. Write in that document's
  plain, terse style, and date the decisions.
- Commit subjects look like `Authoring app, milestone N: ...`, `Add <Entity>: ...`, or
  `Fix ...`. The body lists what changed (and root causes for fixes) and ends with the
  test counts, for example `Tests: 319 Rust, 78 Playwright passed.`
- Panel controls in `app.js` sync per field from the document, skipping only the field
  in use (`guardAssign`, or a control's own `update()`). Do not skip a whole page's
  repaint because focus is inside it: that leaves its sliders and fields stale.
- Playwright specs drive the real controls (typed fields, clicks, mouse and touch
  drags). They do not call internal APIs.
