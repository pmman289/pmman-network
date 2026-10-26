param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$vendor = Join-Path $root 'vendor\windows'
New-Item -ItemType Directory -Force -Path $vendor | Out-Null

# These are deliberately pinned. The hashes make a build fail closed if an
# upstream URL is changed or intercepted; update both the version and hash in
# this file when refreshing a dependency.
$packages = @(
    @{
        Name = 'wireguard-amd64-0.5.3.msi'
        Url = 'https://download.wireguard.com/windows-client/wireguard-amd64-0.5.3.msi'
        Sha256 = '76FCEC042C5989C5B816CD32EAED1E5B1C3B998A4B1C9ECA55F299E3314EF7E4'
    },
    @{
        # Microsoft Edge WebView2 Evergreen Standalone Installer (x64).
        # LinkId=2124701 is the official full offline installer; do not replace
        # this with LinkId=2124703, which is the ~2 MB online bootstrapper.
        Name = 'MicrosoftEdgeWebView2RuntimeInstallerX64.exe'
        Channel = 'Evergreen Standalone x64'
        Url = 'https://go.microsoft.com/fwlink/?linkid=2124701'
        Sha256 = 'F6DF8E4BC857786FF641CD01DA1449169EAF8236C936CED485EA61685BA4DA40'
    }
)

foreach ($package in $packages) {
    $target = Join-Path $vendor $package.Name
    $valid = Test-Path $target -PathType Leaf
    if ($valid) {
        $actual = (Get-FileHash $target -Algorithm SHA256).Hash.ToUpperInvariant()
        $valid = $actual -eq $package.Sha256
        if (!$valid -and !$Force) {
            throw "Pinned runtime hash mismatch for $target. Delete it or rerun with -Force. expected=$($package.Sha256) actual=$actual"
        }
    }
    if (!$valid) {
        $partial = "$target.download"
        Remove-Item -LiteralPath $partial -Force -ErrorAction SilentlyContinue
        Write-Host "Downloading $($package.Name)..."
        & curl.exe -L --fail --retry 3 --connect-timeout 20 --max-time 600 -o $partial $package.Url
        if ($LASTEXITCODE -ne 0) { throw "Download failed: $($package.Url)" }
        $actual = (Get-FileHash $partial -Algorithm SHA256).Hash.ToUpperInvariant()
        if ($actual -ne $package.Sha256) {
            Remove-Item -LiteralPath $partial -Force -ErrorAction SilentlyContinue
            throw "Downloaded runtime hash mismatch for $($package.Name). expected=$($package.Sha256) actual=$actual"
        }
        Move-Item -LiteralPath $partial -Destination $target -Force
    }
    $file = Get-Item -LiteralPath $target
    $channel = if ($package.Channel) { $package.Channel } else { $package.Name }
    Write-Output "runtime=$($file.Name) channel=$channel sha256=$($package.Sha256) bytes=$($file.Length)"
}
