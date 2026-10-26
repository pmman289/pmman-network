param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version,
    [ValidateSet('stable', 'beta')][string]$Channel = 'stable',
    [string]$ArtifactPath,
    [string]$Master = 'https://network.pmman.tech',
    [string]$CredentialPath,
    [string]$MinSupportedVersion = '0.0.1-alpha',
    [string]$MandatoryAfter,
    [string[]]$ReleaseNotes = @('客户端更新：修复问题并改进稳定性。'),
    [switch]$Edit,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (!$ArtifactPath) { $ArtifactPath = Join-Path $root "artifacts\client-updates\Pmman-Network-$Version-x64-update.exe" }
$installer = (Resolve-Path -LiteralPath $ArtifactPath).Path
if ([IO.Path]::GetExtension($installer) -ne '.exe') { throw 'Client release must be an .exe update package.' }
$item = Get-Item -LiteralPath $installer
if ($item.Length -gt 63MB -or $item.Length -eq 0) { throw 'Invalid update package size (maximum 63 MiB including upload headroom).' }
if ($item.VersionInfo.ProductName -ne 'Pmman Network' -or $item.VersionInfo.ProductVersion -ne $Version) {
    throw 'Installer product/version does not match the requested client release.'
}
$hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
$uri = [Uri]$Master
if ($uri.Scheme -ne 'https' -or $uri.UserInfo -or $uri.Query -or $uri.Fragment) { throw 'Master must be a public HTTPS origin without credentials/query/fragment.' }
$Master = $Master.TrimEnd('/')
if ($MandatoryAfter) {
    $date = [DateTimeOffset]::MinValue
    if ($MandatoryAfter -notmatch '^\d{4}-\d{2}-\d{2}T' -or ![DateTimeOffset]::TryParse($MandatoryAfter, [ref]$date)) { throw 'MandatoryAfter must be RFC3339.' }
}
if ($DryRun) {
    Write-Output "client_release_artifact=$installer"
    Write-Output "client_release_sha256=$hash"
    Write-Output 'client_release_publish=dry-run-passed'
    return
}

# Only the master creates, signs and publishes client versions. No SSH, build,
# local signing key, service restart, or server artifact is involved here.
$token = $env:PMMAN_ADMIN_TOKEN
if (!$token) {
    if (!$CredentialPath) { $CredentialPath = Join-Path $root 'artifacts\txgz-credentials.json' }
    $credential = Get-Content -Raw -LiteralPath $CredentialPath | ConvertFrom-Json
    $login = Invoke-RestMethod -Uri "$Master/v1/auth/login" -Method Post -ContentType 'application/json' -TimeoutSec 15 -Body (@{
        username = $credential.admin_user; password = $credential.admin_password
    } | ConvertTo-Json -Compress)
    $token = $login.token
}
Add-Type -AssemblyName System.Net.Http
$http = [System.Net.Http.HttpClient]::new()
$http.Timeout = [TimeSpan]::FromMinutes(5)
$form = [System.Net.Http.MultipartFormDataContent]::new()
$stream = $null
$response = $null
try {
    $http.DefaultRequestHeaders.Authorization = [System.Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $token)
    $fields = @{ version = $Version; minSupportedVersion = $MinSupportedVersion; mandatoryAfter = $MandatoryAfter; releaseNotes = ConvertTo-Json -InputObject @($ReleaseNotes) -Compress }
    foreach ($name in $fields.Keys) { $form.Add([System.Net.Http.StringContent]::new([string]$fields[$name]), $name) }
    $stream = [IO.File]::OpenRead($installer)
    $content = [System.Net.Http.StreamContent]::new($stream)
    $content.Headers.ContentType = [System.Net.Http.Headers.MediaTypeHeaderValue]::new('application/octet-stream')
    $form.Add($content, 'package', [IO.Path]::GetFileName($installer))
    $path = "$Master/v1/admin/client-releases/$Channel"
    if ($Edit) { $path += "/$Version" }
    $response = $http.PostAsync($path, $form).GetAwaiter().GetResult()
    if (!$response.IsSuccessStatusCode) { throw "Master rejected client publication (HTTP $([int]$response.StatusCode)); current release is unchanged." }
    $release = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult() | ConvertFrom-Json
    if ($release.version -ne $Version -or $release.sha256 -ne $hash -or [int64]$release.size -ne $item.Length -or !$release.signature) {
        throw 'Master-created release does not match the submitted artifact.'
    }
    $manifestPath = Join-Path $root "artifacts\client-release-$Channel-$Version.json"
    # Admin-only `active` is not part of the signed manifest.
    $release.PSObject.Properties.Remove('active')
    [IO.File]::WriteAllText($manifestPath, ($release | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
} finally {
    if ($response) { $response.Dispose() }
    $form.Dispose()
    if ($stream) { $stream.Dispose() }
    $http.Dispose()
}
& (Join-Path $PSScriptRoot 'verify-client-release.ps1') -Version $Version -Channel $Channel -Master $Master -ArtifactPath $installer
Write-Output "client_release_manifest=$manifestPath"
Write-Output "client_release_channel=$Channel"
Write-Output "client_release_version=$Version"
Write-Output 'client_release_publish=passed'
