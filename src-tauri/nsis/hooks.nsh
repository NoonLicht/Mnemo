; Mnemo NSIS installer hooks.
;
; The stock Tauri uninstaller removes the install directory and shortcuts but
; knows nothing about the data Mnemo writes at runtime. These hooks make
; uninstalling a full cleanup:
;
;   - the app keeps running in the background when its window is hidden, so it
;     has to be stopped before its files are deleted;
;   - the runtime data lives in %APPDATA%\mnemo (config, index, metadata DB,
;     thumbnails, OCR packs, model packs, agent HTTP token);
;   - WebView2 keeps its own profile per Tauri identifier under
;     %LOCALAPPDATA%\com.user.mnemo;
;   - the auto-launch crate registers HKCU\...\Run with the display name
;     "Mnemo".

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Stopping Mnemo..."
  nsExec::Exec 'taskkill /IM "mnemo.exe" /F'
  Pop $R9
  nsExec::Exec 'taskkill /IM "mnemo-daemon.exe" /F'
  Pop $R9
  nsExec::Exec 'taskkill /IM "mnemo-quick.exe" /F'
  Pop $R9
  Sleep 500
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing Mnemo data..."
  ; Runtime data: config, index, metadata.db, thumbs, tessdata, ocr packs,
  ; model packs, agent HTTP token.
  RMDir /r "$APPDATA\mnemo"
  ; Tauri identifier-scoped directories, used for the WebView2 profile and any
  ; $APPCONFIG-pathed artifacts. Removed whichever of them exist.
  RMDir /r "$APPDATA\com.user.mnemo"
  RMDir /r "$LOCALAPPDATA\com.user.mnemo"
  ; Autostart entry written by the auto-launch crate.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Mnemo"
  DetailPrint "Mnemo data removed."
!macroend
