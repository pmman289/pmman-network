$ErrorActionPreference = 'Stop'

$wireguardCli = Join-Path $env:ProgramFiles 'WireGuard\wg.exe'
$wireguardApp = Join-Path $env:ProgramFiles 'WireGuard\wireguard.exe'
$webviewRoots = @(
    (Join-Path $env:ProgramFiles 'Microsoft\EdgeWebView\Application'),
    (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Microsoft\EdgeWebView\Application'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft\EdgeWebView\Application')
)

if (!(Test-Path $wireguardCli)) { throw "WireGuard CLI missing: $wireguardCli" }
if (!(Test-Path $wireguardApp)) { throw "WireGuard application missing: $wireguardApp" }
$webview = $webviewRoots | Where-Object { Test-Path (Join-Path $_ '*\msedgewebview2.exe') } | Select-Object -First 1
if (!$webview) { throw 'Microsoft Edge WebView2 Runtime is not installed' }

$wgVersion = & $wireguardCli --version 2>&1
Write-Output "wireguard_cli=$wireguardCli"
Write-Output "wireguard_version=$wgVersion"
Write-Output "wireguard_app=$wireguardApp"
Write-Output "webview2_root=$webview"
Write-Output 'windows_prerequisites=passed'
