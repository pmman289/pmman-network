; Lean update installer: WebView2 and WireGuard are already present because
; this package is only accepted by an existing Pmman installation.
!include LogicLib.nsh
Var LegacyUpdater
!define PMMAN_LEGACY_SCRIPT "${__FILEDIR__}\..\..\scripts\updates\legacy-parent.ps1"

; The updater invokes NSIS with /S. Keep this hook as a second guard so a
; future NSIS template change cannot expose the normal install pages during an
; in-place update. Windows may still show the normal per-machine UAC consent.
!macro NSIS_HOOK_PREINSTALL
  SetSilent silent
  ; Compatibility bridge for clients released before the standalone updater.
  ; Those clients launch this NSIS process from their own pmman-client.exe and
  ; then wait for it synchronously. Detect only that exact legacy parent; the
  ; current updater runs from %TEMP%\Pmman Network\updater and is left alive
  ; for its post-install health check.
  StrCpy $LegacyUpdater 0
  ; Rust canonicalize() produces extended-length paths which NSIS does not
  ; support as an installation destination. Accept legacy /D arguments too.
  StrCpy $0 $INSTDIR 4
  ${If} $0 == "\\?\"
    StrCpy $INSTDIR $INSTDIR "" 4
    SetOutPath $INSTDIR
  ${EndIf}
  InitPluginsDir
  File /oname=$PLUGINSDIR\legacy-parent.ps1 "${PMMAN_LEGACY_SCRIPT}"
  ClearErrors
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\legacy-parent.ps1" -InstallDirectory "$INSTDIR"'
  Pop $0
  Pop $1
  ${If} $0 == 42
    StrCpy $LegacyUpdater 1
    DetailPrint "Legacy updater detected; installer will relaunch the client."
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="Pmman Network P2P" program="$INSTDIR\pmman-network-service.exe"' $0
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="Pmman Network P2P" dir=in action=allow protocol=UDP program="$INSTDIR\pmman-network-service.exe" profile=private,public' $0
  ${If} $0 != 0
    ; A centrally managed firewall policy may reject this optional rule. Do
    ; not abort an otherwise valid application update; the administrator can
    ; add the rule separately when policy permits it.
    DetailPrint "P2P firewall rule was not changed (netsh exit code $0)."
  ${EndIf}
  ${If} $LegacyUpdater == 1
    ; The legacy updater was terminated above, so it cannot perform the
    ; normal health-probe relaunch. Start the newly installed client here.
    Exec '"$INSTDIR\pmman-client.exe"'
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="Pmman Network P2P" program="$INSTDIR\pmman-network-service.exe"' $0
!macroend
