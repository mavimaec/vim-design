# common.ps1 — shared helpers for vbuild.ps1 / vactions.ps1.
# Cross-platform PowerShell 7 (pwsh); no Windows-only cmdlets.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Repository root = parent of devops/.
$script:RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..' '..')).Path

function Get-RepoRoot { return $script:RepoRoot }

function Write-Section {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host ''
    Write-Host "==== $Message ====" -ForegroundColor Cyan
}

function Write-Info  { param([string]$Message) Write-Host "  $Message" }
function Write-Warn  { param([string]$Message) Write-Host "  WARN: $Message" -ForegroundColor Yellow }
function Write-Ok    { param([string]$Message) Write-Host "  OK: $Message" -ForegroundColor Green }

# Make sure ~/.cargo/bin is on PATH for this process (rustup installs there).
function Add-CargoBinToPath {
    $cargoBin = Join-Path $HOME '.cargo' 'bin'
    if ((Test-Path $cargoBin) -and (-not (($env:PATH -split [IO.Path]::PathSeparator) -contains $cargoBin))) {
        $env:PATH = $cargoBin + [IO.Path]::PathSeparator + $env:PATH
    }
}

# Run an external command; throw if it exits non-zero.
function Invoke-Exec {
    param(
        [Parameter(Mandatory)][string]$File,
        [string[]]$Arguments = @(),
        [string]$WorkingDirectory,
        [hashtable]$Environment
    )
    $display = "$File $($Arguments -join ' ')"
    Write-Info $display

    $saved = @{}
    if ($Environment) {
        foreach ($k in $Environment.Keys) {
            $saved[$k] = [Environment]::GetEnvironmentVariable($k)
            [Environment]::SetEnvironmentVariable($k, $Environment[$k])
        }
    }
    $prevDir = Get-Location
    try {
        if ($WorkingDirectory) { Set-Location $WorkingDirectory }
        & $File @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "Command failed (exit $LASTEXITCODE): $display"
        }
    }
    finally {
        Set-Location $prevDir
        foreach ($k in $saved.Keys) {
            [Environment]::SetEnvironmentVariable($k, $saved[$k])
        }
    }
}

function Test-CommandExists {
    param([Parameter(Mandatory)][string]$Name)
    return $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}
