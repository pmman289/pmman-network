param(
    [int] $UploadMbps = 2,
    [int] $DownloadMbps = 3,
    [int64] $PayloadBytes = 8388608,
    [switch] $Elevated
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$log = Join-Path $root 'artifacts\verify-txgz-rate-limit.log'
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $PSCommandPath + '"'), '-Elevated')
    $arguments += @('-UploadMbps', $UploadMbps, '-DownloadMbps', $DownloadMbps, '-PayloadBytes', $PayloadBytes)
    $child = Start-Process powershell.exe -Verb RunAs -WindowStyle Hidden -PassThru -ArgumentList $arguments
    Write-Output "verification_pid=$($child.Id)"
    Write-Output "verification_log=$log"
    exit 0
}

Start-Transcript -Path $log -Force | Out-Null
$deployment = Get-Content -Raw (Join-Path $root 'artifacts\txgz-deployment.json') | ConvertFrom-Json
$credentials = Get-Content -Raw (Join-Path $root 'artifacts\txgz-credentials.json') | ConvertFrom-Json
$servicePath = Join-Path $root 'artifacts\standalone\pmman-network-service.exe'
$tunnel = 'pmman-rate-verifier'
$port = 18081
$process = $null
$probeStarted = $false
$adminHeaders = $null
$node = $null
$originalLimit = $null

function Ipc([hashtable] $Request) {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', 'pmman-network', [IO.Pipes.PipeDirection]::InOut)
    try {
        $pipe.Connect(10000)
        $writer = [IO.StreamWriter]::new($pipe)
        $writer.AutoFlush = $true
        $reader = [IO.StreamReader]::new($pipe)
        $writer.WriteLine(($Request | ConvertTo-Json -Compress))
        $task = $reader.ReadLineAsync()
        if (!$task.Wait(90000)) { throw 'IPC request timed out' }
        $reply = $task.Result | ConvertFrom-Json
        if (!$reply.ok) { throw $reply.status.message }
        return $reply.status
    } finally { $pipe.Dispose() }
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
    foreach ($attempt in 1..3) {
        $child = [Diagnostics.Process]::Start($info)
        try {
            $child.StandardInput.Close()
            $stdout = $child.StandardOutput.ReadToEndAsync()
            $stderr = $child.StandardError.ReadToEndAsync()
            if (!$child.WaitForExit(20000)) {
                $child.Kill()
                $child.WaitForExit()
                if ($attempt -eq 3) { throw "Remote observation timed out: $Command" }
                continue
            }
            if ($child.ExitCode -ne 0) {
                throw "Remote observation failed: $Command; $($stderr.Result.Trim())"
            }
            return $stdout.Result.Trim()
        } finally { $child.Dispose() }
    }
}

function Set-NodeLimit([int] $Upload, [int] $Download) {
    $body = @{
        route_table = $node.routeTable
        enabled = $true
        rate_limit = @{ uploadMbps = $Upload; downloadMbps = $Download }
    }
    Invoke-RestMethod -TimeoutSec 15 -Uri "$($deployment.master)/v1/admin/nodes/$($node.nodeId)" -Method Post -Headers $adminHeaders -ContentType 'application/json' -Body ($body | ConvertTo-Json -Compress) | Out-Null
}

function Read-HttpHeaders([IO.Stream] $Stream) {
    $bytes = New-Object System.Collections.Generic.List[byte]
    $tail = ''
    while ($bytes.Count -lt 16384) {
        $one = $Stream.ReadByte()
        if ($one -lt 0) { throw 'HTTP headers ended before response was complete' }
        $bytes.Add([byte]$one)
        $tail = ($tail + [char]$one)
        if ($tail.Length -gt 4) { $tail = $tail.Substring($tail.Length - 4) }
        if ($tail -eq "`r`n`r`n") { break }
    }
    [Text.Encoding]::ASCII.GetString($bytes.ToArray())
}

function Measure-Download([string] $TargetHost, [int64] $ExpectedBytes) {
    $client = New-TcpClientFor $TargetHost
    try {
        $client.ReceiveTimeout = 30000
        $client.SendTimeout = 30000
        $client.Connect($TargetHost, $port)
        $stream = $client.GetStream()
        $request = [Text.Encoding]::ASCII.GetBytes("GET /download HTTP/1.1`r`nHost: $TargetHost`r`nConnection: close`r`n`r`n")
        $watch = [Diagnostics.Stopwatch]::StartNew()
        $stream.Write($request, 0, $request.Length)
        $headers = Read-HttpHeaders $stream
        $match = [regex]::Match($headers, '(?im)^Content-Length:\s*(\d+)\s*$')
        if (!$match.Success -or [int64]$match.Groups[1].Value -ne $ExpectedBytes) { throw 'Rate probe returned an unexpected content length' }
        $buffer = New-Object byte[] 65536
        $received = [int64]0
        while ($received -lt $ExpectedBytes) {
            $count = $stream.Read($buffer, 0, [Math]::Min($buffer.Length, $ExpectedBytes - $received))
            if ($count -le 0) { throw 'Rate probe closed during download' }
            $received += $count
        }
        $watch.Stop()
        [pscustomobject]@{ direction = 'download'; host = $TargetHost; bytes = $received; seconds = $watch.Elapsed.TotalSeconds; mbps = ($received * 8.0 / $watch.Elapsed.TotalSeconds / 1000000.0) }
    } finally { $client.Dispose() }
}

