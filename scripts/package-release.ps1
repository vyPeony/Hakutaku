<#
.SYNOPSIS
    Hakutaku の配布 ZIP を組み立てます（P12-1）。

.DESCRIPTION
    release ビルド済みの単一 EXE（`Hakutaku.exe`）と、同梱する最小限のファイル
    （`hakutaku.yaml.sample`、リリースノートの写し）を、本体配布 ZIP
    `Hakutaku-<版>.zip` へまとめます。加えて、Fixed Version WebView2 Runtime
    一式（`runtime/WebView2Runtime/`。既定では Git 管理外）を、任意追加パッケージ
    `Hakutaku-WebView2Runtime-<Runtime版>.zip` へ別途まとめます（`DIST-007`）。

    `DIST-002`／`DIST-003` により、Hakutaku.exe 単体はフロントエンド資産を
    埋め込んだ単一 EXE として成立し、実測サイズもおおむね 10 MiB 前後（2026-08
    時点）と 100 MB 目標を大きく下回ります。単一 EXE だけでも起動できますが、
    このスクリプトが作る本体 ZIP は「単一 EXE の代替」ではなく、設定サンプルと
    リリースノートを1回のダウンロードで揃えるための配布container です。展開すると
    次の構成になります（10.3 の配布物構成のうち、実行時自動生成分を除いた部分）。

        Hakutaku-<版>.zip を展開すると:
        Hakutaku/
        ├─ Hakutaku.exe
        ├─ hakutaku.yaml.sample   (CFG-015: リネームしない限り既定値起動)
        └─ release-notes.md

        Hakutaku-WebView2Runtime-<Runtime版>.zip を Hakutaku/ の直下へ展開すると:
        Hakutaku/
        └─ WebView2Runtime/       (Fixed Version Runtime 一式。DIST-006)

    `logs`／`temp`／`WebView2` は Hakutaku 自身が起動時に自動生成するため
    （`bootstrap::layout`）、ZIP には含めません。

    ## hakutaku.yaml.sample について

    `hakutaku.yaml` の自動生成はしない設計（`CFG-015`。ファイルが無ければ
    組み込み既定値で起動する）と整合させるため、実ファイル名は
    `hakutaku.yaml.sample` とし、`hakutaku.yaml` そのものは同梱しません。
    内容は組み込み既定値と完全に一致する**有効な YAML**（コメント付き）に
    しています。理由は、うっかり拡張子を落として `hakutaku.yaml` へ
    リネームしても、`config_version` 欠落による安全モード（`CFG-016`）へは
    落ちず、既定値起動と同じ結果になる安全側の構成にするためです。

    このサンプルはリポジトリへ別ファイルとして置かず、このスクリプト内に
    テキストとして埋め込んでいます（`New-ConfigSampleContent`）。配置場所を
    新設せず、スキーマ変更時もこのスクリプト1箇所を直せば済むようにする
    ための判断です。スキーマの正本は [`crates/config/src/schema.rs`]
    （既定値）と [`crates/config/src/load.rs`]（キー名・許容値）です。

    ## 出力先

    生成する ZIP はリポジトリへコミットしません。`-OutputDir` にはリポジトリ外
    または一時領域を指定してください。リポジトリ内のパスを指定した場合は
    エラーで停止します。

.PARAMETER Version
    本体配布 ZIP のファイル名に使う版。省略時は `src-tauri/Tauri.toml` の
    `version` を読み取ります。

.PARAMETER RuntimeVersion
    Runtime 追加パッケージのファイル名に使う Fixed Version Runtime の版。
    既定値は現在固定している版（`docs/development/windows-build-compatibility.md`
    の「Fixed Version WebView2 Runtime の版」と同じ値）。

.PARAMETER OutputDir
    ZIP の出力先ディレクトリ（必須）。リポジトリ外または一時領域を指定します。
    存在しない場合は作成します。

.PARAMETER RepoRoot
    リポジトリのルート。省略時はこのスクリプトの1つ上のディレクトリ。

.PARAMETER ReleaseExePath
    release ビルド済みの `Hakutaku.exe` のパス。省略時は
    `<RepoRoot>/target/x86_64-pc-windows-msvc/release/Hakutaku.exe`。
    事前に `npm run tauri -- build --no-bundle` を実行してください。

.PARAMETER ReleaseNotesPath
    本体 ZIP へ `release-notes.md` として同梱するリリースノートの原本パス。
    省略時は `<RepoRoot>/docs/release-notes/<Version>.md`。存在しない場合は
    警告のうえ同梱をスキップします。

