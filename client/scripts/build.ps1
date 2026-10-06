#Requires -Version 5.0
# ===========================================================================
#  EchoAgent Windows packaging script (PowerShell)
#
#  Produces a platform-signed NSIS installer (.exe). Updater signing happens
#  later on the controlled release machine.
#  Public artifacts use EchoAgent-v<VERSION>-windows-x86_64-setup.exe.
#
#  Usage:
#    powershell -ExecutionPolicy Bypass -File scripts/build.ps1
#    powershell -ExecutionPolicy Bypass -File scripts/build.ps1 -Version 0.2.0
#    powershell -ExecutionPolicy Bypass -File scripts/build.ps1 -AllowUnsignedPlatform  # development only
#
#  Prerequisites:
#    The complete vendored Runtime source is included in the repository.
#    `scripts/setup.ps1` can be used to verify checkout integrity.
# ===========================================================================

[CmdletBinding()]
param(
    [string]$Version,
    [switch]$AllowUnsignedPlatform
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

function Log-Step([string]$msg) {
    Write-Host ""
    Write-Host "===> $msg" -ForegroundColor Cyan
}
function Log-Ok([string]$msg)   { Write-Host "  [OK]   $msg" -ForegroundColor Green }
function Log-Warn([string]$msg) { Write-Host "  [WARN] $msg" -ForegroundColor Yellow }
function Log-Err([string]$msg)  { Write-Host "  [ERR]  $msg" -ForegroundColor Red }
function Log-Info([string]$msg) { Write-Host "         $msg" -ForegroundColor DarkGray }

# Track paths we reference. echo-agent-build path dependencies resolve directly to
# the source snapshot committed under vendor/echo-agent-build.
$script:RustToolchainPath = Join-Path $ProjectRoot "rust-toolchain.toml"
$script:RustToolchainBackup = $null

# ---------------------------------------------------------------------------
# 1. Toolchain sanity check
# ---------------------------------------------------------------------------
Log-Step "Checking toolchain"
foreach ($cmd in @("node", "pnpm", "cargo", "rustc")) {
    if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) {
        Log-Err "$cmd not found on PATH."
        exit 1
    }
    $v = (Invoke-Expression "$cmd --version") 2>$null
    Log-Info ("{0,-7} {1}" -f $cmd, $v)
}
Log-Ok "Core tools present"

$nodeArch = & node -p "process.arch"
if ($LASTEXITCODE -ne 0 -or $nodeArch -ne "x64") {
    Log-Err "Windows installer target is x86_64-pc-windows-msvc, but Node.js architecture is $nodeArch. Use x64 Node.js so the bundled Echo Code IDE runtime matches the installer."
    exit 1
}
Log-Ok "Node.js architecture matches windows-x86_64"

# ---------------------------------------------------------------------------
# 2. Version sync (optional)
# ---------------------------------------------------------------------------
if ($Version) {
    Log-Step "Syncing version -> $Version"
    & node (Join-Path $ProjectRoot "scripts\release-version.mjs") set $Version | Out-Null
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Log-Ok "Bumped and verified all four version sources"
}
$appVersion = (& node (Join-Path $ProjectRoot "scripts\release-version.mjs") check | Select-Object -Last 1).Trim()
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Log-Ok "Release version: $appVersion"

# Prefer an explicitly configured protoc, then PATH, then the documented
# Windows install location. Do not put this machine-specific path in Cargo's
# repository config: Cargo also reads that config on macOS and Linux.
$protoc = $null
if ($env:PROTOC) {
    if (Test-Path $env:PROTOC) {
        $protoc = $env:PROTOC
    } else {
        Log-Err "PROTOC points to a missing file: $env:PROTOC"
        exit 1
    }
} else {
    $protocCommand = Get-Command protoc.exe -ErrorAction SilentlyContinue
    if ($protocCommand) {
        $protoc = $protocCommand.Source
    } else {
        $defaultProtoc = "C:\Tools\protoc\bin\protoc.exe"
        if (Test-Path $defaultProtoc) {
            $env:PROTOC = $defaultProtoc
            $protoc = $defaultProtoc
        }
    }
}
if (-not $protoc) {
    Log-Err "protoc not found. Install protobuf, add protoc.exe to PATH, or set PROTOC."
    exit 1
}
$protocVersion = & $protoc --version
if ($LASTEXITCODE -ne 0) {
    Log-Err "protoc failed to run: $protoc"
    exit $LASTEXITCODE
}
Log-Ok "protoc available: $protocVersion ($protoc)"

