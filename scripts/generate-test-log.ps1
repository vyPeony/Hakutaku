<#
.SYNOPSIS
    Hakutaku の試験用ログファイルを生成します（P04-3。P12-3 で拡張）。

.DESCRIPTION
    日時付きの行と、日本語（既定）または西欧言語を含む可変長のメッセージを持つ
    テキストログを、指定した行数だけ生成します。

    P04-3（転送コストと WebView2 メモリ推移の計測、CFG-022 初期値の決定）の
    実測用データとして作りましたが、行数だけを引数化しており、日時・メッセージの
    形式そのものは変えていませんでした。P12-3 で、次の3点を
    パラメーター化して拡張しました。**既定値は元の挙動と完全に一致**しており、
    `-LineCount`／`-OutputPath`／`-StartTimestamp` だけを指定する既存の呼び出しは
    バイト単位で同じ結果を返します（後方互換）。

    - `-Format`: 既知の日時書式6種（`LOG-DT-001`〜`006`、`docs/requirements/functional.md`
      の「既知の日時書式」）から選択。既定は `LOG-DT-001`（元の書式のまま）。
    - `-Encoding`: 文字コード（`ENC-001`〜`003`）。UTF-8（BOM 有無）、Windows
      ANSI コードページ 932（Shift_JIS）・1252（西欧言語）から選択。既定は
      `Utf8NoBom`（元の挙動のまま）。CP1252 選択時は、コードページで表現できない
      日本語ではなく西欧言語（アクセント付き文字を含む）のメッセージ断片を使う
      （`ENC-004`: 生成環境の OS 言語設定によって ANSI エンコーディングも
      言語も変わり得るため。日本語断片を CP1252 で符号化すると `?` に化ける
      だけで意味のある試験データにならない）。
    - `-ContinuationLineRate`: 日時を持たない継続行（`LOG-014`）の混入率
      （0.0〜1.0）。既定は `0`（継続行なし。元の挙動のまま）。`-LineCount` は
      継続行を含めた**出力ファイルの総行数**を表す（継続行を追加で増やす仕様
      ではない）。1行目は継続元がないため常に通常行になる。

    2000万行規模（5.3 の暫定的な受け入れ条件、P12 作業項目5）の生成に耐えられる
    よう、行ごとに `StreamWriter.WriteLine` で逐次書き込む方式を保っており、
    行数に比例したメモリ増加はしない（生成中ずっとバッファを保持しない）。
    `-ProgressEveryLines` を指定すると、大規模生成中の生存確認用に一定行数ごとの
    進捗を標準出力へ書く（既定は `0` で無出力。元の挙動のまま）。

    出力ファイル末尾の要約行に、生成にかかった時間と速度（行/秒、MiB/秒）を
    記録する。回帰試験の基準値として使う実測（P12 作業項目7、`VER-005`）に使う。

    生成したファイルはリポジトリの外（一時領域など）に置いてください。試験
    データをリポジトリ内に残さないでください。リポジトリ内を出力先に指定した
    場合はエラーで停止します。

.PARAMETER LineCount
    生成する行数（継続行を含む、出力ファイルの総行数）。

.PARAMETER OutputPath
    出力先のファイルパス（絶対パス・相対パスのどちらでも可）。リポジトリ内は
    指定できません。

.PARAMETER StartTimestamp
    生成する最初の行の日時。省略時は現在時刻。

.PARAMETER Format
    日時書式（`LOG-DT-001`〜`006`）。既定は `LOG-DT-001`
    （`yyyy/MM/dd HH:mm:ss.fff`。元の挙動のまま）。

.PARAMETER Encoding
    文字コード。`Utf8NoBom`（既定、元の挙動のまま）／`Utf8Bom`／`CP932`
    （Shift_JIS、Windows ANSI コードページ 932）／`CP1252`（西欧言語、
    Windows ANSI コードページ 1252）。