.PARAMETER RuntimeSourcePath
    Fixed Version Runtime 一式のパス。省略時は
    `<RepoRoot>/runtime/WebView2Runtime`
    （`docs/development/webview2-runtime.md` が定める取得物の保管場所）。
    このフォルダは Git 管理外のため、開発端末ごとに用意されている前提です。

.PARAMETER SkipRuntimePackage
    指定すると Runtime 追加パッケージの作成を省略し、本体 ZIP だけを作ります。
    `RuntimeSourcePath` が用意できていない環境向け。

.PARAMETER Force
    出力先に同名の ZIP が既にある場合、指定すると上書きします。指定しない場合は
    既存 ZIP があるとエラーで停止します。

.EXAMPLE
    # 両方の ZIP を一時領域へ作る（開発端末に runtime/WebView2Runtime がある場合）。
    ./scripts/package-release.ps1 -OutputDir C:\temp\hakutaku-dist

.EXAMPLE
    # Runtime 一式が用意できていない環境で、本体 ZIP だけを作る。
    ./scripts/package-release.ps1 -OutputDir C:\temp\hakutaku-dist -SkipRuntimePackage

.EXAMPLE
    # Runtime 一式を別の場所（別 worktree など）から参照して両方作る。
    ./scripts/package-release.ps1 -OutputDir C:\temp\hakutaku-dist `
        -RuntimeSourcePath C:\Product\Hakutaku\runtime\WebView2Runtime
#>
[CmdletBinding()]
param(
    [string]$Version,

    [string]$RuntimeVersion = "150.0.4078.105",

    [Parameter(Mandatory = $true)]
    [string]$OutputDir,

    [string]$RepoRoot,

    [string]$ReleaseExePath,

    [string]$ReleaseNotesPath,

    [string]$RuntimeSourcePath,

    [switch]$SkipRuntimePackage,

    [switch]$Force
)

$ErrorActionPreference = "Stop"

if (-not $RepoRoot) {
    $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
}
else {
    $RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
}

if (-not $Version) {
    $tauriConfPath = Join-Path $RepoRoot "src-tauri\Tauri.toml"
    if (-not (Test-Path -LiteralPath $tauriConfPath)) {
        throw "バージョンを自動検出できません（$tauriConfPath が見つかりません）。-Version を指定してください。"
    }
    $versionLine = Select-String -LiteralPath $tauriConfPath -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $versionLine) {
        throw "$tauriConfPath から version を読み取れませんでした。-Version を指定してください。"
    }
    $Version = $versionLine.Matches[0].Groups[1].Value
}

if (-not $ReleaseExePath) {
    $ReleaseExePath = Join-Path $RepoRoot "target\x86_64-pc-windows-msvc\release\Hakutaku.exe"
}
if (-not (Test-Path -LiteralPath $ReleaseExePath)) {
    throw "release ビルドの実行ファイルが見つかりません: $ReleaseExePath`n先に 'npm run tauri -- build --no-bundle' を実行してください。"
}

if (-not $ReleaseNotesPath) {
    $ReleaseNotesPath = Join-Path $RepoRoot "docs\release-notes\$Version.md"
}

if (-not $RuntimeSourcePath) {
    $RuntimeSourcePath = Join-Path $RepoRoot "runtime\WebView2Runtime"
}

