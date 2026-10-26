param(
    [string]$Master = 'https://network.pmman.tech',
    [string]$Version = '9.9.99'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$credentials = Get-Content -Raw (Join-Path $root 'artifacts\txgz-credentials.json') | ConvertFrom-Json
$login = Invoke-RestMethod -Uri "$Master/v1/auth/login" -Method Post -ContentType 'application/json' -Body (@{
    username = $credentials.admin_user
    password = $credentials.admin_password
} | ConvertTo-Json -Compress)
$token = $login.token
$auth = "Authorization: Bearer $token"
$package = (Resolve-Path (Join-Path $root 'artifacts\client-updates\Pmman-Network-0.0.1-alpha-x64-update.exe')).Path

function Invoke-CurlRequest([string[]]$Arguments) {
    $raw = (& curl.exe @Arguments 2>&1 | Out-String)
    $match = [regex]::Match($raw, 'HTTP:(\d{3})')
    if (!$match.Success) { throw "Admin release API did not return HTTP status" }
    return [pscustomobject]@{
        Status = [int]$match.Groups[1].Value
        Body = $raw.Substring(0, $match.Index).Trim()
    }
}

function Remove-TestRelease {
    $null = Invoke-CurlRequest @(
        '-sS', '-w', 'HTTP:%{http_code}', '-X', 'DELETE', '-H', $auth,
        "$Master/v1/admin/client-releases/stable/$Version"
    )
}

try {
    Remove-TestRelease
    $create = Invoke-CurlRequest @(
        '-sS', '-w', 'HTTP:%{http_code}', '-H', $auth,
        '-F', "version=$Version", '-F', 'minSupportedVersion=0.0.1-alpha',
        '-F', 'mandatoryAfter=', '-F', 'releaseNotes=["admin CRUD verification"]',
        '-F', "package=@$package;type=application/octet-stream",
        "$Master/v1/admin/client-releases/stable"
    )
    if ($create.Status -ne 200) { throw "Create release failed with HTTP $($create.Status)" }

    $edit = Invoke-CurlRequest @(
        '-sS', '-w', 'HTTP:%{http_code}', '-H', $auth,
        '-F', "version=$Version", '-F', 'minSupportedVersion=0.0.1-alpha',
        '-F', 'mandatoryAfter=', '-F', 'releaseNotes=["admin CRUD edit verification"]',
        "$Master/v1/admin/client-releases/stable/$Version"
    )
    if ($edit.Status -ne 200) { throw "Edit release failed with HTTP $($edit.Status)" }

    $list = Invoke-RestMethod -Uri "$Master/v1/admin/client-releases/stable" -Headers @{ Authorization = "Bearer $token" }
    if (!(@($list) | Where-Object version -eq $Version)) { throw 'Edited release was not returned by admin list' }
    Write-Output 'client_release_admin_list=passed'
    Write-Output 'client_release_admin_create=passed'
    Write-Output 'client_release_admin_edit=passed'
} finally {
    $delete = Invoke-CurlRequest @(
        '-sS', '-w', 'HTTP:%{http_code}', '-X', 'DELETE', '-H', $auth,
        "$Master/v1/admin/client-releases/stable/$Version"
    )
    if ($delete.Status -ne 204 -and $delete.Status -ne 404) { throw "Delete release failed with HTTP $($delete.Status)" }
}
Write-Output 'client_release_admin_delete=passed'
