<#
.SYNOPSIS
    稼働中に追記され続けるログファイルを模擬する試験用スクリプトです
    （P06-5。`tasks/phase-06-large-file-loading.md` に記載の、追記を行う
    テスト用プロセスの作成として、LOG-026・LOG-028 の代替検証に使います）。

.DESCRIPTION
    指定したファイルへ、`LOG-DT-001`（"yyyy/MM/dd HH:mm:ss.fff"）形式の行を
    一定間隔で追記し続けます。次の受け入れ条件の手動・自動検証に使います。

    - `LOG-026`: 読み込み時点の末尾が行の途中で終わっている場合、断片を破棄
      せず未確定行として区別表示すること（`-NoFinalNewline` スイッチ）。
    - `LOG-027`: 共有を許可しない方法（`FileShare.None`）で開かれたログに
      対し、対象と理由を表示し再試行できること（`-HoldOpen` スイッチ）。
    - `LOG-028`: 明示的な再読み込みを指示した場合に限り、開いた後に追記
      された内容が反映されること（デフォルトの追記動作。Hakutaku 側で
      `reload_target` を呼ばない限り反映されないことも合わせて確認できる）。

    対象端末の実機を用意できない開発環境や CI での代替検証として、
    P13（実機での実測）以降でも再利用する前提で書いています（実機を用意
    できるまでの間、「稼働中のログ書き込み」をこのスクリプトで模擬する）。

    出力は UTF-8（BOM なし）固定です（`scripts/generate-test-log.ps1` と同じ
    理由。Hakutaku の日時解析はエンコード判定の結果に依存しますが、BOM 無し
    UTF-8 を既定にして解析の妨げにならないようにしています）。

    生成したファイルはリポジトリの外（一時領域など）に置いてください。試験
    データをリポジトリ内に残さないでください。

.PARAMETER OutputPath
    追記先のファイルパス。存在しない場合は新規作成します。

.PARAMETER IntervalMilliseconds
    1行を追記するごとの待機時間（ミリ秒）。既定は 500ms。

.PARAMETER DurationSeconds
    スクリプトを自動終了させるまでの秒数。省略した場合は Ctrl+C で手動停止
    するまで動作し続けます（自動テストプロセスから使う場合は、確実に終了
    させるために必ず指定してください）。

.PARAMETER HoldOpen
    指定すると、ファイルを `FileShare.None`（他プロセス・他ハンドルからの
    読み取り・書き込みを一切許可しない開き方）で開いたまま追記し続けます。
    同じ端末で稼働する他の業務ソフトウェアが共有を許可しない方法でログを開いている状況（`LOG-027`）を
    再現します。Hakutaku がこのファイルを開こうとすると
    `ERROR_SHARING_VIOLATION` になるはずです（スクリプトを止める＝ロックを
    解除すると、Hakutaku 側の再試行が成功するはずです）。

    指定しない場合は、Hakutaku 自身の開き方（読み取り専用・共有可。
    `ENV-010` の標準ケース）を妨げない `FileShare.ReadWrite` で開きます。

.PARAMETER NoFinalNewline
    指定すると、各行の区切りを「行の直後の改行」ではなく「次の行の直前の
    改行」にします（1行目は改行なしで書き込み、2行目以降は「改行＋本文」を
    書き込む）。この結果、書き込みの合間はもちろん、スクリプトをどの時点で
    止めても（`-DurationSeconds` の経過・Ctrl+C のいずれでも）、ファイルの
    末尾は常に「行の途中で終わっている未確定行」になります（`LOG-026` の
    受け入れ条件「読み込み時点の末尾が行の途中で終わっている場合」を、
    書き込みタイミングに依存せず確実に再現するための設計）。

    指定しない場合は毎回「本文＋改行」を書き込むため、各行は書き込み直後に
    確定行になります（通常のログ書き込みに近い動作）。

.EXAMPLE
    # 通常の追記（共有可）を10秒間、500ms間隔で続ける（LOG-028 の再読み込み
    # 確認に使う。Hakutaku で開いた後、reload_target を呼ぶまで反映されない
    # ことを確認できる）。
    ./scripts/append-log-writer.ps1 -OutputPath C:\temp\hakutaku-append\test.log -DurationSeconds 10

