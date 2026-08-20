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
    | `09-mixed-app/service/legacy/western.log` | 書式・文字コードが異なる4ファイルの時系列統合（`LOG-006`〜`008`、`LOG-015`、`LOG-016`、`CFG-008`） |
    | `10-medium-100k.log` | 10万行、事前定義データソースから常に開ける（`-LargeLineCount 0` の影響を受けない。`PERF-007`） |
    | `11-wide-line.log` | 数千文字の行を含み、横スクロールがログ表示領域の内側だけで起きること（Issue #78） |
    | `hakutaku.yaml` | 正常起動と事前定義データソース・解析プロファイル（`CFG-003`、`CFG-008`、`CFG-014`）。事前定義データソースは、生成した通常のログファイルすべて（`hakutaku-invalid.yaml` と、このスクリプトが生成しない `90-locked.log`／`91-append.log` を除く）を指し、1回の起動で概ねのパターンを網羅する。すべて正常に開けるものだけで構成し、エラー確認用の項目は含めない |
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

# --- 09: 異種ログ4ファイル（書式・文字コードの異なる時系列統合） ------------
# 05 節（同形式・3ファイル）に対し、書式・文字コードが異なる複数ログの統合
# 表示を確認するためのセット（LOG-006〜008、LOG-015、LOG-016、CFG-008）。
# 05 節と同じ理由で、開始時刻を数ミリ秒ずつずらし、統合表示で4ファイルの
# 行が交互に並ぶようにする（同時刻だと日時だけでは順序が決まらない）。
New-Sample -FileName "09-mixed-app.log" -LineCount 300 `
    -Purpose "異種ログの時系列統合／アプリ（LOG-DT-001、UTF-8、継続行あり。LOG-006〜008、LOG-015、LOG-016、CFG-008）" `
    -Start $StartTimestamp.AddMilliseconds(0) `
    -ExtraParameters @{ Format = 'LOG-DT-001'; Encoding = 'Utf8NoBom'; ContinuationLineRate = 0.15; Seed = 7 } | Out-Null
New-Sample -FileName "09-mixed-service.log" -LineCount 300 `
    -Purpose "異種ログの時系列統合／サービス（LOG-DT-002、UTF-8 BOM。LOG-006〜008、LOG-015、LOG-016、CFG-008）" `
    -Start $StartTimestamp.AddMilliseconds(3) `
    -ExtraParameters @{ Format = 'LOG-DT-002'; Encoding = 'Utf8Bom' } | Out-Null
New-Sample -FileName "09-mixed-legacy.log" -LineCount 300 `
    -Purpose "異種ログの時系列統合／旧システム（LOG-DT-005、CP932。LOG-006〜008、LOG-015、LOG-016、CFG-008）" `
    -Start $StartTimestamp.AddMilliseconds(5) `
    -ExtraParameters @{ Format = 'LOG-DT-005'; Encoding = 'CP932' } | Out-Null
New-Sample -FileName "09-mixed-western.log" -LineCount 300 `
    -Purpose "異種ログの時系列統合／海外拠点（LOG-DT-003、CP1252。LOG-006〜008、LOG-015、LOG-016、CFG-008）" `
    -Start $StartTimestamp.AddMilliseconds(9) `
    -ExtraParameters @{ Format = 'LOG-DT-003'; Encoding = 'CP1252' } | Out-Null

# --- 10: 10万行のログ（データソースから常に開けるようにする） -------------
# -LargeLineCount 0 で 08-large.log を省略した場合でも、事前定義データソース
# の「10 大きめのログ（100,000行）」は常に開けるようにするため、行数を
# 固定（-LargeLineCount の影響を受けない）で生成する（PERF-007）。
New-Sample -FileName "10-medium-100k.log" -LineCount 100000 `
    -Purpose "大きめのログ、事前定義データソースから常に開ける（PERF-007）" `
    -ExtraParameters @{ ProgressEveryLines = 50000 } | Out-Null

