[CmdletBinding()]
param([string]$HarnessRoot = 'D:\VMs\Codex-Harness', [switch]$Matrix, [switch]$TestGatewayKlBinding, [switch]$TestGatewayForwarding)

# Trusted host-side orchestration only. The diagnostic payload runs exclusively
# through the SYSTEM broker; this script never selects or changes worker VMs.
$ErrorActionPreference = 'Stop'
if ($TestGatewayKlBinding -and $TestGatewayForwarding) { throw 'Choose one gateway experiment at a time.' }
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$runner = Join-Path $env:USERPROFILE '.agents\skills\hyperv-test-executables\scripts\Invoke-HyperVExecutableTest.ps1'
$trace = Join-Path $HarnessRoot 'Source\setup\Trace-RequestUdp.ps1'
$artifact = Join-Path $root 'acceptance\target\x86_64-pc-windows-msvc\release\pmman-p2p-acceptance.exe'
foreach ($path in @($runner, $trace, $artifact)) {
    if (!(Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing canonical tool/artifact: $path" }
}
$testJob = $null
$traceJob = $null
$requestId = $null
$traceFailure = $null
$deadline = [DateTime]::UtcNow.AddMinutes(10)
try {
    $testJob = Start-Job -ArgumentList $runner,$artifact,$PSScriptRoot,([bool]$Matrix) -ScriptBlock {
        param($runner,$artifact,$scenario,$matrix)
        $arguments='"{HOSTINPUT:scenario}\udp-diagnostic.ps1" -SendDelaySeconds 60 -OutDir "{OUTDIR}"'
        $arguments+=if($matrix){' -Matrix -Rounds 1 -ReceiveTimeoutMs 2000'}else{' -Rounds 3'}
        & $runner -ArtifactPath $artifact -AcceptUacPrompt -Arguments $arguments -ReadOnlyHostInput @{Name='scenario';Path=$scenario;Mode='Auto'} -AllowNetworkWithHostInputs -NetworkProfile PrivateNat -ActionsJson '[{"type":"wait_result_file","path":"{OUTDIR}\\application-result.json","timeoutMs":240000},{"type":"screenshot","name":"udp-diagnostic.png"}]' -AssertResultFile '{OUTDIR}\application-result.json' -AssertResultJsonPointer '/passed' -AssertResultEqualsJson 'true' -ExecutionTimeoutSeconds 360 6>&1
    }
    while (($testJob.State -in @('NotStarted','Running')) -or ($traceJob -and ($traceJob.State -in @('NotStarted','Running')))) {
        foreach ($item in @($testJob | Receive-Job)) {
            $line = $item.ToString()
            if (!$requestId -and $line -match 'Submitted (executable-test-[0-9]{8}T[0-9]{9}Z-[a-f0-9]{8})\.') {
                $requestId = $Matches[1]
                # Plan/apply and submission stay in one process so model/API
                # delays cannot consume the actual packet capture window.
                $traceJob = Start-Job -ArgumentList $trace,$HarnessRoot,$requestId,([bool]$Matrix),([bool]$TestGatewayKlBinding),([bool]$TestGatewayForwarding) -ScriptBlock {
                    param($trace,$harness,$id,$matrix,$testGatewayKlBinding,$testGatewayForwarding)
                    $parameters = @{InstallRoot=$harness;RemoteAddress='1.14.226.49';RemotePorts=@(51840,51841);DurationSeconds=120;TraceNatDrops=$true;TraceWfp=$true;ControlDns=$true;WaitForRequestId=$id}
                    if($matrix){$parameters.RemotePorts=@(53,51840,51841);$parameters.ControlDnsPorts=@(53,51840)}
                    if($testGatewayKlBinding){$parameters.TestGatewayKlBinding=$true}
                    if($testGatewayForwarding){$parameters.TestGatewayForwarding=$true}
                    $plan = & $trace @parameters -PlanOnly | ConvertFrom-Json
                    $plan | ConvertTo-Json -Depth 6
                    & $trace @parameters -ApprovedPlanFingerprint $plan.PlanFingerprint
                }
            }
            if ($line.TrimStart().StartsWith('{')) {
                try { $item | ConvertFrom-Json | Select-Object RequestId,HarnessSucceeded,TestEvaluated,TestPassed,ResultPath,Error | ConvertTo-Json -Compress }
                catch { Write-Output $line }
            } else { Write-Output $line }
        }
        if ($traceJob) {
            # Keep observing the broker to its terminal cleanup even when a
            # maintenance experiment fails; do not abandon the request output.
            $traceErrors = @()
            $traceJob | Receive-Job -ErrorAction SilentlyContinue -ErrorVariable traceErrors
            if ($traceErrors.Count -and !$traceFailure) {
                $traceFailure = ($traceErrors | ForEach-Object ToString) -join '; '
                Write-Warning "Packet diagnostic failed: $traceFailure"
            }
        }
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Bounded orchestration observation deadline exceeded; retain request identity for broker cancellation.' }
        Start-Sleep -Seconds 1
    }
    $testJob | Receive-Job
    if ($traceJob) { $traceJob | Receive-Job -ErrorAction SilentlyContinue }
    if (!$requestId -or !$traceJob) { throw 'No exact submitted request was available to bind packet diagnostics.' }
    Write-Output "request_id=$requestId"
    if ($traceFailure -or $traceJob.State -ne 'Completed') { throw "Packet diagnostic job failed: $($traceJob.State); $traceFailure" }
} finally {
    # Do not terminate active broker or privileged recorder jobs. Their original
    # bounded deadlines/owned cleanup remain authoritative if observation fails.
    foreach ($job in @($testJob,$traceJob)) {
        if ($job -and $job.State -ne 'Running') { $job | Remove-Job }
    }
}
