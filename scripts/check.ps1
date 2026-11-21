#!/usr/bin/env pwsh
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$App = Join-Path $Root "apps/harness-app"
function Assert-Command {
    param([Parameter(Mandatory)][string]$Name)
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command '$Name' was not found on PATH."
    }
}
function Invoke-Step {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Command
    )
    Write-Host ""
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE."
    }
}
Assert-Command cargo
Assert-Command pnpm
if (-not (Test-Path (Join-Path $App "node_modules"))) {
    throw "Frontend dependencies are missing. Run: pnpm --dir apps/harness-app install"
}
Set-Location $Root
Invoke-Step "file size" { node scripts/size-check.mjs }
Invoke-Step "cargo fmt --check" { cargo fmt --all -- --check }
Invoke-Step "cargo clippy" { cargo clippy --workspace --all-targets -- -D warnings }
Invoke-Step "cargo test" { cargo test --workspace }
Invoke-Step "frontend typecheck" { pnpm --dir apps/harness-app typecheck }
Write-Host ""
Write-Host "All checks passed." -ForegroundColor Green
