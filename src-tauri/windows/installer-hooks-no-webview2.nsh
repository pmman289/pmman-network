; Pmman Network installer without the WebView2 payload.
; This package installs WireGuard as the private tunnel runtime and requires
; Microsoft Edge WebView2 Runtime to already exist on the machine. It never
; downloads or installs WebView2.
!include LogicLib.nsh

!macro NSIS_HOOK_POSTINSTALL
  StrCpy $R8 0
  DetailPrint "Checking WireGuard for Windows..."
  IfFileExists "$PROGRAMFILES64\WireGuard\wg.exe" wireguard_ready 0
  IfFileExists "$PROGRAMFILES\WireGuard\wg.exe" wireguard_ready 0
  DetailPrint "Installing WireGuard for Windows..."
  ClearErrors
  ExecWait '"$SYSDIR\msiexec.exe" /i "$INSTDIR\runtime\wireguard-amd64-0.5.3.msi" /qn /norestart ARPSYSTEMCOMPONENT=1' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "WireGuard 安装失败（代码 $0）。请重试安装，或查看系统安装日志。"
    Abort
  ${EndIf}
  StrCpy $R8 1

wireguard_ready:
  Delete "$COMMONPROGRAMS\WireGuard.lnk"
  Delete "$COMMONPROGRAMS\WireGuard\WireGuard.lnk"
  RMDir "$COMMONPROGRAMS\WireGuard"
  Delete "$SMPROGRAMS\WireGuard.lnk"
  Delete "$DESKTOP\WireGuard.lnk"
  ${If} $R8 == 1
    Sleep 1000
    ExecWait '"$SYSDIR\taskkill.exe" /F /IM wireguard.exe' $0
    Sleep 1000
    ExecWait '"$SYSDIR\taskkill.exe" /F /IM wireguard.exe' $0
    ; Hide only a newly-created WireGuard Add/Remove Programs entry.
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

  DetailPrint "Checking Microsoft Edge WebView2 Runtime..."
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
  StrCmp $R1 "" webview_missing
  StrCmp $R2 "" webview_missing
  IfFileExists "$R2\$R1\msedgewebview2.exe" webview_verified webview_missing
webview_missing:
  MessageBox MB_ICONSTOP|MB_OK "此安装包不包含 WebView2 Runtime。请先安装 Microsoft Edge WebView2 Runtime，再重新运行安装程序。"
  Abort

webview_verified:
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="Pmman Network P2P" program="$INSTDIR\pmman-network-service.exe"' $0
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall add rule name="Pmman Network P2P" dir=in action=allow protocol=UDP program="$INSTDIR\pmman-network-service.exe" profile=private,public' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "Pmman Network P2P 防火墙规则配置失败，请重新运行安装程序。"
    Abort
  ${EndIf}
  Delete "$INSTDIR\runtime\wireguard-amd64-0.5.3.msi"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$SYSDIR\netsh.exe" advfirewall firewall delete rule name="Pmman Network P2P" program="$INSTDIR\pmman-network-service.exe"' $0
!macroend
