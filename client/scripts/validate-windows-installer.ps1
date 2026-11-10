#Requires -Version 5.1
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$Installer)

$ErrorActionPreference = 'Stop'
# This acceptance changes the product's HKCU installation record. Never run it
# on a workstation or use a production user's profile for packaging tests.
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'Installer acceptance requires a disposable GitHub Actions Windows runner.'
}
$Installer = (Get-Item -LiteralPath $Installer).FullName
$productKey = 'HKCU:\Software\echoagent\EchoAgent'
$uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\EchoAgent'
if ((Test-Path -LiteralPath $productKey) -or (Test-Path -LiteralPath $uninstallKey)) {
    throw 'The runner already has an EchoAgent install record; use a clean runner.'
}
$defaultDir = Join-Path $env:LOCALAPPDATA 'EchoAgent'
$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ('echo-install-acceptance-' + [guid]::NewGuid())
$customRoot = Join-Path $env:LOCALAPPDATA ('Echo安装验收 ' + [guid]::NewGuid())
$customDir = Join-Path $customRoot '新建 工作区\客户端'
$desktopLink = Join-Path ([Environment]::GetFolderPath('Desktop')) 'EchoAgent.lnk'
$menuLink = Join-Path ([Environment]::GetFolderPath('Programs')) 'EchoAgent.lnk'
. (Join-Path $PSScriptRoot 'windows-shortcut.ps1')
$assertions = 0
$phase = 'first install'
$transcribing = $false
if ($env:ECHO_DESKTOP_VALIDATION_OUTPUT) {
    New-Item -ItemType Directory -Path $env:ECHO_DESKTOP_VALIDATION_OUTPUT -Force | Out-Null
    Start-Transcript -LiteralPath (Join-Path $env:ECHO_DESKTOP_VALIDATION_OUTPUT 'windows-installer.log') | Out-Null
    $transcribing = $true
}

function Assert-True($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:assertions++
}

function Invoke-Installer([string]$Arguments, [bool]$ShouldSucceed = $true) {
    Write-Host "Installer phase: $script:phase; arguments: $Arguments"
    # NSIS requires /D= to be last and unquoted, even for paths with spaces.
    $process = Start-Process -FilePath $Installer -ArgumentList $Arguments -PassThru
    try {
        if (-not $process.WaitForExit(240000)) {
            $process.Kill()
            throw "Installer timed out: $Arguments"
        }
        if ($ShouldSucceed) {
            Assert-True ($process.ExitCode -eq 0) "Installer failed ($($process.ExitCode)): $Arguments"
        } else {
            Assert-True ($process.ExitCode -ne 0) "Unsafe install unexpectedly succeeded: $Arguments"
        }
    } finally { $process.Dispose() }
}

function Assert-Install([string]$Directory) {
    Write-Host "Checking installed payload, registration and shortcuts: $Directory"
    foreach ($leaf in @('echoagent.exe', 'uninstall.exe', 'theia', 'office')) {
        Assert-True (Test-Path -LiteralPath (Join-Path $Directory $leaf)) "Missing installed payload: $Directory\$leaf"
    }
    $record = (Get-Item -LiteralPath $productKey).GetValue('')
    Assert-True ($record -eq $Directory) "Wrong installation record: $record"
    $metadata = Get-ItemProperty -LiteralPath $uninstallKey
    Assert-True ($metadata.InstallLocation.Trim('"') -eq $Directory) 'Wrong Programs and Features installation location'
    Assert-True ($metadata.UninstallString -eq ('"' + (Join-Path $Directory 'uninstall.exe') + '"')) 'Wrong uninstaller registration'
    foreach ($link in @($desktopLink, $menuLink)) {
        Assert-True (Test-Path -LiteralPath $link) "Missing shortcut: $link"
        $shortcut = [EchoInstallerValidation.Shortcuts]::Read($link)
        $target = $shortcut.TargetPath
        $expected = Join-Path $Directory 'echoagent.exe'
        Write-Host "Shortcut (IShellLinkW): $link; target: $target; expected: $expected"
        Assert-True ($target -eq $expected) "Stale shortcut during ${script:phase}: $link; expected '$expected', actual '$target'"
    }
}

