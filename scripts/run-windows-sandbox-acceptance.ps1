param(
    [switch]$Elevated
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$resultDir = Join-Path $root 'artifacts\windows-sandbox'
$inputDir = Join-Path $root 'artifacts\windows-sandbox-input'
$sandboxScriptDir = Join-Path $PSScriptRoot 'windows-sandbox'
$installerDir = Join-Path $root 'artifacts\windows-installer'
$credentialsFile = Join-Path $root 'artifacts\txgz-credentials.json'
$wsb = Join-Path $resultDir 'pmman-zero-prep.wsb'
$result = Join-Path $resultDir 'acceptance.json'

if (!(Test-Path (Join-Path $installerDir 'PMMan-Network-Client-Setup.exe'))) {
    throw 'Missing final installer. Build artifacts/windows-installer first.'
}
if (!(Test-Path $credentialsFile)) {
    throw 'Missing ignored public acceptance credentials under artifacts.'
}
New-Item -ItemType Directory -Force -Path $resultDir | Out-Null
New-Item -ItemType Directory -Force -Path $inputDir | Out-Null
$credentials = Get-Content -LiteralPath $credentialsFile -Raw | ConvertFrom-Json
@{ client_user = $credentials.client_user; client_password = $credentials.client_password } |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $inputDir 'txgz-credentials.json') -Encoding utf8
Remove-Item -LiteralPath $result -Force -ErrorAction SilentlyContinue

# Launch in the interactive user's session. Elevating the Sandbox host can
# place its window on a different desktop and prevents visual acceptance.
if (!(Get-Command WindowsSandbox.exe -ErrorAction SilentlyContinue)) {
    throw 'Windows Sandbox is unavailable; enable its optional Windows feature first.'
}
$wsbCli = (Get-Command wsb.exe -ErrorAction SilentlyContinue).Path
if (!$wsbCli) {
    throw 'The Store Windows Sandbox wsb CLI is unavailable; install or upgrade Windows Sandbox first.'
}

function Get-RunningSandboxIds {
    $payload = & $wsbCli list --raw | ConvertFrom-Json
    @($payload.WindowsSandboxEnvironments | ForEach-Object { [string]$_.Id })
}

$existingSandboxIds = @(Get-RunningSandboxIds)
$sandboxId = $null

$configuration = @"
<Configuration>
  <MemoryInMB>4096</MemoryInMB>
  <Networking>Enable</Networking>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$installerDir</HostFolder>
      <SandboxFolder>C:\pmman-installer</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$sandboxScriptDir</HostFolder>
      <SandboxFolder>C:\pmman-test</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$inputDir</HostFolder>
      <SandboxFolder>C:\pmman-secrets</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$resultDir</HostFolder>
      <SandboxFolder>C:\pmman-results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>cmd.exe /c powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File C:\pmman-test\run-acceptance.ps1 &gt; C:\pmman-results\bootstrap.log 2&gt;&amp;1</Command>
  </LogonCommand>
</Configuration>
"@
$configuration | Set-Content -LiteralPath $wsb -Encoding utf8

try {
    Write-Output "windows_sandbox_config=$wsb"
    Write-Output 'Starting Windows Sandbox. The host will wait up to 8 minutes for the real connection result.'
    $sandbox = Start-Process -FilePath 'WindowsSandbox.exe' -ArgumentList "`"$wsb`"" -PassThru
    $deadline = (Get-Date).AddMinutes(8)
    while ((Get-Date) -lt $deadline -and !(Test-Path $result)) {
        if (!$sandboxId) {
            $newSandboxIds = @(Get-RunningSandboxIds | Where-Object { $_ -notin $existingSandboxIds })
            if ($newSandboxIds.Count -eq 1) { $sandboxId = $newSandboxIds[0] }
        }
        Start-Sleep -Seconds 2
    }
    if (!(Test-Path $result)) {
        if (!$sandbox.HasExited) { Stop-Process -Id $sandbox.Id -Force -ErrorAction SilentlyContinue }
        throw 'Windows Sandbox acceptance timed out; no result file was produced.'
    }

    $payload = Get-Content -LiteralPath $result -Raw | ConvertFrom-Json
    if (!$sandbox.HasExited) { Stop-Process -Id $sandbox.Id -Force -ErrorAction SilentlyContinue }
    $payload | ConvertTo-Json -Depth 5
    if ($payload.state -ne 'passed') { exit 1 }
} finally {
    if (!$sandboxId) {
        $newSandboxIds = @(Get-RunningSandboxIds | Where-Object { $_ -notin $existingSandboxIds })
        if ($newSandboxIds.Count -eq 1) { $sandboxId = $newSandboxIds[0] }
    }
    if ($sandboxId) {
        & $wsbCli stop --id $sandboxId | Out-Null
    }
    Remove-Item -LiteralPath (Join-Path $inputDir 'txgz-credentials.json') -Force -ErrorAction SilentlyContinue
}
