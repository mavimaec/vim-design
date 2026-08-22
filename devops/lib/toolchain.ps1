# toolchain.ps1 — bootstraps the tools vbuild/vactions need.
# Requires common.ps1 to be dot-sourced first.

# The wasm-bindgen crate is pinned to this exact version in Cargo.toml;
# the CLI must match.
$script:WasmBindgenVersion = '0.2.127'

function Install-Rustup {
    if (Test-CommandExists 'rustup') { return }
    Write-Warn 'rustup not found - installing (user-local, no sudo)...'
    if ($IsWindows) {
        throw 'rustup is not installed. Install it from https://rustup.rs and re-run.'
    }
    $installer = Join-Path ([IO.Path]::GetTempPath()) 'rustup-init.sh'
    Invoke-WebRequest -Uri 'https://sh.rustup.rs' -OutFile $installer
    Invoke-Exec -File 'sh' -Arguments @($installer, '-y', '--default-toolchain', 'stable', '--profile', 'minimal')
    Add-CargoBinToPath
}

function Ensure-RustToolchain {
    Write-Section 'Toolchain: Rust'
    Add-CargoBinToPath
    Install-Rustup
    if (-not (Test-CommandExists 'cargo')) {
        throw 'cargo not found even after rustup bootstrap; check your PATH.'
    }
    Write-Ok "rustc $((& rustc --version) -join '')"

    # wasm32 target for stable (single-threaded wasm fallback build).
    $targets = & rustup target list --installed
    if ($targets -notcontains 'wasm32-unknown-unknown') {
        Invoke-Exec -File 'rustup' -Arguments @('target', 'add', 'wasm32-unknown-unknown')
    }

    # nightly + rust-src + wasm32: required for the threaded wasm build
    # (wasm-bindgen-rayon needs atomics, which needs -Z build-std).
    $toolchains = & rustup toolchain list
    if (-not ($toolchains | Where-Object { $_ -match '^nightly' })) {
        Write-Info 'Installing nightly toolchain (for wasm threads)...'
        Invoke-Exec -File 'rustup' -Arguments @('toolchain', 'install', 'nightly', '--profile', 'minimal', '--component', 'rust-src')
    }
    & rustup component add rust-src --toolchain nightly 2>$null | Out-Null
    & rustup target add wasm32-unknown-unknown --toolchain nightly 2>$null | Out-Null
    Write-Ok 'wasm32-unknown-unknown target (stable + nightly) present'
}

function Ensure-WasmBindgenCli {
    Write-Section 'Toolchain: wasm-bindgen-cli'
    Add-CargoBinToPath
    $need = $true
    if (Test-CommandExists 'wasm-bindgen') {
        $v = (& wasm-bindgen --version) -join ''
        if ($v -match [regex]::Escape($script:WasmBindgenVersion)) { $need = $false }
        else { Write-Warn "wasm-bindgen-cli version mismatch ($v), need $script:WasmBindgenVersion" }
    }
    if ($need) {
        Invoke-Exec -File 'cargo' -Arguments @('install', 'wasm-bindgen-cli', '--version', $script:WasmBindgenVersion, '--locked')
    }
    Write-Ok "wasm-bindgen-cli $script:WasmBindgenVersion"
}

function Ensure-CppToolchain {
    Write-Section 'Toolchain: C++ / CMake'
    if (-not (Test-CommandExists 'cmake')) {
        throw 'cmake not found. Install CMake (>= 3.24) and re-run.'
    }
    $hasCxx = (Test-CommandExists 'g++') -or (Test-CommandExists 'clang++') -or (Test-CommandExists 'cl')
    if (-not $hasCxx) {
        throw 'No C++ compiler found (need g++, clang++, or MSVC cl).'
    }
    Write-Ok "cmake $((& cmake --version | Select-Object -First 1) -join '')"
}

function Ensure-WebTestDeps {
    Write-Section 'Toolchain: Node / Playwright'
    if (-not (Test-CommandExists 'node')) {
        throw 'node not found. Install Node.js (>= 20) and re-run.'
    }
    $webTest = Join-Path (Get-RepoRoot) 'web-test'
    if (-not (Test-Path (Join-Path $webTest 'node_modules' '@playwright'))) {
        Invoke-Exec -File 'npm' -Arguments @('install') -WorkingDirectory $webTest
    }
    # Install the chromium browser if missing (idempotent + quick when cached).
    Invoke-Exec -File 'npx' -Arguments @('playwright', 'install', 'chromium') -WorkingDirectory $webTest
    Write-Ok 'Playwright + chromium ready'
}
