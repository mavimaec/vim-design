#!/usr/bin/env pwsh
# vactions.ps1 — run VIM Design actions.
#
#   vactions.ps1 -VimDesignWeb   serve VimDesignWeb and open it in a browser
#   vactions.ps1 -Test           sequentially run all tests:
#                                cargo tests, C++ gtest, Playwright web tests
#   vactions.ps1 -TestWebGpu     opt-in HEADED browser run of the demo on the
#                                real WebGPU backend (needs a desktop session;
#                                headless chromium never composites WebGPU)

[CmdletBinding()]
param(
    [switch]$VimDesignWeb,
    [switch]$Test,
    [switch]$TestWebGpu
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'lib' 'common.ps1')
. (Join-Path $PSScriptRoot 'lib' 'toolchain.ps1')
. (Join-Path $PSScriptRoot 'lib' 'build.ps1')

$root = Get-RepoRoot
$www = Join-Path $root 'crates' 'vim-design-web' 'www'
$serverScript = Join-Path $PSScriptRoot 'lib' 'dev-server.mjs'

function Ensure-WasmBuilt {
    if (-not (Test-Path (Join-Path $www 'pkg'))) {
        Write-Warn 'wasm bundle missing - building it first'
        Ensure-RustToolchain
        Ensure-WasmBindgenCli
        Build-Wasm | Out-Null
    }
}

if (-not ($VimDesignWeb -or $Test -or $TestWebGpu)) {
    Write-Host 'Usage: vactions.ps1 [-VimDesignWeb] [-Test] [-TestWebGpu]'
    exit 1
}

if ($TestWebGpu) {
    Write-Section 'Headed WebGPU verification (opt-in, needs a desktop session)'
    Ensure-WasmBuilt
    Ensure-WebTestDeps
    Invoke-Exec -File 'npx' `
        -Arguments @('playwright', 'test', 'tests/demo-webgpu.spec.js') `
        -WorkingDirectory (Join-Path $root 'web-test') `
        -Environment @{ VIM_WEBGPU_HEADED = '1' }
    Write-Ok 'headed WebGPU run passed (screenshots/demo-scene-webgpu.png)'
    exit 0
}

if ($VimDesignWeb) {
    Ensure-WasmBuilt
    $url = 'http://localhost:8787/index.html'
    Write-Section "VimDesignWeb: serving $www"
    Write-Info "URL: $url  (COOP/COEP headers enabled for wasm threads)"

    # Open the default browser shortly after the server starts.
    $opener = if ($IsMacOS) { 'open' } elseif ($IsWindows) { $null } else { 'xdg-open' }
    Start-Job -ScriptBlock {
        param($opener, $url)
        Start-Sleep -Seconds 1
        if ($null -eq $opener) { Start-Process $url } else { & $opener $url }
    } -ArgumentList $opener, $url | Out-Null

    # Run the dev server in the foreground; Ctrl+C stops it.
    Invoke-Exec -File 'node' -Arguments @($serverScript, $www, '8787')
    exit 0
}

if ($Test) {
    $results = [ordered]@{}

    # 1. Rust: unit + integration tests (vim-design-lib, -ffi, -test).
    Write-Section 'Tests 1/3: cargo test --workspace'
    Add-CargoBinToPath
    try {
        Invoke-Exec -File 'cargo' -Arguments @('test', '--workspace') -WorkingDirectory $root
        $results['Rust (cargo test)'] = 'PASS'
    } catch {
        $results['Rust (cargo test)'] = "FAIL - $_"
    }

    # 2. C++: GoogleTest via ctest (build first if needed).
    Write-Section 'Tests 2/3: VimDesignCppTest (GoogleTest)'
    $cppBuild = Join-Path $root 'cpp' 'vim-design-cpp-test' 'build'
    try {
        if (-not (Test-Path (Join-Path $cppBuild 'CMakeCache.txt'))) {
            Ensure-CppToolchain
            Build-Cpp -Config 'debug'
        }
        Invoke-Exec -File 'ctest' -Arguments @('--test-dir', $cppBuild, '--output-on-failure')
        $results['C++ (GoogleTest)'] = 'PASS'
    } catch {
        $results['C++ (GoogleTest)'] = "FAIL - $_"
    }

    # 3. Web: Playwright threading probe (starts its own COOP/COEP server).
    Write-Section 'Tests 3/3: VimDesignWebTest (Playwright)'
    try {
        Ensure-WasmBuilt
        Ensure-WebTestDeps
        Invoke-Exec -File 'npx' -Arguments @('playwright', 'test') -WorkingDirectory (Join-Path $root 'web-test')
        $results['Web (Playwright)'] = 'PASS'
    } catch {
        $results['Web (Playwright)'] = "FAIL - $_"
    }

    Write-Section 'Test summary'
    $failed = 0
    foreach ($k in $results.Keys) {
        $v = $results[$k]
        if ($v -eq 'PASS') {
            Write-Ok ("{0,-22} PASS" -f $k)
        } else {
            Write-Warn ("{0,-22} {1}" -f $k, $v)
            $failed++
        }
    }
    exit $failed
}
