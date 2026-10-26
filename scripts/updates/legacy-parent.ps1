param([Parameter(Mandatory=$true)][string]$InstallDirectory)
$ErrorActionPreference = 'Stop'
try {
    $expected = [IO.Path]::GetFullPath((Join-Path $InstallDirectory 'pmman-client.exe'))
    $current = Get-CimInstance Win32_Process -Filter "ProcessId=$PID"
    # NSIS may introduce a relaunch process during elevation. Walk only this
    # installer's ancestors, never enumerate/kill other Pmman installations.
    for ($depth = 0; $depth -lt 8 -and $current.ParentProcessId; $depth++) {
        $parent = Get-CimInstance Win32_Process -Filter "ProcessId=$($current.ParentProcessId)"
        if (!$parent -or $parent.CreationDate -gt $current.CreationDate) { break }
        if ($parent.ExecutablePath -and
            $parent.ExecutablePath.Equals($expected, [StringComparison]::OrdinalIgnoreCase) -and
            $parent.CommandLine -match '(?:^|\s)--apply-update(?:\s|$)') {
            # NSIS's existing Restart Manager check will release this image
            # lock. We only record that the installer must take over relaunch.
            exit 42
        }
        $current = $parent
    }
    exit 0
} catch { exit 1 }
