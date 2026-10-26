$ErrorActionPreference = 'Stop'

# This is an ephemeral lab: it never changes the default route and cleans up both peers.
$wgExe = 'C:\Program Files\WireGuard\wireguard.exe'
$wgCli = 'C:\Program Files\WireGuard\wg.exe'
$configPath = Join-Path $env:TEMP "pmman-wg-demo-$PID.conf"
$serverDir = "/tmp/pmman-wg-demo-$PID"
$tunnelName = [IO.Path]::GetFileNameWithoutExtension($configPath)
$serverStarted = $false
$tunnelInstalled = $false

function Invoke-Wsl([string] $Command) {
    $output = ssh -o BatchMode=yes wsl $Command
    if ($LASTEXITCODE -ne 0) { throw "WSL command failed: $Command`n$output" }
    return ($output -join "`n").Trim()
}

function Write-WslText([string] $Path, [string] $Text) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Text))
    Invoke-Wsl "mkdir -p $serverDir; echo $encoded | base64 -d > $Path; chmod 600 $Path"
}

try {
    if (!(Test-Path $wgExe) -or !(Test-Path $wgCli)) { throw 'WireGuard for Windows is not installed.' }
    $principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run this script from an elevated PowerShell (Run as administrator).' }

    $wslIp = Invoke-Wsl "ip -4 -o addr show eth0 | awk '{print `$4}' | cut -d/ -f1"
    $serverPrivate = Invoke-Wsl 'wg genkey'
    $serverPublic = Invoke-Wsl "echo $serverPrivate | wg pubkey"
    $clientPrivate = Invoke-Wsl 'wg genkey'
    $clientPublic = Invoke-Wsl "echo $clientPrivate | wg pubkey"

    $serverConfig = @"
[Interface]
PrivateKey = $serverPrivate
Address = 10.88.0.1/24, fd88:1000::1/64
ListenPort = 51820

[Peer]
PublicKey = $clientPublic
AllowedIPs = 10.88.0.10/32, fd88:1000::a/128
"@
    Write-WslText "$serverDir/server.conf" $serverConfig

    $clientConfig = @"
[Interface]
PrivateKey = $clientPrivate
Address = 10.88.0.10/32, fd88:1000::a/128

[Peer]
PublicKey = $serverPublic
Endpoint = ${wslIp}:51820
AllowedIPs = 10.88.0.1/32, fd88:1000::1/128
PersistentKeepalive = 5
"@
    Set-Content -LiteralPath $configPath -Value $clientConfig -Encoding ascii

    Invoke-Wsl "wg-quick up $serverDir/server.conf"
    $serverStarted = $true

    & $wgExe /installtunnelservice $configPath
    if ($LASTEXITCODE -ne 0) { throw "WireGuard tunnel installation failed with exit code $LASTEXITCODE." }
    $tunnelInstalled = $true
    Start-Sleep -Seconds 3

    $show = & $wgCli show $tunnelName 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { throw "Windows tunnel is not available:`n$show" }
    Write-Output $show.Trim()

    & ping.exe -n 2 -w 3000 10.88.0.1
    if ($LASTEXITCODE -ne 0) { throw 'IPv4 WireGuard probe failed.' }
    & ping.exe -6 -n 2 -w 3000 fd88:1000::1
    if ($LASTEXITCODE -ne 0) { throw 'IPv6 WireGuard probe failed.' }
    Write-Output 'wireguard_demo=passed'
}
finally {
    if ($tunnelInstalled) { & $wgExe /uninstalltunnelservice $tunnelName | Out-Null }
    if ($serverStarted) { try { Invoke-Wsl "wg-quick down $serverDir/server.conf" | Out-Null } catch {} }
    try { Invoke-Wsl "rm -rf $serverDir" | Out-Null } catch {}
    Remove-Item -LiteralPath $configPath -Force -ErrorAction SilentlyContinue
}
