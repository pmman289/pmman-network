param(
    [string]$Version = '',
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$triple = 'x86_64-pc-windows-msvc'
$tauriManifest = Join-Path $root 'src-tauri\tauri.conf.json'
if ([string]::IsNullOrWhiteSpace($Version)) {
    if (!(Test-Path -LiteralPath $tauriManifest)) { throw "Tauri manifest not found: $tauriManifest" }
    $Version = (Get-Content -Raw -LiteralPath $tauriManifest | ConvertFrom-Json).version
}
$displayVersion = if ($Version -eq '0.0.1-alpha') { '0.0.1a' } else { $Version }
$binDir = Join-Path $root 'src-tauri\binaries'
$outputDir = Join-Path $root 'artifacts\client-updates'
$updateConfig = Join-Path $root "artifacts\tauri-update-$Version.json"
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') { throw 'Version must be a valid SemVer such as 0.0.1-alpha' }
if (!(Test-Path $vcvars)) { throw "Visual Studio MSVC environment script not found: $vcvars" }

$running = @(Get-Process -Name pmman-client,pmman-network-service -ErrorAction SilentlyContinue)
if ($running) { throw 'Close Pmman Network before building an update package.' }
$tunnel = Get-Service -Name 'WireGuardTunnel$pmman-client' -ErrorAction SilentlyContinue
if ($tunnel -and $tunnel.Status -ne 'Stopped') { throw 'Disconnect PMMan before building an update package.' }

if (!$SkipBuild) {
    # Keep the Windows frontend and Tauri optional native bindings in the
    # Windows node_modules tree. WSL is reserved for Linux master/node builds;
    # sharing one node_modules tree with WSL can remove the Windows Tauri CLI
    # binding before the NSIS step.
    Push-Location $root
    try { pnpm build } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
    $env:PMMAN_CLIENT_VERSION = $Version
    cmd.exe /v:on /d /s /c ('call "{0}" && set "PATH=%USERPROFILE%\.cargo\bin;!PATH!" && set "RUSTFLAGS=-C target-feature=+crt-static" && set "PMMAN_CLIENT_VERSION={1}" && set "PMMAN_CLIENT_DISPLAY_VERSION={2}" && cd /d {3} && cargo build --release --locked --manifest-path service\Cargo.toml --target {4}' -f $vcvars, $Version, $displayVersion, $root, $triple)
    if ($LASTEXITCODE -ne 0) { throw 'Windows service build failed' }
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item (Join-Path $root "service\target\$triple\release\pmman-network-service.exe") (Join-Path $binDir "pmman-network-service-$triple.exe") -Force
    cmd.exe /v:on /d /s /c ('call "{0}" && set "PATH=%USERPROFILE%\.cargo\bin;!PATH!" && set "RUSTFLAGS=-C target-feature=+crt-static" && set "PMMAN_CLIENT_VERSION={1}" && set "PMMAN_CLIENT_DISPLAY_VERSION={2}" && cd /d {3} && cargo build --release --locked --manifest-path src-tauri\Cargo.toml --bin pmman-client' -f $vcvars, $Version, $displayVersion, $root)
    if ($LASTEXITCODE -ne 0) { throw 'Tauri client build failed' }
}

$tauri = Join-Path $root 'node_modules\@tauri-apps\cli\tauri.js'
if (!(Test-Path $tauri)) {
    $tauri = Get-ChildItem (Join-Path $root 'node_modules\.pnpm') -Directory -Filter '@tauri-apps+cli@*' |
        ForEach-Object { Join-Path $_.FullName 'node_modules\@tauri-apps\cli\tauri.js' } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (!$tauri) { throw 'Tauri CLI not found' }
$config = Get-Content -Raw (Join-Path $root 'src-tauri\tauri-update.conf.json') | ConvertFrom-Json
$config.version = $Version
[IO.File]::WriteAllText(
    $updateConfig,
    ($config | ConvertTo-Json -Depth 20),
    [Text.UTF8Encoding]::new($false)
)
Push-Location $root
try {
    $command = 'call "{0}" && set "PATH=%USERPROFILE%\.cargo\bin;!PATH!" && set "PMMAN_CLIENT_VERSION={1}" && set "PMMAN_CLIENT_DISPLAY_VERSION={2}" && cd /d "{3}" && node "{4}" build --config "{5}" --bundles nsis --ci' -f $vcvars, $Version, $displayVersion, $root, $tauri, $updateConfig
    cmd.exe /v:on /d /s /c $command
    if ($LASTEXITCODE -ne 0) { throw 'Lean NSIS update bundle failed' }
} finally { Pop-Location }

$bundle = Get-ChildItem (Join-Path $root 'src-tauri\target\release\bundle\nsis') -Filter '*.exe' -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (!$bundle) { throw 'Lean update installer was not produced' }
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
$target = Join-Path $outputDir "Pmman-Network-$Version-x64-update.exe"
Copy-Item $bundle.FullName $target -Force
$hash = (Get-FileHash $target -Algorithm SHA256).Hash
$size = (Get-Item $target).Length
Write-Output "client_update_installer=$target"
Write-Output "client_update_version=$Version"
Write-Output "client_update_size=$size"
Write-Output "client_update_sha256=$hash"
Write-Output 'client_update_package=passed'
