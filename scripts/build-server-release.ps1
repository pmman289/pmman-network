param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$target = 'x86_64-unknown-linux-musl'
$linuxRoot = '/mnt/' + $root.Substring(0, 1).ToLowerInvariant() + $root.Substring(2).Replace('\', '/')
$cargoEnv = '. /root/.cargo/env'
$releaseDir = Join-Path $root "artifacts\server-releases\$Version"
$serverTarget = Join-Path $root "server\target\$target\release"
$pmmandnsSource = Join-Path $root 'vendor\pmmandns'
$pmmandnsBinary = Join-Path $pmmandnsSource 'pmmandns-linux-amd64'
if (Test-Path -LiteralPath (Join-Path $releaseDir 'release.json')) { throw 'Release version already exists; use a new version instead of overwriting an artifact.' }

function Invoke-Checked([string]$Label, [scriptblock]$Action) {
    & $Action
    if ($LASTEXITCODE -ne 0) { throw "$Label failed (exit code $LASTEXITCODE)" }
}

New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null

# Linux builds are intentionally executed in the local WSL checkout. The
# remote txgz host only receives finished artifacts from this directory.
Invoke-Checked 'Local WSL admin frontend build' {
    ssh -o BatchMode=yes wsl "cd '$linuxRoot' && pnpm --dir admin build"
}
Invoke-Checked 'Local pmmandns static build' {
    ssh -o BatchMode=yes wsl "cd '$linuxRoot/vendor/pmmandns' && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -buildvcs=false -trimpath -ldflags='-s -w' -o pmmandns-linux-amd64 ./cmd/pmmandns"
}
if (!(Test-Path -LiteralPath $pmmandnsBinary -PathType Leaf)) { throw "Missing pmmandns build artifact: $pmmandnsBinary" }
if (!$SkipTests) {
    Invoke-Checked 'Local pmmandns tests' {
        ssh -o BatchMode=yes wsl "cd '$linuxRoot/vendor/pmmandns' && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go test ./..."
    }
    Invoke-Checked 'Local server tests' {
        ssh -o BatchMode=yes wsl "cd '$linuxRoot' && $cargoEnv && cargo test --locked --manifest-path server/Cargo.toml --target $target"
    }
}
Invoke-Checked 'Local server release build' {
    ssh -o BatchMode=yes wsl "cd '$linuxRoot' && $cargoEnv && PMMAN_SERVER_VERSION='$Version' cargo build --release --locked --manifest-path server/Cargo.toml --target $target"
}
Invoke-Checked 'Local legacy migration build' {
    ssh -o BatchMode=yes wsl "cd '$linuxRoot' && $cargoEnv && cargo build --release --locked --features legacy-migration --bin pmman-migrate --manifest-path server/Cargo.toml --target $target"
}

$master = Join-Path $serverTarget 'pmman-control-plane'
$node = Join-Path $serverTarget 'pmman-node'
$migrator = Join-Path $serverTarget 'pmman-migrate'
if (!(Test-Path -LiteralPath $master -PathType Leaf)) { throw "Missing master artifact: $master" }
if (!(Test-Path -LiteralPath $node -PathType Leaf)) { throw "Missing node artifact: $node" }
if (!(Test-Path -LiteralPath $migrator -PathType Leaf)) { throw "Missing migration artifact: $migrator" }

# The master is deployed as a container. The image is assembled locally from
# the already-built musl binary; txgz only receives and loads this tarball.
$imageTag = "pmman-network/master:$Version"
$imageTar = Join-Path $releaseDir "pmman-master-$Version.tar"
if (Test-Path -LiteralPath $imageTar) { Remove-Item -LiteralPath $imageTar -Force }
$linuxImageTar = $linuxRoot + "/artifacts/server-releases/$Version/pmman-master-$Version.tar"
$windowsDocker = Get-Command docker -ErrorAction SilentlyContinue
if ($windowsDocker) {
    Invoke-Checked 'Local master image build' {
        docker build --file (Join-Path $root 'docker\master.Dockerfile') --tag $imageTag --tag 'pmman-network/master:local' $root
    }
    Invoke-Checked 'Local master image export' { docker save --output $imageTar $imageTag }
} else {
    # Docker Desktop is not required on the Windows build host. WSL owns the
    # local Docker daemon and sees this checkout through /mnt/<drive>.
    Invoke-Checked 'WSL master image build' {
        ssh -o BatchMode=yes wsl "cd '$linuxRoot' && docker build --file docker/master.Dockerfile --tag '$imageTag' --tag 'pmman-network/master:local' ."
    }
    Invoke-Checked 'WSL master image export' {
        ssh -o BatchMode=yes wsl "docker save --output '$linuxImageTar' '$imageTag'"
    }
}

Copy-Item -LiteralPath $master -Destination (Join-Path $releaseDir 'pmman-control-plane') -Force
Copy-Item -LiteralPath $node -Destination (Join-Path $releaseDir 'pmman-node') -Force
Copy-Item -LiteralPath $migrator -Destination (Join-Path $releaseDir 'pmman-migrate') -Force
# The node self-update endpoint serves this resource. Keep the payload name
# versioned while the systemd executable remains /bin/pmman-node.
$nodeResourceName = "pmman-node-$Version"
Copy-Item -LiteralPath $node -Destination (Join-Path $releaseDir $nodeResourceName) -Force
foreach ($helper in @('activate-server-release.py', 'provision.py', 'probe.py')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "txgz\$helper") -Destination (Join-Path $releaseDir $helper)
}

function File-Entry([string]$Path, [string]$Name) {
    $item = Get-Item -LiteralPath $Path
    [ordered]@{
        fileName = $Name
        size = [int64]$item.Length
        sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

$nodeManifest = File-Entry (Join-Path $releaseDir 'pmman-node') 'pmman-node'
$nodeManifest['pmmandnsSha256'] = (Get-FileHash -LiteralPath $pmmandnsBinary -Algorithm SHA256).Hash.ToLowerInvariant()
$manifest = [ordered]@{
    schemaVersion = 1
    product = 'pmman-network-server'
    releaseVersion = $Version
    target = $target
    builtAt = [DateTimeOffset]::UtcNow.ToString('o')
    master = File-Entry (Join-Path $releaseDir 'pmman-control-plane') 'pmman-control-plane'
    migrator = File-Entry (Join-Path $releaseDir 'pmman-migrate') 'pmman-migrate'
    masterImage = File-Entry $imageTar "pmman-master-$Version.tar"
    node = $nodeManifest
    nodeResource = File-Entry (Join-Path $releaseDir $nodeResourceName) $nodeResourceName
    helpers = @('activate-server-release.py', 'provision.py', 'probe.py') | ForEach-Object { File-Entry (Join-Path $releaseDir $_) $_ }
}
$manifestPath = Join-Path $releaseDir 'release.json'
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))

$archive = Join-Path $releaseDir "pmman-server-node-$Version.tar.gz"
if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
Invoke-Checked 'Server release archive' {
    tar.exe -czf $archive -C $releaseDir pmman-control-plane pmman-node pmman-migrate $nodeResourceName (Split-Path -Leaf $imageTar) release.json activate-server-release.py provision.py probe.py
}

$archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
$archiveSize = (Get-Item -LiteralPath $archive).Length
Write-Output "server_release_dir=$releaseDir"
Write-Output "server_release_manifest=$manifestPath"
Write-Output "server_release_archive=$archive"
Write-Output "server_release_archive_size=$archiveSize"
Write-Output "server_release_archive_sha256=$archiveHash"
Write-Output "server_release_version=$Version"
Write-Output 'server_release_build=passed'
