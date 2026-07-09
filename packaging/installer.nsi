; Standalone NSIS installer for the headless (no-WebView2) edition of
; Kokoro Kindle Reader. Bundles the tray host + Slint settings panel + the native
; Dawn WebGPU runtime + the x86 KokoroSapi.dll (Rust, kokoro-sapi), and registers
; the SAPI voice via voice-setup.ps1 (self-elevating). Per-user install, unelevated;
; the registration raises one UAC prompt.
;
; Build via packaging/build-installer.ps1 (stages files into packaging/staging then
; runs makensis). See CLAUDE.md "Packaging / installer".

Unicode true
!include "MUI2.nsh"

!define APPNAME "Kokoro Kindle Reader"
!define COMPANY "phc260"
!define VERSION "0.2.1"
!define STAGING "staging"
!define RUNKEY "Software\Microsoft\Windows\CurrentVersion\Run"
!define RUNVALUE "kokoro-kindle-reader"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\KokoroKindleReader"

Name "${APPNAME}"
OutFile "kokoro-kindle-reader-${VERSION}-setup.exe"
; Fixed install path - the same per-user folder the original app used
; ($LOCALAPPDATA\kokoro-kindle-reader), so this edition installs in place rather
; than a second location. Not overridable (no directory page, no reg override), so
; the path stays consistent across versions/reinstalls.
InstallDir "$LOCALAPPDATA\kokoro-kindle-reader"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "0.2.1.0"
VIAddVersionKey "ProductName" "${APPNAME}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${COMPANY}"
VIAddVersionKey "LegalCopyright" "MIT License"
VIAddVersionKey "FileDescription" "${APPNAME} installer"

!define MUI_ICON "${STAGING}\icon.ico"
!define MUI_UNICON "${STAGING}\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\kokoro-host.exe"
!define MUI_FINISHPAGE_RUN_PARAMETERS "--hidden"
!define MUI_FINISHPAGE_RUN_TEXT "Start Kokoro Kindle Reader (runs in the system tray)"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  ; Upgrade-safe: stop a running instance so its exes/DLLs unlock before we
  ; overwrite them (a fresh install just no-ops these).
  nsExec::ExecToLog 'taskkill /IM kokoro-panel.exe /F'
  nsExec::ExecToLog 'taskkill /IM kokoro-host.exe /F'

  ; Migrate away from the previous parallel location this edition used
  ; ($LOCALAPPDATA\Programs\${APPNAME}); everything now lives in $INSTDIR.
  RMDir /r "$LOCALAPPDATA\Programs\${APPNAME}"

  SetOutPath "$INSTDIR"
  File "${STAGING}\kokoro-host.exe"
  File "${STAGING}\kokoro-panel.exe"
  File "${STAGING}\onnxruntime.dll"
  File "${STAGING}\onnxruntime_providers_shared.dll"
  File "${STAGING}\dxcompiler.dll"
  File "${STAGING}\dxil.dll"
  File "${STAGING}\espeak-ng.dll"
  File "${STAGING}\icon.ico"

  SetOutPath "$INSTDIR\espeak-ng-data"
  File /r "${STAGING}\espeak-ng-data\*.*"

  ; Connect-only x86 SAPI engine + guard scripts (voice-setup.ps1 reads from here),
  ; plus the x86 Kindle-hook DLL + injector the host spawns to force the Kokoro voice.
  SetOutPath "$INSTDIR\resources"
  File "${STAGING}\resources\KokoroSapi.dll"
  File "${STAGING}\resources\kokoro_hook.dll"
  File "${STAGING}\resources\kokoro-inject.exe"
  File "${STAGING}\resources\kindle-voice-guard.ps1"
  File "${STAGING}\resources\voice-setup.ps1"

  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Launch hidden at login - the host must be running for Kindle to narrate. Same
  ; value name the host self-registers via auto-launch, so no double entry.
  WriteRegStr HKCU "${RUNKEY}" "${RUNVALUE}" '"$INSTDIR\kokoro-host.exe" --hidden'

  ; Start Menu shortcuts (Settings opens the panel; the host is tray-only).
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME} Settings.lnk" "$INSTDIR\kokoro-panel.exe" "" "$INSTDIR\icon.ico"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\Uninstall ${APPNAME}.lnk" "$INSTDIR\uninstall.exe"

  ; Add/Remove Programs (per-user).
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName" "${APPNAME}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "${COMPANY}"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayIcon" "$INSTDIR\icon.ico"
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1

  ; Register the x86 SAPI engine + make Kokoro Kindle's default. voice-setup.ps1
  ; self-elevates (one UAC) because regsvr32 -> HKLM/WOW6432Node and the Kindle
  ; guard's reg-load need admin; it self-skips the Kindle step if Kindle isn't
  ; installed, so it never fails the install.
  DetailPrint "Registering the Kokoro SAPI voice (may prompt for administrator)..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\voice-setup.ps1" -Action register -ResourcesDir "$INSTDIR\resources"'
SectionEnd

Section "Uninstall"
  ; Stop running instances so files unlock and the pipe frees.
  nsExec::ExecToLog 'taskkill /IM kokoro-panel.exe /F'
  nsExec::ExecToLog 'taskkill /IM kokoro-host.exe /F'

  ; Revert Kindle to Microsoft David, then unregister the COM server + token - in
  ; that order, while the DLL + guard still exist in resources\. Self-elevates (UAC).
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\voice-setup.ps1" -Action unregister -ResourcesDir "$INSTDIR\resources"'

  DeleteRegValue HKCU "${RUNKEY}" "${RUNVALUE}"
  DeleteRegKey HKCU "${UNINSTKEY}"

  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME} Settings.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\Uninstall ${APPNAME}.lnk"
  RMDir "$SMPROGRAMS\${APPNAME}"

  RMDir /r "$INSTDIR\espeak-ng-data"
  RMDir /r "$INSTDIR\resources"
  Delete "$INSTDIR\kokoro-host.exe"
  Delete "$INSTDIR\kokoro-panel.exe"
  Delete "$INSTDIR\*.dll"
  Delete "$INSTDIR\icon.ico"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  ; Offer to delete the downloaded model + settings. Default keep (/SD IDNO) so a
  ; silent run during an upgrade doesn't force a multi-hundred-MB re-download.
  MessageBox MB_YESNO|MB_ICONQUESTION "Also delete the downloaded voice models and settings?$\n$\nKeep them to let a reinstall skip the model download." /SD IDNO IDNO kkr_keep
    RMDir /r "$APPDATA\com.phc260.kokoro-kindle-reader"
  kkr_keep:
SectionEnd
