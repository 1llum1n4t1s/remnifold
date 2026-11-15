# 梱包済みの Velopack の一式を Cloudflare R2（https://remnifold-updates.kagayoi.com/velopack/）へ配信する。
#
# 入力は pack-windows.ps1（macOS・Linux は pack-macos.sh / pack-linux.sh）の出力 packages\
# （Setup・releases.<channel>.json・full nupkg など）。アプリの自動更新は releases.<channel>.json と
# full nupkg を読むため、Setup だけでなくこれらも置く必要がある。
#
# 方式は同じ Velopack 1.2.161 で R2 配信している Skilvella / Shisui / Edictora に合わせる
# （同梱の R2Distribution.Common.ps1 / R2Transfer.Core.cs は同一のコピー。Node.js・Wrangler 不要）。
# - 厳格な install 用の releases/<version>/index.json とは別の `velopack/` 階層へ置く。既存のオブジェクトには触れない。
# - 版付きの成果物（*.nupkg、版番号を含む名前）は「新規作成のみ」で配信し、既存オブジェクトを上書きしない。
#   同じ名前が既にあれば内容のハッシュを照合し、一致しなければ失敗する。
# - strict 初回導入用の署名済み bootstrap（remnifold-bootstrap-<版>-x86_64-pc-windows-msvc.exe）も packages\ にあれば、
#   版付きの不変キーとして同じ規則（新規作成のみ・ハッシュ照合）で置く。
# - 公開中の releases.<channel>.json の最大版より低い版は配信前に拒否する（-AllowOlderVersion でだけ許す）。
# - 固定名（<packId>-<channel>-Setup.exe、releases.<channel>.json、assets.<channel>.json、RELEASES*）は最新を指すため
#   置き換える。クライアントから未配信の nupkg が見える時間を作らないよう、manifest は最後に置く。
# - 認証: CI（Environment r2-release）は R2_RELEASE_ACCESS_KEY_ID / R2_RELEASE_SECRET_ACCESS_KEY と
#   CLOUDFLARE_ACCOUNT_ID（既存の publisher と同じ権限境界）。手元は CLOUDFLARE_API_TOKEN、
#   無ければ D:\Dev\Secret\secrets.json の cloudflare.api_token。値はログへ出さない。
# - 実行は本人の明示の依頼があるときだけ。-DryRun は置く予定のファイルと分類を表示するだけ（公開中フィードの読み取りと巻き戻しの検査だけは行う。書き込みはしない）。
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $PackagesDirectory,
    # releases.<channel>.json の <channel>（win / osx / linux-x64 / linux-arm64 など）。
    [string] $Channel = 'win',
    [string] $PackId = 'RemnifoldApp',
    # 公開中のフィードより低い版の配信（巻き戻し）を明示的に許す。通常は使わない。
    [switch] $AllowOlderVersion,
    [switch] $DryRun
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Bucket = 'remnifold-updates'
$Prefix = 'velopack'
$BaseUrl = "https://remnifold-updates.kagayoi.com/$Prefix"
# 本人の他アプリと同じ Cloudflare アカウント（秘密情報ではない識別子）。
$AccountId = if ($env:CLOUDFLARE_ACCOUNT_ID) { $env:CLOUDFLARE_ACCOUNT_ID } else { '10901bfadbf1005164774a7350082985' }
$SecretsPath = 'D:\Dev\Secret\secrets.json'

if (![IO.Path]::IsPathFullyQualified($PackagesDirectory)) { throw 'Specify an absolute path to the packages directory.' }
if ($Channel -notmatch '^[a-z0-9-]+$') { throw 'Channel must be lowercase alphanumeric (and hyphens).' }
$packagesRoot = Get-Item -LiteralPath $PackagesDirectory -Force
if (!$packagesRoot.PSIsContainer) { throw 'The packages path must be a directory.' }
$files = @(Get-ChildItem -LiteralPath $packagesRoot.FullName -File -Force)
foreach ($required in "releases.$Channel.json") {
    if ($files.Name -notcontains $required) { throw "Missing required file: $required" }
}
if (!($files | Where-Object { $_.Name -like "$PackId-*full.nupkg" })) { throw 'Missing the full nupkg.' }

# 固定名のメタデータ。manifest (releases.*.json) は最後に置く。
function Get-Order([IO.FileInfo]$File) {
    if ($File.Name -like 'releases.*.json') { return 3 }
    if ($File.Name -like 'RELEASES*') { return 2 }
    if ($File.Name -like 'assets.*.json') { return 1 }
    return 0
}
function Test-Immutable([IO.FileInfo]$File) {
    return $File.Extension -eq '.nupkg' -or $File.Name -match '\d+\.\d+\.\d+'
}
$ordered = @($files | Sort-Object @{ Expression = { Get-Order $_ } }, Name)

