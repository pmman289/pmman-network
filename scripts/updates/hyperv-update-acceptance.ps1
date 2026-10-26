param(
    [Parameter(Mandatory = $true)][string]$OldInstaller,
    [Parameter(Mandatory = $true)][string]$UpdateInstaller,
    [Parameter(Mandatory = $true)][string]$Manifest,
    [Parameter(Mandatory = $true)][string]$NewClientBinary,
    [Parameter(Mandatory = $true)][string]$OutFile
)

$ErrorActionPreference = 'Stop'
$version = '0.1.0-alpha'
$installDir = Join-Path $env:ProgramData 'Pmman Network Update Acceptance'
$updateDir = Join-Path $env:ProgramData "PMMan\updates\$version"
$logPath = Join-Path $env:LOCALAPPDATA 'Pmman Network\update.log'
$result = [ordered]@{
    passed = $false
    version = $version
    phases = [ordered]@{}
    error = $null
}
$oldClient = $null
$updater = $null

function Wait-Exit([Diagnostics.Process]$Process, [int]$TimeoutMs) {
    if (!$Process.WaitForExit($TimeoutMs)) {
        throw "process $($Process.Id) did not exit within $TimeoutMs ms"
    }
    return $Process.ExitCode
}

try {
    if (Test-Path -LiteralPath $installDir) {
        Remove-Item -LiteralPath $installDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $install = Start-Process -FilePath $OldInstaller -ArgumentList @('/S', "/D=$installDir") -PassThru -Wait
    $result.phases.old_install_exit = $install.ExitCode
    if ($install.ExitCode -ne 0) { throw "old installer exit code $($install.ExitCode)" }

    # Seed a user-owned control-plane file and compare its digest after the
    # update. Checking only that the file exists would miss a reset-to-default
    # regression.
    $configPath = Join-Path $installDir 'pmman-client.json'
    $configFixture = '{"masterUrl":"https://acceptance.invalid","username":"acceptance-user","p2pEnabled":true,"selectedNodeId":"node-a"}'
    Set-Content -LiteralPath $configPath -Value $configFixture -Encoding UTF8 -NoNewline
    $configHashBefore = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $result.phases.config_hash_before = $configHashBefore

    New-Item -ItemType Directory -Force -Path $updateDir | Out-Null
    Copy-Item -LiteralPath $UpdateInstaller -Destination (Join-Path $updateDir 'ready.exe') -Force
    Copy-Item -LiteralPath $Manifest -Destination (Join-Path $updateDir 'manifest.json') -Force
    $result.phases.update_staged = $true

    # Keep a deterministic parent process alive while the updater waits. The
    # target directory still contains the real previous client installation;
    # this isolates the replacement protocol from WebView2 startup behavior
    # in an otherwise disconnected clean VM.
    $oldClient = Start-Process -FilePath 'powershell.exe' -ArgumentList @(
        '-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 300'
    ) -PassThru
    Start-Sleep -Seconds 2
    if ($oldClient.HasExited) { throw "old client exited before update, code=$($oldClient.ExitCode)" }
    $result.phases.old_client_pid = $oldClient.Id

    $updaterPath = Join-Path $env:TEMP 'Pmman Network Update Acceptance\pmman-network-updater.exe'
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $updaterPath) | Out-Null
    Copy-Item -LiteralPath $NewClientBinary -Destination $updaterPath -Force
    $ready = Join-Path $updateDir 'ready.exe'
    $sha = (Get-FileHash -LiteralPath $ready -Algorithm SHA256).Hash.ToLowerInvariant()
    $updater = Start-Process -FilePath $updaterPath -ArgumentList @(
        '--apply-update',
        '--installer', "`"$ready`"",
        '--install-dir', "`"$installDir`"",
        '--sha256', "`"$sha`"",
        '--parent-pid', "`"$($oldClient.Id)`""
    ) -PassThru
    Start-Sleep -Seconds 2

    # The product normally terminates itself with ExitProcess after launching
    # the updater. This driver closes the old UI at the same point so the
    # updater's native process-handle wait and replacement path are exercised.
    if (!$oldClient.HasExited) {
        Stop-Process -Id $oldClient.Id -Force
    }
    $result.phases.old_client_released = $true
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -eq (Join-Path $installDir 'pmman-network-service.exe') } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

    $updaterExit = Wait-Exit $updater 300000
    $result.phases.updater_exit = $updaterExit
    if ($updaterExit -ne 0) { throw "updater exit code $updaterExit" }

    $phaseFile = Join-Path $updateDir 'result.json'
    if (!(Test-Path -LiteralPath $phaseFile)) { throw 'missing updater result marker' }
    $phase = Get-Content -Raw -LiteralPath $phaseFile | ConvertFrom-Json
    $result.phases.final_phase = $phase.phase
    if ($phase.phase -ne 'complete') { throw "unexpected updater phase $($phase.phase)" }

    $healthFile = Join-Path $updateDir 'healthy.json'
    if (!(Test-Path -LiteralPath $healthFile)) { throw 'missing new-client health marker' }
    $health = Get-Content -Raw -LiteralPath $healthFile | ConvertFrom-Json
    $result.phases.health = $health
    if ($health.version -ne $version -or !$health.ui -or !$health.ipc) {
        throw 'new-client health probe did not pass'
    }

    if (!(Test-Path -LiteralPath $configPath)) {
        throw 'installed control-plane configuration was not preserved'
    }
    $configHashAfter = (Get-FileHash -LiteralPath $configPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $result.phases.config_hash_after = $configHashAfter
    if ($configHashAfter -ne $configHashBefore) {
        throw 'installed control-plane configuration changed during update'
    }
    $result.phases.installed_product_version = (Get-Item (Join-Path $installDir 'pmman-client.exe')).VersionInfo.ProductVersion
    $result.phases.config_preserved = $true
    $result.passed = $true
} catch {
    $result.error = $_.Exception.Message
} finally {
    if ($updater -and !$updater.HasExited) { Stop-Process -Id $updater.Id -Force -ErrorAction SilentlyContinue }
    if ($oldClient -and !$oldClient.HasExited) { Stop-Process -Id $oldClient.Id -Force -ErrorAction SilentlyContinue }
    Get-Process -Name pmman-network-service -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $logPath) {
        # GuestSetup may surface Get-Content results as remoting-adapted
        # PSObjects. Do not place those objects in the result graph: their
        # provider metadata is recursive and can make ConvertTo-Json consume
        # hundreds of megabytes. Keep only bounded plain log text.
        $result.phases.update_log_tail = @(
            Get-Content -LiteralPath $logPath -Tail 40 |
                ForEach-Object {
                    $line = [string]$_.ToString()
                    if ($line.Length -gt 2000) { $line.Substring(0, 2000) } else { $line }
                }
        )
    }
    $outParent = Split-Path -Parent $OutFile
    New-Item -ItemType Directory -Force -Path $outParent | Out-Null
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutFile -Encoding UTF8
}

# GuestSetup must exit successfully so the broker can collect the result marker
# and evaluate the product assertion, including detailed failure diagnostics.
exit 0
exit 0
