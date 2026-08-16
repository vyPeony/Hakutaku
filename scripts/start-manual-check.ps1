<#
.SYNOPSIS
    手動での動作確認を始めるための環境準備を、一度に済ませます。

.DESCRIPTION
    [手動動作確認の手順](../docs/verification/manual-check.md) を始めるまでに
    必要な準備（ビルド、サンプル生成、設定ファイルの配置、起動）を1回の実行で
    まとめて行います。手順書の 2章・3章を手作業で組み立てなくて済むように
    するためのスクリプトであり、確認手順そのものの正本は手順書です。

    行うこと:

    1. release ビルドの実行ファイル
       （`target/x86_64-pc-windows-msvc/release/Hakutaku.exe`）を毎回ビルド
       して用意する（`node_modules` が無ければ先に `npm ci`）。変更が無い
       ときのビルドは数秒で終わるため、既定では省略しません（Issue #24）。
       前回のビルドのまま進めたい場合は `-SkipBuild` を使います
    2. サンプル一式を `-SampleDir` へ用意する
       （生成そのものは `scripts/generate-sample-logs.ps1` に委ねる）。既存の
       `hakutaku.yaml` があっても、それが指すファイルが1件でも見つからない
       場合（新しいサンプルが追加された後に、それより前の一式が残っている
       場合など）は古い一式とみなし、`-RegenerateSamples` を付けなくても
       自動で作り直す（後述の `-RegenerateSamples` を参照）
    3. `-ConfigMode` に従い、実行ファイル直下の `hakutaku.yaml` を配置・削除する
       （設定ファイルは実行ファイルと同じフォルダの `hakutaku.yaml` 固定。`CFG-014`）
    4. 実行ファイルを起動する（`-NoLaunch` で省略）
    5. 次にやること（手順書の位置、設定の差し替え方、後片付け）を表示する

    手順書 4.1（3つの起動経路）、4.4 手順7・10、4.5 手順4 では、確認の途中で
    設定ファイルを差し替えます。この差し替えは `-ConfigMode` の指定だけで
    再現できます（アプリを終了してから `-ConfigMode invalid -SkipBuild -NoLaunch`
    のように再実行する）。

    書き込む先は次の2つだけです。実データ（個人情報等の機密データを含み得る
    ログ）は一切扱わず、合成データの生成は `generate-sample-logs.ps1` に委ねます。

    - 実行ファイル直下の `hakutaku.yaml`（`target/` 配下。Git 管理外）
    - `-SampleDir`（リポジトリ外。リポジトリ内を指定した場合はエラーで停止）

.PARAMETER ConfigMode
    実行ファイル直下へ置く設定ファイルの状態。手順書 4.1 の3経路にそのまま
    対応します。

    | 値 | 動作 | 対応する手順 |
    | --- | --- | --- |
    | `normal` | サンプルの正常な `hakutaku.yaml` をコピー（既定） | 4.1 手順2、4.4 手順10 |
    | `invalid` | `hakutaku-invalid.yaml` を `hakutaku.yaml` という名前でコピー | 4.1 手順3 |
    | `none` | 実行ファイル直下の `hakutaku.yaml` を削除（無ければ何もしない） | 4.1 手順1、4.4 手順7、4.5 手順4 |

.PARAMETER SkipBuild
    ビルドを省略し、既にある実行ファイルのまま進めます。設定ファイルを
    差し替えるだけの用途（手順書 4.1、4.4 手順7・10、4.5 手順4）で使います。

    実行ファイルが無い場合は、実行すべきコマンドを示してエラーで停止します
    （黙って古い状態のまま進めないため）。実行ファイルより後に変更された
    ビルド入力（`src/`、`crates/`、`src-tauri/`）がある場合は、どのファイルが
    新しいかを添えて警告します（Issue #24。古い実行ファイルで確認して
    「直っていない」と誤判定する事故を防ぐため）。

.PARAMETER NoLaunch
    準備だけ行い、アプリを起動しません。確認の途中で設定だけ差し替えたい場合に
    使います。

