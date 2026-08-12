<#
.SYNOPSIS
    動作確認用のサンプルログ一式と、対応する設定ファイルを生成します。

.DESCRIPTION
    [手動動作確認の手順](../docs/verification/manual-check.md) と対で使う
    スクリプトです。手順書の各確認項目に必要なサンプルを、決まった名前で
    一度に生成します。

    行の生成そのものは `scripts/generate-test-log.ps1` に委ね、このスクリプトは
    「どの書式・文字コード・行数のファイルを、どの名前で作るか」だけを決めます
    （生成ロジックを二重に持たないため）。

    生成物は既定で `%TEMP%\hakutaku-samples` に置き、リポジトリ内を出力先に
    指定した場合はエラーで停止します。試験データをリポジトリへ残さないため
    です。合成データだけを生成し、実データ（個人情報等の機密データを含み得る
    ログ）は一切扱いません。

    既定の `-StartTimestamp` は固定値です。`-LargeLineCount` を変えない限り、
    同じ引数で実行すれば毎回同じ内容のファイルが生成されます（手順書に
    具体的な日時・行数を書けるようにするため）。

    生成するもの:

    | ファイル | 用途（対応する要件） |
    | --- | --- |
    | `01-basic-utf8.log` | 基本の読み込みと表示（`LOG-001`、`LOG-020`） |
    | `02-dt-001.log`〜`02-dt-006.log` | 既知の6書式と元の精度の保持（`LOG-009`、`LOG-024`、`LOG-025`）。`02-dt-004.log` だけは自動判定が曖昧になるため、生成する `hakutaku.yaml` で書式を明示する（`LOG-022`、`CFG-008`。後述） |
    | `03-encoding-utf8-bom.log` | UTF-8 BOM あり（`ENC-003`、`ENC-005`） |
    | `03-encoding-cp932.log` | コードページ 932（`ENC-001`、`ENC-007`、`CFG-008`） |
    | `03-encoding-cp1252.log` | コードページ 1252（`ENC-004`、`CFG-008`） |
    | `04-continuation.log` | 日時なし継続行の結合（`LOG-014`） |
    | `05-merge-a/b/c.log` | 複数ファイルと時系列統合表示（`LOG-006`〜`008`、`LOG-015`） |
    | `06-unconfirmed-tail.log` | 末尾が行の途中で終わる未確定行（`LOG-026`） |
    | `07-leading-no-datetime.log` | 先頭の日時なし行が破棄されないこと（`LOG-014`） |
    | `08-large.log` | 仮想スクロール、行番号ジャンプ、コピー上限（`PERF-007`、`COPY-004`／`005`） |
    | `hakutaku.yaml` | 正常起動と事前定義データソース・解析プロファイル（`CFG-003`、`CFG-008`、`CFG-014`） |
    | `hakutaku-invalid.yaml` | 安全モード起動（`CFG-016`） |

    `02-dt-004.log`（`YYYY/MM/DD HH:mm:ss:SS`）は、自動判定では必ず
    `LOG-DT-005` とも同時に成立するため、設定がなければ常に曖昧判定となり
    日時未解析の生表示へ退避します（`LOG-022`。`crates/core-services/src/loader.rs`
    の doc コメント「日時書式の決め方」）。生成する `hakutaku.yaml` には、この
    ファイル向けに `datetime_format: LOG-DT-004` を明示したプロファイルを
    入れてあるため、設定を置いた状態では日時が解析されます（`CFG-008`）。
    設定を外した状態と見比べると、`LOG-022` の生表示退避も確認できます。

.PARAMETER OutputDir
    生成先フォルダ。既定は `%TEMP%\hakutaku-samples`。リポジトリ内は指定
    できません。

.PARAMETER LargeLineCount
    `08-large.log` の行数。既定は 300000（約 35 MiB。段階0の計測と同じ規模）。
    `0` を指定するとこのファイルを生成しません（生成時間と容量を抑えたい
    場合）。

.PARAMETER StartTimestamp
    生成する最初の行の日時。既定は `2026/07/28 09:00:00.000`（固定値。
    再現性のため）。

.PARAMETER Force
    生成先フォルダに既存の内容があっても続行し、同名ファイルを上書きします。

.EXAMPLE
    # 既定（%TEMP%\hakutaku-samples）へ一式を生成する。
    ./scripts/generate-sample-logs.ps1

.EXAMPLE
    # 大きいファイルを省いて短時間で生成し直す。
    ./scripts/generate-sample-logs.ps1 -LargeLineCount 0 -Force

.EXAMPLE
    # 別のドライブへ生成する。
    ./scripts/generate-sample-logs.ps1 -OutputDir D:\hakutaku-samples