try {
    # Missing targets must be accepted without precreating them: the path
    # guard resolves their existing ancestors before the installer copies files.
    Assert-True (-not (Test-Path -LiteralPath $defaultDir)) 'First install requires a missing default directory'
    Assert-True (-not (Test-Path -LiteralPath $customRoot)) 'Custom install requires missing parent directories'
    New-Item -ItemType Directory -Path $scratch | Out-Null
    Invoke-Installer '/S'
    Assert-Install $defaultDir

    # A deliberately chosen, durable location with Chinese and spaces remains
    # authoritative on subsequent updates. /ARGS must not swallow /D=.
    $phase = 'move to custom directory'
    Invoke-Installer "/S /UPDATE /ARGS --acceptance /D=$customDir"
    Assert-Install $customDir
    $phase = 'update custom directory'
    Invoke-Installer '/S /UPDATE'
    Assert-Install $customDir

    # A durable custom location remains authoritative after its entire payload
    # (including the uninstaller) is lost. Rebuild it without changing the path.
    Remove-Item -LiteralPath $customRoot -Recurse -Force
    Assert-True (-not (Test-Path -LiteralPath $customRoot)) 'Repair requires missing parent directories'
    $phase = 'repair missing custom payload'
    Invoke-Installer '/S /UPDATE'
    Assert-Install $customDir

    # First repair a legacy Temp install whose uninstaller still exists; it
    # must never be launched. Then repeat after cleanup removed the files.
    $legacy = Join-Path $scratch 'EchoAgentInstallTest'
    New-Item -ItemType Directory -Path $legacy | Out-Null
    Set-Content -LiteralPath (Join-Path $legacy 'cleanup-marker.txt') -Value 'disposable'
    Copy-Item -LiteralPath (Join-Path $defaultDir 'uninstall.exe') -Destination $legacy
    $dataMarkers = @()
    $dataRoots = @(
        (Join-Path ([Environment]::GetFolderPath('UserProfile')) '.echo-agent'),
        (Join-Path $env:APPDATA 'com.echoagent.desktop')
    )
    foreach ($dataDir in $dataRoots) {
        New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
        $marker = Join-Path $dataDir ('installer-acceptance-' + [guid]::NewGuid() + '.txt')
        Set-Content -LiteralPath $marker -Value 'keep tasks and settings'
        $dataMarkers += $marker
    }
    $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if (-not (Test-Path -LiteralPath $runKey)) { New-Item -Path $runKey | Out-Null }
    $approvedKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
    if (-not (Test-Path -LiteralPath $approvedKey)) { New-Item -Path $approvedKey -Force | Out-Null }
    $disabledState = [byte[]]@(3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    New-ItemProperty -LiteralPath $approvedKey -Name 'echoagent-acceptance-home' -PropertyType Binary -Value $disabledState -Force | Out-Null
    $unrelatedCommand = '"' + (Join-Path $legacy 'other.exe') + '" --keep'
    Set-ItemProperty -LiteralPath $runKey -Name 'echo-installer-unrelated' -Value $unrelatedCommand
    foreach ($missingUninstaller in @($false, $true)) {
        if ($missingUninstaller) { Remove-Item -LiteralPath (Join-Path $legacy 'uninstall.exe') }
        Set-Item -LiteralPath $productKey -Value $legacy
        Set-ItemProperty -LiteralPath $uninstallKey -Name InstallLocation -Value ('"' + $legacy + '"')
        Set-ItemProperty -LiteralPath $uninstallKey -Name UninstallString -Value ('"' + (Join-Path $legacy 'uninstall.exe') + '"')
        $startupArgs = ' "--background" "--data-home" "D:\用户工作 空间"'
        Set-ItemProperty -LiteralPath $runKey -Name 'echoagent-acceptance-home' -Value ('"' + (Join-Path $legacy 'echoagent.exe') + '"' + $startupArgs)
        foreach ($link in @($desktopLink, $menuLink)) {
            [EchoInstallerValidation.Shortcuts]::Write($link, (Join-Path $legacy 'echoagent.exe'), $legacy, '')
        }
        $mode = if ($missingUninstaller) { '/P /UPDATE' } else { '/S /UPDATE' }
        $phase = "repair legacy Temp installation (missing uninstaller: $missingUninstaller)"
        Invoke-Installer $mode
        Assert-Install $defaultDir
        Assert-True ((Get-Item -LiteralPath $runKey).GetValue('echoagent-acceptance-home') -eq ('"' + (Join-Path $defaultDir 'echoagent.exe') + '"' + $startupArgs)) 'Startup target or arguments were not repaired'
        Assert-True ([Convert]::ToBase64String((Get-Item -LiteralPath $approvedKey).GetValue('echoagent-acceptance-home')) -eq [Convert]::ToBase64String($disabledState)) 'Repair changed the disabled startup choice'
        Assert-True ((Get-Item -LiteralPath $runKey).GetValue('echo-installer-unrelated') -eq $unrelatedCommand) 'Repair changed an unrelated startup command'
        foreach ($dataMarker in $dataMarkers) {
            Assert-True ((Get-Content -LiteralPath $dataMarker -Raw).Trim() -eq 'keep tasks and settings') 'Repair removed user data'
        }
        if (-not $missingUninstaller) {
            Assert-True (Test-Path -LiteralPath (Join-Path $legacy 'uninstall.exe')) 'Repair ran an untrusted legacy uninstaller'
        }
    }

    $unsafeDir = Join-Path $scratch 'RejectedInstall'
    $phase = 'reject temporary directory'
    Invoke-Installer "/S /D=$unsafeDir" $false
    Assert-True (-not (Test-Path -LiteralPath $unsafeDir)) 'Rejected install created a temporary application folder'
    Assert-Install $defaultDir

    # A permanent-looking junction must not bypass the Temp guard.
    $junction = Join-Path $env:LOCALAPPDATA ('EchoInstallJunction-' + [guid]::NewGuid())
    New-Item -ItemType Junction -Path $junction -Target $scratch | Out-Null
    try {
        $phase = 'reject junction into temporary directory'
        Invoke-Installer "/S /D=$junction\RejectedAlias" $false
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $scratch 'RejectedAlias'))) 'Junction bypassed the temporary path guard'
    } finally { [System.IO.Directory]::Delete($junction) }

    # Remove only this acceptance's cache, then launch the repaired installation.
    Remove-Item -LiteralPath $scratch -Recurse -Force
    Assert-Install $defaultDir
    $phase = 'launch repaired desktop'
    node (Join-Path $PSScriptRoot 'prepare-meeting-speech.mjs') --installed-app (Join-Path $defaultDir 'echoagent.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Installed meeting speech verification failed.' }
    node (Join-Path $PSScriptRoot 'validate-packaged-desktop.mjs') (Join-Path $defaultDir 'echoagent.exe')
    if ($LASTEXITCODE -ne 0) { throw 'Repaired desktop WebView/IPC/IDE validation failed' }
    Write-Host "Windows installation acceptance passed ($assertions assertions)."
} catch {
    $message = "Phase: $phase; assertions passed: $assertions; $($_.Exception.Message)"
    $escaped = $message.Replace('%', '%25').Replace("`r", '%0D').Replace("`n", '%0A')
    Write-Host "::error title=Windows installation acceptance failed::$escaped"
    if ($env:ECHO_DESKTOP_VALIDATION_OUTPUT) {
        foreach ($link in @($desktopLink, $menuLink)) {
            if (Test-Path -LiteralPath $link -PathType Leaf) {
                Copy-Item -LiteralPath $link -Destination (Join-Path $env:ECHO_DESKTOP_VALIDATION_OUTPUT ((Split-Path -Leaf (Split-Path -Parent $link)) + '.lnk'))
            }
        }
    }
    throw
} finally {
    try {
        if (Test-Path -LiteralPath $scratch) { Remove-Item -LiteralPath $scratch -Recurse -Force }
        # Keep installed files/registration on the disposable runner for diagnostics.
    } finally {
        if ($transcribing) { Stop-Transcript | Out-Null }
    }
}
