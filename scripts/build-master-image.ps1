param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [switch]$SkipTests
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$linuxRoot = '/mnt/' + $root.Substring(0, 1).ToLowerInvariant() + $root.Substring(2).Replace('\', '/')
$target = 'x86_64-unknown-linux-musl'

function Invoke-Checked([string]$Label, [scriptblock]$Action) {
    & $Action
    if ($LASTEXITCODE -ne 0) { throw "$Label failed (exit code $LASTEXITCODE)" }
}

Push-Location $root
try {
    if (!$SkipTests) {
        Invoke-Checked 'Master tests' {
            ssh -o BatchMode=yes wsl "cd '$linuxRoot' && . /root/.cargo/env && cargo test --locked --manifest-path server/Cargo.toml --target $target"
        }
    }
    Invoke-Checked 'Master musl build' {
        ssh -o BatchMode=yes wsl "cd '$linuxRoot' && . /root/.cargo/env && PMMAN_SERVER_VERSION='$Version' cargo build --release --locked --manifest-path server/Cargo.toml --target $target"
    }
    $binary = Join-Path $root "server\target\$target\release\pmman-control-plane"
    if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { throw "Missing master binary: $binary" }
    Invoke-Checked 'Docker master image build' {
        docker build --file docker/master.Dockerfile --tag "pmman-network/master:$Version" --tag 'pmman-network/master:local' .
    }
    Write-Output "master_image=pmman-network/master:$Version"
    Write-Output 'master_image_build=passed'
}
finally {
    Pop-Location
}