#>
[CmdletBinding()]
param(
    [string]$OutputDir = (Join-Path ([System.IO.Path]::GetTempPath()) "hakutaku-samples"),

    [ValidateRange(0, [int]::MaxValue)]
    [int]$LargeLineCount = 300000,

    [datetime]$StartTimestamp = ([datetime]"2026-07-28T09:00:00.000"),

    [switch]$Force
)

$ErrorActionPreference = "Stop"

$generatorPath = Join-Path $PSScriptRoot "generate-test-log.ps1"
if (-not (Test-Path -LiteralPath $generatorPath)) {
    throw "行生成スクリプトが見つかりません: $generatorPath"
}

# 生成先がリポジトリ内でないことを確認する（試験データをリポジトリへ残さない。
# scripts/package-release.ps1 の OutputDir 判定と同じ考え方）。
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path.TrimEnd('\') + '\'
$outputDirFull = [System.IO.Path]::GetFullPath($OutputDir).TrimEnd('\') + '\'
if ($outputDirFull.StartsWith($repoRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputDir がリポジトリ内です ($OutputDir)。試験データをリポジトリへ残さないため、リポジトリ外（一時領域など）を指定してください。"
}

if (Test-Path -LiteralPath $OutputDir) {
    $existing = Get-ChildItem -LiteralPath $OutputDir -Force -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($existing -and -not $Force) {
        throw "生成先に既存の内容があります: $OutputDir （上書きするには -Force を指定してください）"
    }
}
else {
    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
}

# 生成したファイルの一覧（最後の要約表示に使う）。
$generated = [System.Collections.Generic.List[object]]::new()

function New-Sample {
    <#
        generate-test-log.ps1 を呼んでサンプルを1つ作り、一覧へ記録する。
        $ExtraParameters には -Format／-Encoding／-ContinuationLineRate／-Seed
        など、行生成スクリプト側のパラメーターをそのまま渡す。
    #>
    param(
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][int]$LineCount,
        [Parameter(Mandatory = $true)][string]$Purpose,
        [datetime]$Start = $StartTimestamp,
        [hashtable]$ExtraParameters = @{}
    )

    $path = Join-Path $OutputDir $FileName
    $arguments = @{
        LineCount      = $LineCount
        OutputPath     = $path
        StartTimestamp = $Start
    }
    foreach ($key in $ExtraParameters.Keys) {
        $arguments[$key] = $ExtraParameters[$key]
    }

    & $generatorPath @arguments | Out-Null

    $generated.Add([PSCustomObject]@{
            FileName = $FileName
            Bytes    = (Get-Item -LiteralPath $path).Length
            Purpose  = $Purpose
        })
    return $path
}

Write-Host "サンプルを生成します: $OutputDir"

# --- 01: 基本 -----------------------------------------------------------
New-Sample -FileName "01-basic-utf8.log" -LineCount 2000 `
    -Purpose "基本の読み込みと表示（LOG-001、LOG-020）" | Out-Null

# --- 02: 既知の6書式 ----------------------------------------------------
$formatPurposes = @{
    'LOG-DT-001' = "ミリ秒3桁（LOG-009、LOG-024）"
    'LOG-DT-002' = "ハイフン日付区切り・ミリ秒3桁（LOG-009）"
    'LOG-DT-003' = "1/100秒2桁。「.45」が 450 ミリ秒（LOG-025）"
    'LOG-DT-004' = "1/100秒2桁（コロン区切り）。自動判定は曖昧になるため hakutaku.yaml で書式を明示する（LOG-022、CFG-008）"
    'LOG-DT-005' = "秒精度（LOG-009）"
    'LOG-DT-006' = "分精度。「15:12」が秒まで補われて見えないこと（LOG-024）"
}
foreach ($index in 1..6) {
    $formatId = "LOG-DT-00$index"
    New-Sample -FileName ("02-dt-00{0}.log" -f $index) -LineCount 200 `
        -Purpose $formatPurposes[$formatId] `
        -ExtraParameters @{ Format = $formatId } | Out-Null
}

# --- 03: 文字コード ------------------------------------------------------
New-Sample -FileName "03-encoding-utf8-bom.log" -LineCount 200 `
    -Purpose "UTF-8 BOM あり（ENC-003、ENC-005）" `
    -ExtraParameters @{ Encoding = 'Utf8Bom' } | Out-Null
New-Sample -FileName "03-encoding-cp932.log" -LineCount 200 `
    -Purpose "コードページ 932（ENC-001、ENC-007、CFG-008）" `
    -ExtraParameters @{ Encoding = 'CP932' } | Out-Null
New-Sample -FileName "03-encoding-cp1252.log" -LineCount 200 `
    -Purpose "コードページ 1252／西欧言語（ENC-004、CFG-008）" `
    -ExtraParameters @{ Encoding = 'CP1252' } | Out-Null

# --- 04: 継続行 ----------------------------------------------------------
# -Seed 固定で、継続行の位置を実行のたびに変えない（手順書に期待結果を
# 書けるようにするため）。
New-Sample -FileName "04-continuation.log" -LineCount 500 `
    -Purpose "日時なし継続行の結合（LOG-014）" `
    -ExtraParameters @{ ContinuationLineRate = 0.2; Seed = 42 } | Out-Null

# --- 05: 時系列統合用の3ファイル ----------------------------------------
# 同じ開始時刻だと3ファイルの日時が完全に一致してしまう（行ごとの時刻の
# 進み方が行番号だけで決まるため）。数ミリ秒ずつずらして、統合表示で
# 3ファイルの行が交互に並ぶようにする。
$mergeOffsets = @{ 'a' = 0; 'b' = 7; 'c' = 13 }
foreach ($suffix in @('a', 'b', 'c')) {
    New-Sample -FileName ("05-merge-{0}.log" -f $suffix) -LineCount 300 `
        -Purpose ("時系列統合表示（LOG-006〜008、LOG-015）。開始時刻を {0}ms ずらしている" -f $mergeOffsets[$suffix]) `
        -Start $StartTimestamp.AddMilliseconds($mergeOffsets[$suffix]) | Out-Null
}

# --- 06: 末尾が未確定行のファイル ---------------------------------------
$unconfirmedPath = New-Sample -FileName "06-unconfirmed-tail.log" -LineCount 200 `
    -Purpose "末尾が行の途中で終わる未確定行（LOG-026）"

# 末尾の改行を取り除き、最終行を「書き込み途中の可能性がある未確定行」に
# する（scripts/append-log-writer.ps1 -NoFinalNewline と同じ状態を、追記
# プロセスを動かさずに作る）。
$unconfirmedStream = [System.IO.File]::Open($unconfirmedPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite)
try {
    $length = $unconfirmedStream.Length
    while ($length -gt 0) {
        $unconfirmedStream.Position = $length - 1
        $lastByte = $unconfirmedStream.ReadByte()
        if ($lastByte -ne 0x0A -and $lastByte -ne 0x0D) {
            break
        }
        $length--
    }
    $unconfirmedStream.SetLength($length)
}
finally {
    $unconfirmedStream.Dispose()
}
($generated | Where-Object { $_.FileName -eq "06-unconfirmed-tail.log" }).Bytes =
    (Get-Item -LiteralPath $unconfirmedPath).Length

# --- 07: 先頭に日時なし行があるファイル ---------------------------------
$leadingPath = New-Sample -FileName "07-leading-no-datetime.log" -LineCount 200 `
    -Purpose "先頭の日時なし行が破棄されないこと（LOG-014）"

# 直前に日時付き行が存在しない行は、継続行として結合できない。破棄されず
# 独立した日時未確定の項目になることを確認するため、先頭へ2行を差し込む。
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$leadingText = "起動準備中です`r`n初期化しています`r`n" + [System.IO.File]::ReadAllText($leadingPath, $utf8NoBom)
[System.IO.File]::WriteAllText($leadingPath, $leadingText, $utf8NoBom)
($generated | Where-Object { $_.FileName -eq "07-leading-no-datetime.log" }).Bytes =
    (Get-Item -LiteralPath $leadingPath).Length

# --- 08: 大きいファイル --------------------------------------------------
if ($LargeLineCount -gt 0) {
    New-Sample -FileName "08-large.log" -LineCount $LargeLineCount `
        -Purpose "仮想スクロール・行番号ジャンプ・コピー上限（PERF-007、COPY-004／005）" `
        -ExtraParameters @{ ProgressEveryLines = 100000 } | Out-Null
}
else {
    Write-Host "08-large.log は -LargeLineCount 0 のため生成しません。"
}

# --- 設定ファイル --------------------------------------------------------

function Format-YamlSingleQuoted {
    <#
        パスを YAML の単一引用符スカラーとして書く。単一引用符スカラーでは
        バックスラッシュがエスケープとして解釈されないため、Windows パスを
        そのまま書ける（`'` だけは2つ重ねて表す）。
    #>
    param([Parameter(Mandatory = $true)][string]$Value)
    return "'" + $Value.Replace("'", "''") + "'"
}

$sampleDirYaml = Format-YamlSingleQuoted -Value ($OutputDir.TrimEnd('\'))
$basicYaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "01-basic-utf8.log")
$missingYaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "99-missing.log")
$cp932Yaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "03-encoding-cp932.log")
$cp1252Yaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "03-encoding-cp1252.log")
$dt004Yaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "02-dt-004.log")

