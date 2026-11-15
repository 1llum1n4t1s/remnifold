# Windows の Setup（Velopack）を梱包する。
#
# 出力（packages\）は自動更新の更新元として R2 へ置く一式（Setup・releases.win.json・full nupkg など）。
# 配信は scripts/velopack/publish-r2.ps1 が行う。共通仕様は D:\Dev\APP-DISTRIBUTION.md。
#
# 出力には、nupkg 内の署名済み remnifold.exe を取り出した remnifold-bootstrap-<版>-x86_64-pc-windows-msvc.exe も含む
# （strict 初回導入用。-Sign のときだけ署名・CN・タイムスタンプを検証し、SHA-256 を出力する）。
#
# -Sign は Sigilune（Certum SimplySign のクラウド鍵）で署名する。手元は `sigilune setup` で登録済みであること。
# CI は .github/workflows/release-velopack.yml が Environment の Secrets を SIGILUNE_EMAIL / SIGILUNE_TOTP_URI で渡す。
# アプリは適用前に、パッケージ内の remnifold.exe の署名者 CN を native/src/app_update/trust.rs の値と照合する。
#
# 検証用（本番では使わない）: -PackId / -PackVersion / -Binary を指定すると、別の packId・版・ビルド済みの
# 実行ファイルで未署名の一式を作れる。-Sign と併用はできない（テスト用のビルドに本番の署名をしないため）。
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $WorkDirectory,
    [switch] $Sign,
    # 省略時は作業ディレクトリ内の target。CI は署名用 Secrets を渡さない前段の step で同じ条件の cargo build を済ませ、
    # その出力先を渡す（このスクリプトの cargo build は再コンパイルしない）。
    [string] $CargoTargetDirectory,
    [string] $PackId = 'RemnifoldApp',
    # 省略時は native/Cargo.toml の版。
    [string] $PackVersion,
    # ビルド済みの remnifold.exe（検証用）。指定すると cargo build を行わない。
    [string] $Binary
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
if (!$IsWindows -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'Pack the installer on Windows x64.'
}
if ($Sign -and ($PackId -ne 'RemnifoldApp' -or $PackVersion -or $Binary)) {
    throw '-Sign cannot be combined with -PackId, -PackVersion or -Binary (those are for isolated validation packages).'
}
if ($PackId -notmatch '^[A-Za-z][A-Za-z0-9]*$') { throw 'PackId must be alphanumeric.' }
if (![IO.Path]::IsPathFullyQualified($WorkDirectory)) {
    throw 'Specify an absolute path to an empty work directory.'
}
$work = Get-Item -LiteralPath $WorkDirectory -Force
if (!$work.PSIsContainer -or
    (Get-ChildItem -LiteralPath $work.FullName -Force | Select-Object -First 1)) {
    throw 'The work directory must be an empty, ordinary directory.'
}
for ($folder = $work; $null -ne $folder; $folder = $folder.Parent) {
    if ($folder.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "The work path contains a reparse point: $($folder.FullName)"
    }
}
# GitHub Actions では vpk が色付け用のエスケープシーケンスを出し、`\b` の照合が外れるため取り除いてから比べる。
$vpkHelp = (vpk --help | Out-String) -replace "`e\[[0-9;]*m", ''
if ($vpkHelp -notmatch '\bVelopack CLI 1\.2\.161\b') {
    throw 'Velopack CLI 1.2.161 is required.'
}

