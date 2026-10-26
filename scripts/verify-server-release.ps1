param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [string]$Master = 'https://network.pmman.tech'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$remoteRoot = '/opt/pmman-network-dev'
$manifestPath = Join-Path $root "artifacts\server-releases\$Version\release.json"
if (!(Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw "Missing local release manifest: $manifestPath" }
$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
if ($manifest.releaseVersion -ne $Version) { throw 'Local release manifest version mismatch.' }

function Wait-PublicHealth([int]$TimeoutSeconds = 60) {
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        try {
            $health = Invoke-WebRequest -Uri "$Master/healthz" -UseBasicParsing -TimeoutSec 15
            if ($health.StatusCode -eq 200 -and $health.Content.Trim() -eq 'ok') { return }
        } catch {
            # The container can briefly report starting while Docker replaces it.
        }
        Start-Sleep -Seconds 2
    } while ([DateTimeOffset]::UtcNow -lt $deadline)
    throw 'Public master health check did not become ready before timeout.'
}

function Invoke-Public([string]$Uri, [hashtable]$Headers = @{}) {
    $result = Invoke-RestMethod -Uri $Uri -Headers $Headers -TimeoutSec 15
    foreach ($item in @($result)) { Write-Output $item }
}

Wait-PublicHealth
$identity = Invoke-Public "$Master/v1/version"
if ($identity.product -ne 'pmman-network-server' -or $identity.version -ne $Version) { throw 'Running public master version does not match the release artifact.' }

function Remote-Hash([string]$Path) {
    $line = ssh -4 -o BatchMode=yes txgz "sha256sum '$Path'" 2>$null
    if ($LASTEXITCODE -ne 0 -or !$line) { throw "Unable to read remote hash: $Path" }
    return (($line -split '\s+')[0]).ToLowerInvariant()
}

function Remote-Command([string[]]$Arguments) {
    $output = & ssh -4 -o BatchMode=yes txgz @Arguments 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Remote command failed: $($Arguments -join ' ')"
    }
    return @($output)
}

# The master binary is intentionally no longer the production process on the
# host. It runs from the locally-built image, while the node binary remains a
# host service with the privileges required by WireGuard/nftables/BIRD.
$compose = @(
    'docker', 'compose', '--project-name', 'pmman-network',
    '--env-file', "$remoteRoot/.env", '-f', "$remoteRoot/docker-compose.yml"
)
$runningServices = @(Remote-Command ($compose + @('ps', '--services', '--status', 'running')) | Where-Object { $_ -and $_.Trim() })
if ('postgres' -notin $runningServices -or 'master' -notin $runningServices) {
    throw "Production Compose services are not both running: $($runningServices -join ', ')"
}
$configuredImages = @(Remote-Command ($compose + @('config', '--images')) | Where-Object { $_ -and $_.Trim() })
$expectedImage = "pmman-network/master:$Version"
if ($expectedImage -notin $configuredImages) { throw "Compose is not configured with the expected master image: $expectedImage" }
$null = Remote-Command @('docker', 'image', 'inspect', $expectedImage)
$masterId = (Remote-Command ($compose + @('ps', '-q', 'master')) | Select-Object -First 1).Trim()
$postgresId = (Remote-Command ($compose + @('ps', '-q', 'postgres')) | Select-Object -First 1).Trim()
if (!$masterId -or !$postgresId) { throw 'Unable to resolve production Compose container IDs.' }
function Wait-ContainerHealth([string]$ContainerId, [string]$Label) {
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(60)
    $inspectFormat = "'{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}'"
    do {
        $health = (Remote-Command @('docker', 'inspect', '--format', $inspectFormat, $ContainerId) | Select-Object -First 1).Trim()
        if ($health -eq 'healthy') { return }
        if ($health -ne 'starting') { throw "$Label container health is $health" }
        # HTTP readiness can precede Docker's first scheduled health probe.
        Start-Sleep -Seconds 2
    } while ([DateTimeOffset]::UtcNow -lt $deadline)
    throw "$Label container health did not become ready before timeout."
}
Wait-ContainerHealth $masterId 'Master'
Wait-ContainerHealth $postgresId 'PostgreSQL'
$remoteNodeSha = Remote-Hash "$remoteRoot/bin/pmman-node"
$remoteResourceSha = Remote-Hash "$remoteRoot/node-resources/$($manifest.nodeResource.fileName)"
$expectedNode = ([string]$manifest.node.sha256).ToLowerInvariant()
$expectedResource = ([string]$manifest.nodeResource.sha256).ToLowerInvariant()
if ($remoteNodeSha -ne $expectedNode) { throw 'Remote node binary hash does not match the local release.' }
if ($remoteResourceSha -ne $expectedResource) { throw 'Remote node resource hash does not match the local release.' }
if ($manifest.node.pmmandnsSha256) {
    $expectedPmmandns = ([string]$manifest.node.pmmandnsSha256).ToLowerInvariant()
    if ($expectedPmmandns -notmatch '^[0-9a-f]{64}$') { throw 'Node pmmandns SHA-256 is invalid.' }
}

Write-Output "server_release_version=$Version"
Write-Output 'server_release_public_health=passed'
Write-Output 'server_release_compose_health=passed'
Write-Output 'server_release_nodes=administrator-managed-only'
Write-Output 'server_release_remote_hashes=passed'
Write-Output 'server_release_verify=passed'