# 出力先を作成し、リポジトリ内でないことを確認する（ZIP をコミット対象へ
# 混入させないための安全弁。$RepoRoot 配下を弾く）。
$outputParent = Split-Path -Parent $OutputDir
if ($outputParent -and -not (Test-Path -LiteralPath $outputParent)) {
    New-Item -ItemType Directory -Force -Path $outputParent | Out-Null
}
if (-not (Test-Path -LiteralPath $OutputDir)) {
    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
}
$outputDirFull = (Resolve-Path -LiteralPath $OutputDir).Path.TrimEnd('\') + '\'
$repoRootFull = $RepoRoot.TrimEnd('\') + '\'
if ($outputDirFull.StartsWith($repoRootFull, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputDir がリポジトリ内です ($OutputDir)。生成した ZIP をコミットしないため、リポジトリ外または一時領域を指定してください。"
}

function New-ConfigSampleContent {
    <#
        組み込み既定値（crates/config/src/schema.rs）と完全に一致する、有効な
        hakutaku.yaml.sample の内容を返す。キー名・許容値の正本は
        crates/config/src/load.rs のバリデータ。
    #>
    @'
# Hakutaku 設定ファイルのサンプルです。
#
# このファイルは既定名（hakutaku.yaml.sample）のままでは読み込まれません
# （CFG-015: hakutaku.yaml が存在しない場合は組み込み既定値で起動します）。
# 変更したい項目だけ書き換え、Hakutaku.exe と同じフォルダへ
# 「hakutaku.yaml」という名前で置いてください。
#
# 内容は書き換えなくても、そのまま hakutaku.yaml へリネームして使えます
# （すべて組み込み既定値と同じ値のため、既定値起動と同じ結果になります）。
# 構文または値が不正な hakutaku.yaml を置いた場合は、既定値へ黙って戻さず
# 安全モード（設定由来のデータソース・プロファイル・キャッシュを無効化した
# 状態）で起動します（CFG-016）。

config_version: 1

# メモリ予算（CFG-007）。Rust コアのヒープ確保量の合計に対する上限（MiB）。
memory:
  budget_mib: 2048

# クリップボードコピーの上限（CFG-018）。UTF-8 換算バイト数と行数のうち、
# 先に到達した方で全体コピーを拒否します（COPY-004、COPY-005）。
clipboard:
  max_copy_mib: 16
  max_copy_lines: 100000

# 診断ログのローテーション（CFG-020）。
diagnostics:
  rotate_mib: 10
  keep_generations: 5

# フロントエンドが保持する行データの上限（CFG-022、PERF-012）。
# 可視範囲と前後バッファ分だけを保持し、超過分は遠い範囲から破棄します。
frontend:
  max_rows: 10000
  max_mib: 64

# WebView2 Runtime の選択（CFG-023、DIST-017）。
# force_fixed_version_runtime: true にすると、Evergreen Runtime が導入済み
# でも実行ファイル直下の WebView2Runtime（Fixed Version）を優先使用します。
# 既定は自動判定（Evergreen を優先し、無ければ Fixed Version）。
webview2:
  force_fixed_version_runtime: false

# 解析処理の資源抑制（CFG-024）。対象端末本体での実行を前提に控えめな値です。
performance:
  parse_concurrency: 2
  io_interval_ms: 0
  # normal / below_normal / idle のいずれか。
  process_priority: below_normal

# 事前定義データソース（CFG-003、PROD-006）。既定は空。例:
#
# data_sources:
#   - name: 本日のログ
#     path: C:\ProductLogs\today
data_sources: []

# ログ解析プロファイル（CFG-008）。既定は空。例（絶対パス完全一致、
# または優先度付き glob。文字コード自動判定、または明示指定・
# 任意の Windows コードページ識別子）:
#
# encoding に書けるのは auto、utf-8、windows-<コードページ番号>
# （例: windows-932）だけです。shift_jis のような別名は使えません。
# 任意の Windows コードページを明示する場合は、encoding: auto のまま
# ansi_codepage に番号を書きます（ENC-005、ENC-007）。明示指定は
# 自動判定より優先し、BOM と矛盾する場合は警告して明示指定を使います。
#
# datetime_format に書けるのは auto（既定。内容から自動判定）と、
# LOG-DT-001〜LOG-DT-006 の6書式だけです（大文字・小文字は区別します）。
#
#   LOG-DT-001: YYYY/MM/DD HH:mm:ss.SSS   LOG-DT-002: YYYY-MM-DD HH:mm:ss:SSS
#   LOG-DT-003: YYYY/MM/DD HH:mm:ss.SS    LOG-DT-004: YYYY/MM/DD HH:mm:ss:SS
#   LOG-DT-005: YYYY/MM/DD HH:mm:ss       LOG-DT-006: YYYY/MM/DD HH:mm
#
# 明示すると内容による自動判定を行わず、その書式だけで解析します。
# とくに LOG-DT-004（HH:mm:ss:SS）は、内容からは LOG-DT-005（HH:mm:ss）と
# 区別できないため、明示しないと日時未解析の生表示になります（LOG-022）。
#
# log_profiles:
#   - name: 同じ端末で稼働する他の業務ソフトウェアの標準ログ
#     path_pattern: C:\ProductLogs\*.log
#     priority: 0
#     encoding: auto
#   - name: コードページ 932（Shift_JIS 系）固定のログ
#     path_pattern: C:\ProductLogs\legacy\*.log
#     priority: 10
#     encoding: auto
#     ansi_codepage: 932
#   - name: 1/100秒をコロンで区切るログ（日時書式を明示）
#     path_pattern: C:\ProductLogs\centisecond\*.log
#     priority: 0
#     datetime_format: LOG-DT-004
log_profiles: []
'@
}

function Test-DirectoryHasContent {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) {
        return $false
    }
    return (Get-ChildItem -LiteralPath $Path -Force -ErrorAction SilentlyContinue | Select-Object -First 1) -ne $null
}

function Get-FileSizeReport {
    param([string]$Path)
    $item = Get-Item -LiteralPath $Path
    $bytes = $item.Length
    [PSCustomObject]@{
        Path  = $Path
        Bytes = $bytes
        MiB   = [Math]::Round($bytes / 1MB, 2)
        MB    = [Math]::Round($bytes / 1000000, 2)
    }
}

$stagingRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("hakutaku-package-release-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $stagingRoot | Out-Null

try {
    # ---- 本体 ZIP: Hakutaku-<版>.zip ----
    $bodyStagingDir = Join-Path $stagingRoot "Hakutaku"
    New-Item -ItemType Directory -Force -Path $bodyStagingDir | Out-Null

    Copy-Item -LiteralPath $ReleaseExePath -Destination (Join-Path $bodyStagingDir "Hakutaku.exe")

    $configSamplePath = Join-Path $bodyStagingDir "hakutaku.yaml.sample"
    Set-Content -LiteralPath $configSamplePath -Value (New-ConfigSampleContent) -Encoding utf8NoBOM -NoNewline

    if (Test-Path -LiteralPath $ReleaseNotesPath) {
        Copy-Item -LiteralPath $ReleaseNotesPath -Destination (Join-Path $bodyStagingDir "release-notes.md")
    }
    else {
        Write-Warning "リリースノートが見つかりません: $ReleaseNotesPath （本体 ZIP への同梱を省略します）"
    }

    $bodyZipPath = Join-Path $OutputDir "Hakutaku-$Version.zip"
    if ((Test-Path -LiteralPath $bodyZipPath) -and -not $Force) {
        throw "既に存在します: $bodyZipPath （上書きするには -Force を指定してください）"
    }
    if (Test-Path -LiteralPath $bodyZipPath) {
        Remove-Item -LiteralPath $bodyZipPath -Force
    }
    Compress-Archive -Path $bodyStagingDir -DestinationPath $bodyZipPath -CompressionLevel Optimal

    $bodyReport = Get-FileSizeReport -Path $bodyZipPath
    Write-Host "本体 ZIP を作成しました: $($bodyReport.Path) ($($bodyReport.MiB) MiB / $($bodyReport.MB) MB)"
    if ($bodyReport.MB -gt 100) {
        Write-Warning "本体 ZIP が 100 MB (DIST-003 目標) を超えています。同梱物を見直してください。"
    }

    # ---- Runtime 追加パッケージ: Hakutaku-WebView2Runtime-<Runtime版>.zip ----
    if ($SkipRuntimePackage) {
        Write-Host "Runtime 追加パッケージの作成を省略しました（-SkipRuntimePackage）。"
    }
    elseif (-not (Test-DirectoryHasContent -Path $RuntimeSourcePath)) {
        $runtimeMissingMessage = "Fixed Version Runtime の取得物が見つかりません: $RuntimeSourcePath " +
            "docs/development/webview2-runtime.md の手順で取得・配置するか、-RuntimeSourcePath で別の場所を指定してください。" +
            " Runtime 追加パッケージの作成を省略します。"
        Write-Warning $runtimeMissingMessage
    }
    else {
        $runtimeZipPath = Join-Path $OutputDir "Hakutaku-WebView2Runtime-$RuntimeVersion.zip"
        if ((Test-Path -LiteralPath $runtimeZipPath) -and -not $Force) {
            throw "既に存在します: $runtimeZipPath （上書きするには -Force を指定してください）"
        }
        if (Test-Path -LiteralPath $runtimeZipPath) {
            Remove-Item -LiteralPath $runtimeZipPath -Force
        }

        # Compress-Archive は -Path に渡したフォルダ自身をアーカイブのルート
        # エントリとして含める（中身だけを平坦化しない）。これにより ZIP を
        # 直接 Hakutaku/ 直下へ展開すると Hakutaku/WebView2Runtime/ になる
        # （DIST-006 の配置）。フォルダ内容は変更しない（DIST-011）ため、
        # ステージング用のコピーは作らずソースを直接圧縮する。
        Compress-Archive -Path $RuntimeSourcePath -DestinationPath $runtimeZipPath -CompressionLevel Optimal

        $runtimeReport = Get-FileSizeReport -Path $runtimeZipPath
        Write-Host "Runtime 追加パッケージを作成しました: $($runtimeReport.Path) ($($runtimeReport.MiB) MiB / $($runtimeReport.MB) MB)"
        Write-Host "（DIST-007: この ZIP のサイズは本体 ZIP の 100 MB 目標に含めません）"
    }
}
finally {
    Remove-Item -LiteralPath $stagingRoot -Recurse -Force -ErrorAction SilentlyContinue
}
