param(
    [switch]$SkipBuild,
    [switch]$ForceRuntimeDownload,
    [switch]$WithoutWebView2
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$download = Join-Path $PSScriptRoot 'download-windows-runtime.ps1'
if (!$WithoutWebView2) {
    & $download -Force:$ForceRuntimeDownload
}

if (!$SkipBuild) {
    & (Join-Path $PSScriptRoot 'build-client.ps1')
    if ($LASTEXITCODE -ne 0) { throw 'Windows client build failed' }
}

$client = Join-Path $root 'src-tauri\target\release\pmman-client.exe'
$service = Join-Path $root 'src-tauri\binaries\pmman-network-service-x86_64-pc-windows-msvc.exe'
if (!(Test-Path $client)) { throw "Missing release client: $client" }
if (!(Test-Path $service)) { throw "Missing Tauri sidecar: $service" }
if (!(Test-Path (Join-Path $root 'dist'))) { throw 'Missing frontend dist; run pnpm build first' }

$tauri = Join-Path $root 'node_modules\@tauri-apps\cli\tauri.js'
if (!(Test-Path $tauri)) {
    $tauri = Get-ChildItem (Join-Path $root 'node_modules\.pnpm') -Directory -Filter '@tauri-apps+cli@*' |
        ForEach-Object { Join-Path $_.FullName 'node_modules\@tauri-apps\cli\tauri.js' } |
        Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (!$tauri) {
    throw "Tauri CLI is not installed. Run pnpm install --frozen-lockfile on Windows first."
}

Push-Location $root
$originalConfigContent = $null
try {
    # The normal config embeds the pinned runtime installers. The optional
    # no-WebView2 package uses a generated config with only WireGuard and a
    # hook that verifies WebView2 is already installed on the machine.
    $configPath = Join-Path $root 'src-tauri\tauri.conf.json'
    $configArgument = ' --config "{\"build\":{\"beforeBuildCommand\":\"\"}}"'
    if ($WithoutWebView2) {
        $originalConfigContent = Get-Content -Raw -LiteralPath $configPath
        $config = Get-Content -Raw -LiteralPath $configPath | ConvertFrom-Json
        $config.build.beforeBuildCommand = ''
        # The source config is temporarily changed instead of passed as a
        # second object, because Tauri merges object values and would retain
        # the WebView2 resource. Explicit target paths remain available to
        # the service after installation.
        $config.bundle.resources = [ordered]@{
            '../config/pmman-client.json' = 'pmman-client.json'
            '../vendor/windows/wireguard-amd64-0.5.3.msi' = 'runtime/wireguard-amd64-0.5.3.msi'
        }
        $config.bundle.windows.nsis.installerHooks = 'windows/installer-hooks-no-webview2.nsh'
        [IO.File]::WriteAllText($configPath, ($config | ConvertTo-Json -Depth 20), [Text.UTF8Encoding]::new($false))
    }
    # build-client.ps1 builds the frontend in WSL. Do not run a second
    # Windows-side pnpm command here; this also keeps the installer build free
    # of any Node/Rust dependency beyond the already produced artifacts.
    $vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
    if (!(Test-Path $vcvars)) { throw "Visual Studio MSVC environment script not found: $vcvars" }
    $command = 'call "{0}" && set "PATH=%USERPROFILE%\.cargo\bin;!PATH!" && cd /d "{1}" && node "{2}" build --bundles nsis --ci{3}' -f $vcvars, $root, $tauri, $configArgument
    cmd.exe /v:on /d /s /c $command
    if ($LASTEXITCODE -ne 0) { throw 'Tauri NSIS bundle failed' }
} finally {
    if ($originalConfigContent -ne $null) {
        [IO.File]::WriteAllText(
            (Join-Path $root 'src-tauri\tauri.conf.json'),
            $originalConfigContent,
            [Text.UTF8Encoding]::new($false)
        )
    }
    Pop-Location
}

$bundle = Get-ChildItem (Join-Path $root 'src-tauri\target\release\bundle\nsis') -Filter '*.exe' -File |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (!$bundle) { throw 'NSIS installer was not produced' }
$releaseVersion = (Get-Content -Raw (Join-Path $root 'src-tauri\tauri.conf.json') | ConvertFrom-Json).version
$archiveDir = Join-Path $root 'artifacts\windows-installer'
$suffix = if ($WithoutWebView2) { '_no-webview2' } else { '' }
$archive = Join-Path $archiveDir "Pmman Network_${releaseVersion}${suffix}_x64-setup.exe"
New-Item -ItemType Directory -Force -Path $archiveDir | Out-Null
Copy-Item -LiteralPath $bundle.FullName -Destination $archive -Force
$sha = (Get-FileHash $bundle.FullName -Algorithm SHA256).Hash
Write-Output "windows_installer=$($bundle.FullName)"
Write-Output "windows_installer_archive=$archive"
Write-Output "windows_installer_archive_sha256=$((Get-FileHash $archive -Algorithm SHA256).Hash)"
Write-Output "windows_installer_sha256=$sha"
if ($WithoutWebView2) { Write-Output 'windows_installer_webview2=system-required' }
Write-Output 'zero_prep_windows_installer=passed'
