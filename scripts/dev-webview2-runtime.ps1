<#
.SYNOPSIS
    開発中に、Fixed Version WebView2 Runtime の取得物をビルド出力先へ見せかける
    ディレクトリジャンクションを作成・削除します（P01-1）。

.DESCRIPTION
    実行時に参照される WebView2Runtime は、実行ファイル（Hakutaku.exe）と同じ
    フォルダに置かれている必要があります（DIST-008）。一方、Fixed Version
    Runtime の取得物はリポジトリ直下の runtime\WebView2Runtime に保管するだけで
    （Git 管理外。約66エントリ、合計数百 MB）、実行ファイルの隣ではありません。

    開発中のビルド出力は target\x86_64-pc-windows-msvc\<profile>\ に生成される
    ため、このスクリプトは runtime\WebView2Runtime を
    target\x86_64-pc-windows-msvc\<profile>\WebView2Runtime としても見えるように、
    ディレクトリジャンクションを作成します。

    ジャンクションを選んだ理由:
      - コピーと異なり作成が瞬時で、ディスクを二重消費しない
      - 実体は runtime\WebView2Runtime のまま一つだけなので、
        Runtime のファイル内容を一切変更しないという保証を保ちやすい（DIST-011）
      - Windows のディレクトリジャンクションはシンボリックリンクと違い、
        管理者権限を必要としない

    build.rs による自動コピーは採用していません。理由:
      - 毎ビルドで数百 MB をコピーすることになり、ビルドのたびに時間とディスクを
        浪費する
      - コピーすると実体が二つ（runtime\WebView2Runtime と
        target 配下のコピー）に分かれてしまい、「Fixed Version Runtime の
        ファイル内容を変更しない」（DIST-011）という保証が、どちらの実体を
        指しているのか曖昧になる
    ジャンクションであれば実体は常に一つのままであり、この懸念が生じません。

.PARAMETER Profile
    対象のビルドプロファイル。debug または release。既定は debug。
    target\x86_64-pc-windows-msvc\<Profile>\WebView2Runtime にジャンクションを作ります。

.PARAMETER Remove
    指定すると、対象プロファイルのジャンクションを削除します（新規作成は行いません）。
    削除はジャンクション（参照）だけを外し、runtime\WebView2Runtime の中身には
    一切触れません。

.EXAMPLE
    pwsh scripts/dev-webview2-runtime.ps1
    既定（debug）のビルド出力先にジャンクションを作成します。

.EXAMPLE
    pwsh scripts/dev-webview2-runtime.ps1 -Profile release
    release のビルド出力先にジャンクションを作成します。

.EXAMPLE
    pwsh scripts/dev-webview2-runtime.ps1 -Remove
    既定（debug）のビルド出力先からジャンクションを削除します。
#>
[CmdletBinding()]
param(
    [ValidateSet('debug', 'release')]
    [string]$Profile = 'debug',

    [switch]$Remove
)

$ErrorActionPreference = 'Stop'

# リポジトリのルートは、このスクリプト（scripts\）の1階層上。
$repoRoot = Split-Path -Parent $PSScriptRoot
$sourceRuntimeDir = Join-Path $repoRoot 'runtime\WebView2Runtime'
$targetProfileDir = Join-Path $repoRoot "target\x86_64-pc-windows-msvc\$Profile"
$junctionPath = Join-Path $targetProfileDir 'WebView2Runtime'

# 対象パスがディレクトリジャンクション（またはシンボリックリンク）かどうかを判定する。
# PowerShell 7 (pwsh) の Get-Item は、リパースポイントに対して LinkType
# プロパティ（'Junction' / 'SymbolicLink' など）を提供する。
function Test-IsReparsePoint {
    param([Parameter(Mandatory)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }
    $item = Get-Item -LiteralPath $Path -Force
    return -not [string]::IsNullOrEmpty($item.LinkType)
}

if ($Remove) {
    if (-not (Test-Path -LiteralPath $junctionPath)) {
        Write-Host "ジャンクションは存在しません（何もしません）: $junctionPath"
        exit 0
    }

    if (-not (Test-IsReparsePoint -Path $junctionPath)) {
        Write-Warning "$junctionPath はジャンクションではない実体のディレクトリです。誤って中身を消さないよう、削除を中止します。内容を確認のうえ手動で対処してください。"
        exit 1
    }

    # `rmdir`（/s を付けない）は、ジャンクション自体の参照を外すだけで、
    # リンク先（runtime\WebView2Runtime）の中身には踏み込まない。
    # .NET の Directory.Delete 系はバージョンによって挙動差があり得るため、
    # 挙動が明確な cmd.exe の rmdir を使う。
    & cmd.exe /c "rmdir `"$junctionPath`"" | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Error "ジャンクションの削除に失敗しました: $junctionPath（終了コード $LASTEXITCODE）"
        exit 1
    }

    Write-Host "ジャンクションを削除しました: $junctionPath"
    Write-Host "（$sourceRuntimeDir の中身は変更していません）"
    exit 0
}

if (-not (Test-Path -LiteralPath $sourceRuntimeDir)) {
    Write-Host '案内: runtime\WebView2Runtime が見つかりません。'
    Write-Host '  1. Fixed Version WebView2 Runtime（150.0.4078.105、x64）の一式を入手してください。'
    Write-Host '  2. バージョン付きフォルダの中身を平坦化し、msedgewebview2.exe が'
    Write-Host '     WebView2Runtime 直下に来るようにしてください。'
    Write-Host "  3. 展開結果をこのリポジトリ直下の次の場所に配置してください:"
    Write-Host "       $sourceRuntimeDir"
    Write-Host '  4. 配置が終わったら、このスクリプトを再実行してください。'
    exit 1
}

if (Test-Path -LiteralPath $junctionPath) {
    if (Test-IsReparsePoint -Path $junctionPath) {
        Write-Host "ジャンクションは既に存在します: $junctionPath -> $sourceRuntimeDir"
        exit 0
    }

    Write-Warning "$junctionPath には既に実体のディレクトリが存在します。上書きはしません。"
    Write-Warning '誤って Runtime を複製配置している可能性があります。内容を確認し、不要であれば手動で削除してから再実行してください。'
    exit 1
}

if (-not (Test-Path -LiteralPath $targetProfileDir)) {
    Write-Host "案内: ビルド出力先がまだありません: $targetProfileDir"
    Write-Host "  先に `cargo build --profile $Profile` 等でビルドしてから再実行してください。"
    exit 1
}

New-Item -ItemType Junction -Path $junctionPath -Target $sourceRuntimeDir | Out-Null
Write-Host "ジャンクションを作成しました: $junctionPath -> $sourceRuntimeDir"