function Measure-Upload([string] $TargetHost, [int64] $ExpectedBytes) {
    $client = New-TcpClientFor $TargetHost
    try {
        $client.ReceiveTimeout = 30000
        $client.SendTimeout = 30000
        $client.Connect($TargetHost, $port)
        $stream = $client.GetStream()
        $request = [Text.Encoding]::ASCII.GetBytes("POST /upload HTTP/1.1`r`nHost: $TargetHost`r`nContent-Length: $ExpectedBytes`r`nConnection: close`r`n`r`n")
        $stream.Write($request, 0, $request.Length)
        $buffer = New-Object byte[] 65536
        $sent = [int64]0
        $watch = [Diagnostics.Stopwatch]::StartNew()
        while ($sent -lt $ExpectedBytes) {
            $count = [int][Math]::Min($buffer.Length, $ExpectedBytes - $sent)
            $stream.Write($buffer, 0, $count)
            $sent += $count
        }
        # Completion means the remote has drained the complete request, not
        # merely that Windows accepted the bytes into its TCP send buffer.
        $headers = Read-HttpHeaders $stream
        if ($headers -notmatch '^HTTP/1\.[01] 200 ') { throw 'Upload was not acknowledged by the remote probe' }
        $watch.Stop()
        [pscustomobject]@{ direction = 'upload'; host = $TargetHost; bytes = $sent; seconds = $watch.Elapsed.TotalSeconds; mbps = ($sent * 8.0 / $watch.Elapsed.TotalSeconds / 1000000.0) }
    } finally { $client.Dispose() }
}

function Assert-Limited($Sample, [double] $Limit) {
    $upper = $Limit * 1.35 + 0.25
    if ($Sample.mbps -gt $upper) { throw "$($Sample.direction) exceeded configured limit: $([math]::Round($Sample.mbps, 3)) Mbps > $([math]::Round($upper, 3)) Mbps" }
    if ($Sample.mbps -lt [Math]::Max(0.05, $Limit * 0.15)) { throw "$($Sample.direction) did not transfer enough data to prove the path: $([math]::Round($Sample.mbps, 3)) Mbps" }
}

function New-TcpClientFor([string] $TargetHost) {
    $address = [Net.IPAddress]::Parse($TargetHost)
    if ($address.AddressFamily -eq [Net.Sockets.AddressFamily]::InterNetworkV6) {
        return [Net.Sockets.TcpClient]::new([Net.Sockets.AddressFamily]::InterNetworkV6)
    }
    return [Net.Sockets.TcpClient]::new([Net.Sockets.AddressFamily]::InterNetwork)
}

function Test-TcpConnect([string] $TargetHost, [int] $TargetPort) {
    $client = New-TcpClientFor $TargetHost
    try {
        $client.ReceiveTimeout = 5000
        $client.SendTimeout = 5000
        $client.Connect($TargetHost, $TargetPort)
        return $true
    } finally { $client.Dispose() }
}

