$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$clientDir = Join-Path $root 'artifacts\standalone'
$servicePath = Join-Path $clientDir 'pmman-network-service.exe'
$wg = 'C:\Program Files\WireGuard\wg.exe'
$pipeName = 'pmman-network'
$tunnelName = 'pmman-modes-verifier'
$env:PMMAN_WG_TUNNEL_NAME = $tunnelName

if (!(Test-Path $servicePath)) { throw "Missing standalone service: $servicePath" }
if (!(Test-Path $wg)) { throw "WireGuard CLI not found: $wg" }
if ([string]::IsNullOrWhiteSpace($env:PMMAN_TEST_USERNAME) -or [string]::IsNullOrWhiteSpace($env:PMMAN_TEST_PASSWORD)) {
    throw 'PMMAN_TEST_USERNAME and PMMAN_TEST_PASSWORD must be set; verification refuses to use demo credentials.'
}

function Send-Ipc([hashtable] $Request) {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream(
        '.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::None
    )
    try {
        $pipe.Connect(15000)
        $reader = New-Object System.IO.StreamReader($pipe)
        $writer = New-Object System.IO.StreamWriter($pipe)
        $writer.AutoFlush = $true
        $writer.WriteLine(($Request | ConvertTo-Json -Compress))
        $line = $reader.ReadLine()
        if ([string]::IsNullOrWhiteSpace($line)) { throw '后台服务返回空响应' }
        $response = $line | ConvertFrom-Json
        if (!$response.ok) { throw [string]$response.status.message }
        return $response.status
    } finally {
        if ($pipe) { $pipe.Dispose() }
    }
}

function Get-ServiceProcess {
    Get-Process -Name pmman-network-service -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $servicePath -or !$_.Path } |
        Select-Object -First 1
}

if (!(Get-ServiceProcess)) {
    Start-Process -FilePath $servicePath -Verb RunAs | Out-Null
}

$status = $null
for ($i = 0; $i -lt 30; $i++) {
    try {
        $status = Send-Ipc @{ action = 'status' }
        break
    } catch {
        Start-Sleep -Milliseconds 300
    }
}
if (!$status) { throw '后台服务命名管道未在 9 秒内就绪' }

$status = Send-Ipc @{ action = 'login'; username = $env:PMMAN_TEST_USERNAME; password = $env:PMMAN_TEST_PASSWORD }
if (!$status.authenticated) { throw "登录失败：$($status.message)" }
if (!$status.allowIpv4Global -or !$status.allowIpv6Global -or !$status.allowIpv4Split -or !$status.allowIpv6Split) {
    throw 'Configured verification account does not have all traffic capabilities'
}

function Disconnect-Client {
    try { $null = Send-Ipc @{ action = 'disconnect' } } catch {}
    Start-Sleep -Seconds 2
}

try {
    $null = Send-Ipc @{ action = 'set_traffic_mode'; ipv4_mode = 'global'; ipv6_mode = 'global' }
    $status = Send-Ipc @{ action = 'connect' }
    if ($status.state -ne 'connected') { throw "IPv4/IPv6 global connect failed: $($status.message)" }
    if ($status.ipv4Mode -ne 'global' -or $status.ipv6Mode -ne 'global') { throw 'Global mode was not returned by the service' }
    $globalRoutes = $status.allowedIps -join ', '
    Write-Output "global_allowed_ips=$globalRoutes"
    foreach ($route in @('0.0.0.0/1', '128.0.0.0/1', '::/1', '8000::/1')) {
        if ($globalRoutes -notmatch [regex]::Escape($route)) { throw "Global route missing: $route" }
    }
    Write-Output 'global_modes=passed'
    Disconnect-Client

    $null = Send-Ipc @{ action = 'set_traffic_mode'; ipv4_mode = 'split'; ipv6_mode = 'split' }
    $status = Send-Ipc @{ action = 'connect' }
    if ($status.state -ne 'connected') { throw "IPv4/IPv6 split connect failed: $($status.message)" }
    if ($status.ipv4Mode -ne 'split' -or $status.ipv6Mode -ne 'split') { throw 'Split mode was not returned by the service' }
    $splitRoutes = $status.allowedIps -join ', '
    Write-Output "split_allowed_ips=$splitRoutes"
    foreach ($route in @('1.1.1.0/24', '8.8.8.0/24', '2606:4700:4700::/48', '2001:4860::/32')) {
        if ($splitRoutes -notmatch [regex]::Escape($route)) { throw "Split route missing: $route" }
    }
    $configText = $status.dnsServers -join ', '
    foreach ($dns in @('10.88.0.1', 'fd88:1000::1')) {
        if ($configText -notmatch [regex]::Escape($dns)) { throw "Gateway DNS missing: $dns" }
    }
    Write-Output 'split_modes=passed'
} finally {
    Disconnect-Client
}

Write-Output 'traffic_mode_real_verification=passed'
