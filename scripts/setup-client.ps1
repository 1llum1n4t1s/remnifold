[CmdletBinding()]
param(
    [string]$BaseUrl,
    [string]$CodexHome,
    [switch]$Force
)


# 検証済みの作業対象だけを標準APIで削除する。
function Remove-LocalWorkItem {
    param([string[]]$LiteralPath, [string]$AllowedRoot)
    if ([string]::IsNullOrWhiteSpace($AllowedRoot) -or -not [IO.Path]::IsPathRooted($AllowedRoot)) { throw 'Cleanup root must be absolute.' }
    $boundary = [IO.Path]::GetFullPath($AllowedRoot).TrimEnd('\','/') + [IO.Path]::DirectorySeparatorChar
    foreach ($target in $LiteralPath) {
        if ([string]::IsNullOrWhiteSpace($target) -or -not [IO.Path]::IsPathRooted($target)) { throw 'Cleanup target must be absolute.' }
        $resolved = [IO.Path]::GetFullPath($target)
        if (-not $resolved.StartsWith($boundary, [StringComparison]::OrdinalIgnoreCase)) { throw 'Cleanup target is outside the allowed root.' }
        $ancestor = $resolved
        while ($ancestor) {
            if (Test-Path -LiteralPath $ancestor) {
                $item = Get-Item -LiteralPath $ancestor -Force -ErrorAction Stop
                if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Cleanup path contains a reparse point.' }
            }
            $ancestor = Split-Path $ancestor -Parent
        }
        if (-not (Test-Path -LiteralPath $resolved)) { throw "Cleanup target is missing: $resolved" }
        $item = Get-Item -LiteralPath $resolved -Force -ErrorAction Stop
        $leafLink = [bool]($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
        if ($item.PSIsContainer -and -not $leafLink) {
            if (@(Get-ChildItem -LiteralPath $resolved -Recurse -Force -ErrorAction Stop | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count) { throw 'Cleanup directory contains a reparse point.' }
        }
        Remove-Item -LiteralPath $resolved -Recurse:($item.PSIsContainer -and -not $leafLink) -Force -ErrorAction Stop
        if (Test-Path -LiteralPath $resolved) { throw "Cleanup target remains: $resolved" }
    }
}

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($CodexHome)) {
    $CodexHome = [Environment]::GetEnvironmentVariable('CODEX_HOME', 'Process')
}
if ([string]::IsNullOrWhiteSpace($CodexHome)) {
    $CodexHome = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)) '.codex'
}
$resolvedHome = [System.IO.Path]::GetFullPath($CodexHome)
if (-not (Test-Path -LiteralPath $resolvedHome -PathType Container)) {
    New-Item -ItemType Directory -Path $resolvedHome | Out-Null
}

$configPath = Join-Path $resolvedHome 'remnifold.json'
$previousConfigPath = Join-Path $resolvedHome 'kagayoi-memory.json'
$legacyConfigPath = Join-Path $resolvedHome 'supermemory.json'
$sourceConfigPath = if (Test-Path -LiteralPath $configPath -PathType Leaf) { $configPath } elseif (Test-Path -LiteralPath $previousConfigPath -PathType Leaf) { $previousConfigPath } elseif (Test-Path -LiteralPath $legacyConfigPath -PathType Leaf) { $legacyConfigPath } else { $configPath }
$existingConfig = [ordered]@{}
if (Test-Path -LiteralPath $sourceConfigPath -PathType Leaf) {
    try {
        $parsedConfig = Get-Content -Raw -LiteralPath $sourceConfigPath | ConvertFrom-Json -AsHashtable
        if ($null -eq $parsedConfig) { throw 'empty configuration' }
        foreach ($entry in $parsedConfig.GetEnumerator()) { $existingConfig[$entry.Key] = $entry.Value }
    } catch {
        if (-not $Force) { throw "Configuration at $sourceConfigPath is invalid. Use -Force to replace it." }
        $existingConfig = [ordered]@{}
    }
}

if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
    $BaseUrl = [Environment]::GetEnvironmentVariable('REMNIFOLD_API_URL', 'Process')
}
if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
    $BaseUrl = [Environment]::GetEnvironmentVariable('KAGAYOI_MEMORY_API_URL', 'Process')
}
if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
    $BaseUrl = [Environment]::GetEnvironmentVariable('SUPERMEMORY_API_URL', 'Process')
}
if ([string]::IsNullOrWhiteSpace($BaseUrl) -and $existingConfig.Contains('baseUrl')) {
    $BaseUrl = [string]$existingConfig.baseUrl
}
if ([string]::IsNullOrWhiteSpace($BaseUrl)) { throw 'BaseUrl or REMNIFOLD_API_URL is required.' }