.EXAMPLE
    # 共有違反（LOG-027）を再現する。Ctrl+C で停止するまで排他ロックを保持する。
    ./scripts/append-log-writer.ps1 -OutputPath C:\temp\hakutaku-append\test.log -HoldOpen

.EXAMPLE
    # 末尾を常に未確定行のままにする（LOG-026）。10秒後に自動終了する。
    ./scripts/append-log-writer.ps1 -OutputPath C:\temp\hakutaku-append\test.log -DurationSeconds 10 -NoFinalNewline
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputPath,

    [ValidateRange(1, [int]::MaxValue)]
    [int]$IntervalMilliseconds = 500,

    [ValidateRange(1, [int]::MaxValue)]
    [int]$DurationSeconds,

    [switch]$HoldOpen,

    [switch]$NoFinalNewline
)

# 日本語を含む短いメッセージ断片（generate-test-log.ps1 と同じ考え方だが、
# こちらは追記の連続性・タイミングの確認が主目的のため語数は少なくしている）。
$messageFragments = @(
    "稼働中の書き込みを継続しています",
    "追記テスト行です",
    "警告: 一時的な遅延を検出しました",
    "内部状態を記録しました",
    "利用者操作を検知しました"
)

function New-LogLine {
    param(
        [datetime]$Timestamp,
        [int]$Index
    )

    # LOG-DT-001: "yyyy/MM/dd HH:mm:ss.fff"（桁数固定・区切り厳密）。
    $timestampText = $Timestamp.ToString("yyyy/MM/dd HH:mm:ss.fff")
    $fragment = $messageFragments[$Index % $messageFragments.Length]
    return "$timestampText $fragment [連番=$Index]"
}

$outputDir = Split-Path -Parent $OutputPath
if ($outputDir -and -not (Test-Path -LiteralPath $outputDir)) {
    New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
}
if (-not (Test-Path -LiteralPath $OutputPath)) {
    New-Item -ItemType File -Path $OutputPath | Out-Null
}

# HoldOpen: FileShare.None（LOG-027 の再現。他ハンドルからの共有を一切
# 許可しない）。通常時: FileShare.ReadWrite（Hakutaku 自身の開き方
# （読み取り専用・共有可）を妨げない。ENV-010 の標準ケース）。
$shareMode = if ($HoldOpen) { [System.IO.FileShare]::None } else { [System.IO.FileShare]::ReadWrite }
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

# ストリームはスクリプトの実行中ずっと開いたままにする（HoldOpen の場合は
# これがロック保持そのものであり、通常時も開閉のたびに発生するファイル
# システムの往復コストを避けるため）。
$stream = New-Object System.IO.FileStream(
    $OutputPath,
    [System.IO.FileMode]::Append,
    [System.IO.FileAccess]::Write,
    $shareMode
)

$startedAt = Get-Date
$index = 0
try {
    Write-Host (
        "追記を開始します: $OutputPath " +
        "(間隔=${IntervalMilliseconds}ms, HoldOpen=$($HoldOpen.IsPresent), " +
        "NoFinalNewline=$($NoFinalNewline.IsPresent))"
    )
    if ($HoldOpen) {
        Write-Host "共有を許可しない方法（FileShare.None）で開いています（LOG-027 再現）。停止するまでロックを保持します。"
    }

    while ($true) {
        if ($DurationSeconds -and ((Get-Date) - $startedAt).TotalSeconds -ge $DurationSeconds) {
            break
        }

        $line = New-LogLine -Timestamp (Get-Date) -Index $index

        if ($NoFinalNewline) {
            # 1行目は改行なしで書く。2行目以降は「直前の行の終端を確定
            # させる改行」を本文の前に置く。これにより、書き込みの合間・
            # スクリプト終了時のいずれでも、ファイルの末尾は常に改行の無い
            # （＝未確定行の）状態になる（doc コメント参照）。
            $text = if ($index -eq 0) { $line } else { "`n$line" }
        }
        else {
            $text = "$line`n"
        }

        $bytes = $utf8NoBom.GetBytes($text)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush()

        $index++
        Start-Sleep -Milliseconds $IntervalMilliseconds
    }
}
finally {
    $stream.Dispose()
    Write-Host "追記を終了しました: $OutputPath (合計 $index 行)"
}
