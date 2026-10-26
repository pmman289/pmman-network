param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [ValidateSet('stable', 'beta')]
    [string]$Channel = 'stable',
    [string]$Master = 'https://network.pmman.tech',
    [string]$ArtifactPath
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (!$ArtifactPath) { $ArtifactPath = Join-Path $root "artifacts\client-updates\Pmman-Network-$Version-x64-update.exe" }
$artifact = (Resolve-Path -LiteralPath $ArtifactPath -ErrorAction Stop).Path
$localSize = (Get-Item -LiteralPath $artifact).Length
$localHash = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()

$latest = Invoke-RestMethod -Uri "$Master/v1/client/releases/$Channel/latest?platform=windows&arch=x64" -TimeoutSec 15
if (!$latest.available -or $latest.manifest.version -ne $Version) { throw 'Public master did not return the expected client release.' }
$manifest = $latest.manifest
if ([int64]$manifest.size -ne [int64]$localSize -or ([string]$manifest.sha256).ToLowerInvariant() -ne $localHash) {
    throw 'Published client manifest does not match the local artifact.'
}

$download = Join-Path $root "artifacts\client-updates\verify-$Channel-$Version.exe"
try {
    Invoke-WebRequest -Uri "$Master/v1/client/releases/$Channel/$Version/download" -OutFile $download -TimeoutSec 300
    $downloadSize = (Get-Item -LiteralPath $download).Length
    $downloadHash = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($downloadSize -ne $localSize -or $downloadHash -ne $localHash) { throw 'Public client package download failed SHA-256/size verification.' }
}
finally {
    Remove-Item -LiteralPath $download -Force -ErrorAction SilentlyContinue
}

Write-Output "client_release_channel=$Channel"
Write-Output "client_release_version=$Version"
Write-Output 'client_release_manifest=passed'
Write-Output 'client_release_download=passed'
Write-Output 'client_release_verify=passed'
