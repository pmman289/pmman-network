$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$statePath = Join-Path $root 'artifacts\real-lab.json'
$wg = 'C:\Program Files\WireGuard\wg.exe'

if (!(Test-Path $statePath)) { throw 'Real lab is not running.' }
if (!(Test-Path $wg)) { throw "WireGuard CLI not found: $wg" }
$state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json
$routeTable = if ($state.RouteTable) { [string]$state.RouteTable } else { '51820' }
$adminUser = if ($env:PMMAN_ADMIN_USER) { $env:PMMAN_ADMIN_USER } else { 'admin' }
if ([string]::IsNullOrWhiteSpace($env:PMMAN_ADMIN_PASSWORD)) {
    throw 'PMMAN_ADMIN_PASSWORD must be set; verification refuses to use a default password.'
}
$adminPassword = $env:PMMAN_ADMIN_PASSWORD
$loginBody = @{ username = $adminUser; password = $adminPassword } | ConvertTo-Json -Compress
$login = Invoke-RestMethod -Method Post -Uri 'http://127.0.0.1:8080/v1/auth/login' -ContentType 'application/json' -Body $loginBody
$headers = @{ Authorization = "Bearer $($login.token)" }
$deviceResponse = Invoke-RestMethod -Headers $headers -Uri 'http://127.0.0.1:8080/v1/admin/devices'
$devices = @($deviceResponse)
if ($devices.Count -eq 1 -and $devices[0].PSObject.Properties.Name -contains 'value') {
    $devices = @($devices[0].value)
}
$active = @($devices | Where-Object status -eq 'active')
if ($active.Count -ne 1) {
    throw "Expected exactly one active development device; found $($active.Count). Revoke stale devices from http://127.0.0.1:8080/admin."
}
$device = $active[0]

function Find-WireGuardPeer([string] $dump, [string] $publicKey) {
    foreach ($line in ($dump -split "`r?`n")) {
        $fields = $line -split "\s+"
        if ($fields.Count -ge 8 -and $fields[0] -eq $publicKey) {
            return $fields
        }
    }
    return $null
}

Write-Output '--- control plane device ---'
$device | Select-Object device_id,user_id,public_key,ipv4,ipv6,status,config_version,last_seen_at | Format-List

Write-Output '--- Windows WireGuard peer ---'
$windowsPublicKey = (& $wg show pmman-client public-key 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($windowsPublicKey)) {
    throw "Unable to read the Windows tunnel public key: $windowsPublicKey"
}
if ($windowsPublicKey -ne $device.public_key) {
    throw 'Windows public key does not match the active control-plane device.'
}
$windowsDump = (& $wg show pmman-client dump 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw "Unable to read the Windows WireGuard dump: $windowsDump" }
$windowsPeer = Find-WireGuardPeer $windowsDump $state.ServerPublicKey
if ($null -eq $windowsPeer) { throw 'Windows tunnel does not contain the current gateway peer.' }
$windowsHandshake = [UInt64]$windowsPeer[4]
$windowsRx = [UInt64]$windowsPeer[5]
$windowsTx = [UInt64]$windowsPeer[6]
if ($windowsHandshake -eq 0) { throw 'Windows WireGuard peer has no handshake.' }
if (($windowsRx + $windowsTx) -eq 0) { throw 'Windows WireGuard peer has no encrypted traffic.' }
Write-Output "client_public_key=$windowsPublicKey"
Write-Output "gateway_public_key=$($state.ServerPublicKey)"
Write-Output "latest_handshake_epoch=$windowsHandshake"
Write-Output "transfer_rx_bytes=$windowsRx"
Write-Output "transfer_tx_bytes=$windowsTx"

Write-Output '--- WSL gateway peer ---'
$wslDump = (ssh -o BatchMode=yes wsl 'sudo wg show pmman0 dump' | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Unable to read the WSL WireGuard dump.' }
$wslPeer = Find-WireGuardPeer $wslDump $windowsPublicKey
if ($null -eq $wslPeer) { throw 'WSL gateway does not contain the same client public key.' }
$wslHandshake = [UInt64]$wslPeer[4]
if ($wslHandshake -eq 0) { throw 'WSL gateway peer has no handshake.' }
Write-Output "wsl_client_public_key=$($wslPeer[0])"
Write-Output "wsl_latest_handshake_epoch=$wslHandshake"
Write-Output "wsl_transfer_rx_bytes=$($wslPeer[5])"
Write-Output "wsl_transfer_tx_bytes=$($wslPeer[6])"

Write-Output '--- node return routes ---'
$ipv4Routes = (ssh -o BatchMode=yes wsl "sudo ip -4 route show table $routeTable" | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw "Unable to read IPv4 return routes from table $routeTable." }
if ($ipv4Routes -notmatch "(?m)^$([regex]::Escape($device.ipv4))\s+.*\bdev\s+pmman0\b") {
    throw "IPv4 client route is missing from table ${routeTable}: $ipv4Routes"
}
$ipv6Routes = (ssh -o BatchMode=yes wsl "sudo ip -6 route show table $routeTable" | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw "Unable to read IPv6 return routes from table $routeTable." }
if ($ipv6Routes -notmatch "(?m)^$([regex]::Escape($device.ipv6))\s+.*\bdev\s+pmman0\b") {
    throw "IPv6 client route is missing from table ${routeTable}: $ipv6Routes"
}
Write-Output "route_table=$routeTable"
Write-Output "ipv4_return_route=$($device.ipv4) dev pmman0"
Write-Output "ipv6_return_route=$($device.ipv6) dev pmman0"

Write-Output '--- IPv4 TCP gateway probe (required) ---'
$gatewayHttp = if ($state.GatewayHttp) { $state.GatewayHttp } else { 'http://10.88.0.1:8081/' }
$response = Invoke-WebRequest -Uri $gatewayHttp -UseBasicParsing -TimeoutSec 5
if ($response.StatusCode -ne 200) { throw "Gateway TCP probe returned HTTP $($response.StatusCode)." }
Write-Output "gateway_tcp_http=$($response.StatusCode)"

Write-Output '--- ICMP diagnostics (non-blocking) ---'
& ping.exe -n 2 10.88.0.1
if ($LASTEXITCODE -ne 0) { Write-Warning 'IPv4 ICMP is blocked or unavailable; WireGuard handshake/TCP traffic already passed.' }
& ping.exe -6 -n 2 fd88:1000::1
if ($LASTEXITCODE -ne 0) { Write-Warning 'IPv6 ICMP is blocked by the WSL/Windows firewall; this is diagnostic only.' }

# Native ping may leave a non-zero process exit code even though it is
# intentionally diagnostic. Return success after the required checks pass.
$global:LASTEXITCODE = 0
Write-Output 'real_wireguard_verification=passed'