try { $endpoint = [Uri]$BaseUrl } catch { throw 'BaseUrl must be an absolute URL.' }
$endpointHost = $endpoint.DnsSafeHost.Trim('[', ']').ToLowerInvariant()
$ipAddress = $null
$localEndpoint = $endpointHost -eq 'localhost' -or
    ([System.Net.IPAddress]::TryParse($endpointHost, [ref]$ipAddress) -and [System.Net.IPAddress]::IsLoopback($ipAddress))
$hostedService = $endpoint.DnsSafeHost -eq 'supermemory.ai' -or $endpoint.DnsSafeHost.EndsWith('.supermemory.ai', [StringComparison]::OrdinalIgnoreCase)
if (-not $endpoint.IsAbsoluteUri -or $endpoint.UserInfo -or $endpoint.Query -or $endpoint.Fragment -or
    ($endpoint.Scheme -ne 'https' -and -not ($localEndpoint -and $endpoint.Scheme -eq 'http')) -or $hostedService) {
    throw 'Use an HTTPS endpoint for this self-hosted API (HTTP is accepted only for localhost).'
}
$normalizedUrl = $endpoint.AbsoluteUri.TrimEnd('/')

$apiKey = [Environment]::GetEnvironmentVariable('REMNIFOLD_API_KEY', 'Process')
if ([string]::IsNullOrWhiteSpace($apiKey)) {
    $apiKey = [Environment]::GetEnvironmentVariable('KAGAYOI_MEMORY_API_KEY', 'Process')
}
if ([string]::IsNullOrWhiteSpace($apiKey)) {
    $apiKey = [Environment]::GetEnvironmentVariable('SUPERMEMORY_CODEX_API_KEY', 'Process')
}
if ([string]::IsNullOrWhiteSpace($apiKey)) {
    $apiKey = [Environment]::GetEnvironmentVariable('CLOUDFLARE_MEMORY_API_KEY', 'Process')
}
if ([string]::IsNullOrWhiteSpace($apiKey) -and $existingConfig.Contains('apiKey') -and $existingConfig.Contains('baseUrl')) {
    try { $existingNormalizedUrl = ([Uri][string]$existingConfig.baseUrl).AbsoluteUri.TrimEnd('/') } catch { $existingNormalizedUrl = '' }
    if ($existingNormalizedUrl -eq $normalizedUrl) { $apiKey = [string]$existingConfig.apiKey }
}
if ([string]::IsNullOrWhiteSpace($apiKey)) {
    $secret = Read-Host 'Memory API key' -AsSecureString
    $apiKey = ConvertFrom-SecureString $secret -AsPlainText
}
if ([string]::IsNullOrWhiteSpace($apiKey)) { throw 'An API key is required.' }

$headers = @{ Authorization = "Bearer $apiKey" }
try {
    $session = Invoke-RestMethod -Uri "$normalizedUrl/v3/session" -Headers $headers -Method Get -MaximumRedirection 0
} catch {
    throw 'The authenticated memory session could not be verified.'
}
if ([string]::IsNullOrWhiteSpace([string]$session.user.id)) { throw 'The server returned an invalid session response.' }

if ((Test-Path -LiteralPath $configPath) -and -not $Force) {
    throw "Configuration already exists at $configPath. Use -Force to replace it."
}
$config = [ordered]@{}
foreach ($entry in $existingConfig.GetEnumerator()) { $config[$entry.Key] = $entry.Value }
$config.baseUrl = $normalizedUrl
$config.apiKey = $apiKey
if (-not $config.Contains('recallMode')) { $config.recallMode = 'direct' }
if (-not $config.Contains('maxMemories')) { $config.maxMemories = 3 }
if (-not $config.Contains('similarityThreshold')) { $config.similarityThreshold = 0.7 }
$temporaryPath = "$configPath.$PID.$([Guid]::NewGuid().ToString('N')).tmp"
try {
    [System.IO.File]::WriteAllText($temporaryPath, ($config | ConvertTo-Json -Depth 20), [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::Move($temporaryPath, $configPath, $true)
} finally {
    if (Test-Path -LiteralPath $temporaryPath) { if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {  Remove-LocalWorkItem -LiteralPath ($ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($temporaryPath)) -AllowedRoot $resolvedHome } else { Remove-Item -LiteralPath $temporaryPath -Force } }
}
Write-Output "Connected to $normalizedUrl and wrote $configPath."
