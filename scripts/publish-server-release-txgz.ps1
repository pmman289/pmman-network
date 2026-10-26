param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [string]$ArtifactDir,
    [switch]$Bootstrap,
    [switch]$DryRun,
    [switch]$SkipHealth
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$remoteRoot = '/opt/pmman-network-dev'
$artifact = if ($ArtifactDir) { (Resolve-Path -LiteralPath $ArtifactDir).Path } else { Join-Path $root "artifacts\server-releases\$Version" }

if (!(Test-Path -LiteralPath $artifact -PathType Container)) { throw "Missing server release artifact directory: $artifact" }
$manifestPath = Join-Path $artifact 'release.json'
if (!(Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw "Missing server release manifest: $manifestPath" }
$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
if ($manifest.product -ne 'pmman-network-server' -or $manifest.releaseVersion -ne $Version) {
    throw 'Server release manifest product/version mismatch.'
}
if ($manifest.target -ne 'x86_64-unknown-linux-musl') { throw 'Server release target must be x86_64-unknown-linux-musl.' }

function Assert-Payload($entry) {
    $name = [string]$entry.fileName
    if (!$name -or $name -notmatch '^[A-Za-z0-9_.-]+$' -or $name -in @('.', '..')) { throw "Invalid release payload name: $name" }
    $path = Join-Path $artifact $name
    if (!(Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing release payload: $path" }
    $item = Get-Item -LiteralPath $path
    $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ([int64]$item.Length -ne [int64]$entry.size -or $hash -ne ([string]$entry.sha256).ToLowerInvariant()) {
        throw "SHA-256/size mismatch for release payload: $name"
    }
    return $path
}

$master = Assert-Payload $manifest.master
$masterImage = Assert-Payload $manifest.masterImage
$migrator = Assert-Payload $manifest.migrator
$node = Assert-Payload $manifest.node
$resource = Assert-Payload $manifest.nodeResource
if ($manifest.master.fileName -ne 'pmman-control-plane' -or $manifest.node.fileName -ne 'pmman-node' -or $manifest.nodeResource.fileName -ne "pmman-node-$Version") { throw 'Manifest payload names do not match the server release contract.' }
$helpers = @($manifest.helpers)
if ($helpers.Count -ne 3 -or @($helpers.fileName | Sort-Object -Unique).Count -ne 3) { throw 'Missing/duplicate deployment helpers in artifact.' }
foreach ($helper in $helpers) { $null = Assert-Payload $helper }
foreach ($name in @('activate-server-release.py', 'provision.py', 'probe.py')) {
    if ($name -notin $helpers.fileName) { throw "Missing helper: $name" }
}
$releaseArchive = Join-Path $artifact "pmman-server-node-$Version.tar.gz"
if (!(Test-Path -LiteralPath $releaseArchive -PathType Leaf)) {
    throw "Missing reproducible release archive: $releaseArchive"
}

if ($DryRun) {
    Write-Output "server_release_artifact=$artifact"
    Write-Output "server_release_manifest_sha256=$((Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant())"
    Write-Output 'server_release_publish=dry-run-passed'
    return
}

$stamp = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds().ToString()
$stage = "$remoteRoot/.staging/server-$Version-$stamp"
ssh -4 -o BatchMode=yes txgz "mkdir -p '$stage' '$remoteRoot/scripts/txgz' && chmod 700 '$stage'"
if ($LASTEXITCODE -ne 0) { throw 'Remote release staging preparation failed.' }
try {
    scp -4 -q $manifestPath "txgz:$stage/release.json"
    if ($LASTEXITCODE -ne 0) { throw 'Server release manifest upload failed.' }
    scp -4 -q $master "txgz:$stage/$([IO.Path]::GetFileName($master))"
    if ($LASTEXITCODE -ne 0) { throw 'Master binary upload failed.' }
    scp -4 -q $masterImage "txgz:$stage/$([IO.Path]::GetFileName($masterImage))"
    if ($LASTEXITCODE -ne 0) { throw 'Master image upload failed.' }
    scp -4 -q $migrator "txgz:$stage/$([IO.Path]::GetFileName($migrator))"
    if ($LASTEXITCODE -ne 0) { throw 'Migration binary upload failed.' }
    scp -4 -q $node "txgz:$stage/$([IO.Path]::GetFileName($node))"
    if ($LASTEXITCODE -ne 0) { throw 'Node binary upload failed.' }
    scp -4 -q $resource "txgz:$stage/$([IO.Path]::GetFileName($resource))"
    if ($LASTEXITCODE -ne 0) { throw 'Node resource upload failed.' }
    foreach ($helper in $helpers) {
        scp -4 -q (Join-Path $artifact $helper.fileName) "txgz:$stage/$($helper.fileName)"
        if ($LASTEXITCODE -ne 0) { throw 'Artifact deployment helper upload failed.' }
    }
    if ($Bootstrap) {
        $clientKey = Join-Path $root 'artifacts\release-keys\client.key'
        if (!(Test-Path -LiteralPath $clientKey -PathType Leaf)) { throw "Missing client release signing key for bootstrap: $clientKey" }
        scp -4 -q $clientKey "txgz:$remoteRoot/client-release.key"
        if ($LASTEXITCODE -ne 0) { throw 'Bootstrap client release key upload failed.' }
        ssh -4 -o BatchMode=yes txgz "chmod 600 '$remoteRoot/client-release.key'"
        if ($LASTEXITCODE -ne 0) { throw 'Bootstrap client release key permission update failed.' }
    }
    $bootstrapArgument = if ($Bootstrap) { ' --bootstrap' } else { '' }
    ssh -4 -o BatchMode=yes txgz "python3 '$stage/activate-server-release.py' '$stage' '$Version'$bootstrapArgument"
    if ($LASTEXITCODE -ne 0) { throw 'Remote server release activation failed; previous release rollback was attempted.' }
}
catch {
    throw "Server publication failed; project staging retained at ${stage}. $($_.Exception.Message)"
}

if (!$SkipHealth) {
    $credentialPath = Join-Path $root 'artifacts\txgz-credentials.json'
    if (!(Test-Path -LiteralPath $credentialPath -PathType Leaf)) {
        if (!$Bootstrap) { throw "Missing acceptance credentials: $credentialPath" }
        scp -4 -q "txgz:$remoteRoot/credentials.json" $credentialPath
        if ($LASTEXITCODE -ne 0) { throw 'Unable to retrieve txgz acceptance credentials after bootstrap.' }
    }
    & (Join-Path $PSScriptRoot 'verify-server-release.ps1') -Version $Version
    if ($LASTEXITCODE -ne 0) { throw 'Public server release verification failed.' }
}
Write-Output "server_release_version=$Version"
Write-Output 'server_release_publish=passed'
