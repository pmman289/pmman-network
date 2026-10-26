param([string]$Installer,[string]$GroupId,[switch]$KeepUsers,[switch]$HostThirdClient,[ValidateSet('PrivateNat')][string]$NetworkProfile='PrivateNat',[ValidateSet(2,3)][int]$ClientCount=3)
$ErrorActionPreference='Stop'
$root=Split-Path -Parent $PSScriptRoot
if(!$Installer){$Installer=(Get-ChildItem (Join-Path $root 'src-tauri\target\release\bundle\nsis') -Filter '*.exe'|Sort-Object LastWriteTime -Descending|Select-Object -First 1).FullName}
if(!$GroupId){$GroupId=[Guid]::NewGuid().ToString('N')}
$artifact=Join-Path $root 'acceptance\target\x86_64-pc-windows-msvc\release\pmman-p2p-acceptance.exe'
foreach($path in @($Installer,$artifact)){if(!(Test-Path -LiteralPath $path -PathType Leaf)){throw "Missing build artifact: $path"}}
$queueRunner=Join-Path $env:USERPROFILE '.agents\skills\hyperv-test-executables\scripts\Get-HyperVExecutableTestQueue.ps1'
if(!(Test-Path -LiteralPath $queueRunner)){throw "Missing Hyper-V queue diagnostic: $queueRunner"}
$queue=& $queueRunner | ConvertFrom-Json
if(!$queue.BrokerHealthy -or !$queue.PlatformHealthy){throw "Hyper-V broker is not healthy: $($queue.BrokerStatus); $($queue.HealthReasons -join ', ')"}
$vmClientCount=if($HostThirdClient){2}else{$ClientCount}
if($HostThirdClient -and $ClientCount -ne 3){throw 'HostThirdClient requires ClientCount=3 so the fixture still contains a,b,c'}
if([int]$queue.PoolMaxWorkers -lt $vmClientCount){throw "Hyper-V pool has only $($queue.PoolMaxWorkers) workers; $vmClientCount VM clients require a matching request group."}
$secrets=Get-Content -Raw (Join-Path $root 'artifacts\txgz-credentials.json')|ConvertFrom-Json
$master='https://network.pmman.tech'
$login=Invoke-RestMethod -Uri "$master/v1/auth/login" -Method Post -ContentType 'application/json' -Body (@{username=$secrets.admin_user;password=$secrets.admin_password}|ConvertTo-Json -Compress)
$headers=@{Authorization="Bearer $($login.token)"}
$fixture=[ordered]@{master=$master;clients=[ordered]@{}}
$roles=@('a','b','c') | Select-Object -First $ClientCount
$vmRoles=@('a','b') | Select-Object -First $vmClientCount
$jobs=@()
$hostJob=$null
$outcomes=[Collections.Generic.List[object]]::new()
$observations=[Collections.Generic.List[object]]::new()
$observedDevices=@{}
$fixturePath=Join-Path $root "artifacts\p2p-$GroupId-credentials.json"
$evidencePath=Join-Path $root "artifacts\p2p-$GroupId-public-evidence.json"
$hostOutDir=Join-Path $root "artifacts\p2p-$GroupId-host-c"
$hostStopFile=Join-Path $hostOutDir 'stop'
$deployment=Get-Content -Raw (Join-Path $root 'artifacts\txgz-deployment.json')|ConvertFrom-Json
foreach($node in $deployment.nodes){if($node.interface -notmatch '^pmman-dev-[a-z0-9-]+$' -or $node.route_table -notmatch '^[0-9]+$'){throw 'Unexpected acceptance interface or route table'}}
function Read-PublicEvidence{
    $allDevices=Invoke-RestMethod -Uri "$master/v1/admin/devices" -Headers $headers -TimeoutSec 10
    $devices=@($allDevices|Where-Object username -in @($fixture.clients.Values|ForEach-Object username))
    foreach($device in $devices){$observedDevices[$device.public_key]=$device}
    # Structured, read-only SSH observations. Never request wg dump, which
    # contains node private keys, and never expose administrator credentials.
    $nodeJson=ConvertTo-Json -InputObject @($deployment.nodes|Select-Object id,interface,route_table) -Compress
    $python='import json,subprocess,time; nodes='+$nodeJson+'; run=lambda a: subprocess.check_output(a,text=True,timeout=3).splitlines(); print(json.dumps({"unix":int(time.time()),"nodes":[{"id":n["id"],"handshakes":run(["wg","show",n["interface"],"latest-handshakes"]),"routes4":run(["ip","-4","route","show","table",n["route_table"]]),"routes6":run(["ip","-6","route","show","table",n["route_table"]])} for n in nodes]}))'
    # Encode this fixed read-only program to preserve quotes across Windows
    # argv, SSH and the remote shell. Its inputs contain no credentials.
    $encoded=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($python))
    $command="python3 -c 'import base64;exec(base64.b64decode("+'"'+$encoded+'"'+"))'"
    $info=[Diagnostics.ProcessStartInfo]::new();$info.FileName=(Get-Command ssh.exe).Source
    $info.Arguments='-n -T -o BatchMode=yes -o ConnectTimeout=5 -o ServerAliveInterval=2 -o ServerAliveCountMax=2 txgz "'+$command.Replace('"','\"')+'"'
    $info.UseShellExecute=$false;$info.CreateNoWindow=$true;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
    $child=[Diagnostics.Process]::Start($info)
    try{
        $stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync()
        if(!$child.WaitForExit(12000)){$child.Kill();$child.WaitForExit();throw 'Public-node read-only observation timeout'}
        if($child.ExitCode -ne 0){throw "Public-node read-only observation failed: $($stderr.Result.Trim())"}
        $remote=$stdout.Result|ConvertFrom-Json
    }finally{$child.Dispose()}
    $entry=[pscustomobject]@{utc=[DateTimeOffset]::UtcNow.ToString('o');devices=$devices;remote=$remote}
    $observations.Add($entry)
    $observations|ConvertTo-Json -Depth 12|Set-Content -LiteralPath $evidencePath -Encoding utf8
    return $entry
}
try{
foreach($role in $roles){
    $username="p2p-$($GroupId.Substring(0,10))-$role";$password=[Guid]::NewGuid().ToString('N')+[Guid]::NewGuid().ToString('N')
    $null=Invoke-RestMethod -Uri "$master/v1/admin/users" -Method Post -Headers $headers -ContentType 'application/json' -Body (@{username=$username;password=$password;role='user'}|ConvertTo-Json -Compress)
    $fixture.clients[$role]=@{username=$username;password=$password;node=if($role -eq 'a'){'txgz-a'}else{'txgz-b'}}
}
$fixture|ConvertTo-Json -Depth 6|Set-Content -LiteralPath $fixturePath -Encoding utf8
$runner=Join-Path $env:USERPROFILE '.agents\skills\hyperv-test-executables\scripts\Invoke-HyperVExecutableTest.ps1'
$scenarioDir=Join-Path $PSScriptRoot 'p2p'
$actions='[{"type":"wait_result_file","path":"{OUTDIR}\\application-result.json","timeoutMs":700000},{"type":"screenshot","name":"client-result.png"}]'
if($HostThirdClient){
    New-Item -ItemType Directory -Force $hostOutDir|Out-Null
    Remove-Item -LiteralPath $hostStopFile -Force -ErrorAction SilentlyContinue
    $hostScript=Join-Path $scenarioDir 'host-acceptance.ps1'
    $hostJob=Start-Job -ScriptBlock { param($script,$credentials,$out,$stop) & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script -Credentials $credentials -OutDir $out -StopFile $stop } -ArgumentList $hostScript,$fixturePath,$hostOutDir,$hostStopFile
    Start-Sleep -Seconds 2
}
Write-Output "p2p_group=$GroupId artifact_sha256=$((Get-FileHash $artifact).Hash) installer_sha256=$((Get-FileHash $Installer).Hash)"
    $baseline=Read-PublicEvidence
    foreach($role in $vmRoles){
        $jobs+=Start-Job -ScriptBlock {
            param($runner,$artifact,$installer,$scenarioDir,$fixturePath,$role,$group,$actions,$networkProfile,$clientCount,$thirdMayRelay)
            $thirdArgument=if($thirdMayRelay){' -ThirdMayRelay'}else{''}
            $arguments='"{HOSTINPUT:scenario}\guest-acceptance.ps1" -Installer "{HOSTINPUT:installer}" -Credentials "{HOSTINPUT:credentials}" -Role '+$role+' -OutDir "{OUTDIR}"'+$thirdArgument
            & $runner -ArtifactPath $artifact -Arguments $arguments -NetworkProfile $networkProfile -GroupId $group -GroupSize $clientCount -AcceptUacPrompt -AllowNetworkWithHostInputs -ReadOnlyHostInput @(@{Name='scenario';Path=$scenarioDir;Mode='Auto'},@{Name='installer';Path=$installer;Mode='Auto'},@{Name='credentials';Path=$fixturePath;Mode='Auto'}) -ActionsJson $actions -AssertResultFile '{OUTDIR}\application-result.json' -AssertResultJsonPointer '/passed' -AssertResultEqualsJson 'true' -ExecutionTimeoutSeconds 900
        } -ArgumentList $runner,$artifact,$Installer,$scenarioDir,$fixturePath,$role,$GroupId,$actions,$NetworkProfile,$vmClientCount,$HostThirdClient
    }
    function Receive-Summary { $jobs|Receive-Job|ForEach-Object {if($_ -is [string] -and $_.TrimStart().StartsWith('{')){try{$item=$_|ConvertFrom-Json;if($item.RequestId){$outcomes.Add($item)};$item|Select-Object RequestId,HarnessSucceeded,TestEvaluated,TestPassed,OverallSucceeded,ResultPath,Error|ConvertTo-Json -Compress}catch{$_}}else{$_}} }
    while(@($jobs|Where-Object State -eq 'Running').Count){
        Receive-Summary
        try{$null=Read-PublicEvidence}catch{Write-Warning $_.Exception.Message}
        Start-Sleep -Seconds 5
    }
    Receive-Summary
    $outcomes | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $root "artifacts\p2p-$GroupId-harness-results.json") -Encoding utf8
    if($outcomes.Count -ne $vmClientCount -or @($outcomes | Where-Object { !$_.HarnessSucceeded -or !$_.TestEvaluated -or !$_.TestPassed -or !$_.OverallSucceeded }).Count){throw "P2P group $GroupId failed; inspect recorded broker/application evidence."}
    if($HostThirdClient){
        $hostResultPath=Join-Path $hostOutDir 'application-result.json';$hostDeadline=[DateTime]::UtcNow.AddSeconds(45)
        do{if(Test-Path -LiteralPath $hostResultPath){break};Start-Sleep -Seconds 1}while([DateTime]::UtcNow -lt $hostDeadline)
        if(!(Test-Path -LiteralPath $hostResultPath)){throw 'Host third-client result was not produced'}
        $hostResult=Get-Content -Raw -LiteralPath $hostResultPath|ConvertFrom-Json
        if(!$hostResult.passed){throw "Host third-client failed: $($hostResult.error)"}
        $hostResult|ConvertTo-Json -Depth 12|Set-Content -LiteralPath (Join-Path $root "artifacts\p2p-$GroupId-host-result.json") -Encoding utf8
    }
    foreach($outcome in $outcomes){
        $application=Get-Content -Raw (Join-Path $outcome.ResultPath 'application-result.json')|ConvertFrom-Json
        $sessions=@(@{key=$application.publicKey;gateway=$application.gatewayPublicKey})
        if($application.firstSessionPublicKey){$sessions+=@{key=$application.firstSessionPublicKey;gateway=$application.firstSessionGatewayPublicKey}}
        foreach($session in $sessions){
            $device=@($observedDevices.Values|Where-Object public_key -eq $session.key)
            if($device.Count -ne 1){throw 'This-session Windows public key was not observed on the public master'}
            $node=$deployment.nodes|Where-Object id -eq $device[0].node_id
            if($session.gateway -ne $node.public_key){throw 'Windows gateway public key did not match the selected public node'}
            $matching=@($observations|Where-Object {
                $observedNode=$_.remote.nodes|Where-Object id -eq $device[0].node_id
                @($observedNode.handshakes|Where-Object { $fields=$_ -split '\s+'; $fields[0] -eq $session.key -and [long]$fields[1] -ge [long]$baseline.remote.unix }).Count -gt 0
            })
            if(!$matching.Count){throw 'Public node did not record a fresh handshake for this-session Windows public key'}
        }
    }
    $cleanupDeadline=[DateTime]::UtcNow.AddSeconds(30)
    do{
        $final=Read-PublicEvidence;$remaining=@()
        foreach($device in $observedDevices.Values){
            $node=$final.remote.nodes|Where-Object id -eq $device.node_id
            if(@($node.handshakes|Where-Object {($_ -split '\s+')[0] -eq $device.public_key}).Count -or @($node.routes4|Where-Object {($_ -split '\s+')[0].Split('/')[0] -eq $device.ipv4.Split('/')[0]}).Count -or @($node.routes6|Where-Object {($_ -split '\s+')[0].Split('/')[0] -eq $device.ipv6.Split('/')[0]}).Count){$remaining+=$device.device_id}
        }
        if($remaining.Count){Start-Sleep -Seconds 2}
    }while($remaining.Count -and [DateTime]::UtcNow -lt $cleanupDeadline)
    if($remaining.Count){throw 'This-group node peer or return route survived disconnect'}
    Write-Output 'public_master_node_handshakes_and_owned_peer_cleanup=passed'
}finally{
    if($HostThirdClient){
        New-Item -ItemType File -Force -Path $hostStopFile|Out-Null
        if($hostJob){Wait-Job $hostJob -Timeout 45|Out-Null;Receive-Job $hostJob -ErrorAction SilentlyContinue|Out-Null;Remove-Job $hostJob -Force -ErrorAction SilentlyContinue}
    }
    $jobs|Remove-Job -Force
    if(!$KeepUsers){foreach($entry in $fixture.clients.Values){try{$null=Invoke-RestMethod -Uri "$master/v1/admin/users/$($entry.username)/status" -Method Post -Headers $headers -ContentType 'application/json' -Body '{"enabled":false}'}catch{Write-Warning 'Test account disablement failed; retry from fixture identity.'}}}
}
