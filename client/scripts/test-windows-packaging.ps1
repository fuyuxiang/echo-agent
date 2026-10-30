#Requires -Version 5.0
[CmdletBinding()]
param([string]$NodePath = (Get-Command node -ErrorAction Stop).Source)

$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "windows-packaging.ps1")
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("echo-packaging-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $testRoot | Out-Null
$script:assertions = 0

function Assert-True($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:assertions++
}

function Assert-Fails([scriptblock]$Action, [string]$Pattern, $ExitCode = $null) {
    $failure = $null
    try { & $Action | Out-Null } catch { $failure = $_ }
    Assert-True ($null -ne $failure) "Expected failure: $Pattern"
    Assert-True ($failure.Exception.Message -match $Pattern) "Unexpected failure: $failure"
    if ($null -ne $ExitCode) {
        Assert-True ($failure.Exception.Data["ExitCode"] -eq $ExitCode) "Exit code was not preserved: $failure"
    }
}

function Stop-TestProcessTree($Process) {
    if ($Process.HasExited) { return }
    if ($PSVersionTable.PSVersion.Major -ge 7) {
        $Process.Kill($true)
    } else {
        # .NET Framework has no Kill(entireProcessTree) overload. This process
        # is owned by this fixture; keep its native children inside the timeout.
        $killStart = New-Object System.Diagnostics.ProcessStartInfo
        $killStart.FileName = Join-Path $env:SystemRoot 'System32/taskkill.exe'
        $killStart.Arguments = "/PID $($Process.Id) /T /F"
        $killStart.UseShellExecute = $false
        $killStart.CreateNoWindow = $true
        $killer = [System.Diagnostics.Process]::Start($killStart)
        try {
            if (-not $killer.WaitForExit(5000)) { $killer.Kill() }
        } finally { $killer.Dispose() }
        if (-not $Process.HasExited) { $Process.Kill() }
    }
    if (-not $Process.WaitForExit(5000)) { throw "Packaging fixture process could not be stopped" }
}

function Invoke-TestProcess([string]$Source, [int]$TimeoutMs = 20000) {
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = (Get-Process -Id $PID).Path
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Source))
    $start.Arguments = "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $encoded"
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = [System.Diagnostics.Process]::Start($start)
    try {
        # Both reads must start before waiting. Sequential ReadToEnd deadlocks
        # when the unconsumed stream fills its (small on Windows) pipe buffer.
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutMs)) {
            throw "Packaging fixture timed out after $TimeoutMs ms (PID $($process.Id))"
        }
        $reads = [System.Threading.Tasks.Task[]]@($stdout, $stderr)
        if (-not [System.Threading.Tasks.Task]::WaitAll($reads, $TimeoutMs)) {
            throw "Packaging fixture output did not close after $TimeoutMs ms"
        }
        return @{ ExitCode = $process.ExitCode; Stdout = $stdout.Result; Stderr = $stderr.Result }
    } finally {
        try { Stop-TestProcessTree $process } finally { $process.Dispose() }
    }
}

