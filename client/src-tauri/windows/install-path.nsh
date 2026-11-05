; EchoAgent installation policy. SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
; All checks run before maintenance/uninstall pages and again before copying.
Var EchoExplicitDirectory
Var EchoRepairLegacy
Var EchoRepairShortcuts
Var EchoRepairHint
Var EchoCandidate
Var EchoRoot
Var EchoUnsafe
Var EchoPreviousInstallDir
Var EchoPreviousBinaryName

!macro EchoInstallPathMessages
  LangString echoChooseDirectory 1033 "Choose a permanent installation folder. Temporary folders can be removed by cleanup."
  LangString echoChooseDirectory 2052 "请选择长期保留的安装位置。临时目录可能被系统清理，不能用于安装客户端。"
  LangString echoRepairHint 1033 "The previous installation record was invalid. This installation will repair the location and shortcuts; your tasks and settings will be kept."
  LangString echoRepairHint 2052 "检测到旧版本的安装位置异常，本次会修复安装位置和快捷方式，并保留任务与设置。"
  LangString echoInvalidDirectory 1033 "This folder cannot safely retain the application. Choose a permanent folder outside temporary and cache directories."
  LangString echoInvalidDirectory 2052 "此位置无法可靠保留客户端。请选择临时目录、缓存目录以外的安装文件夹。"
!macroend

!macro EchoInstallPathFunctions
; Resolve existing ancestors through junctions and 8.3 aliases without creating
; any directories. The missing leaf is appended to its resolved parent.
Function EchoCanonicalPath
  Exch $0
  Push $1
  Push $2
  Push $3
  Push $4
  StrCpy $2 ""
  ClearErrors
  GetFullPathName $0 "$0"
  IfErrors echo_canonical_error
  echo_canonical_open:
    System::Call 'kernel32::CreateFileW(w r0, i 0, i 7, p 0, i 3, i 0x02000000, p 0) p.r1'
    StrCmp $1 -1 0 echo_canonical_found
    ${GetParent} "$0" $3
    ${GetFileName} "$0" $4
    StrCmp $3 "" echo_canonical_error
    StrLen $1 $3
    ${If} $1 = 2
      StrCpy $1 $3 1 1
      ${If} $1 == ":"
        StrCpy $3 "$3\"
      ${EndIf}
    ${EndIf}
    StrCmp $3 $0 echo_canonical_error
    StrCpy $2 "\$4$2"
    StrCpy $0 $3
    Goto echo_canonical_open
  echo_canonical_found:
    System::Call 'kernel32::GetFinalPathNameByHandleW(p r1, w.r3, i ${NSIS_MAX_STRLEN}, i 0) i.r4'
    System::Call 'kernel32::CloseHandle(p r1)'
    IntCmp $4 0 echo_canonical_error
    IntCmp $4 ${NSIS_MAX_STRLEN} echo_canonical_error 0 echo_canonical_error
    StrCpy $4 $3 8
    StrCmp $4 "\\?\UNC\" 0 +3
      StrCpy $3 $3 "" 8
      StrCpy $3 "\\$3"
    StrCpy $4 $3 4
    StrCmp $4 "\\?\" 0 +2
      StrCpy $3 $3 "" 4
    StrCpy $0 "$3$2"
    StrCpy $4 $0 1 -1
    StrCmp $4 "\" 0 echo_canonical_done
      StrCpy $0 $0 -1
    Goto echo_canonical_done
  echo_canonical_error:
    StrCpy $0 ""
  echo_canonical_done:
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Exch $0
FunctionEnd

