; Worse Harness per-user installer, modelled on the Antigravity desktop installer:
; installs to %LOCALAPPDATA%\Programs\Worse Harness without admin rights, adds Start menu and
; desktop shortcuts, an Add/Remove Programs entry and an uninstaller. User data in ~/.harness is
; never touched, also not by the uninstaller.
;
; Build (CI, .github/workflows/release.yml):
;   makensis /DVERSION=0.1.13 /DSRC=<dir with Worse-Harness.exe> /DOUTFILE=<setup.exe> installer.nsi
;
; Switches used by the app (apps/harness-slint/src/install):
;   /S       no UI at all (optional)
;   /LAUNCH  progress window only, then start Harness
;   /UPDATE  an update: keep the user's choice about the desktop shortcut

Unicode true
ManifestDPIAware true

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef SRC
  !define SRC "..\..\dist\Worse-Harness-windows-x64"
!endif
!ifndef OUTFILE
  !define OUTFILE "..\..\dist\Worse-Harness-windows-x64-setup.exe"
!endif

!define APP "Worse Harness"
!define EXE "Worse-Harness.exe"
!define UNINSTALLER "Uninstall Worse Harness.exe"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\WorseHarness"
!define APP_PATHS_KEY "Software\Microsoft\Windows\CurrentVersion\App Paths\${EXE}"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
BrandingText "${APP} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "CompanyName" "${APP}"
VIAddVersionKey "LegalCopyright" "${APP} contributors"

!define MUI_ICON "harness.ico"
!define MUI_UNICON "harness.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP}"

!insertmacro MUI_PAGE_INSTFILES
; Started by the app (/LAUNCH): only the progress window, like the Antigravity updater; it closes by itself
; and Harness starts again.
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipFinishWhenLaunched
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Var Launch
Var Update

Function .onInit
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/LAUNCH" $R1
  ${IfNot} ${Errors}
    StrCpy $Launch 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    StrCpy $Update 1
  ${EndIf}
FunctionEnd

Function SkipFinishWhenLaunched
  ${If} $Launch == 1
    Abort
  ${EndIf}
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR"
  ; A running Harness (or one of its helper processes) locks the exe. Windows still allows a
  ; rename, so the old file moves aside and the app deletes it on its next start.
  Delete "$INSTDIR\${EXE}.old"
  ${If} ${FileExists} "$INSTDIR\${EXE}"
    Rename "$INSTDIR\${EXE}" "$INSTDIR\${EXE}.old"
  ${EndIf}
  File "${SRC}\${EXE}"
  File "harness.ico"
  File /nonfatal "${SRC}\LICENSE"
  File /nonfatal "${SRC}\NOTICE"
  WriteUninstaller "$INSTDIR\${UNINSTALLER}"

  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\harness.ico" 0
  ${If} $Update != 1
    CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}" "" "$INSTDIR\harness.ico" 0
  ${EndIf}

  ; Win+R "Worse-Harness" and shell lookups find the installed copy.
  WriteRegStr HKCU "${APP_PATHS_KEY}" "" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${APP_PATHS_KEY}" "Path" "$INSTDIR"

  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\harness.ico"
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\${UNINSTALLER}"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALLER}" /S'
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://github.com/rraahh133/Worse"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"

  ; "Launch at startup" set by a portable copy now starts the installed one.
  ReadRegStr $0 HKCU "${RUN_KEY}" "Harness"
  ${If} $0 != ""
    WriteRegStr HKCU "${RUN_KEY}" "Harness" '"$INSTDIR\${EXE}"'
  ${EndIf}

  ${If} $Launch == 1
    Exec '"$INSTDIR\${EXE}"'
  ${EndIf}
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\${EXE}"
  ${If} ${FileExists} "$INSTDIR\${EXE}"
    MessageBox MB_OK|MB_ICONEXCLAMATION "Close ${APP} and run the uninstaller again." /SD IDOK
    Abort
  ${EndIf}
  Delete "$INSTDIR\${EXE}.old"
  Delete "$INSTDIR\harness.ico"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\NOTICE"
  Delete "$INSTDIR\${UNINSTALLER}"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "${APP_PATHS_KEY}"
  DeleteRegValue HKCU "${RUN_KEY}" "Harness"
  ; ~/.harness (chats, settings, accounts) stays, like Antigravity keeps its data.
SectionEnd