try {
    foreach ($name in @("build.ps1", "windows-packaging.ps1", "test-windows-packaging.ps1")) {
        $tokens = $null
        $parseErrors = $null
        [System.Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot $name), [ref]$tokens, [ref]$parseErrors
        ) | Out-Null
        Assert-True ($parseErrors.Count -eq 0) "Parser errors in ${name}: $parseErrors"
    }

    # Reproduce the CI pipe pressure without relying on formatting or CLIXML.
    $pressure = Invoke-TestProcess '[Console]::Error.Write(("e" * 262144)); [Console]::Out.Write(("o" * 262144)); exit 7'
    Assert-True ($pressure.ExitCode -eq 7) "Pipe pressure lost the child exit code"
    Assert-True ($pressure.Stdout.Length -eq 262144) "Pipe pressure truncated stdout"
    Assert-True ($pressure.Stderr.Length -eq 262144) "Pipe pressure truncated stderr"
    $timeoutPidFile = Join-Path $testRoot 'timeout.pid'
    $timeoutSource = 'Set-Content -LiteralPath ''__PID_FILE__'' -Value $PID; Start-Sleep -Seconds 30'
    $timeoutSource = $timeoutSource.Replace('__PID_FILE__', $timeoutPidFile.Replace("'", "''"))
    Assert-Fails { Invoke-TestProcess $timeoutSource -TimeoutMs 3000 } "Packaging fixture timed out"
    Assert-True (Test-Path -LiteralPath $timeoutPidFile) "Timed fixture did not reach its blocking operation"
    $timedPid = [int](Get-Content -LiteralPath $timeoutPidFile)
    Assert-True ($null -eq (Get-Process -Id $timedPid -ErrorAction SilentlyContinue)) "Timed fixture was left running"

    $PSNativeCommandUseErrorActionPreference = $true
    $output = Invoke-PackagingCommand $NodePath @("-e", "console.error('expected native warning'); console.log('1.2.3')")
    Assert-True ($output -eq "1.2.3") "Native stderr contaminated version output"
    Assert-True ($ErrorActionPreference -eq "Stop") "Native invocation relaxed caller error handling"
    Assert-True $PSNativeCommandUseErrorActionPreference "Native preference leaked to caller"
    Assert-Fails { Invoke-PackagingCommand $NodePath @("-e", "console.error('expected native failure'); process.exit(7)") } "Command failed" 7
    Assert-Fails { Invoke-PackagingCommand (Join-Path $testRoot "missing-command.exe") } "not recognized|cannot find|could not be loaded"
    Assert-Fails { Get-Item -LiteralPath (Join-Path $testRoot "missing-file") } "Cannot find path"
    $merged = @(& {
        Invoke-PackagingCommand $NodePath @("-e", "console.error('logged warning'); console.log('logged-ok')")
    } *>&1 | Tee-Object -Variable logged)
    Assert-True (($merged | Out-String) -match "logged warning") "Merged logging lost native stderr"
    Assert-True (($logged | Out-String) -match "logged-ok") "Merged logging aborted a successful command"

    # pnpm on Windows can resolve to a .ps1 shim that invokes node and exits.
    $shim = Join-Path $testRoot "pnpm[shim].ps1"
    Set-Content -LiteralPath $shim -Value '& $args[0] -e $args[1]; exit $LASTEXITCODE'
    $output = Invoke-PackagingCommand $shim @($NodePath, "console.log('shim-ok')")
    Assert-True ($output -eq "shim-ok") "PowerShell command shim did not execute"
    Assert-Fails { Invoke-PackagingCommand $shim @($NodePath, "process.exit(9)") } "Command failed" 9

    foreach ($valid in @("0.0.0", "1.2.3", "1.2.3-rc.1+build.7", "1.2.3-0", "1.2.3-01a")) {
        Assert-True ((Get-PackagingReleaseVersion @($valid)) -eq $valid) "Rejected valid version $valid"
    }
    foreach ($invalid in @($null, "", " ", "01.2.3", "1.2", "v1.2.3", "1.2.3-01", "1.2.3+", "1.2.3`nextra")) {
        Assert-Fails { Get-PackagingReleaseVersion -Output @($invalid) } "empty or invalid output"
    }
    Assert-Fails { Get-PackagingReleaseVersion -Output @() } "empty or invalid output"
    Assert-Fails { Get-PackagingReleaseVersion -Output @("debug", "1.2.3") } "empty or invalid output"

    # Execute the real entry point, with native fixtures stopping before protoc
    # or any costly build. The checkout name reproduces wildcard interpretation.
    $project = Join-Path $testRoot "Repo[1] with spaces"
    $scripts = Join-Path $project "scripts"
    New-Item -ItemType Directory -Path $scripts | Out-Null
    foreach ($name in @("build.ps1", "windows-packaging.ps1")) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $scripts
    }
    $quotedNode = $NodePath.Replace("'", "''")
    $quotedRoot = $testRoot.Replace("'", "''")
    $quotedProject = $project.Replace("'", "''")
    foreach ($mode in @("valid", "empty", "invalid", "failure", "location", "valid-logged", "failure-logged")) {
        $source = @'
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath '__ROOT__'
$before = (Get-Location).Path
$env:PROTOC = Join-Path $before 'missing-protoc.exe'
function node {
    if ($args[0] -eq '--version') { & '__NODE__' --version }
    elseif ($args[0] -eq '-p') { & '__NODE__' -e "console.log('x64')" }
    elseif ('__MODE__' -like 'failure*') { & '__NODE__' -e "console.error('version mismatch'); process.exit(7)" }
    elseif ('__MODE__' -eq 'empty') { & '__NODE__' -e 'process.exit(0)' }
    elseif ('__MODE__' -eq 'invalid') { & '__NODE__' -e "console.log('not-a-version')" }
    else { & '__NODE__' -e "console.error('expected version warning'); console.log('1.2.3-rc.1+build.7')" }
}
function pnpm { if ($args[0] -ne '--version') { throw 'Unexpected expensive build' }; 'test-pnpm'; $global:LASTEXITCODE = 0 }
function cargo { 'test-cargo'; $global:LASTEXITCODE = 0 }
function rustc { 'test-rustc'; $global:LASTEXITCODE = 0 }
if ('__MODE__' -eq 'location') { function Push-Location { throw 'simulated location failure' } }
if ('__MODE__' -like '*-logged') {
    & (Join-Path '__PROJECT__' 'scripts/build.ps1') *>&1 | Tee-Object -Variable buildLog | Out-Host
} else {
    & (Join-Path '__PROJECT__' 'scripts/build.ps1')
}
$code = $LASTEXITCODE
if ((Get-Location).Path -ne $before) { throw 'Caller working directory was not restored' }
exit $code
'@
        $source = $source.Replace('__ROOT__', $quotedRoot).Replace('__NODE__', $quotedNode)
        $source = $source.Replace('__PROJECT__', $quotedProject).Replace('__MODE__', $mode)
        $result = Invoke-TestProcess $source
        $expectedCode = if ($mode -like "failure*") { 7 } else { 1 }
        Assert-True ($result.ExitCode -eq $expectedCode) "${mode}: unexpected exit $($result.ExitCode): $($result.Stdout) $($result.Stderr)"
        Assert-True ($result.Stdout -notmatch "Unexpected expensive build|Build succeeded") "${mode}: continued after failure"
        Assert-True ($result.Stderr -notmatch "Caller working directory") "${mode}: changed caller location"
        if ($mode -like "valid*") {
            Assert-True ($result.Stdout -match "Release version: 1\.2\.3-rc\.1\+build\.7") "Literal checkout or native warning prevented version check: $($result.Stdout) $($result.Stderr)"
            Assert-True ($result.Stdout -match "PROTOC points to a missing file") "Did not stop at the expected prerequisite"
        } elseif ($mode -eq "location") {
            Assert-True ($result.Stdout -match "simulated location failure") "Location failure was swallowed"
            Assert-True ($result.Stdout -notmatch "Checking toolchain") "Executed tools in unrelated working directory"
        } elseif ($mode -notlike "failure*") {
            Assert-True ($result.Stdout -match "empty or invalid output") "${mode}: invalid version did not fail early: $($result.Stdout)"
        }
    }

    Write-Host "Windows packaging guards passed ($script:assertions assertions; PowerShell $($PSVersionTable.PSVersion))."
} finally {
    Remove-Item -LiteralPath $testRoot -Recurse -Force
}
exit 0
