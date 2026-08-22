# build.ps1 — build steps for the Rust workspace, wasm bundles, and C++ tests.
# Requires common.ps1 (and for bootstrap, toolchain.ps1) to be dot-sourced first.

function Build-RustNative {
    param([Parameter(Mandatory)][ValidateSet('debug', 'release')][string]$Config)
    Write-Section "Rust workspace ($Config, native)"
    $cargoArgs = @('build', '--workspace')
    if ($Config -eq 'release') { $cargoArgs += '--release' }
    Invoke-Exec -File 'cargo' -Arguments $cargoArgs -WorkingDirectory (Get-RepoRoot)
    Write-Ok "native build done (target/$Config; FFI header at crates/vim-design-ffi/include/vim_design.h)"
}

# Builds BOTH wasm configurations of vim-design-web:
#   1. threaded  -> crates/vim-design-web/www/pkg     (nightly, atomics, rayon)
#   2. fallback  -> crates/vim-design-web/www/pkg-st  (stable, single-threaded)
# The page loads ./pkg/, so the threaded build is what runs; if the threaded
# build fails, the fallback is copied into pkg/ so the app still works.
# NOTE: wasm is always compiled --release: the threading probe measures
# timings, and debug-profile wasm would make them meaningless.
function Build-Wasm {
    Write-Section 'VimDesignWeb wasm (threaded + single-threaded fallback)'
    $root = Get-RepoRoot
    $www = Join-Path $root 'crates' 'vim-design-web' 'www'
    $pkg = Join-Path $www 'pkg'
    $pkgSt = Join-Path $www 'pkg-st'

    # --- single-threaded fallback (stable) ---
    Write-Info 'building single-threaded fallback (stable)...'
    Invoke-Exec -File 'cargo' -WorkingDirectory $root -Arguments @(
        'build', '-p', 'vim-design-web', '--release',
        '--target', 'wasm32-unknown-unknown'
    ) -Environment @{ CARGO_TARGET_DIR = 'target-wasm-st' }
    Invoke-Exec -File 'wasm-bindgen' -WorkingDirectory $root -Arguments @(
        '--target', 'web', '--out-dir', $pkgSt,
        (Join-Path $root 'target-wasm-st' 'wasm32-unknown-unknown' 'release' 'vim_design_web.wasm')
    )
    Write-Ok 'single-threaded wasm bundle -> www/pkg-st'

    # --- threaded (nightly + atomics + build-std) ---
    $threadedOk = $false
    try {
        Write-Info 'building threaded wasm (nightly, atomics, -Z build-std)...'
        Invoke-Exec -File 'cargo' -WorkingDirectory $root -Arguments @(
            'build', '-p', 'vim-design-web', '--release',
            '--target', 'wasm32-unknown-unknown',
            '-Z', 'build-std=std,panic_abort',
            '--features', 'threads'
        ) -Environment @{
            RUSTUP_TOOLCHAIN = 'nightly'
            # NOTE: recent nightlies (verified on 1.100.0-nightly 2026-08-21) no
            # longer pass the threading linker args to wasm-ld automatically
            # when the atomics feature is enabled. All of these are required:
            # without --shared-memory the module links non-shared memory and
            # initThreadPool fails with a DataCloneError; wasm-bindgen's
            # threads transform additionally needs --import-memory and the
            # exported TLS symbols.
            RUSTFLAGS        = '-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=1073741824 -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base'
            CARGO_TARGET_DIR = 'target-wasm-threads'
        }
        Invoke-Exec -File 'wasm-bindgen' -WorkingDirectory $root -Arguments @(
            '--target', 'web', '--out-dir', $pkg,
            (Join-Path $root 'target-wasm-threads' 'wasm32-unknown-unknown' 'release' 'vim_design_web.wasm')
        )
        $threadedOk = $true
        Write-Ok 'threaded wasm bundle -> www/pkg'
    }
    catch {
        Write-Warn "threaded wasm build FAILED: $_"
        Write-Warn 'falling back: copying single-threaded bundle into www/pkg'
        if (Test-Path $pkg) { Remove-Item -Recurse -Force $pkg }
        Copy-Item -Recurse -Force $pkgSt $pkg
    }
    return $threadedOk
}

function Build-Cpp {
    param([Parameter(Mandatory)][ValidateSet('debug', 'release')][string]$Config)
    Write-Section "VimDesignCppTest (CMake + GoogleTest, $Config)"
    $root = Get-RepoRoot
    $src = Join-Path $root 'cpp' 'vim-design-cpp-test'
    $buildDir = Join-Path $src 'build'
    $targetDir = Join-Path $root 'target' $Config
    $cmakeConfig = if ($Config -eq 'release') { 'Release' } else { 'Debug' }

    Invoke-Exec -File 'cmake' -Arguments @(
        '-S', $src, '-B', $buildDir,
        "-DCMAKE_BUILD_TYPE=$cmakeConfig",
        "-DVIM_DESIGN_TARGET_DIR=$targetDir"
    )
    Invoke-Exec -File 'cmake' -Arguments @('--build', $buildDir, '--config', $cmakeConfig, '--parallel')
    Write-Ok "C++ tests built in $buildDir"
}

# Removes build outputs (untracked generated files). Deliberately NOT
# `git clean -xdf`: with a fresh repo everything is untracked and git clean
# would delete sources. This removes exactly the known generated dirs.
function Invoke-CleanRepo {
    Write-Section 'Clean'
    $root = Get-RepoRoot
    $paths = @(
        'target', 'target-wasm-st', 'target-wasm-threads',
        'crates/vim-design-ffi/include',
        'crates/vim-design-web/www/pkg',
        'crates/vim-design-web/www/pkg-st',
        'cpp/vim-design-cpp-test/build',
        'web-test/node_modules',
        'web-test/test-results',
        'web-test/playwright-report',
        'web-test/screenshots'
    )
    foreach ($p in $paths) {
        $full = Join-Path $root $p
        if (Test-Path $full) {
            Write-Info "removing $p"
            Remove-Item -Recurse -Force $full
        }
    }
    Write-Ok 'clean done'
}
