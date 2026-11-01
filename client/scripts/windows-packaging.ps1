# Shared guards for the Windows release entry point. Keep compatible with 5.1.

function Invoke-PackagingCommand {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @()
    )

    # Windows PowerShell 5.1 turns native stderr into NativeCommandError when
    # output is captured or redirected. Only native exit codes indicate failure;
    # keep this preference local so filesystem/cmdlet errors still stop builds.
    $ErrorActionPreference = "Continue"
    $PSNativeCommandUseErrorActionPreference = $false
    $global:LASTEXITCODE = $null
    & $FilePath @ArgumentList
    $commandExit = $LASTEXITCODE
    if ($null -eq $commandExit -or $commandExit -ne 0) {
        $failure = [System.Exception]::new("Command failed: $FilePath $($ArgumentList -join ' ') (exit $commandExit)")
        $failure.Data["ExitCode"] = if ($null -eq $commandExit) { 1 } else { [int]$commandExit }
        throw $failure
    }
}

function Get-PackagingReleaseVersion {
    param([AllowNull()][AllowEmptyCollection()][object[]]$Output)

    # A successful process is not enough: require exactly one SemVer line before
    # starting expensive builds or deriving installer paths from the response.
    $lines = @($Output)
    $version = if ($lines.Count -eq 1) { ([string]$lines[0]).Trim() } else { "" }
    $number = '(0|[1-9][0-9]*)'
    $identifier = '(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
    $semver = '\A' + $number + '\.' + $number + '\.' + $number +
        '(?:-' + $identifier + '(?:\.' + $identifier + ')*)?' +
        '(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\z'
    if (-not [regex]::IsMatch($version, $semver)) {
        throw "Release version check returned empty or invalid output. Expected one SemVer line; received: $($lines -join ' | ')"
    }
    return $version
}

function Get-PackagingVcVars {
    param(
        [string]$VsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe",
        [string]$BuildTools = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
    )

    # node-gyp 11 and node-pty's Spectre libraries require the VS 2022 toolchain.
    # Selecting `-latest` without a range can silently choose VS 2026 instead.
    if (Test-Path -LiteralPath $VsWhere -PathType Leaf) {
        $installations = @(Invoke-PackagingCommand $VsWhere @(
            "-latest", "-products", "*", "-version", "[17.0,18.0)",
            "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property", "installationPath"
        ))
        if ($installations.Count -gt 1) { throw "Visual Studio locator returned multiple installation paths." }
        if ($installations.Count -eq 1 -and -not [string]::IsNullOrWhiteSpace([string]$installations[0])) {
            $vcvars = Join-Path ([string]$installations[0]).Trim() "VC\Auxiliary\Build\vcvars64.bat"
            if (Test-Path -LiteralPath $vcvars -PathType Leaf) { return $vcvars }
        }
    }
    $vcvars = Join-Path $BuildTools "VC\Auxiliary\Build\vcvars64.bat"
    if (Test-Path -LiteralPath $vcvars -PathType Leaf) { return $vcvars }
    throw "Visual Studio 2022 C++ x64 build tools were not found. Install the Desktop development with C++ workload and MSVC v143 Spectre-mitigated libraries. VS 2026 alone is incompatible with the locked node-gyp 11 toolchain. GitHub Actions packaging must use windows-2022."
}

function Initialize-PackagingMsvc {
    $vcvars = Get-PackagingVcVars
    $envOut = @(Invoke-PackagingCommand cmd @("/c", "`"$vcvars`" >nul 2>&1 && set"))
    foreach ($line in $envOut) {
        if ($line -match '^([^=]+)=(.*)$') {
            Set-Item -LiteralPath "env:$($Matches[1])" -Value $Matches[2]
        }
    }
    if ($env:VisualStudioVersion -notmatch '^17\.') {
        throw "Visual Studio 2022 environment initialization failed: VisualStudioVersion=$env:VisualStudioVersion"
    }
    if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
        throw "link.exe is unavailable after initializing Visual Studio 2022."
    }
    # npm 11 prefers package-config options; keep the legacy alias for older
    # lifecycle scripts. Both must override stale values from a caller's shell.
    $env:npm_package_config_node_gyp_msvs_version = "2022"
    $env:npm_config_msvs_version = "2022"
    return $vcvars
}
