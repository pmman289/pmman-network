; Pmman's Windows runtime bootstrap.
; Tauri copies mapped resources below $INSTDIR before this hook runs.
; WireGuard is an internal runtime: Pmman owns the tunnel lifecycle and does
; not expose the WireGuard GUI as a second user-facing application.
; WebView2's Evergreen Standalone Installer is bundled in full, so a clean
; machine does not need Internet access during installation.

!include LogicLib.nsh

!macro NSIS_HOOK_POSTINSTALL
  StrCpy $R8 0
  DetailPrint "Checking WireGuard for Windows..."
  IfFileExists "$PROGRAMFILES64\WireGuard\wg.exe" wireguard_ready 0
  IfFileExists "$PROGRAMFILES\WireGuard\wg.exe" wireguard_ready 0

  DetailPrint "Installing WireGuard for Windows..."
  ClearErrors
  ; Hide the runtime from Add/Remove Programs. Pmman is the product users
  ; install and remove; the signed WireGuard driver is an implementation
  ; dependency shared by the Pmman tunnel manager.
  ExecWait '"$SYSDIR\msiexec.exe" /i "$INSTDIR\runtime\wireguard-amd64-0.5.3.msi" /qn /norestart ARPSYSTEMCOMPONENT=1' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "WireGuard 安装失败（代码 $0）。请重试安装，或查看系统安装日志。"
    Abort
  ${EndIf}
  StrCpy $R8 1

wireguard_ready:
  ; The MSI may create a public WireGuard shortcut. Remove only those
  ; shortcuts; do not touch an existing WireGuard installation's binaries or
  ; profiles. The Pmman client remains the only user-facing entry point.
  Delete "$COMMONPROGRAMS\WireGuard.lnk"
  Delete "$COMMONPROGRAMS\WireGuard\WireGuard.lnk"
  RMDir "$COMMONPROGRAMS\WireGuard"
  Delete "$SMPROGRAMS\WireGuard.lnk"
  Delete "$DESKTOP\WireGuard.lnk"
  ${If} $R8 == 1
    ; Some WireGuard MSI versions start the manager after installation. Close
    ; only that newly installed manager; Pmman has not created its tunnel yet.
    Sleep 1000
    ExecWait '"$SYSDIR\taskkill.exe" /F /IM wireguard.exe' $0
    Sleep 1000
    ExecWait '"$SYSDIR\taskkill.exe" /F /IM wireguard.exe' $0

    ; ARPSYSTEMCOMPONENT is not honored by every WireGuard MSI build. Remove
    ; only the newly-created WireGuard display entries so Pmman remains the
    ; only product shown to the user. This does not remove the MSI files or
    ; driver and does not touch an existing WireGuard installation.
    SetRegView 64
    StrCpy $R9 0
hide_wireguard_arp_64:
    EnumRegKey $R7 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall" $R9
    StrCmp $R7 "" hide_wireguard_arp_32
    ReadRegStr $R6 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R7" "DisplayName"
    StrCmp $R6 "WireGuard" 0 hide_wireguard_arp_64_next
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R7"
    IntOp $R9 $R9 - 1
hide_wireguard_arp_64_next:
    IntOp $R9 $R9 + 1
    Goto hide_wireguard_arp_64

hide_wireguard_arp_32:
    SetRegView 32
    StrCpy $R9 0
hide_wireguard_arp_32_loop:
    EnumRegKey $R7 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall" $R9
    StrCmp $R7 "" hide_wireguard_arp_done
    ReadRegStr $R6 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R7" "DisplayName"
    StrCmp $R6 "WireGuard" 0 hide_wireguard_arp_32_next
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\$R7"
    IntOp $R9 $R9 - 1
hide_wireguard_arp_32_next:
    IntOp $R9 $R9 + 1
    Goto hide_wireguard_arp_32_loop
hide_wireguard_arp_done:
    SetRegView 64
  ${EndIf}

  DetailPrint "Checking Microsoft Edge WebView2..."
  ; Use the real WebView2 Runtime App GUID and its exact version/location.
  ; Do not launch the Tauri executable from the elevated NSIS process.
  StrCpy $R1 ""
  StrCpy $R2 ""
  SetRegView 64
  ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ReadRegStr $R2 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "location"
  StrCmp $R1 "" webview_check_32
  StrCmp $R2 "" webview_check_32
  IfFileExists "$R2\$R1\msedgewebview2.exe" webview_verified webview_check_32
webview_check_32:
  StrCpy $R1 ""
  StrCpy $R2 ""
  SetRegView 32
  ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ReadRegStr $R2 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "location"
  StrCmp $R1 "" webview_check_user
  StrCmp $R2 "" webview_check_user
  IfFileExists "$R2\$R1\msedgewebview2.exe" webview_verified webview_check_user
webview_check_user:
  StrCpy $R1 ""
  StrCpy $R2 ""
  ReadRegStr $R1 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ReadRegStr $R2 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "location"
  StrCmp $R1 "" webview_install
  StrCmp $R2 "" webview_install
  IfFileExists "$R2\$R1\msedgewebview2.exe" webview_verified webview_install

webview_install:
  DetailPrint "Installing Microsoft Edge WebView2..."
  ClearErrors
  ExecWait '"$INSTDIR\runtime\MicrosoftEdgeWebView2RuntimeInstallerX64.exe" /silent /install' $0
  ; The standalone installer can return a non-zero maintenance/restart code
  ; after it has deployed the Runtime. Do not mistake that code for a missing
  ; runtime here; the client loader check below is authoritative at first run.
  DetailPrint "WebView2 installer exit code: $0"
  Sleep 3000

webview_verified:
  ; Inbound peer UDP is required for LAN/public NAT traversal. Scope the rule
  ; to Pmman's exact service executable; do not change default firewall policy.
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="Pmman Network P2P" program="$INSTDIR\pmman-network-service.exe"' $0
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="Pmman Network P2P" dir=in action=allow protocol=UDP program="$INSTDIR\pmman-network-service.exe" profile=private,public' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "Pmman Network P2P 防火墙规则配置失败，请重新运行安装程序。"
    Abort
  ${EndIf}
  ; The dependency installers are no longer needed after installation. Keep
  ; the app directory small and avoid leaving executable installers behind.
  Delete "$INSTDIR\runtime\wireguard-amd64-0.5.3.msi"
  Delete "$INSTDIR\runtime\MicrosoftEdgeWebView2RuntimeInstallerX64.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="Pmman Network P2P" program="$INSTDIR\pmman-network-service.exe"' $0
  ; Do not remove WireGuard here: other VPN profiles may use the same driver.
  ; The Pmman tunnel itself is removed by the client before uninstalling.
!macroend
