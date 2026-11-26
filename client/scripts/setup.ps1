#Requires -Version 5.0
# ===========================================================================
#  EchoAgent source setup check (Windows)
#
#  The embedded Runtime is committed directly to this repository. This script
#  verifies that a checkout contains the maintained Agent source.
#  Idempotent: safe to re-run.
#
#  Usage:
#    powershell -ExecutionPolicy Bypass -File scripts/setup.ps1
# ===========================================================================

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

function Log-Step([string]$msg) { Write-Host ""; Write-Host "===> $msg" -ForegroundColor Cyan }
function Log-Ok([string]$msg)   { Write-Host "  [OK]   $msg" -ForegroundColor Green }
function Log-Info([string]$msg) { Write-Host "         $msg" -ForegroundColor DarkGray }

Log-Step "Verifying Agent source"
& node (Join-Path $ProjectRoot "scripts\verify-agent-source.mjs")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Log-Ok "No submodule initialization or upstream checkout is required"

Log-Step "Setup complete"
Log-Info "Next:"
Log-Info "  pnpm install"
Log-Info "  pnpm dev            # or: powershell -File scripts\build.ps1"
