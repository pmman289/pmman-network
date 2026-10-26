param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [switch]$SkipTests,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# Compatibility entry point for the first txgz installation. New releases
# should call build-server-release.ps1 and publish-server-release-txgz.ps1
# separately so CI can retain and promote an immutable artifact.
if (!$SkipBuild) {
    & (Join-Path $PSScriptRoot 'build-server-release.ps1') -Version $Version -SkipTests:$SkipTests
    if ($LASTEXITCODE -ne 0) { throw 'Local server release build failed.' }
}
& (Join-Path $PSScriptRoot 'publish-server-release-txgz.ps1') -Version $Version -Bootstrap
if ($LASTEXITCODE -ne 0) { throw 'Server release publication failed.' }

foreach ($name in @('deployment.json', 'credentials.json')) {
    $destination = Join-Path $root "artifacts\txgz-$($name.Replace('.json','')).json"
    scp -q "txgz:/opt/pmman-network-dev/$name" $destination
    if ($LASTEXITCODE -ne 0) { throw "Failed to download txgz $name." }
}
Write-Output "txgz_release_version=$Version"
Write-Output 'txgz_deploy=passed'
Write-Output 'next_release_use=build-server-release.ps1 then publish-server-release-txgz.ps1'