# --- 11: 横に長い行を含むログ --------------------------------------------
# Issue #78: 行を折り返さない設計（white-space: pre）のため、極端に長い行が
# あると横スクロールが必要になる。そのスクロールがログ表示領域の内側だけで
# 起き、ウィンドウ全体（左ペインを含む）が横に流れないことを確認するための
# サンプル。行数は少なくてよい（横方向だけが論点のため）。
$widePath = New-Sample -FileName "11-wide-line.log" -LineCount 50 `
    -Purpose "横に長い行の横スクロール封じ込め（Issue #78）" `
    -ExtraParameters @{ Format = 'LOG-DT-001' }

# 行生成スクリプトは可変長といっても高々200文字程度の行しか作らないため、
# 生成後に決まった行だけを引き伸ばす（06・07 節と同じ後処理方式）。乱数を
# 使わず、対象行の位置も長さも固定にして、手順書に期待結果を書けるように
# する。長さは2,000〜4,000文字級（1行がウィンドウ幅の数十倍になる）。
$wideLineWidths = @{ 4 = 2000; 14 = 2500; 24 = 3000; 34 = 3500; 44 = 4000 }
$widePayloadSeed = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-"
$widePayloadBase = $widePayloadSeed * [Math]::Ceiling(4000 / $widePayloadSeed.Length)
$wideLines = [System.IO.File]::ReadAllLines($widePath, $utf8NoBom)
foreach ($lineIndex in $wideLineWidths.Keys) {
    $wideLines[$lineIndex] += " ペイロード=" + $widePayloadBase.Substring(0, $wideLineWidths[$lineIndex])
}
[System.IO.File]::WriteAllLines($widePath, $wideLines, $utf8NoBom)
($generated | Where-Object { $_.FileName -eq "11-wide-line.log" }).Bytes =
    (Get-Item -LiteralPath $widePath).Length

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

$cp932Yaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "03-encoding-cp932.log")
$cp1252Yaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "03-encoding-cp1252.log")
$dt004Yaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "02-dt-004.log")
$mixedLegacyYaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "09-mixed-legacy.log")
$mixedWesternYaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir "09-mixed-western.log")

