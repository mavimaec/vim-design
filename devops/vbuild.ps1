#!/usr/bin/env pwsh
# vbuild.ps1 — build the whole VIM Design codebase, bootstrapping tools as needed.
#
#   vbuild.ps1 -Clean            cleans generated/build outputs
#   vbuild.ps1 -Debug            builds Rust (native + wasm) and C++ in debug
#   vbuild.ps1 -Release          builds Rust (native + wasm) and C++ in release
#   vbuild.ps1 -Clean -Release   clean, then release build
#
# Note: the wasm bundles are always compiled with --release (the threading
# probe measures timings; debug wasm would be meaningless) regardless of
# -Debug/-Release, which govern the native Rust + C++ builds.

# No [CmdletBinding()]: it would inject the common -Debug parameter, which
# collides with our -Debug switch.
param(
    [switch]$Clean,
    [switch]$Debug,
    [switch]$Release
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'lib' 'common.ps1')
. (Join-Path $PSScriptRoot 'lib' 'toolchain.ps1')
. (Join-Path $PSScriptRoot 'lib' 'build.ps1')

if (-not ($Clean -or $Debug -or $Release)) {
    Write-Host 'Usage: vbuild.ps1 [-Clean] [-Debug] [-Release]'
    exit 1
}

$sw = [System.Diagnostics.Stopwatch]::StartNew()

if ($Clean) {
    Invoke-CleanRepo
}

if ($Debug -or $Release) {
    $config = if ($Release) { 'release' } else { 'debug' }

    Ensure-RustToolchain
    Ensure-WasmBindgenCli
    Ensure-CppToolchain

    Build-RustNative -Config $config
    $threadedWasm = Build-Wasm
    Build-Cpp -Config $config

    Write-Section 'Build summary'
    Write-Ok "native Rust workspace   : target/$config"
    Write-Ok 'FFI header              : crates/vim-design-ffi/include/vim_design.h'
    if ($threadedWasm) {
        Write-Ok 'wasm (threaded)         : crates/vim-design-web/www/pkg'
    } else {
        Write-Warn 'wasm threaded build failed; www/pkg contains the single-threaded fallback'
    }
    Write-Ok 'wasm (single-threaded)  : crates/vim-design-web/www/pkg-st'
    Write-Ok 'C++ gtest binary        : cpp/vim-design-cpp-test/build'
}

$sw.Stop()
Write-Host ''
Write-Host ("vbuild finished in {0:n1}s" -f $sw.Elapsed.TotalSeconds) -ForegroundColor Cyan