.PARAMETER ContinuationLineRate
    日時を持たない継続行（`LOG-014`）の混入率（0.0〜1.0）。既定は `0`。
    1行目は継続元がないため対象外（常に通常行）。

.PARAMETER Seed
    継続行の混入判定に使う乱数のシード。省略時は実行のたびに変わる。
    再現性が必要な場合（回帰試験の基準値作成など）に指定する。

.PARAMETER ProgressEveryLines
    指定した行数ごとに進捗を標準出力へ書く。既定は `0`（無出力）。

.EXAMPLE
    # 約30万行（P04-3 の実測規模）を一時領域へ生成する。元の呼び出しのまま。
    ./scripts/generate-test-log.ps1 -LineCount 300000 -OutputPath C:\temp\hakutaku-measure\test.log

.EXAMPLE
    # P12 の規模検証（2000万行）で再利用する場合。
    ./scripts/generate-test-log.ps1 -LineCount 20000000 -OutputPath D:\hakutaku-scale\huge.log -ProgressEveryLines 1000000

.EXAMPLE
    # 6書式・複数コードページ・継続行混在を確認する小規模生成（P12 作業項目5）。
    ./scripts/generate-test-log.ps1 -LineCount 5000 -OutputPath C:\temp\hakutaku-fmt\dt002-cp932.log `
        -Format LOG-DT-002 -Encoding CP932 -ContinuationLineRate 0.1 -Seed 42

.EXAMPLE
    # CP1252（西欧言語）のログを生成する。
    ./scripts/generate-test-log.ps1 -LineCount 5000 -OutputPath C:\temp\hakutaku-fmt\dt005-cp1252.log `
        -Format LOG-DT-005 -Encoding CP1252
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [int]::MaxValue)]
    [int]$LineCount,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath,

    [datetime]$StartTimestamp = (Get-Date),

    [ValidateSet('LOG-DT-001', 'LOG-DT-002', 'LOG-DT-003', 'LOG-DT-004', 'LOG-DT-005', 'LOG-DT-006')]
    [string]$Format = 'LOG-DT-001',

    [ValidateSet('Utf8NoBom', 'Utf8Bom', 'CP932', 'CP1252')]
    [string]$Encoding = 'Utf8NoBom',

    [ValidateRange(0.0, 1.0)]
    [double]$ContinuationLineRate = 0,

    [Nullable[int]]$Seed = $null,

    [ValidateRange(0, [int]::MaxValue)]
    [int]$ProgressEveryLines = 0
)

# LOG-DT-001〜006（docs/requirements/functional.md の「既知の日時書式」）に
# 対応する .NET カスタム書式文字列。区切り文字（'/' と '-'、'.' と ':'）と
# 秒未満の桁数（fff=ミリ秒3桁、ff=1/100秒2桁）を表どおりに厳密に対応させる。
$formatPatterns = @{
    'LOG-DT-001' = 'yyyy/MM/dd HH:mm:ss.fff'
    'LOG-DT-002' = 'yyyy-MM-dd HH:mm:ss:fff'
    'LOG-DT-003' = 'yyyy/MM/dd HH:mm:ss.ff'
    'LOG-DT-004' = 'yyyy/MM/dd HH:mm:ss:ff'
    'LOG-DT-005' = 'yyyy/MM/dd HH:mm:ss'
    'LOG-DT-006' = 'yyyy/MM/dd HH:mm'
}
$timestampPattern = $formatPatterns[$Format]

# 日本語を含む、長さが変わるメッセージの断片（UTF-8／CP932 向け）。行ごとに
# 組み合わせを変えて可変長にする（固定文だけだと転送コストの実測が非現実的に
# 均一になるため）。
$messageFragmentsJapanese = @(
    "起動処理を開始しました",
    "設定ファイルを読み込みました: hakutaku.yaml",
    "ファイル選択ダイアログを表示しました",
    "ログファイルを読み込みました。行数と予約量を記録します",
    "範囲取得の要求を受け付けました",
    "警告: メモリ予算のソフトしきい値に到達しました",
    "エラー: 診断ログフォルダへ書き込めません。権限を確認してください",
    "デバイス接続を確認しました。処理を継続します",
    "ネットワーク応答がタイムアウトしました。再試行します",
    "利用者操作: クリップボードへコピーしました",
    "内部状態の整合性チェックが完了しました。異常なし",
    "解析プロファイルの照合に失敗しました。既定プロファイルへ切り替えます"
)

