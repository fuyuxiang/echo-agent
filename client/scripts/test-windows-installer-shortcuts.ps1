#Requires -Version 5.1
[CmdletBinding()]
param([string]$MakeNsis = "${env:ProgramFiles(x86)}\NSIS\makensis.exe")

$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_OS -ne 'Windows') {
    throw 'Shortcut regression tests require a disposable GitHub Actions Windows runner.'
}
$root = Join-Path $env:LOCALAPPDATA ('EchoShortcutTest-' + [guid]::NewGuid())
$product = Split-Path -Leaf $root
$target = Join-Path $root '新建 工作区\客户端'
$previous = Join-Path $root 'previous'
$links = @(
    (Join-Path ([Environment]::GetFolderPath('Desktop')) "$product.lnk"),
    (Join-Path ([Environment]::GetFolderPath('Programs')) "$product.lnk")
)
$shell = New-Object -ComObject WScript.Shell
$assertions = 0

function Assert-True($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:assertions++
}

function Invoke-Fixture([string]$Flags = '', [int]$ExpectedExit = 0) {
    $process = Start-Process -FilePath (Join-Path $root 'fixture.exe') -ArgumentList "/S $Flags /D=$target" -PassThru
    try {
        if (-not $process.WaitForExit(30000)) { $process.Kill(); throw 'Shortcut fixture timed out' }
        Assert-True ($process.ExitCode -eq $ExpectedExit) "Unexpected fixture exit: $($process.ExitCode), expected $ExpectedExit"
    } finally { $process.Dispose() }
}

function Set-StaleLinks([string]$TargetPath) {
    foreach ($link in $links) {
        $shortcut = $shell.CreateShortcut($link)
        try {
            $shortcut.TargetPath = $TargetPath
            $shortcut.WorkingDirectory = $previous
            $shortcut.Arguments = '--stale-installation'
            $shortcut.Save()
        } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) }
    }
}

try {
    New-Item -ItemType Directory -Path $target, $previous -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot 'System32\where.exe') -Destination (Join-Path $target 'echoagent.exe')
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot 'System32\where.exe') -Destination (Join-Path $previous 'echoagent.exe')
    # Compile the actual production functions with the bundler's upstream COM
    # macros. No app, IDE, model call or release build is needed for this check.
    $template = Get-Content -LiteralPath (Join-Path $PSScriptRoot '../src-tauri/windows/installer.nsi') -Raw
    $functions = foreach ($name in @('CreateOrUpdateStartMenuShortcut', 'CreateOrUpdateDesktopShortcut')) {
        $match = [regex]::Match($template, "(?ms)^Function $name\r?\n.*?^FunctionEnd")
        Assert-True $match.Success "Missing production function: $name"
        $match.Value
    }
    $source = @'
Unicode true
RequestExecutionLevel user
!include LogicLib.nsh
!include FileFunc.nsh
!include "Win\COM.nsh"
!include "Win\Propkey.nsh"
!include "__UTILS__"
!define PRODUCTNAME "__PRODUCT__"
!define MAINBINARYNAME "echoagent"
!define BUNDLEID "com.echoagent.shortcut-fixture"
!define STARTMENUFOLDER ""
Name "__PRODUCT__"
OutFile "__OUTPUT__"
Var EchoRepairShortcuts
Var NoShortcutMode
Var UpdateMode
Var WixMode
Var OldMainBinaryName
Var AppStartMenuFolder
__FUNCTIONS__
Section
  SetShellVarContext current
  SetOutPath $INSTDIR
  StrCpy $AppStartMenuFolder ""
  StrCpy $OldMainBinaryName "echoagent.exe"
  StrCpy $EchoRepairShortcuts 1
  StrCpy $UpdateMode 1
  StrCpy $NoShortcutMode 0
  StrCpy $WixMode 0
  ${GetOptions} $CMDLINE "/NORMAL" $0
  ${IfNot} ${Errors}
    StrCpy $EchoRepairShortcuts 0
  ${EndIf}
  ${GetOptions} $CMDLINE "/NS" $0
  ${IfNot} ${Errors}
    StrCpy $NoShortcutMode 1
  ${EndIf}
  Call CreateOrUpdateStartMenuShortcut
  Call CreateOrUpdateDesktopShortcut
SectionEnd
'@
    $source = $source.Replace('__UTILS__', (Join-Path $PSScriptRoot 'fixtures/windows-shortcut-utils.nsh')).Replace('__PRODUCT__', $product).Replace('__OUTPUT__', (Join-Path $root 'fixture.exe')).Replace('__FUNCTIONS__', ($functions -join "`r`n"))
    $scriptPath = Join-Path $root 'fixture.nsi'
    Set-Content -LiteralPath $scriptPath -Value $source -Encoding UTF8
    & $MakeNsis /V2 $scriptPath
    if ($LASTEXITCODE -ne 0) { throw 'Could not compile the production shortcut fixture' }

    # A matching target must not bypass a repair: old working-directory,
    # arguments and link-tracking metadata must also be discarded.
    foreach ($oldTarget in @((Join-Path $target 'echoagent.exe'), (Join-Path $previous 'echoagent.exe'))) {
        Set-StaleLinks $oldTarget
        Invoke-Fixture
        foreach ($link in $links) {
            $shortcut = $shell.CreateShortcut($link)
            try {
                Assert-True ($shortcut.TargetPath -eq (Join-Path $target 'echoagent.exe')) "Stale target: $link -> $($shortcut.TargetPath)"
                Assert-True ($shortcut.WorkingDirectory -eq $target) "Stale working directory: $link"
                Assert-True ($shortcut.Arguments -eq '') "Stale arguments: $link"
            } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) }
        }
    }
    foreach ($link in $links) { Set-Content -LiteralPath $link -Value 'malformed shortcut' }
    Invoke-Fixture
    foreach ($link in $links) {
        $shortcut = $shell.CreateShortcut($link)
        try { Assert-True ($shortcut.TargetPath -eq (Join-Path $target 'echoagent.exe')) "Malformed shortcut was not repaired: $link" }
        finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) }
    }
    Set-StaleLinks (Join-Path $previous 'echoagent.exe')
    Invoke-Fixture '/NS'
    foreach ($link in $links) {
        $shortcut = $shell.CreateShortcut($link)
        try { Assert-True ($shortcut.TargetPath -eq (Join-Path $previous 'echoagent.exe')) '/NS was ignored' }
        finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shortcut) }
    }
    foreach ($link in $links) { Remove-Item -LiteralPath $link }
    Invoke-Fixture '/NORMAL'
    foreach ($link in $links) { Assert-True (-not (Test-Path -LiteralPath $link)) 'Ordinary update recreated a removed shortcut' }
    # A directory at the link path prevents saving a .lnk. The installer must
    # report failure rather than silently leave a broken installation record.
    foreach ($blockedLink in $links) {
        foreach ($link in $links) { if (Test-Path -LiteralPath $link) { Remove-Item -LiteralPath $link -Recurse -Force } }
        New-Item -ItemType Directory -Path $blockedLink | Out-Null
        try { Invoke-Fixture -ExpectedExit 3 }
        finally { Remove-Item -LiteralPath $blockedLink -Recurse -Force }
    }
    Write-Host "Production Windows shortcut tests passed ($assertions assertions)."
} finally {
    foreach ($link in $links) { if (Test-Path -LiteralPath $link) { Remove-Item -LiteralPath $link -Recurse -Force } }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($shell)
}