$signing = @()
# 本人の他アプリと共通のコード署名証明書（Certum）。thumbprint は年次更新で変わるため、署名者は CN で照合する。
$signerCommonName = 'Open Source Developer Yuichiro Shinozaki'
if ($Sign) {
    . (Join-Path $PSScriptRoot 'Sigilune.Common.ps1')
    $sigilune = Resolve-SigiluneExe
    # ビルド前にログインし、認証の失敗を早く検出する（有効なセッションは再利用する）。
    Connect-Sigilune -SigiluneExe $sigilune
    # Velopack はファイルごとに署名テンプレートを実行する。1 ファイルずつ直列に署名し、
    # Sigilune が署名後に有効性・署名者・タイムスタンプを検証する。--replace は既存の署名を置き換える。
    $signing = @(
        '--signParallel', '1',
        '--signTemplate', "`"$sigilune`" sign --replace {{file}}"
    )
}

$repo = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$build = if ($CargoTargetDirectory) {
    if (![IO.Path]::IsPathFullyQualified($CargoTargetDirectory)) { throw 'Specify an absolute path for the Cargo target directory.' }
    $CargoTargetDirectory
} else {
    Join-Path $work.FullName 'target'
}
$payload = Join-Path $work.FullName 'payload'
$packages = Join-Path $work.FullName 'packages'
$temp = Join-Path $work.FullName 'temp'
New-Item -ItemType Directory -Path $payload, $packages, $temp | Out-Null

$previousTemp = $env:TEMP
$previousTmp = $env:TMP
Push-Location -LiteralPath $repo
try {
    # Rust と Velopack の一時ファイルを、梱包専用のディレクトリに閉じる。
    $env:TEMP = $temp
    $env:TMP = $temp
    if (!$PackVersion) {
        $metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
        $PackVersion = ($metadata.packages | Where-Object name -eq 'remnifold').version
    }
    if ($Binary) {
        if (![IO.Path]::IsPathFullyQualified($Binary) -or !(Test-Path -LiteralPath $Binary -PathType Leaf)) {
            throw 'Binary must be an absolute path to an existing remnifold.exe.'
        }
        Copy-Item -LiteralPath $Binary -Destination (Join-Path $payload 'remnifold.exe')
    } else {
        # 本番の梱包は既定の機能だけでビルドする（update-feed-test などのテスト用の機能を含めない）。
        cargo build --locked --release --target x86_64-pc-windows-msvc --target-dir $build --bin remnifold
        Copy-Item -LiteralPath (Join-Path $build 'x86_64-pc-windows-msvc/release/remnifold.exe') -Destination $payload
    }
    Copy-Item -LiteralPath (Join-Path $repo 'LICENSE'), (Join-Path $repo 'README.md'), (Join-Path $repo 'CHANGELOG.md') -Destination $payload
    # packId は作業フォルダー %LOCALAPPDATA%\remnifold と衝突しない名前にする（Velopack の導入先は
    # %LOCALAPPDATA%\<packId>）。従来の install の導入先（Programs\Remnifold）とも別。
    $packOptions = @(
        '--packId', $PackId,
        '--packTitle', 'Remnifold',
        '--packAuthors', 'Kagayoi',
        '--packVersion', $PackVersion,
        '--channel', 'win',
        '--runtime', 'win-x64',
        '--instLocation', 'PerUser',
        '--mainExe', 'remnifold.exe',
        '--packDir', $payload,
        '--outputDir', $packages,
        '--shortcuts', 'StartMenuRoot',
        '--noPortable',
        '--delta', 'None'
    )
    vpk pack @packOptions @signing
    $setup = Join-Path $packages "$PackId-win-Setup.exe"
    if (!(Test-Path -LiteralPath $setup -PathType Leaf)) { throw 'Setup executable was not generated.' }
    # 自動更新の更新元（R2）には、Setup のほか releases.win.json と full nupkg が必要。
    # Setup だけでは、アプリ側から新版を見つけられない。既定のチャンネル（win）の full nupkg は `<id>-<版>-full.nupkg`。
    foreach ($required in @('releases.win.json', "$PackId-$PackVersion-full.nupkg")) {
        if (!(Test-Path -LiteralPath (Join-Path $packages $required) -PathType Leaf)) { throw "Update feed file was not generated: $required" }
    }
    # 版付きの不変名の Setup も置く（固定名 Setup は最新版を指す URL として毎回置き換える）。
    Copy-Item -LiteralPath $setup -Destination (Join-Path $packages "$PackId-$PackVersion-win-Setup.exe")
    # bootstrap（Setup を使わない strict な初回導入用）の実行ファイル。nupkg 内の lib/app/remnifold.exe は
    # vpk が署名した後の実体なので、それをそのまま版付きの名前で取り出す。これは同じ remnifold.exe であり、
    # `install` サブコマンドを持つ。Velopack の導入先以外では更新処理が何もしないため、単体で起動して使える。
    # 版付きの不変キーで R2 へ置く（publish-r2.ps1 が packages 直下の全ファイルを新規作成のみで置く）。
    # 署名の検証は -Sign のときだけ行う（検証用の未署名パッケージでも、ファイルが生成されることは確かめられる）。
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $bootstrap = Join-Path $packages "remnifold-bootstrap-$PackVersion-x86_64-pc-windows-msvc.exe"
    $bootstrapArchive = [IO.Compression.ZipFile]::OpenRead((Join-Path $packages "$PackId-$PackVersion-full.nupkg"))
    try {
        $bootstrapEntries = @($bootstrapArchive.Entries | Where-Object { $_.FullName -ieq 'lib/app/remnifold.exe' })
        if ($bootstrapEntries.Count -ne 1) { throw 'The full nupkg must contain exactly one lib/app/remnifold.exe.' }
        [IO.Compression.ZipFileExtensions]::ExtractToFile($bootstrapEntries[0], $bootstrap)
    } finally {
        $bootstrapArchive.Dispose()
    }
    if (!(Test-Path -LiteralPath $bootstrap -PathType Leaf)) { throw 'Bootstrap executable was not generated.' }
    if ($Sign) {
        $signature = Get-AuthenticodeSignature -LiteralPath $setup
        if ($signature.Status -ne 'Valid' -or
            $signature.SignerCertificate.Subject -notlike "CN=$signerCommonName*" -or
            !$signature.TimeStamperCertificate) {
            throw 'Setup failed Authenticode verification (signer and timestamp).'
        }
        # 利用者は取得後に同じ検査（Status=Valid・署名者 CN・タイムスタンプ）を手元で行ってから実行する。
        # 検査を通らない bootstrap は配信しない。
        $bootstrapSignature = Get-AuthenticodeSignature -LiteralPath $bootstrap
        if ($bootstrapSignature.Status -ne 'Valid' -or
            $bootstrapSignature.SignerCertificate.Subject -notlike "CN=$signerCommonName*" -or
            !$bootstrapSignature.TimeStamperCertificate) {
            throw 'Bootstrap executable failed Authenticode verification (signer and timestamp).'
        }
        Write-Host "Bootstrap SHA256 $((Get-FileHash -LiteralPath $bootstrap -Algorithm SHA256).Hash)"
        # アプリ（native/src/app_update/trust.rs）は適用前に、nupkg 内の全ての PE（.exe・.dll）の署名者を同じ CN で
        # 照合し、主実行ファイルと更新器（Squirrel.exe）がちょうど1つずつあることも要求する。
        # 配信前に同じ検査を通し、ここで通らない一式は配信しない（配信すると、全利用者の更新が適用前に拒否される）。
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $nupkg = Join-Path $packages "$PackId-$PackVersion-full.nupkg"
        $archive = [IO.Compression.ZipFile]::OpenRead($nupkg)
        try {
            $names = @($archive.Entries | ForEach-Object { $_.FullName })
            if (@($names | Where-Object { $_ -ieq 'lib/app/remnifold.exe' }).Count -ne 1) { throw 'The full nupkg must contain exactly one lib/app/remnifold.exe.' }
            if (@($names | Where-Object { $_ -ieq 'lib/app/Squirrel.exe' }).Count -ne 1) { throw 'The full nupkg must contain exactly one lib/app/Squirrel.exe.' }
            $verified = 0
            foreach ($entry in @($archive.Entries | Where-Object { $_.FullName -match '(?i)\.(exe|dll)$' })) {
                $extracted = Join-Path $temp ("verify-{0}.bin" -f [Guid]::NewGuid().ToString('N'))
                [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $extracted)
                $inner = Get-AuthenticodeSignature -LiteralPath $extracted
                Remove-Item -LiteralPath $extracted -Force
                if ($inner.Status -ne 'Valid' -or $inner.SignerCertificate.Subject -notlike "CN=$signerCommonName*") {
                    throw "$($entry.FullName) inside the full nupkg failed Authenticode verification ($($inner.Status))."
                }
                $verified++
            }
            Write-Host "Verified the signer of $verified executable(s) inside the full nupkg."
        } finally {
            $archive.Dispose()
        }
    }
    Get-FileHash -LiteralPath $setup -Algorithm SHA256
} finally {
    $env:TEMP = $previousTemp
    $env:TMP = $previousTmp
    Pop-Location
}