# フィード（releases.<channel>.json）に載る $PackId の最大版を返す。無ければ $null。
function Get-FeedMaxVersion($Feed) {
    $versions = @($Feed.Assets | Where-Object { $_.PackageId -eq $PackId -and $_.Version } |
        ForEach-Object { [System.Management.Automation.SemanticVersion]::Parse([string] $_.Version) })
    if ($versions.Count -eq 0) { return $null }
    return ($versions | Sort-Object | Select-Object -Last 1)
}
# 公開中のフィードより低い版の配信は、既存の利用者の更新を巻き戻せないうえフィードの整合も崩すため、配信前に拒否する
# （-AllowOlderVersion でだけ許す）。フィードがまだ無い初回（HTTP 404）は通す。それ以外の取得失敗は、
# 巻き戻しの有無を確かめられないので失敗にする（-DryRun だけは、オフラインでも計画を表示できるよう警告に留める）。
function Assert-NotOlderThanPublished {
    $local = Get-FeedMaxVersion (Get-Content -LiteralPath (Join-Path $packagesRoot.FullName "releases.$Channel.json") -Raw | ConvertFrom-Json)
    if ($null -eq $local) { throw "releases.$Channel.json has no $PackId asset." }
    $uri = "$BaseUrl/releases.$Channel.json"
    try {
        $response = Invoke-WebRequest -Uri $uri -Headers @{ 'Cache-Control' = 'no-cache' } -SkipHttpErrorCheck -TimeoutSec 60
    } catch {
        if ($DryRun) { Write-Warning "Could not read the published feed; skipping the rollback check: $($_.Exception.Message)"; return }
        throw "Could not read the published feed to check for a rollback: $($_.Exception.Message)"
    }
    if ($response.StatusCode -eq 404) { Write-Host "No published $Channel feed yet; first publication."; return }
    if ($response.StatusCode -ne 200) {
        if ($DryRun) { Write-Warning "Published feed returned HTTP $($response.StatusCode); skipping the rollback check."; return }
        throw "Published feed returned HTTP $($response.StatusCode); cannot check for a rollback."
    }
    $published = Get-FeedMaxVersion ($response.Content | ConvertFrom-Json)
    if ($null -ne $published -and $local -lt $published) {
        if ($AllowOlderVersion) { Write-Warning "Publishing $local although $published is already published (-AllowOlderVersion)."; return }
        throw "Refusing to publish ${local}: the published $Channel feed is already at ${published}. Use -AllowOlderVersion only for an intentional rollback."
    }
    Write-Host "Version check passed ($local, published: $(if ($null -eq $published) { 'none' } else { $published }))."
}
Assert-NotOlderThanPublished

if ($DryRun) {
    foreach ($file in $ordered) {
        $kind = if (Test-Immutable $file) { 'immutable' } else { 'replace' }
        Write-Host ("{0,-10} {1}/{2}" -f $kind, $Prefix, $file.Name)
    }
    return
}

. (Join-Path $PSScriptRoot 'R2Distribution.Common.ps1')
$accessKey = $env:R2_RELEASE_ACCESS_KEY_ID
$secretKey = $env:R2_RELEASE_SECRET_ACCESS_KEY
$apiToken = $env:CLOUDFLARE_API_TOKEN
if (-not (($accessKey -and $secretKey) -or $apiToken)) {
    if (-not (Test-Path -LiteralPath $SecretsPath)) { throw 'No R2 credentials are set and the local secrets file was not found.' }
    $secrets = Get-Content -LiteralPath $SecretsPath -Raw | ConvertFrom-Json
    if (-not $secrets.cloudflare.api_token) { throw 'secrets.json has no cloudflare.api_token.' }
    $apiToken = $secrets.cloudflare.api_token
}

foreach ($file in $ordered) {
    $immutable = Test-Immutable $file
    $cacheControl = if ($immutable) { 'public, max-age=31536000, immutable' } else { 'no-cache, must-revalidate' }
    $contentType = switch ($file.Extension.ToLowerInvariant()) {
        '.json' { 'application/json'; break }
        '.exe' { 'application/vnd.microsoft.portable-executable'; break }
        default { if ($file.Name -like 'RELEASES*') { 'text/plain; charset=utf-8' } else { 'application/octet-stream' } }
    }
    Write-Host "  upload $Prefix/$($file.Name)"
    $arguments = @{
        AccountId = $AccountId; Bucket = $Bucket; FilePath = $file.FullName; Key = "$Prefix/$($file.Name)"
        ContentType = $contentType; CacheControl = $cacheControl; Immutable = $immutable
    }
    if ($accessKey -and $secretKey) {
        $arguments.AccessKeyId = $accessKey
        $arguments.SecretAccessKey = $secretKey
    } else {
        $arguments.ApiToken = $apiToken
    }
    $null = Publish-R2Object @arguments
}
# 固定名のオブジェクトは Cache-Control: no-cache, must-revalidate で置いているため、キャッシュの破棄は行わない
# （必要なら、Cloudflare のダッシュボードで該当 URL を手動で破棄する）。
Write-Host "Published to $BaseUrl/"