try {
    if (!(Test-Path $servicePath)) { throw "Missing service artifact: $servicePath" }
    $adminLogin = Invoke-RestMethod -TimeoutSec 15 -Uri "$($deployment.master)/v1/auth/login" -Method Post -ContentType 'application/json' -Body (@{ username = $credentials.admin_user; password = $credentials.admin_password } | ConvertTo-Json -Compress)
    $adminHeaders = @{ Authorization = "Bearer $($adminLogin.token)" }
    $allNodes = Invoke-RestMethod -TimeoutSec 15 -Uri "$($deployment.master)/v1/admin/nodes" -Headers $adminHeaders
    $nodeIndex = [Array]::IndexOf([string[]]$allNodes.nodeId, 'txgz-a')
    if ($nodeIndex -lt 0) { throw 'txgz-a was not returned by the master' }
    # Windows PowerShell 5.1 can keep a JSON array as one PSObject with
    # array-valued properties. Normalize the selected row explicitly.
    $node = [pscustomobject]@{
        nodeId = $allNodes.nodeId[$nodeIndex]
        healthy = [bool]$allNodes.healthy[$nodeIndex]
        routeTable = $allNodes.routeTable[$nodeIndex]
        rateLimit = $allNodes.rateLimit[$nodeIndex]
    }
    if (!$node -or !$node.healthy) { throw 'txgz-a is not healthy' }
    $originalLimit = $node.rateLimit
    Set-NodeLimit $UploadMbps $DownloadMbps

    $env:PMMAN_CONTROL_PLANE = $deployment.master
    $env:PMMAN_WG_TUNNEL_NAME = $tunnel
    $process = Start-Process $servicePath -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 1
    $status = Ipc @{ action = 'login'; username = $credentials.client_user; password = $credentials.client_password }
    $status = Ipc @{ action = 'select_node'; node_id = 'txgz-a' }
    $status = Ipc @{ action = 'connect' }
    if ($status.state -ne 'connected' -or $status.tunnelMode -ne 'wireguard') { throw "Real txgz-a connection failed: $($status.message)" }
    if ($status.nodeRateLimit.uploadMbps -ne $UploadMbps -or $status.nodeRateLimit.downloadMbps -ne $DownloadMbps) { throw 'Connected snapshot did not carry the configured node limit' }
    $wgEndpoint = (& 'C:\Program Files\WireGuard\wg.exe' show $tunnel endpoints | Out-String).Trim()
    if ($wgEndpoint -notmatch '\s127\.0\.0\.1:\d+$') { throw "WireGuard bypassed the local rate proxy: $wgEndpoint" }
    Write-Output "stage=connected ipv4=$($status.ipv4) ipv6=$($status.ipv6) upload_limit=$($status.nodeRateLimit.uploadMbps) download_limit=$($status.nodeRateLimit.downloadMbps)"

    $null = Invoke-TxgzCommand "rm -f /tmp/pmman-rate-probe.pid; nohup python3 /opt/pmman-network-dev/scripts/txgz/rate_probe.py --port $port --size $PayloadBytes >/tmp/pmman-rate-probe.log 2>&1 </dev/null & echo started"
    $probeStarted = $true
    $ready = $false
    for ($attempt = 0; $attempt -lt 10; $attempt++) {
        try {
            if (Invoke-TxgzCommand 'test -f /tmp/pmman-rate-probe.pid && echo ready') { $ready = $true; break }
        } catch {}
        Start-Sleep -Seconds 1
    }
    if (!$ready) { throw 'Remote rate probe did not become ready' }
    $remoteListener = Invoke-TxgzCommand "ss -lnt '( sport = :$port )' | tail -n +2"
    if (!$remoteListener) { throw "Remote rate probe has no TCP listener on port $port" }
    $route4 = @(Get-NetRoute -DestinationPrefix '10.88.0.1/32' -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty InterfaceAlias)
    $route6 = @(Get-NetRoute -DestinationPrefix 'fd88:1000::1/128' -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty InterfaceAlias)
    Write-Output "stage=probe_ready route4=$($route4 -join ',') route6=$($route6 -join ',') remote_listener=$($remoteListener.Trim())"
    Write-Output "gateway_tcp_8081_ipv4=$(Test-TcpConnect '10.88.0.1' 8081)"

    $download = Measure-Download '10.88.0.1' $PayloadBytes
    $upload = Measure-Upload 'fd88:1000::1' $PayloadBytes
    Assert-Limited $download $DownloadMbps
    Assert-Limited $upload $UploadMbps
    Write-Output "initial_limit_passed upload_mbps=$([Math]::Round($upload.mbps,3)) download_mbps=$([Math]::Round($download.mbps,3))"

    Set-NodeLimit 1 1
    $changed = $false
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        Start-Sleep -Seconds 1
        $status = Ipc @{ action = 'status' }
        if ($status.nodeRateLimit.uploadMbps -eq 1 -and $status.nodeRateLimit.downloadMbps -eq 1) { $changed = $true; break }
    }
    if (!$changed) { throw 'Online heartbeat did not apply the changed node limit' }
    $download2 = Measure-Download '10.88.0.1' $PayloadBytes
    $upload2 = Measure-Upload 'fd88:1000::1' $PayloadBytes
    Assert-Limited $download2 1
    Assert-Limited $upload2 1
    Write-Output "online_update_passed upload_mbps=$([Math]::Round($upload2.mbps,3)) download_mbps=$([Math]::Round($download2.mbps,3))"
    Write-Output 'txgz_rate_limit_verification=passed'
} catch {
    Write-Output "txgz_rate_limit_verification=failed error=$($_.Exception.Message)"
    exit 1
} finally {
    try { $null = Ipc @{ action = 'disconnect' } } catch {}
    if ($probeStarted) { try { $null = Invoke-TxgzCommand 'if test -f /tmp/pmman-rate-probe.pid; then kill "$(cat /tmp/pmman-rate-probe.pid)" 2>/dev/null || true; fi; rm -f /tmp/pmman-rate-probe.pid /tmp/pmman-rate-probe.log' } catch {} }
    if ($node -and $originalLimit) {
        try {
            $body = @{ route_table = $node.routeTable; enabled = $true; rate_limit = @{ uploadMbps = $originalLimit.uploadMbps; downloadMbps = $originalLimit.downloadMbps } }
            Invoke-RestMethod -TimeoutSec 15 -Uri "$($deployment.master)/v1/admin/nodes/$($node.nodeId)" -Method Post -Headers $adminHeaders -ContentType 'application/json' -Body ($body | ConvertTo-Json -Compress) | Out-Null
        } catch {}
    }
    if ($process -and !$process.HasExited) { Stop-Process -Id $process.Id -Force }
    Stop-Transcript | Out-Null
}
