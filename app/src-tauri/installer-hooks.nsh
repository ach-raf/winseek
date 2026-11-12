; winseek NSIS hooks (Tauri bundler). The install is per-machine, so the installer is already
; elevated: the name service is registered here and the app's first-run panel never needs its own
; UAC prompt. searchd.exe ships as a resource next to the app; `--install` / `--uninstall` are the
; same code paths as the app's "Install name service" button:
; protected %ProgramData%\winseek DACL, LocalSystem auto-start service, interactive users may start it.

!macro NSIS_HOOK_PREINSTALL
  ; Upgrade / reinstall: stop and remove the running service first so searchd.exe can be replaced.
  ${If} ${FileExists} "$INSTDIR\searchd.exe"
    DetailPrint "Stopping the winseek name service"
    nsExec::ExecToLog '"$INSTDIR\searchd.exe" --uninstall'
    Pop $0
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Registering the winseek name service"
  nsExec::ExecToLog '"$INSTDIR\searchd.exe" --install'
  Pop $0
  ${If} $0 != 0
    DetailPrint "searchd --install failed ($0); see %ProgramData%\winseek\searchd.log"
    ${IfNot} ${Silent}
      MessageBox MB_ICONEXCLAMATION "The winseek name service could not be registered ($0).$\r$\nwinseek will offer to install it on first run. Details: %ProgramData%\winseek\searchd.log"
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Stop + delete the service. %ProgramData%\winseek (name index) and %LOCALAPPDATA%\winseek
  ; (models, semantic index) are kept.
  ${If} ${FileExists} "$INSTDIR\searchd.exe"
    DetailPrint "Removing the winseek name service"
    nsExec::ExecToLog '"$INSTDIR\searchd.exe" --uninstall'
    Pop $0
  ${EndIf}
  ; "Start with Windows" (written by the app for the installing user)
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "winseek"
!macroend
