Unicode true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
Name "Quota"
OutFile "..\dist\Quota-1.1.1-windows-x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\TaskbarAiQuota"
RequestExecutionLevel user
SetCompressor /SOLID lzma
VIProductVersion "1.1.1.0"
VIAddVersionKey "ProductName" "Quota"
VIAddVersionKey "FileDescription" "Quota Windows x64 installer"
VIAddVersionKey "FileVersion" "1.1.1"
VIAddVersionKey "LegalCopyright" "Quota contributors"
!define REGKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\TaskbarAiQuota"
!define WEBKEY "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\quota.exe"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Korean"
Function .onInit
  ${IfNot} ${RunningX64}
    Abort "Windows 11 x64 is required"
  ${EndIf}
  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  IntCmp $0 22000 supported unsupported supported
  unsupported:
    Abort "Windows 11 x64 is required"
  supported:
  SetRegView 32
FunctionEnd
!macro StopQuota
  IfFileExists "$INSTDIR\quota.exe" 0 stopped
  ExecWait '"$INSTDIR\quota.exe" --shutdown'
  StrCpy $1 0
  waitForExit:
    ClearErrors
    FileOpen $2 "$INSTDIR\quota.exe" a
    IfErrors busy
    FileClose $2
    Goto stopped
  busy:
    Sleep 200
    IntOp $1 $1 + 1
    IntCmp $1 60 timedout waitForExit timedout
  timedout:
    Abort "Close Quota and try again."
  stopped:
!macroend
Section
  ReadRegStr $0 HKLM "${WEBKEY}" "pv"
  StrCmp $0 "0.0.0.0" +2
  StrCmp $0 "" 0 runtimeFound
  ReadRegStr $0 HKCU "${WEBKEY}" "pv"
  StrCmp $0 "0.0.0.0" +2
  StrCmp $0 "" 0 runtimeFound
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "..\target\installer-tools\MicrosoftEdgeWebview2Setup.exe"
  ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" /silent /install' $0
  ${If} $0 != 0
     Abort "WebView2 installation failed"
  ${EndIf}
  runtimeFound:
  !insertmacro StopQuota
  SetOutPath "$INSTDIR"
  File "..\target\x86_64-pc-windows-msvc\release\quota.exe"
  File "..\ICONS-LICENSE.txt"
  File "TaskbarQuota-LICENSE.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Quota.lnk" "$INSTDIR\quota.exe"
  WriteRegStr HKCU "${REGKEY}" "DisplayName" "Quota"
  WriteRegStr HKCU "${REGKEY}" "DisplayVersion" "1.1.1"
  WriteRegStr HKCU "${REGKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${REGKEY}" "DisplayIcon" "$INSTDIR\quota.exe"
  WriteRegStr HKCU "${REGKEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
SectionEnd
Section "Uninstall"
  !insertmacro StopQuota
  Delete "$SMPROGRAMS\Quota.lnk"
  Delete "$INSTDIR\quota.exe"
  Delete "$INSTDIR\ICONS-LICENSE.txt"
  Delete "$INSTDIR\TaskbarQuota-LICENSE.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${REGKEY}"
SectionEnd