Function EchoCheckRoot
  Push $0
  Push $1
  StrCmp $EchoRoot "" echo_root_done
  ExpandEnvStrings $EchoRoot "$EchoRoot"
  Push $EchoRoot
  Call EchoCanonicalPath
  Pop $EchoRoot
  StrCmp $EchoRoot "" echo_root_done
  StrLen $0 $EchoRoot
  StrCpy $1 $EchoCandidate $0
  StrCmp $1 $EchoRoot 0 echo_root_done
  StrCpy $1 $EchoCandidate 1 $0
  StrCmp $1 "" echo_root_unsafe
  StrCmp $1 "\" 0 echo_root_done
  echo_root_unsafe:
    StrCpy $EchoUnsafe 1
  echo_root_done:
  Pop $1
  Pop $0
FunctionEnd

Function EchoCheckDirectory
  Push $0
  Push $1
  StrCpy $EchoUnsafe 1
  StrCpy $0 $EchoCandidate 4
  StrCmp $0 "\\?\" echo_check_done
  StrCmp $0 "\\.\" echo_check_done
  ; Require an absolute drive or UNC path before canonicalization.
  StrCpy $0 $EchoCandidate 2
  StrCmp $0 "\\" echo_check_absolute
  StrCpy $0 $EchoCandidate 1 1
  StrCmp $0 ":" 0 echo_check_done
  StrCpy $0 $EchoCandidate 1 2
  StrCmp $0 "\" echo_check_absolute
  StrCmp $0 "/" 0 echo_check_done
  echo_check_absolute:
  Push $EchoCandidate
  Call EchoCanonicalPath
  Pop $EchoCandidate
  StrCmp $EchoCandidate "" echo_check_done
  ${GetRoot} "$EchoCandidate" $0
  StrCmp $EchoCandidate $0 echo_check_done
  StrCmp $EchoCandidate "$0\" echo_check_done
  StrCpy $EchoUnsafe 0
  StrCpy $EchoRoot $TEMP
  Call EchoCheckRoot
  ReadEnvStr $EchoRoot "TEMP"
  Call EchoCheckRoot
  ReadEnvStr $EchoRoot "TMP"
  Call EchoCheckRoot
  StrCpy $EchoRoot "$LOCALAPPDATA\Temp"
  Call EchoCheckRoot
  StrCpy $EchoRoot "$WINDIR\Temp"
  Call EchoCheckRoot
  StrCpy $EchoRoot "$LOCALAPPDATA\Microsoft\Windows\INetCache"
  Call EchoCheckRoot
  echo_check_done:
  Pop $1
  Pop $0
FunctionEnd

Function EchoInvalidDirectory
  DetailPrint "$(echoInvalidDirectory) $INSTDIR"
  ${IfNot} ${Silent}
  ${AndIf} $PassiveMode <> 1
    MessageBox MB_OK|MB_ICONEXCLAMATION "$(echoInvalidDirectory)$\r$\n$INSTDIR"
  ${EndIf}
FunctionEnd