.PARAMETER SampleDir
    サンプル一式の置き場所。既定は `%TEMP%\hakutaku-samples`
    （`generate-sample-logs.ps1` の既定と同じ）。`-OutputDir` としてそのまま
    渡します。リポジトリ内は指定できません。

.PARAMETER LargeLineCount
    `generate-sample-logs.ps1` の同名パラメーターへそのまま渡します（指定した
    ときだけ渡すため、省略時は生成側の既定 300000 が使われます）。`0` を指定
    すると `08-large.log` を生成せず、事前定義データソースにも含めません
    （手順書 4.9・4.10 は実施できなくなります）。`10-medium-100k.log`
    （10万行）は `0` を指定しても常に生成されます。事前定義データソースの
    「10 大きめのログ（100,000行）」から常に開けるようにするためです。

    以前に `-LargeLineCount 0` で生成した一式が `-SampleDir` に残っている
    場合、その `hakutaku.yaml` が指すファイルはすべて存在するため（後述の
    自動判定は「指すファイルが存在するか」だけを見て、行数や引数の変化までは
    追わない）、生成は省略され続け `08-large.log` は事前定義データソースに
    含まれないままになります。行数を変えたい場合は `-RegenerateSamples` を
    付けてください。

.PARAMETER RegenerateSamples
    サンプルが既にある場合でも `-Force` 付きで生成し直します。指定しない
    場合、`-SampleDir` に `hakutaku.yaml` があり、かつそれが指すファイルが
    すべて存在すれば生成を省略します。1件でも見つからない場合は、サンプル
    一式が古い（または壊れている）と判断し、`-RegenerateSamples` を指定
    しなくても自動で `-Force` 付きの生成し直しへ切り替わります（何が
    足りなかったかを実行時に表示します）。

.PARAMETER CleanUp
    後片付けだけを行います（手順書 5章）。サンプルフォルダと、実行ファイル直下
    の `hakutaku.yaml` を削除し、ビルド・生成・起動は行いません。誤って準備と
    後片付けを同時に指示することを防ぐため、`-SampleDir` 以外のパラメーターとは
    併用できません。

    既定以外の場所へサンプルを生成した場合は、準備のときと同じ `-SampleDir` を
    付けて実行してください（どのサンプルを片付けるかを特定するために必要です）。

.EXAMPLE
    # 既定の準備（ビルド→サンプル生成→正常な設定を配置→起動）。
    ./scripts/start-manual-check.ps1

.EXAMPLE
    # 手順書 4.1 手順3（安全モード）の状態へ差し替える。アプリを終了してから実行する。
    ./scripts/start-manual-check.ps1 -ConfigMode invalid -SkipBuild -NoLaunch

.EXAMPLE
    # 手順書 4.1 手順1（設定なし）の状態にして、そのまま起動する。
    ./scripts/start-manual-check.ps1 -ConfigMode none -SkipBuild

.EXAMPLE
    # 大きいファイルを省いて短時間で準備し直す。
    ./scripts/start-manual-check.ps1 -LargeLineCount 0 -RegenerateSamples

.EXAMPLE
    # 確認が終わったあとの後片付け（サンプルと配置した設定を削除する）。
    ./scripts/start-manual-check.ps1 -CleanUp

.EXAMPLE
    # 既定以外の場所へ生成した場合の後片付け（準備のときと同じ -SampleDir を付ける）。
    ./scripts/start-manual-check.ps1 -CleanUp -SampleDir D:\hakutaku-samples
#>
[CmdletBinding()]
param(
    [ValidateSet('normal', 'invalid', 'none')]
    [string]$ConfigMode = 'normal',

    [switch]$SkipBuild,

    [switch]$NoLaunch,

    [string]$SampleDir = (Join-Path ([System.IO.Path]::GetTempPath()) "hakutaku-samples"),

    [ValidateRange(0, [int]::MaxValue)]
    [int]$LargeLineCount,

    [switch]$RegenerateSamples,

    [switch]$CleanUp
)

$ErrorActionPreference = "Stop"

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$exePath = Join-Path $repoRoot "target\x86_64-pc-windows-msvc\release\Hakutaku.exe"
$exeDir = Split-Path -Parent $exePath
$exeConfigPath = Join-Path $exeDir "hakutaku.yaml"
$manualCheckPath = Join-Path $repoRoot "docs\verification\manual-check.md"

