$ErrorActionPreference = 'Stop'
$resultPath = 'C:\pmman-results\acceptance.json'
$logPath = 'C:\pmman-results\acceptance.log'
$appRoot = 'C:\Program Files\Pmman Network'
$installer = 'C:\pmman-installer\PMMan-Network-Client-Setup.exe'
$credentialFile = 'C:\pmman-secrets\txgz-credentials.json'

function Write-Log([string]$message) {
    $line = "$(Get-Date -Format o) $message"
    Add-Content -LiteralPath $logPath -Value $line -Encoding utf8
}

function Send-Ipc([hashtable]$request) {
    $pipe = [System.IO.Pipes.NamedPipeClientStream]::new('.', 'pmman-network', [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::None)
    $pipe.Connect(15000)
    try {
        $writer = [System.IO.StreamWriter]::new($pipe, [System.Text.UTF8Encoding]::new($false), 1024, $true)
        $writer.AutoFlush = $true
        $reader = [System.IO.StreamReader]::new($pipe, [System.Text.UTF8Encoding]::new($false), $false, 1024, $true)
        $writer.WriteLine(($request | ConvertTo-Json -Compress))
        $read = $reader.ReadLineAsync()
        if (!$read.Wait(65000)) { throw 'IPC response timed out' }
        $line = $read.Result
        if ([string]::IsNullOrWhiteSpace($line)) { throw 'Empty IPC response' }
        return $line | ConvertFrom-Json
    } finally {
        $pipe.Dispose()
    }
}

function Assert-Ok($response, [string]$stage) {
    if (!$response.ok) { throw "$stage failed: $($response.status.message)" }
    return $response.status
}

function Get-WebViewDiagnostic {
    $roots = @(
        (Join-Path $env:ProgramFiles 'Microsoft\EdgeWebView\Application'),
        (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Microsoft\EdgeWebView\Application'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\EdgeWebView\Application')
    )
    $dirs = @($roots | ForEach-Object {
        if (Test-Path $_) { @(Get-ChildItem -LiteralPath $_ -Directory -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name) }
    })
    $guid = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    $registry = @()
    foreach ($path in @(
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$guid",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$guid",
        "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$guid"
    )) {
        $item = Get-ItemProperty -LiteralPath $path -ErrorAction SilentlyContinue
        if ($item) { $registry += "${path}|pv=$($item.pv)|location=$($item.location)" }
    }
    "dirs=$($dirs -join ',');registry=$($registry -join ';')"
}

New-Item -ItemType Directory -Force -Path 'C:\pmman-results' | Out-Null
Set-Content -LiteralPath $logPath -Value '' -Encoding utf8
$result = [ordered]@{
    state = 'failed'
    installerExitCode = $null
    prerequisites = $false
    service = $false
    authenticated = $false
    connected = $false
    tunnelMode = $null
    ipv4 = $null
    ipv6 = $null
    ipv4Gateway = $false
    ipv6Gateway = $false
    disconnected = $false
    userFacingEntryOnly = $false
    webviewLoader = $false
    uiWindow = $false
    webviewDiagnostic = $null
    error = $null
}
$clientProcess = $null

try {
    Write-Log 'starting silent installer'
    $install = Start-Process -FilePath $installer -ArgumentList '/S' -PassThru
    # The bundled Evergreen Standalone installer is about 202 MB compressed
    # into the NSIS package. A clean Windows Sandbox can need several minutes
    # to unpack and register it on first install.
    if (!$install.WaitForExit(360000)) {
        Write-Log "installer timeout pid=$($install.Id)"
        Stop-Process -Id $install.Id -Force -ErrorAction SilentlyContinue
        throw 'Installer did not finish within 360 seconds'
    }
    $result.installerExitCode = $install.ExitCode
    if ($install.ExitCode -ne 0) { throw "Installer exit code: $($install.ExitCode)" }
    $result.webviewDiagnostic = Get-WebViewDiagnostic
    $loader = Start-Process -FilePath (Join-Path $appRoot 'pmman-client.exe') -ArgumentList '--check-webview2' -PassThru -Wait
    if ($loader.ExitCode -ne 0) { throw "Actual WebView2 loader check failed: $($loader.ExitCode); $($result.webviewDiagnostic)" }
    $result.webviewLoader = $true

    $wg = Join-Path $env:ProgramFiles 'WireGuard\wg.exe'
    $wgui = Join-Path $env:ProgramFiles 'WireGuard\wireguard.exe'
    $webview = @(
        (Join-Path $env:ProgramFiles 'Microsoft\EdgeWebView\Application'),
        (Join-Path ([Environment]::GetFolderPath('ProgramFilesX86')) 'Microsoft\EdgeWebView\Application'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\EdgeWebView\Application')
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (!(Test-Path $wg) -or !(Test-Path $wgui) -or !$webview) { throw 'WireGuard or WebView2 runtime missing' }
    if (!(Test-Path (Join-Path $appRoot 'pmman-client.json'))) { throw 'Installed master configuration missing' }
    $pmmanShortcut = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Pmman Network.lnk'
    $wireguardShortcuts = @(
        (Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\WireGuard.lnk'),
        (Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\WireGuard\WireGuard.lnk'),
        (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\WireGuard.lnk'),
        (Join-Path ([Environment]::GetFolderPath('Desktop')) 'WireGuard.lnk')
    ) | Where-Object { Test-Path $_ }
    $wireguardArp = @(
        (Get-ItemProperty 'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue),
        (Get-ItemProperty 'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue)
    ) | Where-Object { $_.DisplayName -match '^WireGuard( for Windows)?$' }
    if (!(Test-Path $pmmanShortcut)) { throw 'Pmman Network shortcut missing' }
    if ($wireguardShortcuts.Count -gt 0) { throw "WireGuard user shortcuts remain: $($wireguardShortcuts -join ', ')" }
    if ($wireguardArp.Count -gt 0) { throw 'WireGuard remains visible in installed applications' }
    $wireguardWindows = @(Get-Process -Name 'wireguard' -ErrorAction SilentlyContinue | Where-Object {
        $_.MainWindowHandle -ne 0 -or -not [string]::IsNullOrWhiteSpace($_.MainWindowTitle)
    })
    if ($wireguardWindows.Count -gt 0) { throw 'WireGuard manager window remains visible after installation' }
    $result.userFacingEntryOnly = $true
    $result.prerequisites = $true

    Write-Log 'starting installed client'
    $clientProcess = Start-Process -FilePath (Join-Path $appRoot 'pmman-client.exe') -PassThru
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        Start-Sleep -Seconds 1
        $clientProcess.Refresh()
        if ($clientProcess.HasExited) { throw 'Client exited before UI became ready' }
        $viewProcesses = @(Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.ParentProcessId -eq $clientProcess.Id })
        if ($clientProcess.MainWindowTitle -eq 'Pmman Network' -and $viewProcesses.Count -gt 0) {
            $result.uiWindow = $true
            break
        }
    }
    if (!$result.uiWindow) { throw 'PMMan UI/WebView process was not created' }
    $status = $null
    for ($attempt = 0; $attempt -lt 30 -and !$status; $attempt++) {
        Start-Sleep -Seconds 1
        try { $status = Assert-Ok (Send-Ipc @{ action = 'status' }) 'status' } catch { }
    }
    if (!$status) { throw 'Background service did not respond' }
    $result.service = $true

    $credentials = Get-Content -LiteralPath $credentialFile -Raw | ConvertFrom-Json
    Write-Log 'logging in with ignored test credentials'
    $status = Assert-Ok (Send-Ipc @{ action = 'login'; username = [string]$credentials.client_user; password = [string]$credentials.client_password }) 'login'
    $result.authenticated = [bool]$status.authenticated
    if (!$result.authenticated) { throw 'Client authentication failed' }

    Write-Log 'requesting real WireGuard connection'
    $status = Assert-Ok (Send-Ipc @{ action = 'connect' }) 'connect'
    $result.connected = $status.state -eq 'connected'
    $result.tunnelMode = $status.tunnelMode
    $result.ipv4 = $status.ipv4
    $result.ipv6 = $status.ipv6
    if (!$result.connected -or $result.tunnelMode -ne 'wireguard') { throw "No real WireGuard connection: state=$($status.state) mode=$($status.tunnelMode)" }

    & ping.exe -n 2 -w 2000 10.88.0.1 | Out-Null
    $result.ipv4Gateway = $LASTEXITCODE -eq 0
    & ping.exe -6 -n 2 -w 2000 fd88:1000::1 | Out-Null
    $result.ipv6Gateway = $LASTEXITCODE -eq 0
    $result.clientPublicKey = ((& $wg show pmman-client public-key) | Out-String).Trim()
    $result.handshake = ((& $wg show pmman-client latest-handshakes) | Out-String).Trim()
    Write-Log "real connection established ipv4=$($result.ipv4) ipv6=$($result.ipv6) ipv4Gateway=$($result.ipv4Gateway) ipv6Gateway=$($result.ipv6Gateway)"

    $status = Assert-Ok (Send-Ipc @{ action = 'disconnect' }) 'disconnect'
    $result.disconnected = $status.state -eq 'disconnected'
    if (!$result.disconnected) { throw "Unexpected disconnect state: $($status.state)" }
    if (Get-Service -Name 'WireGuardTunnel$pmman-client' -ErrorAction SilentlyContinue) { throw 'Tunnel service remains after disconnect' }
    $result.state = 'passed'
    Write-Log 'sandbox acceptance passed'
} catch {
    $result.error = $_.Exception.Message
    Write-Log "sandbox acceptance failed: $($result.error)"
} finally {
    try { Send-Ipc @{ action = 'disconnect' } | Out-Null } catch { }
    if ($clientProcess -and !$clientProcess.HasExited) { Stop-Process -Id $clientProcess.Id -Force -ErrorAction SilentlyContinue }
    Get-Process -Name pmman-network-service -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    $result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $resultPath -Encoding utf8
}

Start-Sleep -Seconds 2
