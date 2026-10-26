$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$output = if ($args.Count -gt 0) { [IO.Path]::GetFullPath($args[0]) } else { Join-Path $root 'artifacts\standalone' }
New-Item -ItemType Directory -Force -Path $output | Out-Null
$running = @(Get-Process -Name pmman-client,pmman-network-service -ErrorAction SilentlyContinue)
if ($running) {
    Write-Warning ("Closing processes that lock the standalone artifacts: " + (($running | ForEach-Object { "$($_.ProcessName)#$($_.Id)" }) -join ', '))
    $running | Where-Object MainWindowHandle -ne 0 | ForEach-Object { $_.CloseMainWindow() | Out-Null }
    Start-Sleep -Seconds 1
    $remaining = @(Get-Process -Name pmman-client,pmman-network-service -ErrorAction SilentlyContinue)
    if ($remaining) {
        Write-Warning 'A graceful close did not finish; stopping the remaining PMMan processes.'
        $remaining | Stop-Process -Force
        Start-Sleep -Milliseconds 500
    }
}
$tunnel = Get-Service -Name 'WireGuardTunnel$pmman-client' -ErrorAction SilentlyContinue
if ($tunnel -and $tunnel.Status -ne 'Stopped') {
    throw 'WireGuardTunnel$pmman-client is still installed. Disconnect the client before packaging.'
}
$client = Join-Path $root 'src-tauri\target\release\pmman-client.exe'
$service = Join-Path $root 'service\target\x86_64-pc-windows-msvc\release\pmman-network-service.exe'
if (!(Test-Path $service)) { $service = Join-Path $root 'service\target\release\pmman-network-service.exe' }
foreach ($file in @($client, $service)) { if (!(Test-Path $file)) { throw "Missing compiled binary: $file" } }
Copy-Item $client,$service $output -Force
$settings = Join-Path $root 'config\pmman-client.json'
if (Test-Path $settings) { Copy-Item $settings $output -Force }
Write-Output "Standalone package prepared at: $output"
Write-Output "Double-click to run (Windows will show the standard UAC confirmation): $output\pmman-client.exe"