# 事前定義データソースの一覧（ファイル名 → 表示名の対応表）。「1回実行すれば
# 概ねのパターンを網羅した状態でアプリが起動する」ようにするため、生成した
# 通常のログファイルすべて（hakutaku-invalid.yaml と、このスクリプトが生成
# しない 90-locked.log／91-append.log を除く）を data_sources へ登録する。
# 順序はファイル名の昇順（このスクリプトの生成順）に揃えている。ADR-0008に
# より data_sources の記載順がそのまま source_ordinal（時系列統合表示の並び
# 順）になるため、順序を変えると 4.8 節の期待結果も変わる。ファイルが増える
# たびに変数を1つずつ増やす代わりにこの対応表を foreach で回すことで、
# 20件超の記述の重複を避けている（08-large.log の条件付き追加も同じ仕組み）。
$dataSourceFiles = [System.Collections.Generic.List[object]]::new()
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "01-basic-utf8.log"; DisplayName = "01 基本のログ（2,000行）" })
$dtDisplayNames = @(
    "02 日時書式 LOG-DT-001（ミリ秒3桁）",
    "02 日時書式 LOG-DT-002（ハイフン区切り・ミリ秒3桁）",
    "02 日時書式 LOG-DT-003（1/100秒2桁）",
    "02 日時書式 LOG-DT-004（1/100秒2桁・コロン区切り）",
    "02 日時書式 LOG-DT-005（秒精度）",
    "02 日時書式 LOG-DT-006（分精度）"
)
foreach ($index in 1..6) {
    $dataSourceFiles.Add([PSCustomObject]@{
            FileName    = ("02-dt-00{0}.log" -f $index)
            DisplayName = $dtDisplayNames[$index - 1]
        })
}
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "03-encoding-utf8-bom.log"; DisplayName = "03 文字コード UTF-8 BOM あり" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "03-encoding-cp932.log"; DisplayName = "03 文字コード CP932" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "03-encoding-cp1252.log"; DisplayName = "03 文字コード CP1252（西欧言語）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "04-continuation.log"; DisplayName = "04 継続行（約2割が日時なし）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "05-merge-a.log"; DisplayName = "05 統合 a（同形式・+0ms）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "05-merge-b.log"; DisplayName = "05 統合 b（同形式・+7ms）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "05-merge-c.log"; DisplayName = "05 統合 c（同形式・+13ms）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "06-unconfirmed-tail.log"; DisplayName = "06 未確定行（末尾が改行で終わらない）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "07-leading-no-datetime.log"; DisplayName = "07 先頭の日時なし行" })
if ($LargeLineCount -gt 0) {
    # 08-large.log は実際に生成したときだけ登録する（存在しないファイルを指す
    # データソースを作らないため。事前定義データソースはすべて正常に開ける
    # ものだけで構成する方針は DESCRIPTION のとおり）。行数表記は実際に生成
    # した値へ合わせる（既定の 300000 以外を指定した場合も追従する）。
    $largeLineCountText = $LargeLineCount.ToString("N0", [System.Globalization.CultureInfo]::InvariantCulture)
    $largeDisplayName = "08 大きいログ（{0}行）" -f $largeLineCountText
    $dataSourceFiles.Add([PSCustomObject]@{ FileName = "08-large.log"; DisplayName = $largeDisplayName })
}
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "09-mixed-app.log"; DisplayName = "09 異種 アプリ（LOG-DT-001／UTF-8／継続行あり）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "09-mixed-service.log"; DisplayName = "09 異種 サービス（LOG-DT-002／UTF-8 BOM）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "09-mixed-legacy.log"; DisplayName = "09 異種 旧システム（LOG-DT-005／CP932）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "09-mixed-western.log"; DisplayName = "09 異種 海外拠点（LOG-DT-003／CP1252）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "10-medium-100k.log"; DisplayName = "10 大きめのログ（100,000行）" })
$dataSourceFiles.Add([PSCustomObject]@{ FileName = "11-wide-line.log"; DisplayName = "11 横に長い行（数千文字）" })

$dataSourceLines = [System.Collections.Generic.List[string]]::new()
foreach ($entry in $dataSourceFiles) {
    $entryPathYaml = Format-YamlSingleQuoted -Value (Join-Path $OutputDir $entry.FileName)
    $entryNameYaml = Format-YamlSingleQuoted -Value $entry.DisplayName
    $dataSourceLines.Add("  - name: $entryNameYaml")
    $dataSourceLines.Add("    path: $entryPathYaml")
}
$dataSourceBlock = $dataSourceLines -join "`n"

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
# 生成した通常のログファイルすべてを指すため、この設定だけで概ねのパターンを
# 開けます（-LargeLineCount 0 のときは 08-large.log の分だけ1件減ります）。
data_sources:
$dataSourceBlock

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
  # 異種ログの文字コードを実行環境の ANSI コードページに左右されず確定させる
  # ため（統合表示の期待結果を環境非依存にするため）、09-mixed-legacy.log／
  # 09-mixed-western.log にも明示のプロファイルを与える。
  - name: 'サンプル: 異種 旧システム（CP932）'
    path_pattern: $mixedLegacyYaml
    priority: 0
    encoding: auto
    ansi_codepage: 932
  - name: 'サンプル: 異種 海外拠点（CP1252）'
    path_pattern: $mixedWesternYaml
    priority: 0
    encoding: windows-1252
"@

# here-string 部分はスクリプトファイル自身の改行（既定 CRLF）で書き出される
# が、$dataSourceBlock は -join "`n" で組み立てているため LF のみになり、
# そのまま書き出すと1ファイル内で改行コードが混在する。利用者が実行ファイル
# 直下へコピーして編集する設定ファイルのため、混在は避けたい。スクリプト
# ファイル自身が LF で取得された環境（改行コード変換を伴うチェックアウト設定
# など）でも結果が変わらないよう、一度 LF へ正規化してから CRLF へ揃える。
$configText = ($configText -replace "`r`n", "`n") -replace "`n", "`r`n"

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