# ---------------------------------------------------------------------------
# 3. MSVC environment (cargo x86_64-pc-windows-msvc needs link.exe + SDK).
#    Reuses the same vcvars/vswhere dance as dev.bat.
# ---------------------------------------------------------------------------
Log-Step "Locating MSVC environment"
$vcvars = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path $vcvars)) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $instPath = & $vswhere -latest -products * -property installationPath 2>$null
        if ($instPath -and (Test-Path (Join-Path $instPath "VC\Auxiliary\Build\vcvars64.bat"))) {
            $vcvars = Join-Path $instPath "VC\Auxiliary\Build\vcvars64.bat"
        }
    }
}
if (-not (Test-Path $vcvars)) {
    Log-Err "vcvars64.bat not found."
    Log-Err "Install the 'Desktop development with C++' workload in Visual Studio or VS Build Tools."
    exit 1
}
Log-Info "Using: $vcvars"

# Run vcvars64.bat in a cmd subprocess and import its env into this session.
$envOut = & cmd /c "`"$vcvars`" >nul 2>&1 && set"
foreach ($line in $envOut) {
    if ($line -match '^([^=]+)=(.*)$') {
        Set-Item -Path "env:$($Matches[1])" -Value $Matches[2]
    }
}
if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
    Log-Err "link.exe still not on PATH after vcvars."
    exit 1
}
Log-Ok "MSVC link.exe available: $((Get-Command link.exe).Source)"
if ($vcvars -match '\\2022\\') {
    # node-gyp can otherwise select a newer Visual Studio installation that
    # lacks the Spectre libraries required by node-pty.
    $env:npm_config_msvs_version = "2022"
}

# ---------------------------------------------------------------------------
# 4. Vendored Runtime sanity check. Path dependencies in Cargo.toml resolve
#    directly into vendor/echo-agent-build, which is tracked by this repository.
# ---------------------------------------------------------------------------
Log-Step "Checking vendored Runtime source"
& node (Join-Path $ProjectRoot "scripts\verify-vendored-runtime.mjs")
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Log-Ok "Vendored Runtime source is complete"

Log-Step "Building and staging Echo Code IDE"
& pnpm ide:build
if ($LASTEXITCODE -ne 0) { throw "Theia IDE build failed" }
& pnpm ide:stage
if ($LASTEXITCODE -ne 0) { throw "Theia runtime staging failed" }
$stagedTheiaEntry = Join-Path $ProjectRoot "src-tauri\resources\theia\browser\lib\backend\main.js"
$stagedNode = Join-Path $ProjectRoot "src-tauri\resources\theia\node\node.exe"
if (-not (Test-Path $stagedTheiaEntry -PathType Leaf) -or -not (Test-Path $stagedNode -PathType Leaf)) {
    Log-Err "Echo Code IDE or its Node.js runtime is missing from the staged Windows resources."
    exit 1
}
$stagedNodeArch = & $stagedNode -p "process.arch"
if ($LASTEXITCODE -ne 0 -or $stagedNodeArch -ne "x64") {
    Log-Err "Staged Node.js architecture is $stagedNodeArch; the Windows installer requires x64."
    exit 1
}
Log-Ok "Vendored Theia IDE and Node runtime staged"
Log-Step "Checking Windows IDE terminal lifecycle"
& $stagedNode (Join-Path $ProjectRoot "scripts\validate-theia-pty.mjs") (Join-Path $ProjectRoot "src-tauri\resources\theia\browser")
if ($LASTEXITCODE -ne 0) { throw "Theia Windows terminal validation failed" }
Log-Ok "Windows IDE terminal lifecycle passed"

# ---------------------------------------------------------------------------
# 5. NSIS tool cache (work around GitHub download timeouts in CN).
#    Pre-place nsis-3.11 + nsis_tauri_utils.dll in Tauri's cache so the
#    bundler skips its own (often failing) download.
#
#    Tauri's expected layout (flat — zip's top-level nsis-3.11/ prefix is
#    stripped on extract):
#      %LOCALAPPDATA%\tauri\NSIS\makensis.exe
#      %LOCALAPPDATA%\tauri\NSIS\Plugins\x86-unicode\additional\nsis_tauri_utils.dll
# ---------------------------------------------------------------------------
Log-Step "Ensuring NSIS tool cache"
$nsisCacheRoot = Join-Path $env:LOCALAPPDATA "tauri\NSIS"
$makensis      = Join-Path $nsisCacheRoot "makensis.exe"

if (Test-Path $makensis) {
    Log-Ok "NSIS cache hit: $makensis"
} else {
    Log-Info "NSIS not cached; downloading via gh-proxy mirror"
    $tmpDir = Join-Path $env:TEMP "echoagent-nsis-prep"
    New-Item -ItemType Directory -Force -Path $tmpDir | Out-Null

    $nsisZipUrl = "https://gh-proxy.com/https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip"
    $dllUrl     = "https://gh-proxy.com/https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll"
    $nsisZip    = Join-Path $tmpDir "nsis-3.11.zip"
    $utilsDll   = Join-Path $tmpDir "nsis_tauri_utils.dll"

    $expectedZipHash = "EF7FF767E5CBD9EDD22ADD3A32C9B8F4500BB10D"
    $expectedDllHash = "75197FEE3C6A814FE035788D1C34EAD39349B860"

    # Helper: download a URL to a file with up to 2 attempts, verifying SHA1.
    # Mirrors occasionally return truncated/corrupt content, so verify + retry.
    function Download-Verified([string]$url, [string]$outPath, [string]$expectedSha1, [string]$label) {
        for ($attempt = 1; $attempt -le 2; $attempt++) {
            if (Test-Path $outPath) { Remove-Item $outPath -Force }
            try {
                Log-Info "Fetching $label (attempt $attempt)"
                Invoke-WebRequest -Uri $url -OutFile $outPath -UseBasicParsing -ErrorAction Stop
            } catch {
                Log-Warn "$label attempt $attempt network error: $($_.Exception.Message)"
                continue
            }
            $hash = (Get-FileHash $outPath -Algorithm SHA1).Hash
            if ($hash -eq $expectedSha1) {
                Log-Ok "$label verified (SHA1 $hash)"
                return $true
            }
            Log-Warn "$label SHA1 mismatch: got $hash expected $expectedSha1"
        }
        # All attempts failed; remove the corrupt file so we don't leave junk.
        if (Test-Path $outPath) { Remove-Item $outPath -Force }
        return $false
    }

    $downloadOk = $true
    try {
        $zipOk = Download-Verified $nsisZipUrl $nsisZip $expectedZipHash "nsis-3.11.zip"
        $dllOk = Download-Verified $dllUrl     $utilsDll $expectedDllHash "nsis_tauri_utils.dll"
        $downloadOk = $zipOk -and $dllOk

        if ($downloadOk) {
            if (Test-Path $nsisCacheRoot) { Remove-Item $nsisCacheRoot -Recurse -Force }
            # Extract to a staging dir first, then flatten the nsis-3.11/ prefix
            # so files land directly under $nsisCacheRoot (matching Tauri's layout).
            $staging = Join-Path $tmpDir "nsis-extract"
            if (Test-Path $staging) { Remove-Item $staging -Recurse -Force }
            Expand-Archive -Path $nsisZip -DestinationPath $staging -Force
            New-Item -ItemType Directory -Force -Path $nsisCacheRoot | Out-Null
            $inner = Join-Path $staging "nsis-3.11"
            Copy-Item -Path (Join-Path $inner "*") -Destination $nsisCacheRoot -Recurse -Force

            # nsis_tauri_utils.dll goes under an `additional` subdir (Tauri's convention).
            $pluginDir = Join-Path $nsisCacheRoot "Plugins\x86-unicode\additional"
            New-Item -ItemType Directory -Force -Path $pluginDir | Out-Null
            Copy-Item $utilsDll -Destination $pluginDir -Force
            if (Test-Path $makensis) {
                Log-Ok "NSIS cached at $makensis"
            } else {
                Log-Warn "Extraction looked OK but makensis.exe still missing; Tauri will retry."
            }
        } else {
            Log-Warn "Mirror download/verification failed after retries."
            Log-Warn "Continuing; Tauri will attempt its own download next."
        }
    } catch {
        Log-Warn "Mirror setup threw: $($_.Exception.Message)"
        Log-Warn "Continuing; Tauri will attempt its own download next."
    }
}

# ---------------------------------------------------------------------------
# 6. Build the platform installer. The override disables updater signing here;
#    the controlled release machine creates and signs canonical updater files.
# ---------------------------------------------------------------------------
Log-Step "Building NSIS installer (pnpm tauri build --bundles nsis)"
try {
    & pnpm tauri build --target x86_64-pc-windows-msvc --bundles nsis
    $buildExit = $LASTEXITCODE
} catch {
    Log-Err "pnpm tauri build threw: $($_.Exception.Message)"
    $buildExit = 1
}

# ---------------------------------------------------------------------------
# 7. Give every public artifact one cross-platform naming convention.
# ---------------------------------------------------------------------------
$bundleDir = Join-Path $ProjectRoot "src-tauri\target\x86_64-pc-windows-msvc\release\bundle\nsis"
$canonicalInstaller = $null
if ($buildExit -eq 0 -and (Test-Path $bundleDir)) {
    $defaultInstaller = Join-Path $bundleDir ("EchoAgent_{0}_x64-setup.exe" -f $appVersion)
    $canonicalName = "EchoAgent-v{0}-windows-x86_64-setup.exe" -f $appVersion
    $canonicalInstaller = Join-Path $bundleDir $canonicalName

    if (-not (Test-Path $defaultInstaller -PathType Leaf)) {
        Log-Err "Expected Tauri NSIS installer not found: $defaultInstaller"
        exit 1
    }
    $versionInfo = (Get-Item $defaultInstaller).VersionInfo
    $coreParts = (($appVersion -split '[-+]')[0] -split '\.')
    if ($versionInfo.FileMajorPart -ne [int]$coreParts[0] -or
        $versionInfo.FileMinorPart -ne [int]$coreParts[1] -or
        $versionInfo.FileBuildPart -ne [int]$coreParts[2]) {
        Log-Err "Installer file version $($versionInfo.FileVersion) does not match release version $appVersion"
        exit 1
    }

    $authenticode = Get-AuthenticodeSignature $defaultInstaller
    if ($authenticode.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        if ($AllowUnsignedPlatform) {
            Log-Warn "Authenticode check failed ($($authenticode.Status)); development override accepted."
        } else {
            Log-Err "Authenticode check failed: $($authenticode.Status) $($authenticode.StatusMessage)"
            Log-Err "Configure Windows code signing, or use -AllowUnsignedPlatform for development only."
            exit 1
        }
    } else {
        Log-Ok "Windows Authenticode signature is valid"
    }
    Move-Item -Force $defaultInstaller $canonicalInstaller
    Log-Ok "Normalized release name for windows-x86_64"
}

# ---------------------------------------------------------------------------
# 8. Report artifacts.
# ---------------------------------------------------------------------------
if ($buildExit -eq 0 -and (Test-Path $canonicalInstaller -PathType Leaf)) {
    Log-Step "Build succeeded. Artifacts:"
    Get-Item $canonicalInstaller | ForEach-Object {
        $sizeMb = "{0:N1}" -f ($_.Length / 1MB)
        Log-Ok ("{0,-40} {1} MB" -f $_.Name, $sizeMb)
        Log-Info $_.FullName
    }
    Log-Info "Run scripts/prepare-update-artifacts.sh on the release machine to create updater files."
} else {
    Log-Err "Build failed (exit $buildExit). See output above."
    if ($buildExit -eq 0) { $buildExit = 1 }
}

if ($buildExit -ne 0) { exit $buildExit }
