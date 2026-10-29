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