Function EchoInitializeInstallPath
  Push $0
  Push $1
  StrCpy $EchoRepairLegacy 0
  StrCpy $EchoRepairShortcuts 0
  StrCpy $EchoRepairHint ""
  ReadRegStr $0 SHCTX "${MANUPRODUCTKEY}" ""
  StrCpy $EchoPreviousInstallDir $0
  ReadRegStr $EchoPreviousBinaryName SHCTX "${UNINSTKEY}" "MainBinaryName"
  ${If} $EchoPreviousBinaryName == ""
    StrCpy $EchoPreviousBinaryName "${MAINBINARYNAME}.exe"
  ${EndIf}
  StrCmp $0 "" echo_check_target
  StrCpy $EchoCandidate $0
  Call EchoCheckDirectory
  ${If} $EchoUnsafe = 1
    StrCpy $EchoRepairLegacy 1
  ${ElseIfNot} ${FileExists} "$0\uninstall.exe"
    ; Cache cleanup may have removed the old uninstaller as well. Never launch
    ; a missing or untrusted temporary uninstaller just to repair registration.
    StrCpy $EchoRepairLegacy 1
  ${EndIf}
  echo_check_target:
  StrCpy $EchoCandidate $INSTDIR
  Call EchoCheckDirectory
  ${If} $EchoUnsafe = 1
    ${If} $EchoExplicitDirectory = 1
      Call EchoInvalidDirectory
      SetErrorLevel 2
      Quit
    ${EndIf}
    StrCpy $EchoRepairLegacy 1
    !if "${INSTALLMODE}" == "perMachine"
      StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
    !else
      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"
    !endif
    StrCpy $EchoCandidate $INSTDIR
    Call EchoCheckDirectory
    ${If} $EchoUnsafe = 1
      Call EchoInvalidDirectory
      SetErrorLevel 2
      Quit
    ${EndIf}
  ${EndIf}
  StrCpy $INSTDIR $EchoCandidate
  ${If} $EchoRepairLegacy = 1
    StrCpy $EchoRepairShortcuts 1
    StrCpy $EchoRepairHint "$\r$\n$\r$\n$(echoRepairHint)"
    DetailPrint "$(echoRepairHint) $INSTDIR"
  ${ElseIf} $0 != ""
    ; /D= supplied by the updater may repair a stale but otherwise safe record.
    StrCpy $1 $INSTDIR
    StrCpy $EchoCandidate $0
    Call EchoCheckDirectory
    ${If} $EchoCandidate != $1
      StrCpy $EchoRepairShortcuts 1
    ${EndIf}
  ${EndIf}
  Pop $1
  Pop $0
FunctionEnd

; Keep existing startup choices and arguments when the executable moves.
; Changing the Run command does not enable a missing item or change the user's
; StartupApproved state. Custom-home items are matched by their exact target.
Function EchoRepairStartupCommands
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  ${If} $EchoRepairShortcuts <> 1
  ${OrIf} $EchoPreviousInstallDir == ""
    Goto echo_startup_done
  ${EndIf}
  StrCpy $0 "$EchoPreviousInstallDir\$EchoPreviousBinaryName"
  StrCpy $1 0
  echo_startup_next:
    ClearErrors
    EnumRegValue $2 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" $1
    IfErrors echo_startup_done
    IntOp $1 $1 + 1
    ReadRegStr $3 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" $2
    StrLen $4 $0
    StrCpy $5 $3 1
    ${If} $5 == '$\"'
      IntOp $4 $4 + 2
      StrCpy $6 '$\"$0$\"'
    ${Else}
      StrCpy $6 $0
    ${EndIf}
    StrCpy $5 $3 $4
    StrCmp $5 $6 0 echo_startup_next
    StrCpy $5 $3 1 $4
    StrCmp $5 "" echo_startup_repair
    StrCmp $5 " " 0 echo_startup_next
    echo_startup_repair:
      StrCpy $3 $3 "" $4
      WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" $2 '$\"$INSTDIR\${MAINBINARYNAME}.exe$\"$3'
      Goto echo_startup_next
  echo_startup_done:
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
FunctionEnd

Function EchoDirectoryLeave
  StrCpy $EchoCandidate $INSTDIR
  Call EchoCheckDirectory
  ${If} $EchoUnsafe = 1
    Call EchoInvalidDirectory
    Abort
  ${EndIf}
  StrCpy $INSTDIR $EchoCandidate
FunctionEnd

Function EchoRequireDurableDirectory
  StrCpy $EchoCandidate $INSTDIR
  Call EchoCheckDirectory
  ${If} $EchoUnsafe = 1
    Call EchoInvalidDirectory
    SetErrorLevel 2
    Quit
  ${EndIf}
  StrCpy $INSTDIR $EchoCandidate
  ; The directory page can change the destination after .onInit. Carry that
  ; choice through to shortcut/startup repair as well as the file copy.
  ${If} $EchoPreviousInstallDir != ""
  ${AndIf} $INSTDIR != $EchoPreviousInstallDir
    StrCpy $EchoRepairShortcuts 1
  ${EndIf}
FunctionEnd
!macroend
