#Requires -Version 5.0
param(
    [string]$RuntimeRoot = (Join-Path (Split-Path -Parent $PSScriptRoot) "agent")
)

$ErrorActionPreference = "Stop"

# Maintenance helper for importing a newer upstream Runtime snapshot. Normal
# setup and release builds do not run it because the result is already vendored.
if (-not (Test-Path (Join-Path $RuntimeRoot "Cargo.toml"))) {
    throw "Embedded Runtime source is incomplete at: $RuntimeRoot"
}

# Construct the legacy namespace without retaining it in the source tree.
$legacyRuntimeNamespace = -join @([char]120, [char]46, [char]97, [char]105)
$targetRuntimeNamespace = "echo.agent"
$relativePaths = @(git -C $RuntimeRoot grep -Iil -F $legacyRuntimeNamespace -- .)

if ($LASTEXITCODE -gt 1) {
    throw "Unable to inspect the Embedded Runtime namespace."
}

$utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
$updatedFiles = 0
foreach ($relativePath in $relativePaths) {
    if ([string]::IsNullOrWhiteSpace($relativePath)) { continue }
    $sourcePath = Join-Path $RuntimeRoot $relativePath
    $content = [System.IO.File]::ReadAllText($sourcePath)
    $updated = [System.Text.RegularExpressions.Regex]::Replace(
        $content,
        [System.Text.RegularExpressions.Regex]::Escape($legacyRuntimeNamespace),
        $targetRuntimeNamespace,
        [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
    )
    if ($updated -ne $content) {
        [System.IO.File]::WriteAllText($sourcePath, $updated, $utf8WithoutBom)
        $updatedFiles++
    }
}

if ($updatedFiles -eq 0) {
    Write-Host "  [OK]   Embedded Runtime namespace already uses $targetRuntimeNamespace" -ForegroundColor Green
} else {
    Write-Host "  [OK]   Migrated $updatedFiles Embedded Runtime files to $targetRuntimeNamespace" -ForegroundColor Green
}