# -CleanUp は「準備の逆」であり、準備用のパラメーターと同時に指定された場合は
# どちらを意図したのか判断できない（例: -CleanUp -ConfigMode normal）。黙って
# 一方を無視せず、指定の誤りとして停止する。
# -SampleDir だけは例外で、準備の指示ではなく「どのサンプルを片付けるか」の
# 特定に必要なため併用できる（既定以外の場所へ生成した場合、これが無いと
# 片付ける手段が無くなる）。
if ($CleanUp) {
    $conflicting = @('ConfigMode', 'SkipBuild', 'NoLaunch', 'LargeLineCount', 'RegenerateSamples') |
        Where-Object { $PSBoundParameters.ContainsKey($_) }
    if ($conflicting) {
        throw "-CleanUp と併用できるのは -SampleDir だけです（同時に指定された引数: $($conflicting -join '、')）。準備と後片付けは別々に実行します。"
    }
}

# 書き込み先がリポジトリ内でないことを確認する（試験データをリポジトリへ
# 残さない。generate-sample-logs.ps1 の OutputDir 判定と同じ方式で、生成側へ
# 渡す前にこちら側でも同じ理由で弾く）。
$repoRootPrefix = $repoRoot.TrimEnd('\') + '\'
$sampleDirFull = [System.IO.Path]::GetFullPath($SampleDir).TrimEnd('\') + '\'
if ($sampleDirFull.StartsWith($repoRootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "SampleDir がリポジトリ内です ($SampleDir)。試験データをリポジトリへ残さないため、リポジトリ外（一時領域など）を指定してください。"
}

# --- 後片付けモード ------------------------------------------------------
if ($CleanUp) {
    Write-Host "後片付けします（docs/verification/manual-check.md 5章）。"

    if (Test-Path -LiteralPath $SampleDir) {
        Remove-Item -LiteralPath $SampleDir -Recurse -Force
        Write-Host "  削除しました: $SampleDir"
    }
    else {
        Write-Host "  サンプルはありません: $SampleDir"
    }

    # 実行ファイル直下の設定は、確認のために置いたものであり配布物には含めない
    # （残すと次回のビルド成果物と混ざる）。実行ファイル自体は消さない。
    if (Test-Path -LiteralPath $exeConfigPath) {
        Remove-Item -LiteralPath $exeConfigPath -Force
        Write-Host "  削除しました: $exeConfigPath"
    }
    else {
        Write-Host "  配置した設定はありません: $exeConfigPath"
    }

    Write-Host ""
    Write-Host "append-log-writer.ps1 を起動したままにしていないか確認してください（手順書 5章 手順2）。"
    Write-Host "リポジトリへ試験データが入っていないことは 'git status' で確認してください（同 手順5）。"
    return
}

# --- 1. 実行ファイルの用意 -----------------------------------------------

# 実行ファイルより後に変更されたビルド入力を、新しい順に返す（`-SkipBuild`
# のときだけ使う）。
#
# ここでいうビルド入力は、実行ファイルの中身を変えるものに限る。`src/` は
# フロントエンド一式で、`src-tauri/Tauri.toml` の frontendDist = "../src" に
# より実行ファイルへ埋め込まれる（そのため CSS や JS だけの変更でも作り直しが
# 要る）。`crates/`・`src-tauri/` は Rust 側の実装。`src-tauri/gen/` は
# tauri-build がビルド中に生成する出力であって入力ではないため除く。
function Get-BuildInputsNewerThan {
    param(
        [Parameter(Mandatory = $true)][string]$RepoRoot,
        [Parameter(Mandatory = $true)][datetime]$Since
    )

    $generatedPrefix = (Join-Path $RepoRoot "src-tauri\gen").TrimEnd('\') + '\'
    $newerInputs = foreach ($inputRoot in @('src', 'crates', 'src-tauri')) {
        $inputRootPath = Join-Path $RepoRoot $inputRoot
        if (-not (Test-Path -LiteralPath $inputRootPath)) {
            continue
        }
        Get-ChildItem -LiteralPath $inputRootPath -Recurse -File -Force -ErrorAction SilentlyContinue |
            Where-Object {
                $_.LastWriteTime -gt $Since -and
                -not $_.FullName.StartsWith($generatedPrefix, [System.StringComparison]::OrdinalIgnoreCase)
            }
    }

    $newerInputs | Sort-Object LastWriteTime -Descending
}

# 5章「次にやること」でも古さを繰り返し示すために保持する（0 は「古くない」）。
$staleBuildInputCount = 0

if ($SkipBuild) {
    if (-not (Test-Path -LiteralPath $exePath)) {
        throw "実行ファイルが見つかりません: $exePath`n-SkipBuild を外して実行するか、先に 'npm run tauri -- build --no-bundle' を実行してください。"
    }

    # 既定を毎回ビルドにしたため、古い実行ファイルを掴む事故はこの経路でしか
    # 起こらない（Issue #24）。常に出る注意書きは見落とされるので、実際に
    # 実行ファイルより新しいビルド入力があるときだけ、何が新しいかを添えて
    # 警告する（変更していないときに警告を出すと、同じ理由で効かなくなる）。
    $exeWriteTime = (Get-Item -LiteralPath $exePath).LastWriteTime
    $newerBuildInputs = @(Get-BuildInputsNewerThan -RepoRoot $repoRoot -Since $exeWriteTime)
    $staleBuildInputCount = $newerBuildInputs.Count
    if ($newerBuildInputs.Count -gt 0) {
        Write-Warning "実行ファイルは変更より古いままです（-SkipBuild のためビルドしません）。このまま起動すると、変更前の実行ファイルで確認することになります。"
        Write-Host "  実行ファイル: $exePath （$($exeWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))）"
        Write-Host "  実行ファイルより新しいビルド入力: $($newerBuildInputs.Count) 件"
        foreach ($newerBuildInput in ($newerBuildInputs | Select-Object -First 5)) {
            $relativeInputPath = $newerBuildInput.FullName.Substring($repoRoot.Length).TrimStart('\')
            Write-Host "    $relativeInputPath （$($newerBuildInput.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))）"
        }
        if ($newerBuildInputs.Count -gt 5) {
            Write-Host "    ほか $($newerBuildInputs.Count - 5) 件"
        }
        Write-Host "  最新の状態で確認するには、-SkipBuild を外して実行し直してください。"
    }
    else {
        Write-Host "ビルドを省略します（-SkipBuild）: $exePath"
        Write-Host "  実行ファイルより新しいビルド入力はありません。"
    }
}
else {
    # 依存が未取得のままでは tauri CLI 自体が起動できないため、ビルドより先に
    # 取得する（package-lock.json に固定された版で入れるため install ではなく ci）。
    if (-not (Test-Path -LiteralPath (Join-Path $repoRoot "node_modules"))) {
        Write-Host "node_modules が無いため依存を取得します: npm ci"
        Push-Location $repoRoot
        try {
            & npm ci
        }
        finally {
            Pop-Location
        }
        if ($LASTEXITCODE -ne 0) {
            throw "npm ci が失敗しました (終了コード $LASTEXITCODE)。"
        }
    }

    Write-Host "release ビルドを実行します: npm run tauri -- build --no-bundle"
    Write-Host "  （変更が無ければ数秒で終わります。src/ のフロントエンドまたは Rust 側を変更した場合は数分かかります）"
    Push-Location $repoRoot
    try {
        & npm run tauri -- build --no-bundle
    }
    finally {
        Pop-Location
    }
    if ($LASTEXITCODE -ne 0) {
        throw "ビルドが失敗しました (終了コード $LASTEXITCODE)。"
    }
    if (-not (Test-Path -LiteralPath $exePath)) {
        throw "ビルドは成功しましたが実行ファイルが見つかりません: $exePath"
    }
    Write-Host "ビルドしました: $exePath"
}

# --- 2. サンプル一式の用意 -----------------------------------------------
$sampleConfigPath = Join-Path $SampleDir "hakutaku.yaml"
$invalidConfigPath = Join-Path $SampleDir "hakutaku-invalid.yaml"

# 生成の完了は hakutaku.yaml の有無で判定する。設定ファイルは
# generate-sample-logs.ps1 が最後に書くもののひとつであり、途中で中断した
# フォルダを「生成済み」と誤認しにくいため。
#
# それだけでは不十分なため、既存の hakutaku.yaml が指すファイルが実際に
# 存在するかも確認する（P12 で追加したサンプルより前に作られた一式が
# 残っている場合、hakutaku.yaml 自体は存在していても中身は古いままであり、
# 新しいデータソース（09-mixed-*.log、10-medium-100k.log 等）が一切出てこ
# ない。「1回実行すれば概ねのパターンを網羅した状態でアプリが起動する」を
# 満たすには、利用者が -RegenerateSamples を手で付けなくてもこの状態を
# 検出して作り直す必要がある）。
$needsRegenerate = -not (Test-Path -LiteralPath $sampleConfigPath)
if (-not $needsRegenerate) {
    try {
        $sampleConfigLines = Get-Content -LiteralPath $sampleConfigPath -ErrorAction Stop
        $missingSampleFiles = [System.Collections.Generic.List[string]]::new()
        $sawDataSourcePath = $false
        foreach ($configLine in $sampleConfigLines) {
            # 生成する YAML は「- name: '…'」の次行が「    path: '…'」という
            # 固定の形式（log_profiles 側は path_pattern のため混ざらない。
            # generate-sample-logs.ps1 の Format-YamlSingleQuoted 参照）。
            # 単一引用符スカラーの規則どおり '' を ' へ戻して読み取る。
            if ($configLine -match "^\s*path:\s*'(.*)'\s*$") {
                $sawDataSourcePath = $true
                $rawSamplePath = $Matches[1] -replace "''", "'"
                if (-not (Test-Path -LiteralPath $rawSamplePath)) {
                    $missingSampleFiles.Add($rawSamplePath)
                }
            }
        }
        if (-not $sawDataSourcePath) {
            # 想定した path: 行が1件も見つからない場合は、想定外の内容の
            # hakutaku.yaml が置かれていたとみなし、安全側に倒して作り直す。
            throw "data_sources の path: 行が見つかりませんでした。"
        }
        if ($missingSampleFiles.Count -gt 0) {
            $needsRegenerate = $true
            Write-Host "既存のサンプルが古いため作り直します: $sampleConfigPath"
            Write-Host "  設定が指すファイルのうち $($missingSampleFiles.Count) 件が見つかりません:"
            foreach ($missingSampleFile in $missingSampleFiles) {
                Write-Host "    $missingSampleFile"
            }
        }
    }
    catch {
        $needsRegenerate = $true
        Write-Host "既存のサンプルの設定を解析できなかったため作り直します: $sampleConfigPath （$($_.Exception.Message)）"
    }
}

if ($RegenerateSamples -or $needsRegenerate) {
    $generatorPath = Join-Path $PSScriptRoot "generate-sample-logs.ps1"
    if (-not (Test-Path -LiteralPath $generatorPath)) {
        throw "サンプル生成スクリプトが見つかりません: $generatorPath"
    }

    $generatorArguments = @{ OutputDir = $SampleDir }
    if ($PSBoundParameters.ContainsKey('LargeLineCount')) {
        $generatorArguments['LargeLineCount'] = $LargeLineCount
    }
    # 生成先に何か残っている場合、生成側は -Force 無しでは停止する。作り直しを
    # 指示された場合と、中断などで内容が中途半端に残っている場合の両方で上書き
    # させる（利用者に Remove-Item を求めないため）。
    if (Test-Path -LiteralPath $SampleDir) {
        $existing = Get-ChildItem -LiteralPath $SampleDir -Force -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($existing) {
            $generatorArguments['Force'] = $true
        }
    }

    Write-Host "サンプルを用意します（generate-sample-logs.ps1 を実行します）。"
    & $generatorPath @generatorArguments
}
else {
    Write-Host "サンプルがあるため生成を省略します: $SampleDir （作り直すには -RegenerateSamples）"
}

# --- 3. 設定ファイルの配置 -----------------------------------------------
switch ($ConfigMode) {
    'normal' {
        if (-not (Test-Path -LiteralPath $sampleConfigPath)) {
            throw "サンプルの設定ファイルが見つかりません: $sampleConfigPath`n-RegenerateSamples を付けて実行し直してください。"
        }
        Copy-Item -LiteralPath $sampleConfigPath -Destination $exeConfigPath -Force
        Write-Host "設定を配置しました（正常）: $exeConfigPath"
    }
    'invalid' {
        # 安全モード（CFG-016）の確認は、不正な設定が「hakutaku.yaml」という
        # 名前で置かれている状態でしか再現できない（設定ファイル名は固定。CFG-014）。
        if (-not (Test-Path -LiteralPath $invalidConfigPath)) {
            throw "不正設定のサンプルが見つかりません: $invalidConfigPath`n-RegenerateSamples を付けて実行し直してください。"
        }
        Copy-Item -LiteralPath $invalidConfigPath -Destination $exeConfigPath -Force
        Write-Host "設定を配置しました（不正／安全モード確認用）: $exeConfigPath"
    }
    'none' {
        if (Test-Path -LiteralPath $exeConfigPath) {
            Remove-Item -LiteralPath $exeConfigPath -Force
            Write-Host "設定を削除しました（設定なしの起動経路）: $exeConfigPath"
        }
        else {
            Write-Host "設定はありません（設定なしの起動経路）: $exeConfigPath"
        }
    }
}

# --- 4. 起動 --------------------------------------------------------------
if ($NoLaunch) {
    Write-Host "起動しません（-NoLaunch）。準備だけ完了しました。"
}
else {
    # 作業ディレクトリを実行ファイルのフォルダにする。設定・logs・temp・
    # WebView2 はいずれも実行ファイル直下を基準に解決されるため（CFG-014、
    # SEC-009）、呼び出し元の作業ディレクトリの影響を受けないようにする。
    Write-Host "起動します: $exePath"
    Start-Process -FilePath $exePath -WorkingDirectory $exeDir | Out-Null
}

# --- 5. 次にやること -----------------------------------------------------
Write-Host ""
Write-Host "次にやること"
Write-Host "  確認手順: $manualCheckPath"
Write-Host "  サンプル: $SampleDir"
if ($staleBuildInputCount -gt 0) {
    # 冒頭の警告は、この一覧まで読み進める間に流れてしまう。確認を始める直前に
    # もう一度、実行ファイルが古いままであることを示す（Issue #24）。
    Write-Host "  実行ファイル: $exePath （古いままです。$staleBuildInputCount 件の変更が反映されていません）"
}
else {
    Write-Host "  実行ファイル: $exePath"
}
Write-Host "  現在の設定: $ConfigMode （$exeConfigPath）"
Write-Host ""
Write-Host "  設定を差し替える（手順書 4.1、4.4 手順7・10、4.5 手順4）。アプリを終了してから実行してください:"
Write-Host "    ./scripts/start-manual-check.ps1 -ConfigMode invalid -SkipBuild -NoLaunch   # 安全モード（4.1 手順3）"
Write-Host "    ./scripts/start-manual-check.ps1 -ConfigMode none    -SkipBuild -NoLaunch   # 設定なし（4.1 手順1、4.4 手順7）"
Write-Host "    ./scripts/start-manual-check.ps1 -ConfigMode normal  -SkipBuild -NoLaunch   # 正常な設定へ戻す（4.4 手順10）"
Write-Host ""
Write-Host "  追記中のログの確認（手順書 4.12・4.13）。別の PowerShell で実行します:"
Write-Host "    ./scripts/append-log-writer.ps1 -OutputPath '$(Join-Path $SampleDir "90-locked.log")' -HoldOpen"
Write-Host "    ./scripts/append-log-writer.ps1 -OutputPath '$(Join-Path $SampleDir "91-append.log")' -DurationSeconds 30"
Write-Host ""
Write-Host "  後片付け（手順書 5章）:"
Write-Host "    ./scripts/start-manual-check.ps1 -CleanUp"
