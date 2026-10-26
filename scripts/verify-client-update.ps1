param(
    [string]$Master = 'https://network.pmman.tech',
    [string]$Version = '0.0.1-alpha'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$oldTarget = Join-Path $root 'artifacts\verify-client-update-old'
$triple = 'x86_64-pc-windows-msvc'
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
$service = Join-Path $oldTarget "$triple\release\pmman-network-service.exe"

if (!(Test-Path $service)) {
    cmd.exe /v:on /d /s /c ('call "{0}" && set "PATH=%USERPROFILE%\.cargo\bin;!PATH!" && set "RUSTFLAGS=-C target-feature=+crt-static" && cd /d {1} && cargo build --release --locked --target {2} --target-dir artifacts\verify-client-update-old --manifest-path service\Cargo.toml' -f $vcvars, $root, $triple)
    if ($LASTEXITCODE -ne 0) { throw 'Old-version service build failed' }
}

$latest = Invoke-RestMethod -Uri "$Master/v1/client/releases/stable/latest?platform=windows&arch=x64" -TimeoutSec 15
if (!$latest.available -or $latest.manifest.version -ne $Version) { throw 'Published client update was not returned by the master' }
$downloadPath = Join-Path $root "artifacts\client-updates\downloaded-$Version.exe"
Invoke-WebRequest -Uri "$Master$($latest.download)" -OutFile $downloadPath -UseBasicParsing
$hash = (Get-FileHash $downloadPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($hash -ne $latest.manifest.sha256 -or (Get-Item $downloadPath).Length -ne [int64]$latest.manifest.size) {
    throw 'Public update download failed SHA-256 or size verification'
}

$info = [Diagnostics.ProcessStartInfo]::new()
$info.FileName = $service
$info.UseShellExecute = $false
$info.CreateNoWindow = $true
$info.Environment['PMMAN_CONTROL_PLANE'] = $Master
$info.Environment['PMMAN_CLIENT_VERSION'] = '0.0.1-alpha'
$process = [Diagnostics.Process]::Start($info)
function Ipc([string]$Action) {
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', 'pmman-network', [IO.Pipes.PipeDirection]::InOut)
    try {
        $pipe.Connect(10000)
        $writer = [IO.StreamWriter]::new($pipe)
        $writer.AutoFlush = $true
        $reader = [IO.StreamReader]::new($pipe)
        $writer.WriteLine((@{ action = $Action; update_channel = 'stable' } | ConvertTo-Json -Compress))
        $line = $reader.ReadLine()
        if (!$line) { throw 'Empty IPC response' }
        $response = $line | ConvertFrom-Json
        if (!$response.ok) { throw $response.status.message }
        return $response.status
    } finally { $pipe.Dispose() }
}

try {
    $status = Ipc 'check_update'
    if ($status.clientUpdate.availableVersion -ne $Version) { throw 'IPC update check did not return the published version' }
    $status = Ipc 'download_update'
    if (!$status.clientUpdate.ready -or !(Test-Path $status.clientUpdate.installerPath)) { throw 'IPC update download did not produce a ready installer' }
    $readyHash = (Get-FileHash $status.clientUpdate.installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($readyHash -ne $latest.manifest.sha256) { throw 'Service-cached installer hash mismatch' }
    Write-Output "client_update_public_download=passed"
    Write-Output "client_update_ipc_check=passed"
    Write-Output "client_update_ipc_download=passed"
    Write-Output "client_update_version=$Version"
    Write-Output "client_update_sha256=$readyHash"
} finally {
    if ($process -and !$process.HasExited) { $process.Kill(); $process.WaitForExit() }
}
