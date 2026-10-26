param([string]$Installer,[string]$Credentials,[string]$Role,[string]$OutDir,[switch]$ThirdMayRelay)
$ErrorActionPreference='Stop'
$result=[ordered]@{passed=$false;role=$Role;phase='install';checks=@();error=$null}
$appRoot=Join-Path $env:ProgramFiles 'Pmman Network'
$log=Join-Path $OutDir 'application.log'
$service=$null;$client=$null;$echoJob=$null;$udpJob=$null
$firewallPrefix='pmman-acceptance-'+[Guid]::NewGuid().ToString('N')
$firewallNames=@("$firewallPrefix-tcp","$firewallPrefix-udp","$firewallPrefix-direct")
function Log([string]$message){Add-Content -LiteralPath $log -Value "$(Get-Date -Format o) $message" -Encoding utf8}
function Ipc([hashtable]$request){
    $pipe=[IO.Pipes.NamedPipeClientStream]::new('.','pmman-network',[IO.Pipes.PipeDirection]::InOut)
    try{$pipe.Connect(20000);$writer=[IO.StreamWriter]::new($pipe,[Text.UTF8Encoding]::new($false),1024,$true);$writer.AutoFlush=$true;$reader=[IO.StreamReader]::new($pipe,[Text.UTF8Encoding]::new($false),$false,1024,$true)
        $writer.WriteLine(($request|ConvertTo-Json -Compress));$read=$reader.ReadLineAsync();if(!$read.Wait(65000)){throw 'IPC timeout'};$response=$read.Result|ConvertFrom-Json;if(!$response.ok){throw $response.status.message};return $response.status
    }finally{$pipe.Dispose()}
}
function Save-Progress{$result|ConvertTo-Json -Depth 12|Set-Content -LiteralPath (Join-Path $OutDir 'progress.json') -Encoding utf8}
function Check([bool]$ok,[string]$name){if(!$ok){throw "Check failed: $name"};$result.checks+= $name;Log "passed=$name";Save-Progress}
function Resolve-UdpEndpoint([string]$endpoint){
    if($endpoint -notmatch '^(?<host>\[[^\]]+\]|[^:]+):(?<port>[0-9]{1,5})$'){throw "Invalid UDP endpoint: $endpoint"}
    $hostName=$Matches.host.Trim('[',']');$port=[int]$Matches.port
    $address=[Net.Dns]::GetHostAddresses($hostName)|Where-Object AddressFamily -eq ([Net.Sockets.AddressFamily]::InterNetwork)|Select-Object -First 1
    if(!$address -or $port -lt 1 -or $port -gt 65535){throw "Invalid IPv4 UDP endpoint: $endpoint"}
    [pscustomobject]@{Host=$hostName;Port=$port;Address=$address}
}
function Wait-Peer([string]$username,[string]$path,[int]$seconds=20){
    $deadline=[DateTime]::UtcNow.AddSeconds($seconds)
    do{$s=Ipc @{action='status'};$p=$s.p2pPeers|Where-Object username -eq $username|Select-Object -First 1;if($p -and (!$path -or $p.path -eq $path)){return $p};Start-Sleep -Milliseconds 200}while([DateTime]::UtcNow -lt $deadline)
    throw "Peer $username did not reach $path"
}
function Own-PeerCandidates($identity){
    # The partner sends only our directory device UUID over the authenticated
    # tunnel. Authenticate as this ordinary user; never expose admin credentials.
    $ownId=[Guid]::Parse((Get-Content -Raw -LiteralPath (Join-Path $OutDir "device-id-$Role")).Trim()).ToString('D')
    if($settings.master -ne 'https://network.pmman.tech'){throw 'Expected the public HTTPS acceptance master'}
    [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12
    $auth=$null;$directory=$null;$headers=@{}
    try{
        $auth=Invoke-RestMethod -Uri "$($settings.master)/v1/auth/login" -Method Post -ContentType 'application/json' -Body (@{username=$me.username;password=$me.password}|ConvertTo-Json -Compress) -TimeoutSec 15
        $headers.Authorization="Bearer $($auth.token)"
        # This API is POST-only. The service restores its own candidate
        # advertisement on its next normal refresh; wait for that before block.
        $directory=Invoke-RestMethod -Uri "$($settings.master)/v1/devices/$ownId/p2p" -Method Post -Headers $headers -ContentType 'application/json' -Body (@{public_key=$result.publicKey;enabled=$true;candidates=@()}|ConvertTo-Json -Compress) -TimeoutSec 15
        if($directory.device_id -ne $ownId){throw 'Own directory identity mismatch'}
        $target=@($directory.peers|Where-Object { $_.device_id -eq $identity.deviceId -and $_.public_key -eq $identity.publicKey })
        if($target.Count -ne 1 -or !$target[0].candidates.Count){throw 'Current target pair candidates unavailable'}
        # Return only public endpoints, never relay_token or pair secrets.
        [pscustomobject]@{candidates=@($target[0].candidates);relay=$directory.relay_endpoint}
    }finally{$headers.Clear();$auth=$null;$directory=$null}
}
function Echo([string]$ip,[string]$payload){
    $tcp=[Net.Sockets.TcpClient]::new();try{$task=$tcp.ConnectAsync($ip,45880);if(!$task.Wait(5000)){throw 'TCP connect timeout'};$stream=$tcp.GetStream();$stream.ReadTimeout=5000;$stream.WriteTimeout=5000;$bytes=[Text.Encoding]::UTF8.GetBytes($payload);$stream.Write($bytes,0,$bytes.Length);$buffer=New-Object byte[] $bytes.Length;$offset=0;while($offset -lt $bytes.Length){$n=$stream.Read($buffer,$offset,$bytes.Length-$offset);if($n -eq 0){throw 'TCP unexpectedly closed'};$offset+=$n};if([Text.Encoding]::UTF8.GetString($buffer) -ne $payload){throw 'TCP data mismatch'}}finally{$tcp.Dispose()}
}
function Udp-Echo([string]$ip){
    $address=[Net.IPAddress]::Parse($ip)
    $udp=[Net.Sockets.UdpClient]::new($address.AddressFamily);$udp.Client.ReceiveTimeout=5000;$udp.Connect($address,45881)
    try{
        for($i=0;$i -lt 200;$i++){
            if($i -eq 50){$null=Ipc @{action='set_p2p';p2p_enabled=$false}}
            if($i -eq 100){$null=Ipc @{action='set_p2p';p2p_enabled=$true}}
            # Fill the configured 1280-byte tunnel MTU, including IP/UDP.
            # Tiny payloads would miss relay encapsulation fragmentation bugs.
            $size=if($address.AddressFamily -eq [Net.Sockets.AddressFamily]::InterNetworkV6){1232}else{1252}
            $text="udp-$Role-$i-"+('data'*400)
            $payload=[Text.Encoding]::UTF8.GetBytes($text.Substring(0,$size));$null=$udp.Send($payload,$payload.Length)
            $any=if($address.AddressFamily -eq [Net.Sockets.AddressFamily]::InterNetworkV6){[Net.IPAddress]::IPv6Any}else{[Net.IPAddress]::Any}
            $source=[Net.IPEndPoint]::new($any,0);$received=$udp.Receive([ref]$source)
            if([Convert]::ToBase64String($received) -ne [Convert]::ToBase64String($payload)){throw "UDP loss/duplicate/corruption at $i"}
            Start-Sleep -Milliseconds 20
        }
    }finally{$udp.Dispose()}
}
function Verify-Handshakes{
    $dump=@(& $wg show pmman-client latest-handshakes)
    Check ($LASTEXITCODE -eq 0) 'wireguard-handshake-query'
    $handshakes=@{};foreach($line in $dump){$fields=$line -split '\s+';if($fields.Count -ge 2){$handshakes[$fields[0]]=[long]$fields[1]}}
    $demanded=@($result.peerIdentities|Where-Object username -in $result.demandedUsers)
    foreach($key in @($result.gatewayPublicKey)+@($demanded|ForEach-Object publicKey)){
        Check ($key -and $handshakes.ContainsKey($key) -and $handshakes[$key] -ge $result.connectionStartedUnix -and $handshakes[$key] -gt 0) "fresh-handshake-$key"
    }
    if($Role -ne 'a'){
        $currentKeys=@((Ipc @{action='status'}).p2pPeers|ForEach-Object publicKey)
        foreach($unvisited in @($result.peerIdentities|Where-Object username -ne $other.username)){
            $stillIdle=if($handshakes.ContainsKey($unvisited.publicKey)){$handshakes[$unvisited.publicKey] -eq 0}else{$unvisited.publicKey -notin $currentKeys}
            Check $stillIdle 'unvisited-pair-has-no-wireguard-handshake'
        }
    }
    $result.handshakes=$dump;Save-Progress
}
try{
    New-Item -ItemType Directory -Force $OutDir|Out-Null
    Log 'installer-start'
    $install=Start-Process -FilePath $Installer -ArgumentList '/S' -PassThru
    if(!$install.WaitForExit(420000)){throw 'Installer timeout'};Check ($install.ExitCode -eq 0) 'bundled-runtime-install'
    $settings=Get-Content -Raw $Credentials|ConvertFrom-Json
    $me=$settings.clients.$Role
    $otherRole=if($Role -eq 'a'){'b'}else{'a'};$other=$settings.clients.$otherRole
    $result.demandedUsers=@($other.username)
    if($Role -eq 'a' -and $settings.clients.c){$result.demandedUsers+= $settings.clients.c.username}
    $client=Start-Process -FilePath (Join-Path $appRoot 'pmman-client.exe') -PassThru
    for($i=0;$i -lt 60;$i++){try{$status=Ipc @{action='status'};break}catch{Start-Sleep -Milliseconds 500}}
    $result.phase='connect'
    $status=Ipc @{action='login';username=$me.username;password=$me.password}
    $status=Ipc @{action='set_p2p';p2p_enabled=$true}
    # Retain independent UDP evidence when node selection fails; no fixture
    # result is allowed to replace the service's actual challenge probe.
    $probe=[Net.Sockets.UdpClient]::new([Net.Sockets.AddressFamily]::InterNetwork);$probe.Client.ReceiveTimeout=2500
    try{
        $selectedNode=$status.nodes|Where-Object nodeId -eq $me.node|Select-Object -First 1
        if(!$selectedNode){throw "Node $($me.node) missing from master snapshot"}
        $probeTarget=Resolve-UdpEndpoint $selectedNode.probeEndpoint
        $result.independentUdpTarget=$selectedNode.probeEndpoint
        $packet=New-Object byte[] 24;[Text.Encoding]::ASCII.GetBytes('PMMPING1').CopyTo($packet,0);[Guid]::NewGuid().ToByteArray().CopyTo($packet,8)
        $target=[Net.IPEndPoint]::new($probeTarget.Address,$probeTarget.Port);$probeStarted=[DateTimeOffset]::UtcNow;$result.independentUdpSentUtc=$probeStarted.ToString('o');$null=$probe.Send($packet,24,$target);$result.independentUdpLocal=$probe.Client.LocalEndPoint.ToString();$source=[Net.IPEndPoint]::new([Net.IPAddress]::Any,0)
        $response=$probe.Receive([ref]$source)
        if(!$source.Equals($target) -or [Convert]::ToBase64String($response) -ne [Convert]::ToBase64String($packet)){throw 'Independent UDP echo did not match this-session challenge and endpoint'}
        $result.independentUdpBytes=$response.Length;$result.independentUdpReceivedUtc=[DateTimeOffset]::UtcNow.ToString('o');$result.independentUdpElapsedMs=([DateTimeOffset]::UtcNow-$probeStarted).TotalMilliseconds
    }catch{$result.independentUdpError=$_.Exception.Message}finally{$probe.Dispose()}
    $result.nodes=$status.nodes
    Get-NetFirewallRule -DisplayName 'Pmman Network P2P' -ErrorAction SilentlyContinue | Get-NetFirewallApplicationFilter | Select-Object Program | ConvertTo-Json | Set-Content (Join-Path $OutDir 'firewall.json')
    $status=Ipc @{action='select_node';node_id=$me.node}
    $result.connectionStartedUnix=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    $status=Ipc @{action='connect'}
    Check ($status.state -eq 'connected' -and $status.tunnelMode -eq 'wireguard') 'real-gateway-wireguard'
    $result.ipv4=$status.ipv4;$result.ipv6=$status.ipv6
    $wg=Join-Path $env:ProgramFiles 'WireGuard\wg.exe'
    $result.publicKey=(& $wg show pmman-client public-key).Trim()
    Check ($LASTEXITCODE -eq 0 -and $result.publicKey.Length -eq 44) 'this-session-client-public-key'
    $localIp=$status.ipv4.Split('/')[0]
    New-NetFirewallRule -Name $firewallNames[0] -DisplayName 'PMMan acceptance TCP echo' -Direction Inbound -Action Allow -Protocol TCP -LocalPort 45880 -Profile Any -Program (Join-Path $PSHOME 'powershell.exe')|Out-Null
    New-NetFirewallRule -Name $firewallNames[1] -DisplayName 'PMMan acceptance UDP echo' -Direction Inbound -Action Allow -Protocol UDP -LocalPort 45881 -Profile Any -Program (Join-Path $PSHOME 'powershell.exe')|Out-Null
    $udpJob=Start-Job -ArgumentList $OutDir -ScriptBlock {
        param($out)
        $udp=[Net.Sockets.UdpClient]::new([Net.Sockets.AddressFamily]::InterNetworkV6);$udp.Client.DualMode=$true;$udp.Client.Bind([Net.IPEndPoint]::new([Net.IPAddress]::IPv6Any,45881))
        Set-Content (Join-Path $out 'udp-ready') 'ready'
        while($true){$source=[Net.IPEndPoint]::new([Net.IPAddress]::IPv6Any,0);$bytes=$udp.Receive([ref]$source);$null=$udp.Send($bytes,$bytes.Length,$source)}
    }
    $echoJob=Start-Job -ArgumentList $OutDir,$Role -ScriptBlock {
        param($out,$localRole)
        $listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::IPv6Any,45880);$listener.Server.DualMode=$true;$listener.Start()
        Set-Content (Join-Path $out 'tcp-ready') 'ready'
        # TCP reads have no message boundaries. Keep a bounded suffix so split
        # scenario control markers are recognized without accumulating payloads.
        while($true){$tcp=$listener.AcceptTcpClient();$stream=$tcp.GetStream();$buffer=New-Object byte[] 65536;$suffix='';try{while(($n=$stream.Read($buffer,0,$buffer.Length)) -gt 0){$text=$suffix+[Text.Encoding]::UTF8.GetString($buffer,0,$n);if($text -match "acceptance-device-$localRole-([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})"){Set-Content (Join-Path $out "device-id-$localRole") $Matches[1]};if($text.Contains('pmman-acceptance-done')){Set-Content (Join-Path $out 'peer-done') 'done'};foreach($role in @('b','c')){if($text.Contains("reverse-demand-$role")){Set-Content (Join-Path $out "peer-reverse-$role") 'done'}};if($text.Contains('p2p-peer-disable')){Set-Content (Join-Path $out 'peer-disable') 'requested'};if($text.Contains('p2p-peer-enable')){Set-Content (Join-Path $out 'peer-enable') 'requested'};$suffix=$text.Substring([Math]::Max(0,$text.Length-64));$stream.Write($buffer,0,$n)}}catch{}finally{$tcp.Dispose()}}
    }
    $readyDeadline=[DateTime]::UtcNow.AddSeconds(15)
    while((!(Test-Path (Join-Path $OutDir 'tcp-ready')) -or !(Test-Path (Join-Path $OutDir 'udp-ready'))) -and [DateTime]::UtcNow -lt $readyDeadline){Start-Sleep -Milliseconds 100}
    Check ((Test-Path (Join-Path $OutDir 'tcp-ready')) -and (Test-Path (Join-Path $OutDir 'udp-ready'))) 'dual-stack-echo-listeners-ready'
    $peer=Wait-Peer $other.username '' 90
    foreach($entry in $settings.clients.PSObject.Properties){if($entry.Name -ne $Role){$null=Wait-Peer $entry.Value.username '' 90}}
    $initialStatus=Ipc @{action='status'}
    $result.peerIdentities=@($initialStatus.p2pPeers|Select-Object deviceId,username,publicKey,ipv4,ipv6,path)
    $peerKeys=@($initialStatus.p2pPeers|ForEach-Object publicKey)
    $gatewayKeys=@(& $wg show pmman-client peers|Where-Object {$_ -notin $peerKeys})
    Check ($LASTEXITCODE -eq 0 -and $gatewayKeys.Count -eq 1) 'unique-gateway-peer-public-key'
    $result.gatewayPublicKey=$gatewayKeys[0];Save-Progress
    $peerIp=$peer.ipv4.Split('/')[0];$peerIp6=$peer.ipv6.Split('/')[0]
    Start-Sleep -Seconds 3
    $result.phase='demand'
    if($Role -eq 'a'){
        if($settings.clients.c){$third=Wait-Peer $settings.clients.c.username '' 90;Check ($third.path -eq 'idle') 'third-client-remains-idle-before-demand'}
        Check ((Wait-Peer $other.username '').path -eq 'idle') 'no-proactive-full-mesh'
        Start-Sleep -Seconds 5
        $started=[Diagnostics.Stopwatch]::StartNew();Echo $peerIp 'first-demand';$result.firstPacketMs=$started.ElapsedMilliseconds
        $peer=Wait-Peer $other.username 'direct';$result.direct=$peer;$result.directEstablishedMs=$started.ElapsedMilliseconds
        Check ($result.firstPacketMs -lt 5000) 'first-flow-under-5s'
        Check ($result.directEstablishedMs -lt 5000) 'direct-under-5s'
        Echo $peerIp6 'ipv6-direct';Check $true 'ipv6-peer-data'
        $result.phase='continuous-switch'
        $tcp=[Net.Sockets.TcpClient]::new($peerIp,45880);$stream=$tcp.GetStream();$stream.ReadTimeout=5000
        try{
            $payload=[Text.Encoding]::UTF8.GetBytes(('pmman-data-'*300))
            for($i=0;$i -lt 750;$i++){
                if($i -eq 30){$null=Ipc @{action='set_p2p';p2p_enabled=$false}}
                if($i -eq 80){$null=Ipc @{action='set_p2p';p2p_enabled=$true}}
                $payload=if($i -eq 130){[Text.Encoding]::UTF8.GetBytes('p2p-peer-disable')}elseif($i -eq 200){[Text.Encoding]::UTF8.GetBytes('p2p-peer-enable')}else{[Text.Encoding]::UTF8.GetBytes(('pmman-data-'*300))}
                $stream.Write($payload,0,$payload.Length);$buffer=New-Object byte[] $payload.Length;$offset=0
                while($offset -lt $payload.Length){$n=$stream.Read($buffer,$offset,$payload.Length-$offset);if($n -eq 0){throw 'Active TCP connection broke'};$offset+=$n}
                if([Convert]::ToBase64String($buffer) -ne [Convert]::ToBase64String($payload)){throw 'Sustained payload corrupted'}
                if($i -eq 130){$null=Wait-Peer $other.username 'relay';Check $true 'remote-p2p-disable-preserves-established-tcp'}
                if($i -eq 200){$null=Wait-Peer $other.username 'direct';Check $true 'remote-p2p-enable-recovers-established-tcp'}
                Start-Sleep -Milliseconds 50
            }
        }finally{$tcp.Dispose()}
        Check $true 'same-tcp-survives-live-toggle-and-active-idle-deadline'
        Udp-Echo $peerIp;Check $true 'udp-socket-survives-live-toggle'
        Udp-Echo $peerIp6;Check $true 'ipv6-udp-socket-survives-live-toggle'
        $peer=Wait-Peer $other.username 'direct';Check ($peer.relayTxBytes -gt 0 -and $peer.directTxBytes -gt 0) 'direct-and-relay-real-traffic'
        $result.phase='forced-direct-failure'
        $endpoints=Own-PeerCandidates $peer
        Start-Sleep -Seconds 2
        Echo $peerIp 'pre-failure-directory-refresh'
        $peer=Wait-Peer $other.username 'direct'
        $candidates=@(@($endpoints.candidates)+@($peer.directEndpoint)|Sort-Object -Unique)
        $relay=[Uri]("udp://"+$endpoints.relay)
        $result.failureBlockedCandidates=$candidates;$result.failurePreservedRelay=$endpoints.relay
        $directRuleNames=@()
        $beforeFailure=$peer;$result.failureBaseline=$beforeFailure
        $tcp=[Net.Sockets.TcpClient]::new($peerIp,45880);$stream=$tcp.GetStream();$stream.ReadTimeout=5000
        try{
            $stream.WriteTimeout=5000
            foreach($candidate in $candidates){
                $target=[Uri]("udp://"+$candidate);$address=[Net.IPAddress]::Parse($target.DnsSafeHost).ToString()
                if($target.Port -lt 1 -or $target.Port -gt 65535){throw 'Invalid direct candidate port'}
                if($address -eq [Net.IPAddress]::Parse($relay.DnsSafeHost).ToString() -and $target.Port -eq $relay.Port){throw 'Direct candidate overlaps relay; cannot isolate this pair'}
                $name="$firewallPrefix-direct-$($directRuleNames.Count)";$directRuleNames+=$name;$firewallNames+=$name
                New-NetFirewallRule -Name $name -DisplayName 'PMMan acceptance block direct' -Direction Outbound -Action Block -Protocol UDP -RemoteAddress $address -RemotePort $target.Port -Program (Join-Path $appRoot 'pmman-network-service.exe') -Profile Any|Out-Null
            }
            foreach($text in @('must-survive-blocked-direct','second-message-same-connection')){
                $bytes=[Text.Encoding]::UTF8.GetBytes($text);$stream.Write($bytes,0,$bytes.Length);$buffer=New-Object byte[] $bytes.Length;$offset=0
                while($offset -lt $bytes.Length){$n=$stream.Read($buffer,$offset,$bytes.Length-$offset);if(!$n){throw 'Direct failure broke established TCP'};$offset+=$n}
                Check ([Text.Encoding]::UTF8.GetString($buffer) -eq $text) 'blocked-direct-same-tcp-data'
            }
            $fallback=Wait-Peer $other.username 'relay'
            Check ($fallback.fallbackCount -gt $beforeFailure.fallbackCount -and $fallback.relayTxBytes -gt $beforeFailure.relayTxBytes) 'blocked-direct-new-fallback-and-relay-traffic'
            $result.failureFallback=$fallback
            Remove-NetFirewallRule -Name $directRuleNames
            $deadline=[DateTime]::UtcNow.AddSeconds(20)
            do{
                $text='same-tcp-during-direct-recovery';$bytes=[Text.Encoding]::UTF8.GetBytes($text);$stream.Write($bytes,0,$bytes.Length);$buffer=New-Object byte[] $bytes.Length;$offset=0
                while($offset -lt $bytes.Length){$n=$stream.Read($buffer,$offset,$bytes.Length-$offset);if(!$n){throw 'Direct recovery broke established TCP'};$offset+=$n}
                if([Text.Encoding]::UTF8.GetString($buffer) -ne $text){throw 'Direct recovery corrupted established TCP'}
                $peer=Wait-Peer $other.username ''
                if($peer.path -ne 'direct'){Start-Sleep -Milliseconds 100}
            }while($peer.path -ne 'direct' -and [DateTime]::UtcNow -lt $deadline)
            Check ($peer.path -eq 'direct') 'automatic-direct-recovery-preserves-same-tcp'
            Check ($peer.directTxBytes -gt $fallback.directTxBytes -and $peer.publicKey -eq $beforeFailure.publicKey) 'recovered-direct-data-with-same-peer-key'
        }finally{
            $tcp.Dispose()
            Get-NetFirewallRule -Name $directRuleNames -ErrorAction SilentlyContinue | Remove-NetFirewallRule
        }
        $result.finalPeer=$peer
        $result.phase='idle'
        Start-Sleep -Seconds 33
        Check ((Wait-Peer $other.username 'idle' 10).path -eq 'idle') 'idle-session-reclaimed'
        Echo $peerIp 'after-idle';$peer=Wait-Peer $other.username 'direct';Check $true 'demand-after-idle'
        if($settings.clients.c){
            $third=Wait-Peer $settings.clients.c.username 'idle' 10
            $thirdIp=$third.ipv4.Split('/')[0];$thirdIp6=$third.ipv6.Split('/')[0]
            $result.phase='third-client-demand';Echo $thirdIp 'third-demand';$third=Wait-Peer $settings.clients.c.username ''
            if($ThirdMayRelay){Check ($third.path -in @('direct','relay')) 'third-client-path-established'}else{Check ($third.path -eq 'direct') 'third-client-direct-path'}
            Echo $thirdIp6 'third-ipv6';Udp-Echo $thirdIp;Udp-Echo $thirdIp6
            Check $true 'third-client-dual-stack-tcp-udp-demand'
            # Capture exact demanded-peer handshakes before notifying peers to
            # disconnect; directory cleanup legitimately removes their WG rows.
            Verify-Handshakes
            Echo $thirdIp 'pmman-acceptance-done'
            $deadline=[DateTime]::UtcNow.AddSeconds(30);while(!(Test-Path (Join-Path $OutDir 'peer-reverse-c')) -and [DateTime]::UtcNow -lt $deadline){Start-Sleep -Milliseconds 200}
            Check (Test-Path (Join-Path $OutDir 'peer-reverse-c')) 'third-client-reverse-direction-received'
        }
        if(!$settings.clients.c){Verify-Handshakes}
        $result.phase='reconnect-key-rotation'
        $result.firstSessionPublicKey=$result.publicKey;$result.firstSessionHandshakes=$result.handshakes;$result.firstSessionGatewayPublicKey=$result.gatewayPublicKey
        $previousIpv4=$result.ipv4;$previousIpv6=$result.ipv6
        $status=Ipc @{action='disconnect'};Check ($status.state -eq 'disconnected') 'first-session-disconnect'
        Check (!(Get-Service 'WireGuardTunnel$pmman-client' -ErrorAction SilentlyContinue)) 'first-session-tunnel-service-cleaned'
        $status=Ipc @{action='select_node';node_id=$null};Check ($status.nodeSelection -eq 'auto') 'reconnect-selects-automatic-node'
        $result.connectionStartedUnix=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        $status=Ipc @{action='connect'}
        Check ($status.state -eq 'connected' -and $status.tunnelMode -eq 'wireguard') 'second-session-real-gateway-wireguard'
        $measured=@($status.nodes|Where-Object {$_.healthy -and $null -ne $_.latencyMs}|Sort-Object latencyMs)
        if($measured.Count){$selected=$status.nodes|Where-Object nodeId -eq $status.selectedNodeId;Check ($selected.latencyMs -eq $measured[0].latencyMs) 'automatic-node-has-lowest-measured-udp-or-icmp-latency'}
        Check ($status.ipv4 -eq $previousIpv4 -and $status.ipv6 -eq $previousIpv6) 'reconnect-reuses-dual-stack-lease'
        $result.publicKey=(& $wg show pmman-client public-key).Trim()
        Check ($result.publicKey -ne $result.firstSessionPublicKey) 'reconnect-rotates-client-key'
        $peer=Wait-Peer $other.username '' 30
        $currentKeys=@((Ipc @{action='status'}).p2pPeers|ForEach-Object publicKey)
        $gatewayKeys=@(& $wg show pmman-client peers|Where-Object {$_ -notin $currentKeys})
        Check ($LASTEXITCODE -eq 0 -and $gatewayKeys.Count -eq 1) 'second-session-unique-gateway-public-key'
        $result.gatewayPublicKey=$gatewayKeys[0]
        Echo $peerIp 'new-session-demand';$null=Wait-Peer $other.username 'direct' 20
        Echo $peerIp6 'new-session-ipv6';Check $true 'p2p-demand-after-key-rotation'
        $result.peerIdentities=@((Ipc @{action='status'}).p2pPeers|Select-Object deviceId,username,publicKey,ipv4,ipv6,path)
        $result.demandedUsers=@($other.username);Verify-Handshakes
        # Leave a bounded observation window for the independent public-node
        # collector to match this short second session before its cleanup.
        Start-Sleep -Seconds 15
        Echo $peerIp 'pmman-acceptance-done'
        $deadline=[DateTime]::UtcNow.AddSeconds(30);while(!(Test-Path (Join-Path $OutDir 'peer-reverse-b')) -and [DateTime]::UtcNow -lt $deadline){Start-Sleep -Milliseconds 200}
        Check (Test-Path (Join-Path $OutDir 'peer-reverse-b')) 'reverse-direction-received'
        Echo $peerIp 'pmman-acceptance-done'
    }else{
        $peer=Wait-Peer $other.username 'direct' 300
        Check $true 'inbound-demand-direct'
        if($Role -eq 'b'){Echo $peerIp "acceptance-device-a-$($peer.deviceId)"}
        if($Role -eq 'c'){Check ((Wait-Peer $settings.clients.b.username '').path -eq 'idle') 'unvisited-b-c-pair-remains-idle'}
        $deadline=[DateTime]::UtcNow.AddSeconds(300);while(!(Test-Path (Join-Path $OutDir 'peer-done')) -and [DateTime]::UtcNow -lt $deadline){
            foreach($change in @(@{name='disable';enabled=$false},@{name='enable';enabled=$true})){
                $marker=Join-Path $OutDir "peer-$($change.name)"
                if(Test-Path $marker){$null=Ipc @{action='set_p2p';p2p_enabled=$change.enabled};Remove-Item -LiteralPath $marker;Check $true "remote-toggle-$($change.name)-applied"}
            }
            Start-Sleep -Milliseconds 200
        }
        Check (Test-Path (Join-Path $OutDir 'peer-done')) 'remote-scenario-finished'
        if($Role -eq 'b'){
            $oldKey=($result.peerIdentities|Where-Object username -eq $other.username).publicKey
            $updated=Wait-Peer $other.username 'direct' 20
            Check ($updated.publicKey -ne $oldKey) 'remote-reconnect-key-rotation-observed'
            Check ($oldKey -notin @(& $wg show pmman-client peers)) 'revoked-old-peer-key-removed'
            $result.peerIdentities=@((Ipc @{action='status'}).p2pPeers|Select-Object deviceId,username,publicKey,ipv4,ipv6,path)
        }
        Verify-Handshakes
        Echo $peerIp "reverse-demand-$Role";Check $true 'reverse-direction-peer-data'
        Start-Sleep -Seconds 3
    }
    $result.phase='disconnect'
    $status=Ipc @{action='disconnect'};Check ($status.state -eq 'disconnected') 'disconnect'
    Check (!(Get-Service 'WireGuardTunnel$pmman-client' -ErrorAction SilentlyContinue)) 'tunnel-service-cleaned'
    $null=Ipc @{action='set_p2p';p2p_enabled=$false}
    if($client -and !$client.HasExited){Stop-Process -Id $client.Id -Force}
    # Stop only the exact installed product executable inside this disposable VM.
    Get-CimInstance Win32_Process -Filter "Name='pmman-network-service.exe'" | Where-Object ExecutablePath -eq (Join-Path $appRoot 'pmman-network-service.exe') | ForEach-Object {Stop-Process -Id $_.ProcessId -Force}
    $client=Start-Process -FilePath (Join-Path $appRoot 'pmman-client.exe') -PassThru
    $status=$null;for($i=0;$i -lt 30 -and !$status;$i++){try{$status=Ipc @{action='status'}}catch{Start-Sleep -Milliseconds 500}}
    Check ($status -and !$status.p2pEnabled) 'p2p-preference-survives-service-restart'
    $null=Ipc @{action='set_p2p';p2p_enabled=$true}
    $result.passed=$true;$result.phase='complete'
}catch{$result.error=$_.Exception.Message;Log "failed_phase=$($result.phase);error=$($result.error)"}
finally{
    if($echoJob){$echoJob|Stop-Job;$echoJob|Remove-Job -Force}
    if($udpJob){$udpJob|Stop-Job;$udpJob|Remove-Job -Force}
    if(!$result.passed){try{$null=Ipc @{action='disconnect'}}catch{}}
    foreach($name in $firewallNames){Get-NetFirewallRule -Name $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule -ErrorAction SilentlyContinue}
    $result|ConvertTo-Json -Depth 12|Set-Content -LiteralPath (Join-Path $OutDir 'application-result.json') -Encoding utf8
}
if(!$result.passed){exit 1}
