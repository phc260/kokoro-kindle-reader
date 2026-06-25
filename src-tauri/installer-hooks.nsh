; NSIS installer hooks for kokoro-kindle-reader.
;
; Registers the bundled x86 SAPI engine (resources\KokoroSapi.dll) so
; "Kokoro (SAPI5)" appears in the Windows voice list and 32-bit hosts like
; Kindle can narrate with it. The engine is connect-only — it forwards each
; Speak to the running app over a named pipe — so registering it just creates
; the COM server + SAPI voice token; no synthesis deps are installed.
;
; Why these specifics (see CLAUDE.md / Dll.cpp):
;   * x86 regsvr32 only. Kindle is 32-bit and loads the DLL in-process by
;     registry path, so it must be registered with C:\Windows\SysWOW64\regsvr32.
;   * Per-user install, but registration self-elevates. installMode is currentUser
;     (tauri.conf.json) so the app installs out of C:\Program Files and the
;     installer runs UNELEVATED. But DllRegisterServer writes HKLM (WOW64-redirected
;     to WOW6432Node) and the Kindle guard does `reg load`, both of which need
;     admin -- so voice-setup.ps1 relaunches itself through UAC and does the
;     privileged register/unregister there.
;   * No shared settings file. Narrator/speed/gain live in the app's webview
;     localStorage and are applied during synthesis, so there's no controls.ini
;     to seed and no writable AssetDir to grant.

!macro NSIS_HOOK_POSTINSTALL
  ; Register the COM server + voice token and make Kokoro the Kindle default.
  ; voice-setup.ps1 self-elevates (one UAC prompt) because the per-user installer
  ; is unelevated but HKLM/reg-load need admin; it self-skips the Kindle step if
  ; the hive is absent (Kindle not installed), so it never fails the install.
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\voice-setup.ps1" -Action register -ResourcesDir "$INSTDIR\resources"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Revert Kindle to Microsoft David, then unregister the COM server + token --
  ; in that order so Kindle's MSIX hive isn't left pointing DefaultTokenId at a
  ; KokoroTTS token that no longer exists. voice-setup.ps1 self-elevates (UAC) and
  ; runs while the DLL + guard still exist in resources\, before files are removed.
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\voice-setup.ps1" -Action unregister -ResourcesDir "$INSTDIR\resources"'

  ; Drop the login autostart entry (lib.rs registers it via tauri-plugin-autostart
  ; under this exact value name). Harmless on an upgrade — the freshly-installed
  ; app re-enables autostart on its first launch.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "kokoro-kindle-reader"

  ; Offer to delete the per-user app data: the downloaded model (Roaming
  ; app_data_dir, ~hundreds of MB) and the WebView2 cache (Local\...\EBWebView).
  ; The default answer is "No (keep)" so a SILENT run preserves the model —
  ; crucially, Tauri's NSIS reuses this uninstaller during an UPGRADE, and we
  ; must not force a multi-hundred-MB re-download on every version bump. An
  ; interactive uninstall still lets the user wipe it. The currentUser uninstaller
  ; already resolves $APPDATA/$LOCALAPPDATA against the installing user's profile,
  ; where the model lives.
  MessageBox MB_YESNO|MB_ICONQUESTION "Also delete the downloaded voice models and cached data?$\n$\nKeep them to let a reinstall skip the model download." /SD IDNO IDNO kkr_keep_appdata
    RMDir /r "$APPDATA\com.phc260.kokoro-kindle-reader"
    RMDir /r "$LOCALAPPDATA\com.phc260.kokoro-kindle-reader"
  kkr_keep_appdata:
!macroend