# 西欧言語のメッセージ断片（CP1252 向け）。CP1252 では日本語を表現できず
# `?` へ化けるだけで意味のある試験データにならないため、`ENC-004` の
# 「生成環境の OS 言語設定によってエンコーディングも言語も変わり得る」を
# 踏まえた別文面にする。CP1252 特有の文字（é、ü、ö、ñ、ç 等）をあえて含め、
# UTF-8 と取り違えても文字化けで気づけるようにしている。
$messageFragmentsLatin = @(
    "Startup sequence initiated",
    "Configuration loaded: hakutaku.yaml",
    "File selection dialog displayed",
    "Log file loaded. Recording line count and reservation size",
    "Range request accepted",
    "Warning: memory budget soft threshold reached",
    "Error: cannot write to diagnostics folder. Check permissions",
    "Device connection confirmed. Continuing operation",
    "Network response timed out. Retrying",
    "User action: copied to clipboard (résumé du presse-papiers)",
    "Internal consistency check completed. No anomalies détectées",
    "Parsing profile mismatch. Falling back to default profile (configuración predeterminada, señor)"
)

$useLatinFragments = $Encoding -eq 'CP1252'
$messageFragments = if ($useLatinFragments) { $messageFragmentsLatin } else { $messageFragmentsJapanese }

function New-LogLine {
    param(
        [datetime]$Timestamp,
        [int]$Index,
        [string]$TimestampPattern,
        [string[]]$Fragments,
        [bool]$Latin
    )

    $timestampText = $Timestamp.ToString($TimestampPattern)
    $fragment = $Fragments[$Index % $Fragments.Length]

    # 行ごとに詳細情報の繰り返し回数（0〜3回）を変え、可変長メッセージにする。
    $detailCount = $Index % 4
    $detail = ""
    if ($detailCount -gt 0) {
        $detailValues = 1..$detailCount
        if ($Latin) {
            $detail = " detail(" + ($detailValues -join ", ") + ")"
        }
        else {
            $detail = " 詳細(" + ($detailValues -join ", ") + ")"
        }
    }

    $label = if ($Latin) { "line" } else { "行番号" }
    return "$timestampText $fragment [$label=$Index]$detail"
}

function New-ContinuationLine {
    # LOG-014: 日時を持たない継続行。直前の日時付き行と同じ論理ログ項目の
    # 一部として扱われる想定のため、行頭に日時を一切含めない（先頭に空白を
    # 入れ、複数行スタックトレース／折り返しメッセージを模す）。
    param(
        [int]$Index,
        [bool]$Latin
    )

    if ($Latin) {
        return "    (continued) additional detail for the previous entry [line=$Index]"
    }
    return "    （継続行）直前の行に続く詳細情報です [行番号=$Index]"
}

function Get-LineEncoding {
    param([string]$Name)

    switch ($Name) {
        'Utf8NoBom' { return New-Object System.Text.UTF8Encoding($false) }
        'Utf8Bom' { return New-Object System.Text.UTF8Encoding($true) }
        'CP932' { return Get-CodePageEncoding -CodePage 932 }
        'CP1252' { return Get-CodePageEncoding -CodePage 1252 }
        default { throw "未知の Encoding です: $Name" }
    }
}

