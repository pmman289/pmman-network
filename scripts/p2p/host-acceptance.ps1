param(
    [string]$Credentials,
    [string]$Role = 'c',
    [string]$OutDir,
    [string]$StopFile
)

$ErrorActionPreference = 'Stop'
$result = [ordered]@{ passed = $false; role = $Role; host = $true; phase = 'preflight'; checks = @(); error = $null }
$jobs = @(); $loggedInHere = $false; $originalP2p = $true
$log = Join-Path $OutDir 'application.log'
function Log([string]$Message) { Add-Content -LiteralPath $log -Value "$(Get-Date -Format o) $Message" -Encoding utf8 }
function Save-Progress { $result | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $OutDir 'progress.json') -Encoding utf8 }
function Check([bool]$Ok, [string]$Name) { if (!$Ok) { throw "Check failed: $Name" }; $result.checks += $Name; Log "passed=$Name"; Save-Progress }
function Ipc([hashtable]$Request) {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', 'pmman-network', [IO.Pipes.PipeDirection]::InOut)
    try {
        $pipe.Connect(20000); $writer = [IO.StreamWriter]::new($pipe, [Text.UTF8Encoding]::new($false), 1024, $true); $writer.AutoFlush = $true
        $reader = [IO.StreamReader]::new($pipe, [Text.UTF8Encoding]::new($false), $false, 1024, $true)
        $writer.WriteLine(($Request | ConvertTo-Json -Compress)); $read = $reader.ReadLineAsync()
        if (!$read.Wait(65000)) { throw 'IPC timeout' }; $response = $read.Result | ConvertFrom-Json
        if (!$response.ok) { throw [string]$response.status.message }; return $response.status
    } finally { $pipe.Dispose() }
}
function Wait-Peer([string]$Username, [string]$Path = '', [int]$Seconds = 120) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    do { $status = Ipc @{ action = 'status' }; $peer = @($status.p2pPeers | Where-Object username -eq $Username | Select-Object -First 1)
        if ($peer.Count -eq 1 -and (!$Path -or $peer[0].path -eq $Path)) { return $peer[0] }; Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Peer $Username did not reach $Path"
}
function Send-Tcp([string]$Ip, [string]$Text) {
    $address = [Net.IPAddress]::Parse($Ip); $tcp = [Net.Sockets.TcpClient]::new($address.AddressFamily)
    try { $task = $tcp.ConnectAsync($address, 45880); if (!$task.Wait(5000)) { throw "TCP connect timeout: $Ip" }
        $stream = $tcp.GetStream(); $stream.ReadTimeout = 5000; $stream.WriteTimeout = 5000; $bytes = [Text.Encoding]::UTF8.GetBytes($Text); $stream.Write($bytes, 0, $bytes.Length)
        $buffer = New-Object byte[] $bytes.Length; $offset = 0; while ($offset -lt $bytes.Length) { $n = $stream.Read($buffer, $offset, $bytes.Length - $offset); if ($n -eq 0) { throw 'TCP unexpectedly closed' }; $offset += $n }
        if ([Convert]::ToBase64String($buffer) -ne [Convert]::ToBase64String($bytes)) { throw 'TCP data mismatch' }
    } finally { $tcp.Dispose() }
}
function Start-TcpEcho([string]$Ip, [string]$Family) {
    Start-Job -ArgumentList $OutDir, $Ip, $Family -ScriptBlock {
        param($Out, $Address, $AddressFamily); $ErrorActionPreference = 'Stop'; $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Parse($Address), 45880); $listener.Start(); Set-Content (Join-Path $Out "tcp-ready-$AddressFamily") 'ready'
        while ($true) { $tcp = $listener.AcceptTcpClient(); $stream = $tcp.GetStream(); $buffer = New-Object byte[] 65536; $suffix = ''
            try { while (($n = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) { $text = [Text.Encoding]::UTF8.GetString($buffer, 0, $n); $combined = $suffix + $text
                    if ($combined -match 'acceptance-device-a-([0-9a-fA-F-]{36})') { Set-Content (Join-Path $Out 'device-id-a') $Matches[1] }; if ($combined.Contains('pmman-acceptance-done')) { Set-Content (Join-Path $Out 'peer-done') 'done' }
                    $suffix = $combined.Substring([Math]::Max(0, $combined.Length - 128)); $stream.Write($buffer, 0, $n) } } catch {} finally { $tcp.Dispose() }
        }
    }
}
function Start-UdpEcho([string]$Ip, [string]$Family) {
    Start-Job -ArgumentList $OutDir, $Ip, $Family -ScriptBlock {
        param($Out, $Address, $AddressFamily); $ErrorActionPreference = 'Stop'; $familyValue = if ($AddressFamily -eq 'v6') { [Net.Sockets.AddressFamily]::InterNetworkV6 } else { [Net.Sockets.AddressFamily]::InterNetwork }; $udp = [Net.Sockets.UdpClient]::new($familyValue); $udp.Client.Bind([Net.IPEndPoint]::new([Net.IPAddress]::Parse($Address), 45881)); Set-Content (Join-Path $Out "udp-ready-$AddressFamily") 'ready'
        while ($true) { $source = if ($familyValue -eq [Net.Sockets.AddressFamily]::InterNetworkV6) { [Net.IPEndPoint]::new([Net.IPAddress]::IPv6Any, 0) } else { [Net.IPEndPoint]::new([Net.IPAddress]::Any, 0) }; $bytes = $udp.Receive([ref]$source); $null = $udp.Send($bytes, $bytes.Length, $source) }
    }
}
try {
    New-Item -ItemType Directory -Force $OutDir | Out-Null; $settings = Get-Content -Raw -LiteralPath $Credentials | ConvertFrom-Json
    if ($settings.master -ne 'https://network.pmman.tech') { throw 'Host acceptance requires the public HTTPS master' }
    $before = Ipc @{ action = 'status' }; if ($before.authenticated -or $before.state -eq 'connected') { throw 'Host PMMan client already has a user session; refusing to overwrite it' }; $originalP2p = [bool]$before.p2pEnabled
    $login = Ipc @{ action = 'login'; username = $settings.clients.$Role.username; password = $settings.clients.$Role.password }; $loggedInHere = $true; Check ($login.authenticated -and $login.username -eq $settings.clients.$Role.username) 'host-client-login'
    $null = Ipc @{ action = 'set_p2p'; p2p_enabled = $true }; $null = Ipc @{ action = 'select_node'; node_id = $null }; $result.phase = 'connect'; $result.connectionStartedUnix = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(); $connected = Ipc @{ action = 'connect' }
    Check ($connected.state -eq 'connected' -and $connected.tunnelMode -eq 'wireguard') 'host-real-wireguard-connection'; $result.ipv4 = $connected.ipv4; $result.ipv6 = $connected.ipv6; $result.selectedNodeId = $connected.selectedNodeId
    $local4 = $connected.ipv4.Split('/')[0]; $local6 = $connected.ipv6.Split('/')[0]; Check ([Net.IPAddress]::Parse($local4) -is [Net.IPAddress]) 'host-overlay-ipv4-assigned'; Check ([Net.IPAddress]::Parse($local6) -is [Net.IPAddress]) 'host-overlay-ipv6-assigned'
    $result.phase = 'listeners'; $jobs += Start-TcpEcho $local4 'v4'; $jobs += Start-TcpEcho $local6 'v6'; $jobs += Start-UdpEcho $local4 'v4'; $jobs += Start-UdpEcho $local6 'v6'
    $ready = @('tcp-ready-v4','tcp-ready-v6','udp-ready-v4','udp-ready-v6'); $readyDeadline = [DateTime]::UtcNow.AddSeconds(15); do { Start-Sleep -Milliseconds 100 } while ((@($ready | Where-Object { !(Test-Path (Join-Path $OutDir $_)) }).Count) -and [DateTime]::UtcNow -lt $readyDeadline); Check (@($ready | Where-Object { Test-Path (Join-Path $OutDir $_) }).Count -eq 4) 'host-overlay-only-echo-listeners-ready'
    $result.phase = 'p2p'; $deadline = [DateTime]::UtcNow.AddMinutes(15); while ([DateTime]::UtcNow -lt $deadline -and !(Test-Path (Join-Path $OutDir 'peer-done'))) { if (Test-Path -LiteralPath $StopFile) { throw 'Host acceptance stopped before the peer scenario completed' }; Start-Sleep -Milliseconds 250 }
    Check (Test-Path (Join-Path $OutDir 'peer-done')) 'host-received-real-peer-traffic'; $peer = @(Wait-Peer $settings.clients.a.username '')[0]; Check ($peer.path -in @('direct','relay')) 'host-peer-path-established'; if ($peer.path -eq 'relay') { Check ($peer.relayRxBytes -gt 0) 'host-relay-received-real-traffic' } else { Check ($peer.directRxBytes -gt 0) 'host-direct-received-real-traffic' }; $result.peer = $peer; $result.peerPublicKey = $peer.publicKey
    $result.phase = 'reverse'; Send-Tcp $peer.ipv4 'reverse-demand-c'; Send-Tcp $peer.ipv6 'reverse-demand-c'; Check $true 'host-reverse-direction-real-traffic'; $result.reverse = 'sent'; Save-Progress
    while (!(Test-Path -LiteralPath $StopFile) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 250 }; $result.phase = 'disconnect'; $status = Ipc @{ action = 'disconnect' }; Check ($status.state -eq 'disconnected') 'host-client-disconnect'; $result.passed = $true; $result.phase = 'complete'
} catch { $result.error = $_.Exception.Message; Log "failed_phase=$($result.phase);error=$($result.error)" }
finally {
    # Write the failure/success receipt before any best-effort cleanup IPC can
    # block. The orchestrator must be able to distinguish a host test failure
    # from a cleanup timeout.
    $result | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $OutDir 'application-result.json') -Encoding utf8
    foreach ($job in $jobs) { try { Stop-Job $job -ErrorAction SilentlyContinue; Remove-Job $job -Force -ErrorAction SilentlyContinue } catch {} }
    try { $null = Ipc @{ action = 'disconnect' } } catch {}
    if ($loggedInHere) {
        try { $null = Ipc @{ action = 'set_p2p'; p2p_enabled = $originalP2p } } catch {}
        try { $null = Ipc @{ action = 'logout' } } catch {}
    }
}
if (!$result.passed) { exit 1 }
