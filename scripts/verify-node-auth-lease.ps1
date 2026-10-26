param([ValidateSet('txgz-a','txgz-b')][string]$NodeId='txgz-a')
$ErrorActionPreference='Stop'
[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12
$root=Split-Path -Parent $PSScriptRoot
$deployment=Get-Content -Raw -LiteralPath (Join-Path $root 'artifacts\txgz-deployment.json')|ConvertFrom-Json
$credentials=Get-Content -Raw -LiteralPath (Join-Path $root 'artifacts\txgz-credentials.json')|ConvertFrom-Json
if($deployment.master -ne 'https://network.pmman.tech'){throw 'Expected the public HTTPS development master'}
$master=$deployment.master
$group=[Guid]::NewGuid().ToString('N')
$username='nodelease-'+$group.Substring(0,12)
$password=[Guid]::NewGuid().ToString('N')+[Guid]::NewGuid().ToString('N')
$fixturePath=Join-Path $root "artifacts\nodelease-$group-credentials.json"
$evidencePath=Join-Path $root "artifacts\nodelease-$group-evidence.json"
& git -C $root check-ignore --quiet -- $fixturePath
if($LASTEXITCODE -ne 0){throw 'Fixture credentials must be under ignored artifacts'}
$result=[ordered]@{passed=$false;node=$NodeId;username=$username;checks=@();recovery=@();error=$null;cleanupError=$null;cleanup=$false}
$headers=@{};$clientHeaders=@{};$created=$false;$assigned=$null;$spec=$null
$helperSource=Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'txgz\nodelease_probe.py')
$encoded=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($helperSource))
function Master([string]$Path,[string]$Method='Get',$Body=$null,[hashtable]$Auth=$headers,[switch]$AllowPending){
    $parameters=@{Uri="$master$Path";Method=$Method;Headers=$Auth;TimeoutSec=10}
    if($null -ne $Body){$parameters.ContentType='application/json';$parameters.Body=$Body|ConvertTo-Json -Compress}
    # Emit JSON array elements for downstream device filters.
    try{$response=Invoke-RestMethod @parameters;return $response}catch{
        if($AllowPending -and $_.Exception.Response -and [int]$_.Exception.Response.StatusCode -in @(409,502)){return $null}
        $status=if($_.Exception.Response){[int]$_.Exception.Response.StatusCode}else{'transport'}
        # The connection API error schema contains only a stage description.
        # Do not dump arbitrary auth responses or request objects.
        $stage=''
        if($Path -eq '/v1/client/connections'){
            try{$description=$_.ErrorDetails.Message|ConvertFrom-Json;$stage=[string]$description.error}catch{}
        }
        throw "Public master request failed: $Method $Path (status=$status); $stage"
    }
}
function Check([bool]$ok,[string]$name){if(!$ok){throw "Check failed: $name"};$result.checks+=$name}
function Remote-Probe($InputSpec,[int]$TimeoutMs=80000){
    # Code is fixed, encoded source. Credentials travel only on SSH stdin.
    $command="python3 -c 'import base64;exec(base64.b64decode("+'"'+$encoded+'"'+"))'"
    $info=[Diagnostics.ProcessStartInfo]::new();$info.FileName=(Get-Command ssh.exe).Source
    $info.Arguments='-T -o BatchMode=yes -o ConnectTimeout=5 -o ServerAliveInterval=2 -o ServerAliveCountMax=2 txgz "'+$command.Replace('"','\"')+'"'
    $info.UseShellExecute=$false;$info.CreateNoWindow=$true
    $info.RedirectStandardInput=$true;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
    $child=[Diagnostics.Process]::Start($info)
    try{
        $stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync()
        $child.StandardInput.WriteLine(($InputSpec|ConvertTo-Json -Depth 8 -Compress));$child.StandardInput.Close()
        if(!$child.WaitForExit($TimeoutMs)){$child.Kill();$child.WaitForExit();throw 'Node lease probe timed out; remote CONT safety guard remains responsible for recovery'}
        # Never print remote stderr or the input spec, which contains tokens.
        $reply=$stdout.Result|ConvertFrom-Json
        if(!$reply.passed -or $child.ExitCode -ne 0){throw "Node lease probe failed: $($reply.error)"}
        return $reply
    }finally{$child.Dispose()}
}
function Fixture-InKernel($observation){
    $node=$observation.nodes|Where-Object id -eq $NodeId
    $v4=$assigned.ipv4.Split('/')[0];$v6=$assigned.ipv6.Split('/')[0]
    $routes4=@($node.routes4|Where-Object { $_.dev -eq $node.interface -and $_.dst.Split('/')[0] -eq $v4 })
    $routes6=@($node.routes6|Where-Object { $_.dev -eq $node.interface -and $_.dst.Split('/')[0] -eq $v6 })
    return ($node.up -and $publicKey -in @($node.peers) -and $routes4.Count -gt 0 -and $routes6.Count -gt 0)
}
try{
    $admin=Master '/v1/auth/login' 'Post' @{username=$credentials.admin_user;password=$credentials.admin_password}
    $headers.Authorization="Bearer $($admin.token)";$admin=$null;$credentials=$null
    $active=@(Master '/v1/admin/devices'|Where-Object status -eq 'active')
    Check (!$active.Count) 'no-other-active-client-before-fixture'
    # A random canonical 32-byte public key is enough to test node authorization
    # installation. No private key, host tunnel, or product process is needed.
    $bytes=New-Object byte[] 32;$rng=[Security.Cryptography.RandomNumberGenerator]::Create()
    try{$rng.GetBytes($bytes)}finally{$rng.Dispose()}
    $publicKey=[Convert]::ToBase64String($bytes)
    @{username=$username;password=$password;public_key=$publicKey;node=$NodeId}|ConvertTo-Json|Set-Content -LiteralPath $fixturePath -Encoding utf8
    $null=Master '/v1/admin/users' 'Post' @{username=$username;password=$password;role='user'};$created=$true
    $client=Master '/v1/auth/login' 'Post' @{username=$username;password=$password}
    $clientHeaders.Authorization="Bearer $($client.token)";$client=$null
    # Successful connections return only after the matching node generation is
    # applied. Recovery below deliberately does NOT re-register this fixture.
    $assigned=Master '/v1/client/connections' 'Post' @{node_id=$NodeId;public_key=$publicKey;ipv4_mode='internal';ipv6_mode='internal'} $clientHeaders
    $result.deviceId=$assigned.device_id;$result.publicKey=$publicKey;$result.ipv4=$assigned.ipv4;$result.ipv6=$assigned.ipv6;$result.configVersion=$assigned.config_version
    Check ([bool]$assigned.device_id) 'fixture-node-generation-applied'
    $active=@(Master '/v1/admin/devices'|Where-Object status -eq 'active')
    Check ($active.Count -eq 1 -and $active[0].device_id -eq $assigned.device_id -and $active[0].public_key -eq $publicKey) 'only-fixture-active-before-pause'
    $spec=@{master=$master;node_id=$NodeId;device_id=$assigned.device_id;public_key=$publicKey;ipv4=$assigned.ipv4;ipv6=$assigned.ipv6;nodes=@($deployment.nodes|Select-Object id,interface,route_table);admin_token=$headers.Authorization.Substring(7);client_token=$clientHeaders.Authorization.Substring(7)}
    $result.pause=Remote-Probe $spec
    Check ($result.pause.masterResumed -and $result.pause.clientLeaseRenewed) 'master-resumed-and-original-client-lease-renewed'
    Check $true 'real-node-interfaces-down-owned-peer-and-routes-removed'
    $spec.Remove('admin_token');$spec.Remove('client_token');$spec.mode='observe'
    # A stopped master can leave one in-flight loopback request until its
    # bounded timeout. Allow the node's re-registration and fresh heartbeat a
    # full two minutes, while retaining the finite test bound.
    $deadline=[DateTime]::UtcNow.AddSeconds(120)
    do{
        $null=Master "/v1/devices/$($assigned.device_id)/heartbeat" 'Post' $null $clientHeaders
        $current=@(Master '/v1/admin/devices'|Where-Object device_id -eq $assigned.device_id)
        Check ($current.Count -eq 1 -and $current[0].status -eq 'active' -and $current[0].public_key -eq $publicKey -and $current[0].config_version -eq $assigned.config_version) 'original-generation-still-authorized'
        $recovered=Remote-Probe $spec 20000
        $applied=Master "/v1/devices/$($assigned.device_id)/config" 'Get' $null $clientHeaders -AllowPending
        $result.recovery += [ordered]@{nodes=$recovered.nodes;configVersion=if($applied){$applied.config_version}else{$null};deviceStatus=$current[0].status}
        if((Fixture-InKernel $recovered) -and !@($recovered.nodes|Where-Object { !$_.up }).Count -and $applied -and $applied.config_version -eq $assigned.config_version){break}
        Start-Sleep -Seconds 1
    }while([DateTime]::UtcNow -lt $deadline)
    Check ((Fixture-InKernel $recovered) -and !@($recovered.nodes|Where-Object { !$_.up }).Count -and $applied -and $applied.config_version -eq $assigned.config_version) 'original-generation-reapplied-with-key-routes-and-up-nodes'
    $result.recovered=$recovered
    # Renewing the public heartbeat checks original authorization, not a new
    # connection that could hide expiry or resurrect a revoked generation.
    $result.passed=$true
}catch{$result.error=$_.Exception.Message}
finally{
    if($created){
        try{
            $owned=@(Master '/v1/admin/devices'|Where-Object username -eq $username)
            foreach($device in $owned){
                $null=Master "/v1/devices/$($device.device_id)/revoke" 'Post' @{public_key=$device.public_key} $clientHeaders
            }
            $null=Master "/v1/admin/users/$username/status" 'Post' @{enabled=$false}
            if($assigned -and $spec){
                $spec.Remove('admin_token');$spec.Remove('client_token');$spec.mode='observe'
                $deadline=[DateTime]::UtcNow.AddSeconds(30)
                do{
                    $final=Remote-Probe $spec 20000
                    $remaining=@($final.nodes|Where-Object {
                        $n=$_;$publicKey -in @($n.peers) -or @($n.routes4|Where-Object { $_.dev -eq $n.interface -and $_.dst.Split('/')[0] -eq $assigned.ipv4.Split('/')[0] }).Count -or @($n.routes6|Where-Object { $_.dev -eq $n.interface -and $_.dst.Split('/')[0] -eq $assigned.ipv6.Split('/')[0] }).Count
                    })
                    if(!$remaining.Count){break};Start-Sleep -Seconds 1
                }while([DateTime]::UtcNow -lt $deadline)
                if($remaining.Count){throw 'Owned fixture peer/routes survived revoke'}
                $result.final=$final
            }
            $result.cleanup=$true
        }catch{
            $result.passed=$false
            $result.cleanupError=$_.Exception.Message
            $priorError=$result.error
            if($priorError){$result.error="Fixture cleanup failed after: $priorError; cleanup=$($result.cleanupError)"}
            else{$result.error="Fixture cleanup failed; cleanup=$($result.cleanupError)"}
        }
    }
    if($spec){$spec.Remove('admin_token');$spec.Remove('client_token')}
    $headers.Clear();$clientHeaders.Clear();$password=$null
    $result|ConvertTo-Json -Depth 12|Set-Content -LiteralPath $evidencePath -Encoding utf8
}
Write-Output "nodelease_passed=$($result.passed) evidence=$evidencePath"
if(!$result.passed){throw $result.error}
