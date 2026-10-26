$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$statePath = Join-Path $root 'artifacts\real-lab.json'
if (!(Test-Path $statePath)) { Write-Output 'real_lab=not_running'; exit 0 }
$state = Get-Content -Raw -LiteralPath $statePath | ConvertFrom-Json
$routeTable = if ($state.RouteTable) { [string]$state.RouteTable } else { '51820' }
# Match only the lab's exact process names/arguments; do not use broad pkill
# patterns that can match the SSH command currently executing this script.
$remoteCleanup = @'
for pid in $(pgrep -x pmman-control-p 2>/dev/null); do sudo kill "$pid" 2>/dev/null || true; done
for pid in $(pgrep -x pmman-node 2>/dev/null); do sudo kill "$pid" 2>/dev/null || true; done
sudo fuser -k 8080/tcp 2>/dev/null || true
sudo fuser -k 8081/tcp 2>/dev/null || true
sudo fuser -k 8090/tcp 8091/tcp 2>/dev/null || true
sudo fuser -k 51821/udp 2>/dev/null || true
if test -f /tmp/pmman-pmmandns.pid; then sudo kill "$(cat /tmp/pmman-pmmandns.pid)" 2>/dev/null || true; rm -f /tmp/pmman-pmmandns.pid; fi
for pid in $(pgrep -x pmmandns 2>/dev/null); do sudo kill "$pid" 2>/dev/null || true; done
sudo fuser -k 53/tcp 53/udp 2>/dev/null || true
sudo ip -4 route flush table __ROUTE_TABLE__ 2>/dev/null || true
sudo ip -6 route flush table __ROUTE_TABLE__ 2>/dev/null || true
sudo wg-quick down __LAB_DIR__/pmman0.conf 2>/dev/null || true
sudo rm -f __LAB_DIR__/pmman0.conf /tmp/pmman-gateway-http.pid
sudo rmdir __LAB_DIR__ 2>/dev/null || true
'@
$remoteCleanup = $remoteCleanup.Replace('__LAB_DIR__', $state.LabDir)
$remoteCleanup = $remoteCleanup.Replace('__ROUTE_TABLE__', $routeTable)
ssh -o BatchMode=yes wsl $remoteCleanup | Out-Null
ssh -o BatchMode=yes wsl 'if test -f /tmp/pmman-udp-relay-51821.pid; then kill "$(cat /tmp/pmman-udp-relay-51821.pid)" 2>/dev/null || true; rm -f /tmp/pmman-udp-relay-51821.pid; fi' | Out-Null
Remove-Item -LiteralPath $statePath -Force
Write-Output 'real_lab=stopped'
