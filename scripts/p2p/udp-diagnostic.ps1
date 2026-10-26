[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $OutDir,
    [string[]] $NodeEndpoint = @('1.14.226.49:51840', '1.14.226.49:51841'),
    [string[]] $DnsEndpoint = @('1.1.1.1:53', '1.0.0.1:53'),
    [switch] $Matrix,
    [ValidateRange(0, 60)]
    [int] $SendDelaySeconds = 0,
    [ValidateRange(1, 5)]
    [int] $Rounds = 1,
    [ValidateRange(0, 30)]
    [int] $RoundIntervalSeconds = 5,
    [ValidateRange(500, 30000)]
    [int] $ReceiveTimeoutMs = 4000
)

$ErrorActionPreference = 'Stop'
$result = [ordered]@{
    passed = $false
    profile = 'PrivateNat'
    tests = @()
    addresses = @()
    routes = @()
    errors = @()
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
try {
    $result.addresses = @(Get-NetIPAddress -AddressFamily IPv4 |
            Select-Object InterfaceIndex, InterfaceAlias, IPAddress, PrefixLength, AddressState)
}
catch {
    $result.errors += "IPv4 address inventory failed: $($_.Exception.Message)"
}
try {
    $result.routes = @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' |
            Where-Object AddressFamily -eq 2 |
            Select-Object InterfaceIndex, InterfaceAlias, NextHop, RouteMetric, Publish, State)
}
catch {
    $result.errors += "IPv4 default-route inventory failed: $($_.Exception.Message)"
}

function Resolve-UdpTarget([string] $Endpoint) {
    if ($Endpoint -notmatch '^(?<host>\[[^\]]+\]|[^:]+):(?<port>[0-9]{1,5})$') {
        throw "UDP endpoint must be host:port or [ipv6]:port: $Endpoint"
    }
    $hostName = $Matches.host.Trim('[', ']')
    $port = [int] $Matches.port
    if ($port -lt 1 -or $port -gt 65535) { throw "UDP endpoint port is invalid: $Endpoint" }
    $address = [Net.Dns]::GetHostAddresses($hostName) |
        Where-Object AddressFamily -eq ([Net.Sockets.AddressFamily]::InterNetwork) |
        Select-Object -First 1
    if (!$address) { throw "UDP endpoint has no IPv4 address: $Endpoint" }
    [pscustomobject]@{ Host = $hostName; Port = $port; Address = $address }
}

function New-Payload([string] $Kind) {
    if ($Kind -eq 'node') {
        $bytes = New-Object byte[] 24
        [Text.Encoding]::ASCII.GetBytes('PMMPING1').CopyTo($bytes, 0)
        [Guid]::NewGuid().ToByteArray().CopyTo($bytes, 8)
        return $bytes
    }
    return [byte[]] (0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0,
        5, 112, 109, 109, 97, 110, 4, 116, 101, 99, 104, 0, 0, 1, 0, 1)
}

$targets = @()
$targets += @($NodeEndpoint | ForEach-Object { [pscustomobject] @{ Endpoint = $_; Kind = 'node' } })
$targets += @($DnsEndpoint | ForEach-Object { [pscustomobject] @{ Endpoint = $_; Kind = 'dns' } })
if ($Matrix) {
    # Non-service port/payload combinations are trace controls, never positive
    # response assertions. Compare their NAT/NIC paths using the host capture.
    $endpoints = @($NodeEndpoint) + @($DnsEndpoint) + @('1.14.226.49:53','1.1.1.1:51840')
    $targets = @()
    foreach ($endpoint in ($endpoints | Select-Object -Unique)) {
        foreach ($kind in @('node','dns')) {
            $expected = ($kind -eq 'node' -and $endpoint -in $NodeEndpoint) -or ($kind -eq 'dns' -and $endpoint -in $DnsEndpoint)
            $targets += [pscustomobject]@{Endpoint=$endpoint;Kind=$kind;ExpectedReply=$expected}
        }
    }
}

if ($SendDelaySeconds) { Start-Sleep -Seconds $SendDelaySeconds }
for ($round = 0; $round -lt $Rounds; $round++) {
foreach ($target in $targets) {
    $socket = $null
    $entry = [ordered]@{
        endpoint = $target.Endpoint
        round = $round
        kind = $target.Kind
        expectedReply = if ($Matrix) { $target.ExpectedReply } else { $true }
        received = $false
        error = $null
    }
    try {
        $resolved = Resolve-UdpTarget $target.Endpoint
        $entry.destination = "$($resolved.Address):$($resolved.Port)"
        $socket = [Net.Sockets.UdpClient]::new([Net.Sockets.AddressFamily]::InterNetwork)
        $socket.Client.ReceiveTimeout = $ReceiveTimeoutMs
        $bytes = New-Payload $target.Kind
        $remote = [Net.IPEndPoint]::new($resolved.Address, $resolved.Port)
        $sentAt = [DateTimeOffset]::UtcNow
        $null = $socket.Send($bytes, $bytes.Length, $remote)
        $entry.sentUtc = $sentAt.ToString('o')
        $entry.local = $socket.Client.LocalEndPoint.ToString()
        $sender = [Net.IPEndPoint]::new([Net.IPAddress]::Any, 0)
        $reply = $socket.Receive([ref] $sender)
        if (!$sender.Address.Equals($resolved.Address) -or $sender.Port -ne $resolved.Port) {
            throw 'UDP response source did not match the requested endpoint'
        }
        if ($target.Kind -eq 'node') {
            if ([Convert]::ToBase64String($reply) -ne [Convert]::ToBase64String($bytes)) {
                throw 'UDP challenge echo did not match this request'
            }
        } elseif ($reply.Length -lt 12 -or $reply[0] -ne $bytes[0] -or $reply[1] -ne $bytes[1] -or ($reply[2] -band 128) -eq 0) {
            throw 'DNS UDP reply did not match the query transaction'
        }
        $entry.received = $true
        $entry.receivedUtc = [DateTimeOffset]::UtcNow.ToString('o')
        $entry.elapsedMs = ([DateTimeOffset]::UtcNow - $sentAt).TotalMilliseconds
        $entry.bytes = $reply.Length
        $entry.source = $sender.ToString()
    }
    catch {
        $entry.error = $_.Exception.Message
    }
    finally {
        if ($socket) { $socket.Dispose() }
    }
    $result.tests += [pscustomobject] $entry
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutDir 'progress.json') -Encoding utf8
}
if ($round -lt $Rounds - 1 -and $RoundIntervalSeconds) { Start-Sleep -Seconds $RoundIntervalSeconds }
}

try {
    $result.nodePassed = @($result.tests | Where-Object { $_.expectedReply -and $_.kind -eq 'node' -and $_.received }).Count -eq (@($NodeEndpoint).Count * $Rounds)
    $result.dnsPassed = @($result.tests | Where-Object { $_.expectedReply -and $_.kind -eq 'dns' -and $_.received }).Count -eq (@($DnsEndpoint).Count * $Rounds)
    $result.passed = $result.nodePassed -and $result.dnsPassed
}
catch {
    $result.errors += "UDP result aggregation failed: $($_.Exception.Message)"
}
finally {
    $result | ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath (Join-Path $OutDir 'application-result.json') -Encoding utf8
}
