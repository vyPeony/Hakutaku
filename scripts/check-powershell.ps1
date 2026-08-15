# scripts/ 配下の PowerShell スクリプトを構文解析し、解析エラーを報告する。
#
# 解析だけを行い、スクリプトは実行しない。PowerShell の抽象構文木パーサーは
# PowerShell Core に同梱されるため、外部モジュールを導入せずに済み、Windows 以外の
# CI ランナーでも実行できる（対象スクリプトが Windows 向けでも解析は可能）。
#
# 使い方: pwsh -File scripts/check-powershell.ps1

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$targets = Get-ChildItem -Path $root -Filter '*.ps1' -Recurse -File |
    Where-Object { $_.FullName -notmatch '[\\/](target|node_modules|runtime)[\\/]' }

$failed = 0
foreach ($target in $targets) {
    $errors = $null
    $null = [System.Management.Automation.Language.Parser]::ParseFile(
        $target.FullName, [ref] $null, [ref] $errors)

    $relative = [System.IO.Path]::GetRelativePath($root, $target.FullName).Replace('\', '/')
    if ($errors -and $errors.Count -gt 0) {
        $failed++
        Write-Host "$relative : 構文エラー $($errors.Count) 件"
        foreach ($e in $errors) {
            Write-Host "  行 $($e.Extent.StartLineNumber): $($e.Message)"
        }
    }
}

if ($failed -gt 0) {
    Write-Host ''
    Write-Host "構文エラーのあるスクリプト: $failed 件"
    exit 1
}

Write-Host "PowerShell スクリプト $($targets.Count) ファイルを解析しました。問題はありません。"
