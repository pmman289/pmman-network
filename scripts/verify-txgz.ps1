param([switch] $Elevated, [switch] $RevokeOnly)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$logName = if ($RevokeOnly) { 'verify-txgz-revoke.log' } else { 'verify-txgz.log' }
$log = Join-Path $root "artifacts\$logName"
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $arguments = @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $PSCommandPath + '"'), '-Elevated'
    )
    if ($RevokeOnly) { $arguments += '-RevokeOnly' }
    $child = Start-Process powershell.exe -Verb RunAs -WindowStyle Hidden -PassThru -ArgumentList $arguments
    Write-Output "verification_pid=$($child.Id)"
    Write-Output "verification_log=$log"
    exit 0
}
if (Test-Path $log) { Copy-Item $log ($log + '.' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.log') }
Start-Transcript -Path $log -Force | Out-Null
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$deployment = Get-Content -Raw (Join-Path $root 'artifacts\txgz-deployment.json') | ConvertFrom-Json
$credentials = Get-Content -Raw (Join-Path $root 'artifacts\txgz-credentials.json') | ConvertFrom-Json
$wg = 'C:\Program Files\WireGuard\wg.exe'
$servicePath = Join-Path $root 'artifacts\standalone\pmman-network-service.exe'
$tunnel = 'pmman-public-verifier'
$env:PMMAN_CONTROL_PLANE = $deployment.master
$env:PMMAN_WG_TUNNEL_NAME = $tunnel

function Ipc([hashtable] $Request) {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', 'pmman-network', [IO.Pipes.PipeDirection]::InOut)
    try {
        $pipe.Connect(10000)
        $writer = [IO.StreamWriter]::new($pipe)
        $writer.AutoFlush = $true
        $reader = [IO.StreamReader]::new($pipe)
        $writer.WriteLine(($Request | ConvertTo-Json -Compress))
        $task = $reader.ReadLineAsync()
        if (!$task.Wait(60000)) { throw 'IPC request timed out' }
        $reply = $task.Result | ConvertFrom-Json
        if (!$reply.ok) { throw $reply.status.message }
        return $reply.status
    } finally { $pipe.Dispose() }
}

function Master([string] $Path, [string] $Method = 'Get', $Body = $null) {
    $parameters = @{ Uri = "$($deployment.master)$Path"; Method = $Method; Headers = $script:headers; TimeoutSec = 15 }
    if ($null -ne $Body) { $parameters.ContentType = 'application/json'; $parameters.Body = $Body | ConvertTo-Json -Compress }
    Invoke-RestMethod @parameters
}

function Invoke-TxgzCommand([string] $Command) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = (Get-Command ssh.exe).Source
    $info.Arguments = '-n -T -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=5 -o ServerAliveCountMax=2 -o HostName=1.14.226.49 -o HostKeyAlias=cn-gz02.pmman.tech txgz "' + $Command.Replace('"', '\"') + '"'
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.RedirectStandardInput = $true
    # All commands here are read-only observations; retry only a stalled SSH
    # child, never an assertion failure or a remote state-changing operation.
    foreach ($attempt in 1..3) {
        $child = [Diagnostics.Process]::Start($info)
        try {
            $child.StandardInput.Close()
            $output = $child.StandardOutput.ReadToEndAsync()
            $errorOutput = $child.StandardError.ReadToEndAsync()
            if (!$child.WaitForExit(20000)) {
                $child.Kill()
                $child.WaitForExit()
                if ($attempt -eq 3) { throw "Remote verification timed out: $Command" }
                Write-Host "remote_observation_retry=$attempt command=$Command"
                continue
            }
            if ($child.ExitCode -ne 0) { throw "Remote verification failed: $Command; $($errorOutput.Result.Trim())" }
            return $output.Result.Trim()
        } finally { $child.Dispose() }
    }
}

function Check-Connection($status, [Int64] $started, [Int64] $remoteStarted) {
    if ($status.state -ne 'connected' -or $status.tunnelMode -ne 'wireguard') { throw 'Expected a real connected tunnel' }
    $node = $deployment.nodes | Where-Object id -eq $status.selectedNodeId
    if (!$node) { throw 'Selected node is not the public test node' }
    $selected = $status.nodes | Where-Object nodeId -eq $node.id
    if (!$selected -or $selected.probeEndpoint -ne $node.probe_endpoint -or
        $selected.wireguardEndpoint -ne $node.wireguard_endpoint -or $null -eq $selected.latencyMs) {
        throw 'Expected a measured UDP RTT and public UDP-only node endpoints from the master'
    }
    $clientKey = (& $wg show $tunnel public-key | Out-String).Trim()
    $devices = @(Master '/v1/admin/devices')
    $device = $devices | Where-Object { $_.status -eq 'active' -and $_.public_key -eq $clientKey }
    if (!$device -or $device.node_id -ne $node.id) { throw 'Master lease does not match Windows key/node' }
    $windowsHandshake = (& $wg show $tunnel latest-handshakes | Out-String).Trim() -split '\s+'
    if ($windowsHandshake[0] -ne $node.public_key -or [Int64]$windowsHandshake[1] -lt $started) { throw 'Missing fresh Windows handshake' }
    $routes4 = Invoke-TxgzCommand "ip -4 route show table $($node.route_table)"
    $routes6 = Invoke-TxgzCommand "ip -6 route show table $($node.route_table)"
    if ($routes4 -notmatch [regex]::Escape($device.ipv4) -or $routes6 -notmatch [regex]::Escape($device.ipv6)) { throw 'Missing dual-stack return routes' }
    foreach ($url in @('http://10.88.0.1:8081/', 'http://[fd88:1000::1]:8081/')) {
        $body = curl.exe --noproxy '*' --fail --silent --max-time 10 $url
        if ($LASTEXITCODE -ne 0 -or $body -notmatch 'PMMan txgz real gateway') { throw "Gateway probe failed: $url" }
    }
    # A WireGuard responder confirms its handshake when authenticated transport
    # traffic arrives, so probe the tunnel before checking the server timestamp.
    $remote = Invoke-TxgzCommand "wg show $($node.interface) latest-handshakes"
    $line = $remote -split "`n" | Where-Object { ($_ -split '\s+')[0] -eq $clientKey }
    if (!$line -or [Int64](($line -split '\s+')[1]) -lt $remoteStarted) {
        throw "Missing fresh remote handshake: peer=$clientKey observed=$line remote_connection_start=$remoteStarted"
    }
    Write-Host "connected_node=$($node.id) udp_rtt_ms=$($selected.latencyMs) mode=$($status.ipv4Mode)/$($status.ipv6Mode) client_public_key=$clientKey windows_handshake=$($windowsHandshake[1]) server_handshake=$(($line -split '\s+')[1]) dual_stack_http=passed"
    return $device
}

function Check-Cleanup {
    Start-Sleep -Seconds 6
    foreach ($node in $deployment.nodes) {
        if (Invoke-TxgzCommand "wg show $($node.interface) peers") { throw 'Remote peer survived disconnect' }
        if (Invoke-TxgzCommand "ip -4 route show table $($node.route_table) 2>/dev/null || true") { throw 'IPv4 return route survived disconnect' }
        if (Invoke-TxgzCommand "ip -6 route show table $($node.route_table) 2>/dev/null || true") { throw 'IPv6 return route survived disconnect' }
    }
    if (Get-Service -Name ('WireGuardTunnel$' + $tunnel) -ErrorAction SilentlyContinue) { throw 'Windows tunnel service survived disconnect' }
    Write-Output 'disconnect_cleanup=passed'
}

$process = $null
try {
    if (Get-Process pmman-network-service -ErrorAction SilentlyContinue) { throw 'Close the existing PMMan client/service before acceptance' }
    $process = Start-Process $servicePath -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 1
    $adminLogin = Invoke-RestMethod -Uri "$($deployment.master)/v1/auth/login" -Method Post -ContentType 'application/json' -Body (@{
        username = $credentials.admin_user; password = $credentials.admin_password
    } | ConvertTo-Json -Compress)
    $script:headers = @{ Authorization = "Bearer $($adminLogin.token)" }
    $status = Ipc @{ action = 'login'; username = $credentials.client_user; password = $credentials.client_password }
    if (@($status.nodes).Count -ne 2) { throw 'Expected two healthy public nodes' }
    $selections = if ($RevokeOnly) { @() } else { @('', 'txgz-a', 'txgz-b') }
    foreach ($selection in $selections) {
        Write-Output "stage=connect selection=$selection"
        $null = Ipc @{ action = 'select_node'; node_id = $(if ($selection) { $selection } else { $null }) }
        $remoteStarted = [Int64](Invoke-TxgzCommand 'date +%s')
        $started = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        $status = Ipc @{ action = 'connect' }
        $device = Check-Connection $status $started $remoteStarted
        if ($selection -and $status.selectedNodeId -ne $selection) { throw 'Manual node selection failed' }
        if (!$selection) {
            $fastest = $status.nodes | Where-Object { $null -ne $_.latencyMs } | Sort-Object latencyMs | Select-Object -First 1
            if ($fastest.nodeId -ne $status.selectedNodeId) { throw 'Automatic selection did not choose lowest latency' }
            Write-Output 'automatic_lowest_latency=passed'
        }
        $null = Ipc @{ action = 'disconnect' }
        Check-Cleanup
    }
    $modes = if ($RevokeOnly) { @() } else { @('split', 'global') }
    foreach ($mode in $modes) {
        Write-Output "stage=connect mode=$mode"
        $null = Ipc @{ action = 'set_traffic_mode'; ipv4_mode = $mode; ipv6_mode = $mode }
        $remoteStarted = [Int64](Invoke-TxgzCommand 'date +%s')
        $started = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        $status = Ipc @{ action = 'connect' }
        $device = Check-Connection $status $started $remoteStarted
        foreach ($dnsServer in @('10.88.0.1', 'fd88:1000::1')) {
            $dns = Resolve-DnsName pmman.tech -Type A -QuickTimeout -Server $dnsServer -DnsOnly -ErrorAction Stop
            if (!$dns) { throw "Gateway DNS resolution failed: $dnsServer" }
        }
        Write-Output 'dual_stack_gateway_dns=passed'
        if ($mode -eq 'global') {
            $external = curl.exe --ipv4 --noproxy '*' --fail --silent --max-time 15 https://www.baidu.com/
            if ($LASTEXITCODE -ne 0) { throw 'Global forwarded IPv4 HTTPS failed' }
            Write-Output 'global_forwarded_https=passed'
        }
        $null = Ipc @{ action = 'disconnect' }
        Check-Cleanup
    }
    Write-Output 'stage=admin_revoke'
    $null = Ipc @{ action = 'set_traffic_mode'; ipv4_mode = 'internal'; ipv6_mode = 'internal' }
    $remoteStarted = [Int64](Invoke-TxgzCommand 'date +%s')
    $started = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $status = Ipc @{ action = 'connect' }
    $device = Check-Connection $status $started $remoteStarted
    $null = Master "/v1/admin/devices/$($device.device_id)/revoke" 'Post'
    $revoked = $false
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        Start-Sleep -Seconds 1
        $status = Ipc @{ action = 'status' }
        if ($status.state -eq 'disconnected') { $revoked = $true; break }
    }
    if (!$revoked) { throw 'Admin revoke did not disconnect the Windows client' }
    Check-Cleanup
    Write-Output 'admin_remote_revoke=passed'
    if (!$RevokeOnly) { Write-Output 'public_wireguard_verification=passed' }
} catch {
    Write-Output "public_wireguard_verification=failed error=$($_.Exception.Message)"
    exit 1
} finally {
    try { $null = Ipc @{ action = 'disconnect' } } catch {}
    if ($process -and !$process.HasExited) { Stop-Process -Id $process.Id -Force }
    Stop-Transcript | Out-Null
}