# 値の正本は crates/config/src/load.rs のバリデータ。ここでは省略可能な項目を
# 省き、動作確認に必要な data_sources と log_profiles だけを明示する
# （省略した項目は組み込み既定値。crates/config/src/schema.rs）。
$configText = @"
# 動作確認用の設定ファイル（scripts/generate-sample-logs.ps1 が生成）。
# Hakutaku.exe と同じフォルダへ「hakutaku.yaml」という名前でコピーして
# 使ってください（CFG-014）。手順は docs/verification/manual-check.md。

config_version: 1

# 事前定義データソース（CFG-003、PROD-006）。左ペインの「参照対象」一覧に
# 名前で並び、クリックで開けます。パスはフロントエンドへ渡しません（SEC-012）。
data_sources:
  - name: 'サンプル: 基本のログ'
    path: $basicYaml
  - name: 'サンプル: フォルダ（未対応の確認用）'
    path: $sampleDirYaml
  - name: 'サンプル: 存在しないファイル（エラー表示の確認用）'
    path: $missingYaml

# ログ解析プロファイル（CFG-008、LOG-021）。glob 記号を含まない path_pattern は
# 絶対パス完全一致として扱われます。encoding に指定できるのは auto または
# utf-8／windows-<コードページ番号> です（crates/format-detection の
# parse_named_encoding）。datetime_format に指定できるのは auto または
# LOG-DT-001〜LOG-DT-006 です。
log_profiles:
  - name: 'サンプル: CP932（ansi_codepage 指定）'
    path_pattern: $cp932Yaml
    priority: 0
    encoding: auto
    ansi_codepage: 932
  - name: 'サンプル: CP1252（encoding 名前指定）'
    path_pattern: $cp1252Yaml
    priority: 0
    encoding: windows-1252
  # HH:mm:ss:SS は内容からは HH:mm:ss と区別できないため、書式を明示しないと
  # 日時未解析の生表示になります（LOG-022）。
  - name: 'サンプル: 1/100秒コロン区切り（datetime_format 指定）'
    path_pattern: $dt004Yaml
    priority: 0
    datetime_format: LOG-DT-004