function Get-CodePageEncoding {
    # .NET (Core/5+) はデフォルトで UTF 系以外のコードページを知らないことが
    # あり、`CodePagesEncodingProvider` の登録が必要な場合がある
    # （Windows PowerShell 5.1 相当の完全な .NET Framework では元々登録不要）。
    # 未登録で失敗した場合だけ登録して再試行する。
    param([int]$CodePage)

    try {
        return [System.Text.Encoding]::GetEncoding($CodePage)
    }
    catch {
        [System.Text.Encoding]::RegisterProvider([System.Text.CodePagesEncodingProvider]::Instance)
        return [System.Text.Encoding]::GetEncoding($CodePage)
    }
}

# 生成先がリポジトリ内でないことを確認する（試験データをリポジトリへ残さない。
# scripts/generate-sample-logs.ps1 の OutputDir 判定と同じ考え方）。フォルダを
# 作る前に判定するのは、弾く経路でリポジトリ内へ空フォルダを残さないため。
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path.TrimEnd('\') + '\'
$outputPathFull = [System.IO.Path]::GetFullPath($OutputPath)
if ($outputPathFull.StartsWith($repoRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputPath がリポジトリ内です ($OutputPath)。試験データをリポジトリへ残さないため、リポジトリ外（一時領域など）を指定してください。"
}

$outputDir = Split-Path -Parent $OutputPath
if ($outputDir -and -not (Test-Path -LiteralPath $outputDir)) {
    New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
}

# 変数名は $Encoding パラメーターと大文字小文字だけの違いにしない
# （PowerShell の変数名は大文字小文字を区別しないため、同名にすると
# ValidateSet 付きのパラメーター変数を上書きしてしまう）。
$textEncoding = Get-LineEncoding -Name $Encoding
$random = if ($null -ne $Seed) { New-Object System.Random($Seed) } else { New-Object System.Random }

$stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
$writer = New-Object System.IO.StreamWriter($OutputPath, $false, $textEncoding)
try {
    $current = $StartTimestamp
    for ($i = 0; $i -lt $LineCount; $i++) {
        # 1行目は継続元となる日時付き行がまだ無いため、常に通常行にする。
        $isContinuation = ($i -gt 0) -and ($ContinuationLineRate -gt 0) -and ($random.NextDouble() -lt $ContinuationLineRate)

        if ($isContinuation) {
            $writer.WriteLine((New-ContinuationLine -Index $i -Latin $useLatinFragments))
            # 継続行は日時を持たないため、時刻は進めない。
        }
        else {
            $writer.WriteLine((New-LogLine -Timestamp $current -Index $i -TimestampPattern $timestampPattern -Fragments $messageFragments -Latin $useLatinFragments))
            # 1〜50msずつ進める（実ログらしい間隔のばらつきを持たせる）。
            $current = $current.AddMilliseconds(1 + ($i % 50))
        }

        if ($ProgressEveryLines -gt 0 -and (($i + 1) % $ProgressEveryLines -eq 0)) {
            $elapsedSec = [Math]::Max($stopwatch.Elapsed.TotalSeconds, 0.001)
            $rate = [Math]::Round(($i + 1) / $elapsedSec, 0)
            Write-Host "進捗: $($i + 1) / $LineCount 行（$rate 行/秒）"
        }
    }
}
finally {
    $writer.Dispose()
    $stopwatch.Stop()
}

$fileInfo = Get-Item -LiteralPath $OutputPath
$sizeMib = [Math]::Round($fileInfo.Length / 1MB, 2)
$elapsedTotalSec = [Math]::Max($stopwatch.Elapsed.TotalSeconds, 0.001)
$linesPerSec = [Math]::Round($LineCount / $elapsedTotalSec, 0)
$mibPerSec = [Math]::Round($sizeMib / $elapsedTotalSec, 2)
Write-Host (
    "生成しました: $OutputPath ($LineCount 行, 約 $sizeMib MiB, " +
    "書式=$Format, 文字コード=$Encoding, 継続行混入率=$ContinuationLineRate) " +
    "所要時間=$([Math]::Round($elapsedTotalSec, 2))秒 " +
    "($linesPerSec 行/秒, $mibPerSec MiB/秒)"
)
