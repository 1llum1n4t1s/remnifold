[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments)]
    [string[]]$Arguments
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$packaged = Join-Path $repositoryRoot 'bin/remnifold.exe'
$targetRoot = $env:CARGO_TARGET_DIR
if ([string]::IsNullOrWhiteSpace($targetRoot)) {
    $targetRoot = Join-Path $repositoryRoot 'target'
} elseif (-not [IO.Path]::IsPathRooted($targetRoot)) {
    $targetRoot = [IO.Path]::GetFullPath((Join-Path (Get-Location).ProviderPath $targetRoot))
}
$development = Join-Path $targetRoot 'debug/remnifold.exe'
if (Test-Path -LiteralPath $packaged -PathType Leaf) {
    & $packaged server-setup @Arguments
} elseif (Test-Path -LiteralPath $development -PathType Leaf) {
    & $development server-setup @Arguments
} else {
    throw 'Build the native executable first: cargo build --locked --manifest-path native/Cargo.toml'
}
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
