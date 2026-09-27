#!/usr/bin/env pwsh
# vpages.ps1 — build the self-contained GitHub Pages site (single-page app).
#
#   vpages.ps1              builds dist/pages
#   vpages.ps1 -Serve       builds, then serves dist/pages on http://localhost:8790/
#                           WITHOUT the COOP/COEP headers, exactly like GitHub Pages
#   vpages.ps1 -SkipBuild   re-assembles dist/pages from the existing www/pkg-st bundle
#
# Site layout (dist/pages):
#   index.html          the authoring app (www/app.html) — or the parametric demo
#                       (www/index.html) while app.html does not exist yet
#   demo.html           the parametric demo (only when app.html exists)
#   pkg/                the single-threaded wasm bundle (GitHub Pages cannot send
#                       COOP/COEP, so the threaded bundle cannot run there)
#   version.json        commit/build stamp shown in the app (feedback loop)
#
# The CI workflow (.github/workflows/pages.yml) runs this script on every push to
# develop and deploys dist/pages.

param(
    [switch]$Serve,
    [switch]$SkipBuild,
    [int]$Port = 8790
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'lib' 'common.ps1')
. (Join-Path $PSScriptRoot 'lib' 'toolchain.ps1')
. (Join-Path $PSScriptRoot 'lib' 'build.ps1')

$root = Get-RepoRoot
$www = Join-Path $root 'crates' 'vim-design-web' 'www'
$dist = Join-Path $root 'dist' 'pages'

if (-not $SkipBuild) {
    Write-Section 'GitHub Pages: single-threaded wasm bundle'
    Add-CargoBinToPath
    Ensure-WasmBindgenCli
    Build-WasmSingleThreaded
}

Write-Section 'GitHub Pages: assemble dist/pages'
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force -Path $dist | Out-Null

# Static files: everything in www except the wasm bundles and the threading probe
# (the probe needs cross-origin isolation, which Pages cannot provide).
$exclude = @('pkg', 'pkg-st', 'probe.html', 'probe.js')
Get-ChildItem -Path $www | Where-Object { $exclude -notcontains $_.Name } | ForEach-Object {
    Copy-Item -Recurse -Force $_.FullName (Join-Path $dist $_.Name)
}
Copy-Item -Recurse -Force (Join-Path $www 'pkg-st') (Join-Path $dist 'pkg')

# The authoring app becomes the site root once it exists.
$appHtml = Join-Path $dist 'app.html'
if (Test-Path $appHtml) {
    Move-Item -Force (Join-Path $dist 'index.html') (Join-Path $dist 'demo.html')
    Move-Item -Force $appHtml (Join-Path $dist 'index.html')
    Write-Ok 'index.html = authoring app, demo.html = parametric demo'
} else {
    Write-Ok 'index.html = parametric demo (www/app.html not present yet)'
}

# Build stamp for the feedback loop: the app shows which commit is running.
$sha = $env:GITHUB_SHA
if (-not $sha) { $sha = (& git -C $root rev-parse HEAD 2>$null) -join '' }
$dirty = $false
if (-not $env:GITHUB_SHA) { $dirty = [bool]((& git -C $root status --porcelain 2>$null) -join '') }
$version = [ordered]@{
    commit   = $sha
    short    = if ($sha.Length -ge 7) { $sha.Substring(0, 7) } else { $sha }
    dirty    = $dirty
    ref      = if ($env:GITHUB_REF_NAME) { $env:GITHUB_REF_NAME } else { ((& git -C $root rev-parse --abbrev-ref HEAD 2>$null) -join '') }
    built_at = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
}
$version | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $dist 'version.json')
New-Item -ItemType File -Force -Path (Join-Path $dist '.nojekyll') | Out-Null

$sizeMb = [math]::Round(((Get-ChildItem -Recurse -File $dist | Measure-Object Length -Sum).Sum / 1MB), 2)
Write-Ok "dist/pages assembled ($sizeMb MB, commit $($version.short))"

if ($Serve) {
    Write-Section "Serving dist/pages on http://localhost:$Port/ (no COOP/COEP, like GitHub Pages)"
    Invoke-Exec -File 'node' -WorkingDirectory $root -Arguments @(
        (Join-Path $root 'devops' 'lib' 'dev-server.mjs'), $dist, "$Port", '--no-isolation'
    )
}
