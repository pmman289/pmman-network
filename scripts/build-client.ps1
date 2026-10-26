$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$triple = 'x86_64-pc-windows-msvc'
$binDir = Join-Path $root 'src-tauri\binaries'
$clientManifest = Join-Path $root 'src-tauri\Cargo.toml'
$clientVersionLine = Get-Content -LiteralPath $clientManifest | Where-Object { $_ -match '^\s*version\s*=\s*"([^"]+)"' } | Select-Object -First 1
$clientVersion = $null
if ($clientVersionLine -and $clientVersionLine -match '^\s*version\s*=\s*"([^"]+)"') {
    $clientVersion = $Matches[1]
}
if (!$clientVersion) {
    throw "Cannot determine the Tauri client version from $clientManifest"
}
$displayVersion = if ($env:PMMAN_CLIENT_DISPLAY_VERSION) { $env:PMMAN_CLIENT_DISPLAY_VERSION } else { $clientVersion }
$running = @(Get-Process -Name pmman-client,pmman-network-service -ErrorAction SilentlyContinue)
if ($running) {
    $ids = ($running | ForEach-Object { "$($_.ProcessName)#$($_.Id)" }) -join ', '
    throw "Cannot build while the PMMan client or service is running: $ids. Disconnect and close the client first."
}
$tunnel = Get-Service -Name 'WireGuardTunnel$pmman-client' -ErrorAction SilentlyContinue
if ($tunnel -and $tunnel.Status -ne 'Stopped') {
    throw 'Cannot build while WireGuardTunnel$pmman-client is installed. Disconnect the client first.'
}
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
cmd.exe /v:on /d /s /c ('call "{0}" && set "PATH=%USERPROFILE%\.cargo\bin;!PATH!" && set "RUSTFLAGS=-C target-feature=+crt-static" && set "PMMAN_CLIENT_VERSION={2}" && set "PMMAN_CLIENT_DISPLAY_VERSION={3}" && cd /d {1} && cargo build --manifest-path service\Cargo.toml --release --target {4}' -f $vcvars, $root, $clientVersion, $displayVersion, $triple)
if ($LASTEXITCODE -ne 0) { throw 'Windows service build failed' }
$service = Join-Path $root "service\target\$triple\release\pmman-network-service.exe"
if (!(Test-Path $service)) { throw "Service binary not found: $service" }
Copy-Item $service (Join-Path $binDir "pmman-network-service-$triple.exe") -Force

$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
Push-Location $root
try {
    # Windows desktop builds must keep the Windows Tauri optional native
    # bindings. Build the frontend locally; WSL is used only for Linux
    # services and nodes.
    pnpm build
    if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
    cmd.exe /v:on /d /s /c ('call "{0}" && set "PATH=%USERPROFILE%\.cargo\bin;!PATH!" && set "RUSTFLAGS=-C target-feature=+crt-static" && cd /d {1} && cargo build --manifest-path src-tauri\Cargo.toml --release --bin pmman-client' -f $vcvars, $root)
    if ($LASTEXITCODE -ne 0) { throw 'Tauri Rust build failed' }
    $releaseClient = Join-Path $root 'src-tauri\target\release\pmman-client.exe'
    if (!(Test-Path $releaseClient)) { throw "Release client binary not found: $releaseClient" }
    Copy-Item src-tauri\target\release\pmman-client.exe . -Force
    Write-Output 'client_rust_build=passed'
} finally { Pop-Location }
Write-Output 'client_build=passed'
