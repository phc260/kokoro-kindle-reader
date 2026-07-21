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
!define VERSION "0.3.2"
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

VIProductVersion "0.3.2.0"
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
  ; overwrite them (a fresh install just no-ops these). Pop and discard: nsExec always
  ; pushes a status, and taskkill returns 128 when the process wasn't running - expected,
  ; not an error. Leaving them unpopped just grows the NSIS stack.
  nsExec::ExecToLog 'taskkill /IM kokoro-panel.exe /F'
  Pop $0
  nsExec::ExecToLog 'taskkill /IM kokoro-host.exe /F'
  Pop $0

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
  ; installed, so it never fails the install. It also copies the elevated-executed
  ; artifacts (KokoroSapi.dll + the guard) into an admin-owned, ACL-locked dir under
  ; %ProgramData% and registers THOSE, so nothing runs elevated from user-writable
  ; %LOCALAPPDATA% (local-EoP hardening); see voice-setup.ps1.
  ; Pop the status: voice-setup.ps1 propagates its elevated half's exit code, so a
  ; declined UAC or a failed regsvr32 lands here. Non-fatal on purpose - the app still
  ; installs and the panel still works; only Kindle narration needs the registration -
  ; but say so instead of finishing with a green bar over a broken voice.
  DetailPrint "Registering the Kokoro SAPI voice (may prompt for administrator)..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\voice-setup.ps1" -Action register -ResourcesDir "$INSTDIR\resources"'
  Pop $0
  StrCmp $0 "0" kkr_reg_ok 0
    DetailPrint "Voice registration FAILED ($0). See %TEMP%\kokoro-voice-setup.log."
    MessageBox MB_OK|MB_ICONEXCLAMATION "The Kokoro SAPI voice could not be registered (error $0).$\n$\nThe app is installed, but Kindle will not narrate with Kokoro until it is. Re-run this installer and approve the administrator prompt.$\n$\nDetails: %TEMP%\kokoro-voice-setup.log" /SD IDOK
  kkr_reg_ok:
SectionEnd

Section "Uninstall"
  ; Stop running instances so files unlock and the pipe frees. Pop and discard - see
  ; the install section.
  nsExec::ExecToLog 'taskkill /IM kokoro-panel.exe /F'
  Pop $0
  nsExec::ExecToLog 'taskkill /IM kokoro-host.exe /F'
  Pop $0

  ; Revert Kindle to Microsoft David, then unregister the COM server + token - in
  ; that order, while the DLL + guard still exist. Self-elevates (UAC); also removes
  ; the admin-owned %ProgramData% copy voice-setup.ps1 created on register.
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\voice-setup.ps1" -Action unregister -ResourcesDir "$INSTDIR\resources"'
  ; Worth surfacing here even though we carry on: the file deletions below remove
  ; resources\ and the ProgramData copy, so after this point there is nothing left to
  ; retry the unregistration WITH - a silent failure would strand the SAPI token for good.
  Pop $0
  StrCmp $0 "0" kkr_unreg_ok 0
    DetailPrint "Voice unregistration FAILED ($0). See %TEMP%\kokoro-voice-setup.log."
    MessageBox MB_OK|MB_ICONEXCLAMATION "The Kokoro SAPI voice could not be unregistered (error $0).$\n$\nUninstall will continue, but a stale 'Kokoro (SAPI5)' entry may remain in the Windows voice list.$\n$\nDetails: %TEMP%\kokoro-voice-setup.log" /SD IDOK
  kkr_unreg_ok:

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
