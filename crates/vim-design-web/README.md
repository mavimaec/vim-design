# vim-design-web

WASM browser test app. Right now it is the **wasm threading probe**: a page
that measures whether rayon-on-wasm achieves true parallelism in the browser.
Rendering is a placeholder (Rust fills a 2D canvas); the wgpu renderer comes
later.

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
