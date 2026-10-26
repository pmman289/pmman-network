$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $elevated = Start-Process -FilePath powershell.exe -Verb RunAs -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath
    ) -Wait -PassThru
    exit $elevated.ExitCode
}
$root = Split-Path -Parent $PSScriptRoot
$logPath = Join-Path $root 'artifacts\verify-real-lab.log'
New-Item -ItemType Directory -Force -Path (Split-Path $logPath) | Out-Null
Remove-Item -LiteralPath $logPath -Force -ErrorAction SilentlyContinue
try {
    & (Join-Path $PSScriptRoot 'verify-real-lab.ps1') 2>&1 | Tee-Object -FilePath $logPath
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} catch {
    $_ | Out-String | Tee-Object -FilePath $logPath -Append
    exit 1
}