"@

$configPath = Join-Path $OutputDir "hakutaku.yaml"
Set-Content -LiteralPath $configPath -Value $configText -Encoding utf8NoBOM
$generated.Add([PSCustomObject]@{
        FileName = "hakutaku.yaml"
        Bytes    = (Get-Item -LiteralPath $configPath).Length
        Purpose  = "正常起動と事前定義データソース・プロファイル（CFG-003、CFG-008、CFG-014）"
    })

# CFG-016（安全モード）の確認用。構文は正しいが値と項目が不正なため、既定値へ
# 黙って戻さず、ファイル名・行・列・理由を示して安全モードで起動するはず。
$invalidConfigText = @"
# 安全モード（CFG-016）の確認用に、わざと不正な値を含めた設定ファイルです。
# Hakutaku.exe と同じフォルダへ「hakutaku.yaml」という名前でコピーして
# 使ってください（正常な設定と入れ替えて使います）。

config_version: 1

memory:
  # 数値ではない値（不正）。
  budget_mib: にせんよんじゅうはち

clipboard:
  # 負数（不正）。
  max_copy_lines: -1

# 未知のキー（不正）。
unknown_section:
  foo: bar
"@

$invalidConfigPath = Join-Path $OutputDir "hakutaku-invalid.yaml"
Set-Content -LiteralPath $invalidConfigPath -Value $invalidConfigText -Encoding utf8NoBOM
$generated.Add([PSCustomObject]@{
        FileName = "hakutaku-invalid.yaml"
        Bytes    = (Get-Item -LiteralPath $invalidConfigPath).Length
        Purpose  = "安全モード起動（CFG-016）"
    })

# --- 要約 ----------------------------------------------------------------
$totalBytes = ($generated | Measure-Object -Property Bytes -Sum).Sum
Write-Host ""
Write-Host "生成しました: $OutputDir （$($generated.Count) ファイル, 合計 約 $([Math]::Round($totalBytes / 1MB, 2)) MiB）"
Write-Host ""
foreach ($item in $generated) {
    Write-Host ("  {0,-28} {1,10} B  {2}" -f $item.FileName, $item.Bytes, $item.Purpose)
}
Write-Host ""
Write-Host "確認手順: docs/verification/manual-check.md"
Write-Host "後片付け: 確認が終わったらフォルダごと削除してください（Remove-Item -Recurse -Force '$OutputDir'）。"
