#![forbid(unsafe_code)]

//! Hakutaku の GUI 非依存な診断ログクレートです。
//!
//! 実行ファイル直下の `logs` フォルダへ、ブートストラップやコア層の診断情報を
//! 記録します。対応する要件 ID は `DIAG-001`〜`DIAG-007`、`SEC-005`、`SEC-009` です。
//! 正本は `docs/security/data-handling.md`（「診断ログの確定要件」）と
//! `tasks/phase-01-bootstrap-webview2.md` です。
//!
//! # マスキングしない方針（`DIAG-003`、`DIAG-004`）
//!
//! 診断ログには、フルパス、ログ本文、DB セル値、DICOM タグ値、認証情報などの
//! 実値を、マスキングや追加設定なしでそのまま記録します。これは仕様どおりの
//! 挙動です。診断ログには個人情報等の機密データが平文で含まれ得るため、`logs` フォルダの
//! 閲覧制限・持ち出し・保管・削除は利用者・導入組織の責任範囲です（`SEC-005`）。
//! この方針は新規作成した診断ログファイルの冒頭にも明記します。
//!
//! # 失敗時の扱い（`DIAG-006`、`DIAG-007`）
//!
//! `logs` フォルダの作成・書き込みに失敗した場合や、昇格プロセスが作成した
//! ログへ非昇格プロセスが書き込めない場合も、[`Diagnostics::open`] は
//! `Err` を返さず、無効化された [`Diagnostics`] と理由（[`DiagnosticsUnavailable`]）
//! を返します。**別の保存先へは自動フォールバックしません。** 呼び出し側は
//! 理由を利用者へ通知したうえで、診断ログなしで動作を継続できます。
//!
//! # レコードの書式と機械的な分割（`DIAG-005`）
//!
//! 1 レコードは 1 行で、欄の区切りは「半角スペース・縦棒・半角スペース」
//! （` | `）です。欄は先頭から次の 9 個で、**本文は必ず最後尾の欄**です。
//!
//! ```text
//! 時刻 | 重要度 | モジュール | 操作種別 | src=ソースID | code=エラーコード | proc=権限 | at=内部位置 | 本文
//! ```
//!
//! 本文はマスキングも加工もせずそのまま記録するため（`DIAG-003`、`DIAG-004`）、
//! **本文に含まれる ` | ` をエスケープしません**。エスケープすると記録された
//! 本文が原文と一致しなくなり、無加工で記録するという要件そのものを崩すため
//! です。区切りに使う文字列を本文から取り除くこともしません。
//!
//! そのため、レコードを機械的に欄へ分割する場合は、**先頭から 8 回だけ区切り、
//! 9 個目（残り全部）を本文とみなします**。Rust なら `line.splitn(9, " | ")`、
//! PowerShell なら `-split ' \| ', 9` が対応します。すべての区切りで分割する
//! 方法は、本文に ` | ` を含むレコードで欄数が変わるため使えません。
//!
//! 行の読み分けは次のとおりです。
//!
//! - `#` で始まる行: ファイル冒頭の見出し（レコードではありません）
//! - タブで始まる行: 直前のレコードの本文の 2 行目以降（[`Record::message`] が
//!   複数行の場合、2 行目以降はタブ 1 個で字下げします）
//! - それ以外の行: 新しいレコードの先頭行
//!
//! # 多重プロセスでの追記とローテーション（`DIAG-002`、`DIAG-006`、`DIAG-007`）
//!
//! 診断ログは追記モードで開くため、昇格プロセスと非昇格プロセスのレコードが
//! 同一ファイルに混在し得ます（`DIAG-007`。ファイル冒頭の見出しにも明記します）。
//! そのためローテーションの判定には、自プロセスが書いたバイト数の累計ではなく、
//! **書き込み直後の実ファイルサイズ**を使います
//! （非公開の `ActiveState::write_record`）。自プロセス分だけを数えると、他プロセスの
//! 追記で実ファイルが `max_file_bytes` を大きく超えても退避されません。
//!
//! 一方、次の3点は**多重プロセス時の既知の性質**として残ります。プロセス間で
//! 退避を調停する仕組み（名前付きミューテックスなど）をこのクレートは持たず、
//! `DIAG-006`／`DIAG-007` の縮退（通知したうえで、診断ログなしでアプリの動作を
//! 継続する）で足りると判断しているためです。診断ログを読むときの前提として
//! ください。
//!
//! 1. 他プロセスが先に退避すると、自プロセスのハンドルは退避済みファイル
//!    （`hakutaku.1.log`）を指したまま追記を続けます。レコード自体は失われ
//!    ませんが、退避済みファイルの末尾に、退避より後の時刻のレコードが並びます
//! 2. 退避（[`std::fs::rename`]）は、他プロセスがハンドルを保持していると
//!    失敗し得ます（Windows）。この場合は `DIAG-006` の扱いに合流し、その時点で
//!    診断ログを無効化して理由を通知します。**判定を書き込みの後に行うため、
//!    直前のレコードは既にファイルへ書き込み済みで、失われません**
//! 3. 複数プロセスがほぼ同時に退避すると世代の繰り下げが二重に走り、
//!    `max_generations` に届く前に古い世代が削除され得ます
//!
//! 単一プロセスで起動する通常の運用（`PRIV-002` の昇格再起動を挟まない場合）
//! では、いずれも発生しません。

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::Local;

/// ローテーション 1 ファイルあたりの既定の最大バイト数です（`DIAG-002`）。
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// ローテーションで保持する既定の世代数です（`DIAG-002`）。
///
/// **この数には現行ファイルを含みます。** 既定値 5 は、現行の `hakutaku.log` と
/// 退避後の `hakutaku.1.log`〜`hakutaku.4.log` を合わせた「現行を含む合計 5
/// ファイル（最大 50 MiB）」を意味し、`hakutaku.5.log` 以降は保持しません。
pub const DEFAULT_MAX_GENERATIONS: u32 = 5;

/// 診断ログファイルの固定名です（`DIAG-001`）。
pub const LOG_FILE_NAME: &str = "hakutaku.log";

/// 新規作成した診断ログファイルの冒頭に必ず書き込む見出しです。
///
/// `SEC-005` の明記（個人情報等の機密データが平文で含まれ得ること、`logs` の管理が
/// 利用者・導入組織の責任範囲であること）を、ファイル単体を読んだだけでも
/// 分かるようにするためのものです。新規作成時とローテーション後の新しい
/// ファイルの両方で書き込みます。
const LOG_HEADER: &str = "\
# Hakutaku 診断ログ
# 診断のため、フルパス、ログ本文、DB セル値、DICOM タグ値、認証情報などの実値を
# マスキングせずそのまま記録します（DIAG-003、DIAG-004）。仕様どおりの動作です。
# このファイルには個人情報等の機密データが平文で含まれ得ます。閲覧制限、持ち出し、保管、削除は
# 利用者・導入組織の責任範囲です（SEC-005）。
# 各レコードの proc= は、その行を出力したプロセスの権限です（DIAG-007）。
#   elevated: 昇格プロセス / normal: 非昇格プロセス / unknown: 判定できなかった
# 追記モードで開くため、昇格・非昇格プロセスのレコードが同一ファイルに
# 混在し得ます。このファイル自体の権限を単一の値では表しません。
";

/// 診断ログ 1 レコードの重要度です（`DIAG-005`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// 起動プロセスの権限（`DIAG-007`）です。
///
/// 判定そのものは `src-tauri` 側（`bootstrap::process`）が行い、このクレートは
/// 受け取った結果を記録するだけです。判定できない場合は `Unknown` を使います。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessElevation {
    Normal,
    Elevated,
    Unknown,
}

/// `DIAG-005` が求める記録項目です。
///
/// `message` はマスキングせずそのまま記録します（`DIAG-003`、`DIAG-004`）。
/// 複数行の場合、2 行目以降はタブ 1 個で字下げし、1 レコードであることが
/// 分かる形式で出力します。
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    pub severity: Severity,
    /// 例: `"bootstrap::runtime"`。
    pub module: &'a str,
    /// 操作種別。例: `"runtime.resolve"`。
    pub operation: &'a str,
    /// セッション内のソース ID。無ければ `None`（出力時は `-`）。
    pub source_id: Option<&'a str>,
    /// アプリ内エラーコード。例: `"HKT-W2-0003"`。無ければ `None`（出力時は `-`）。
    /// 書式と採番規則はリポジトリの `docs/development/error-codes.md` を正本とします。
    pub error_code: Option<&'a str>,
    /// 内部位置またはスタック。`diag_*!` マクロ経由なら `file!():line!()` が自動で入ります。
    pub location: &'a str,
    /// 本文。マスキングしません（`DIAG-003`、`DIAG-004`）。
    pub message: &'a str,
}

/// ローテーション設定です（`DIAG-002`、`CFG-020`）。
///
/// `max_generations` は**現行ファイルを含む**世代数です。既定は
/// [`DEFAULT_MAX_FILE_BYTES`] × [`DEFAULT_MAX_GENERATIONS`]（10 MiB × 5 世代 =
/// 現行を含む合計 5 ファイル、最大 50 MiB）です。設定ファイルからの読み込みは
/// P03（`CFG-020`）が行い、このクレートは値を受け取るだけです。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotationPolicy {
    pub max_file_bytes: u64,
    pub max_generations: u32,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        RotationPolicy {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_generations: DEFAULT_MAX_GENERATIONS,
        }
    }
}

/// 診断ログを使えない理由です（`DIAG-006`）。
///
/// ネイティブダイアログの通知文（`bootstrap::notify::diagnostics_unavailable`）
/// の組み立てに使うことを想定しています。
#[derive(Clone, Debug)]
pub struct DiagnosticsUnavailable {
    /// 対象の絶対パス。呼び出し側（`Layout` など）が絶対パスを渡す前提です。
    pub target: PathBuf,
    /// 日本語の理由。OS のエラー文言を含めてよい。
    pub reason: String,
    /// 取得できた場合の OS エラーコード。
    pub os_error_code: Option<i32>,
}

impl DiagnosticsUnavailable {
    fn from_io(context: &str, target: &Path, error: &io::Error) -> Self {
        DiagnosticsUnavailable {
            target: target.to_path_buf(),
            reason: format!("{context}: {error}"),
            os_error_code: error.raw_os_error(),
        }
    }
}

impl fmt::Display for DiagnosticsUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}（対象: {}", self.reason, self.target.display())?;
        if let Some(code) = self.os_error_code {
            write!(f, "、OS エラーコード: {code}")?;
        }
        write!(f, "）")
    }
}

/// 開いている診断ログの内部状態です。
///
/// `file` は `Option` にしています。ローテーション時に Windows 上でハンドルを
/// 保持したまま rename できないため、退避の前に一度 `None` へ落として
/// ハンドルを明示的に閉じる必要があるからです。
///
/// 現行ファイルのサイズは**保持しません**。多重プロセスで追記し得るため
/// （モジュール doc コメント「多重プロセスでの追記とローテーション」）、
/// 自プロセスが書いた量の累計は実ファイルサイズと一致しないからです。
struct ActiveState {
    dir: PathBuf,
    file: Option<File>,
    policy: RotationPolicy,
    elevation: ProcessElevation,
}

impl ActiveState {
    /// 1 レコードを追記し、**追記後の実ファイルサイズ**でローテーションを
    /// 判定します（`DIAG-002`）。
    ///
    /// 判定に自プロセスの累積書き込み量を使わない理由は、モジュール doc
    /// コメントの「多重プロセスでの追記とローテーション」を参照してください。
    ///
    /// 判定を書き込みの**後**に置いているため、1 ファイルは最大で 1 レコード分
    /// だけ `max_file_bytes` を超え得ます。書き込みの前に判定すると、1 レコードが
    /// 単独で `max_file_bytes` を超える場合に「退避しても収まらない」状態が
    /// 続くため、「超えたら次のレコードから新しいファイルへ」という後判定に
    /// します。退避に失敗した場合でも、このレコード自体は書き込み済みです。
    fn write_record(&mut self, record: &Record<'_>) -> Result<(), DiagnosticsUnavailable> {
        let line = format_record(record, self.elevation);
        let bytes = line.as_bytes();

        let target = generation_path(&self.dir, 0);
        let Some(file) = self.file.as_mut() else {
            // 通常はここへ来ません（rotate 成功後は必ず Some に戻すため）。
            // 到達した場合も panic せず、無効化して呼び出し側の継続を優先します。
            return Err(DiagnosticsUnavailable {
                target,
                reason: "内部状態が不整合です（診断ログのハンドルがありません）".to_string(),
                os_error_code: None,
            });
        };

        file.write_all(bytes).map_err(|error| {
            DiagnosticsUnavailable::from_io("診断ログへ書き込めません", &target, &error)
        })?;
        file.flush().map_err(|error| {
            DiagnosticsUnavailable::from_io("診断ログのフラッシュに失敗しました", &target, &error)
        })?;

        // 追記モードのハンドルは write の直後に必ずファイル末尾を指すため、
        // ここでの位置が「他プロセスの追記を含む実ファイルサイズ」になる。
        // 位置を取得できなかった場合は、記録を止めないことを優先してこの回の
        // 判定だけを見送る（次の書き込みで判定し直される）。
        let size = file.stream_position().ok();
        if size.is_some_and(|size| size > self.policy.max_file_bytes) {
            self.rotate()?;
        }

        Ok(())
    }

    /// `hakutaku.log` → `hakutaku.1.log` → … と退避し、新しい `hakutaku.log` を
    /// 見出し付きで作り直します（`DIAG-002`）。
    ///
    /// 失敗した場合は `Err` を返し、呼び出し側（[`Diagnostics::log`]）が診断ログを
    /// 無効化して理由を通知します（`DIAG-006`）。多重プロセスでは他プロセスの
    /// ハンドル保持により退避が失敗し得ますが、そこまでの調停は行いません
    /// （モジュール doc コメント「多重プロセスでの追記とローテーション」）。
    fn rotate(&mut self) -> Result<(), DiagnosticsUnavailable> {
        // Windows ではハンドルを保持したまま rename できないため、先に閉じる。
        self.file = None;

        self.rotate_generations()?;

        let current_path = generation_path(&self.dir, 0);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&current_path)
            .map_err(|error| {
                DiagnosticsUnavailable::from_io(
                    "ローテーション後の診断ログを作成できません",
                    &current_path,
                    &error,
                )
            })?;

        file.write_all(LOG_HEADER.as_bytes()).map_err(|error| {
            DiagnosticsUnavailable::from_io(
                "ローテーション後の診断ログへ見出しを書き込めません",
                &current_path,
                &error,
            )
        })?;

        self.file = Some(file);
        Ok(())
    }

    /// 世代ファイルのリネームと、上限を超えた世代の削除を行います。
    ///
    /// `max_generations` は現行ファイルを含む合計保持数です。既定 5 の場合、
    /// 退避後は `hakutaku.log`（新規）、`hakutaku.1.log`〜`hakutaku.4.log` の
    /// 合計 5 ファイルを残し、旧 `hakutaku.4.log` は `hakutaku.5.log` へ
    /// 退避せず削除します。
    fn rotate_generations(&self) -> Result<(), DiagnosticsUnavailable> {
        let max_generations = self.policy.max_generations.max(1);

        if max_generations == 1 {
            // 世代退避なし。現行ファイルを削除して新規作成に備える。
            let current = generation_path(&self.dir, 0);
            if current.exists() {
                fs::remove_file(&current).map_err(|error| {
                    DiagnosticsUnavailable::from_io(
                        "古い診断ログを削除できません",
                        &current,
                        &error,
                    )
                })?;
            }
            return Ok(());
        }

        // 保持上限を超える最も古い世代を削除する。
        let oldest = generation_path(&self.dir, max_generations - 1);
        if oldest.exists() {
            fs::remove_file(&oldest).map_err(|error| {
                DiagnosticsUnavailable::from_io(
                    "退避上限を超えた診断ログを削除できません",
                    &oldest,
                    &error,
                )
            })?;
        }

        // 世代番号の大きい方から順にリネームし、上書きを避ける。
        for generation in (1..max_generations - 1).rev() {
            let from = generation_path(&self.dir, generation);
            let to = generation_path(&self.dir, generation + 1);
            if from.exists() {
                fs::rename(&from, &to).map_err(|error| {
                    DiagnosticsUnavailable::from_io("診断ログの世代を退避できません", &from, &error)
                })?;
            }
        }

        let current = generation_path(&self.dir, 0);
        if current.exists() {
            let first = generation_path(&self.dir, 1);
            fs::rename(&current, &first).map_err(|error| {
                DiagnosticsUnavailable::from_io("診断ログを退避できません", &current, &error)
            })?;
        }

        Ok(())
    }
}

/// 診断ログ本体です。無効状態（`DIAG-006`）でも同じ型で扱えます。
///
/// 複数スレッドから安全に使えるよう `Sync + Send` です（内部で `Mutex` を
/// 使用）。ミューテックスが poison していても panic せず、書き込みを諦めて
/// 無効化した扱いにします。
pub struct Diagnostics {
    /// 開いた対象ファイルの絶対パス。`is_active()` が真の間だけ `log_path()`
    /// から参照可能にします。無効化後も内部的には保持しますが公開しません。
    log_path: Option<PathBuf>,
    active: Mutex<Option<ActiveState>>,
    unavailable_reason: OnceLock<DiagnosticsUnavailable>,
}

impl Diagnostics {
    /// `logs_dir` を作成し、ログファイルを開きます。
    ///
    /// 失敗しても `Err` を返さず、無効化された `Diagnostics` と理由を
    /// 返します（`DIAG-006`）。**別の保存先へは自動フォールバックしません。**
    pub fn open(
        logs_dir: &Path,
        policy: RotationPolicy,
        elevation: ProcessElevation,
    ) -> (Self, Option<DiagnosticsUnavailable>) {
        match Self::try_open(logs_dir, policy, elevation) {
            Ok(active) => {
                let log_path = generation_path(logs_dir, 0);
                let diagnostics = Diagnostics {
                    log_path: Some(log_path),
                    active: Mutex::new(Some(active)),
                    unavailable_reason: OnceLock::new(),
                };
                (diagnostics, None)
            }
            Err(reason) => {
                let diagnostics = Diagnostics::unavailable(reason.clone());
                (diagnostics, Some(reason))
            }
        }
    }

    fn try_open(
        logs_dir: &Path,
        policy: RotationPolicy,
        elevation: ProcessElevation,
    ) -> Result<ActiveState, DiagnosticsUnavailable> {
        let target = generation_path(logs_dir, 0);

        fs::create_dir_all(logs_dir).map_err(|error| {
            DiagnosticsUnavailable::from_io("logs フォルダを作成できません", logs_dir, &error)
        })?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|error| {
                DiagnosticsUnavailable::from_io("診断ログファイルを開けません", &target, &error)
            })?;

        // 空のときだけ見出しを書く。既に他プロセスが見出しを書いた後に開いた
        // 場合へ二重に書き足さないための判定であり、長さを取得できなかった
        // 場合は「新規作成された」とみなして見出しを書く（見出しが 1 つも無い
        // ファイルを作らないことを優先する。`SEC-005` の明記）。
        let existing_size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if existing_size == 0 {
            file.write_all(LOG_HEADER.as_bytes()).map_err(|error| {
                DiagnosticsUnavailable::from_io("診断ログの見出しを書き込めません", &target, &error)
            })?;
        }

        Ok(ActiveState {
            dir: logs_dir.to_path_buf(),
            file: Some(file),
            policy,
            elevation,
        })
    }

    /// 明示的に無効な診断ログを作ります（呼び出し側が `logs` を諦めた場合）。
    pub fn unavailable(reason: DiagnosticsUnavailable) -> Self {
        let cell = OnceLock::new();
        // 生成直後の空セルへの set は必ず成功するため、失敗は無視して構わない。
        let _ = cell.set(reason);
        Diagnostics {
            log_path: None,
            active: Mutex::new(None),
            unavailable_reason: cell,
        }
    }

    /// 現在アクティブ（書き込み可能）かどうかを返します。
    pub fn is_active(&self) -> bool {
        self.lock_active().is_some()
    }

    /// 開いているログファイルの絶対パスです。アクティブな間だけ `Some` を返します。
    pub fn log_path(&self) -> Option<&Path> {
        if self.is_active() {
            self.log_path.as_deref()
        } else {
            None
        }
    }

    /// レコードを記録します。無効時は何もしません。失敗しても panic しません。
    ///
    /// 書き込み中に失敗した場合（ディスク満杯、権限不足、昇格プロセスが
    /// 作成したファイルへ非昇格プロセスから書き込めない場合など）は、
    /// その場で内部状態を無効化し、理由を [`Diagnostics::unavailable_reason`]
    /// から取得できるようにします（`DIAG-006`、`DIAG-007`）。
    pub fn log(&self, record: &Record<'_>) {
        let mut guard = self.lock_active();
        let failure = match guard.as_mut() {
            Some(active) => active.write_record(record).err(),
            None => return,
        };

        if let Some(reason) = failure {
            *guard = None;
            drop(guard);
            // 無効化はこの分岐でしか起きないため、二重 set は発生しない。
            let _ = self.unavailable_reason.set(reason);
        }
    }

    /// 書き込み中に発生し、無効化に至った理由です（`DIAG-006` の通知文用）。
    pub fn unavailable_reason(&self) -> Option<&DiagnosticsUnavailable> {
        self.unavailable_reason.get()
    }

    /// ミューテックスが poison していても panic せず、内容を回収します。
    /// このクレートの実行経路では `Mutex` 保持中に panic しない設計のため、
    /// poison は通常発生しませんが、防御的に処理します。
    fn lock_active(&self) -> std::sync::MutexGuard<'_, Option<ActiveState>> {
        match self.active.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// 世代番号からファイルパスを組み立てます。`0` は現行ファイル（`LOG_FILE_NAME`
/// そのもの）、`n >= 1` は `hakutaku.{n}.log` を返します。
fn generation_path(dir: &Path, generation: u32) -> PathBuf {
    if generation == 0 {
        return dir.join(LOG_FILE_NAME);
    }

    let base = Path::new(LOG_FILE_NAME);
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("hakutaku");
    match base.extension().and_then(|s| s.to_str()) {
        Some(ext) => dir.join(format!("{stem}.{generation}.{ext}")),
        None => dir.join(format!("{stem}.{generation}")),
    }
}

/// `DIAG-005` の 1 レコードを、先頭行 1 行（複数行本文は 2 行目以降をタブで
/// 字下げ）の形式へ整形します。`src`・`code` が無い場合は `-` にします。
///
/// 欄の区切りは ` | `、欄は 9 個で本文が最後尾です。本文内の ` | ` は
/// エスケープしないため、読み手は先頭から 8 回だけ分割します。規定の全文は
/// モジュール doc コメントの「レコードの書式と機械的な分割」を正本とし、
/// ここへは複製しません。
fn format_record(record: &Record<'_>, elevation: ProcessElevation) -> String {
    let timestamp = Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z");
    let severity = severity_label(record.severity);
    let source = record.source_id.unwrap_or("-");
    let code = record.error_code.unwrap_or("-");
    let proc = elevation_label(elevation);
    let message = indent_continuation(record.message);

    format!(
        "{timestamp} | {severity} | {module} | {operation} | src={source} | code={code} | proc={proc} | at={location} | {message}\n",
        module = record.module,
        operation = record.operation,
        location = record.location,
    )
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Trace => "TRACE",
        Severity::Debug => "DEBUG",
        Severity::Info => "INFO",
        Severity::Warn => "WARN",
        Severity::Error => "ERROR",
    }
}

fn elevation_label(elevation: ProcessElevation) -> &'static str {
    match elevation {
        ProcessElevation::Normal => "normal",
        ProcessElevation::Elevated => "elevated",
        ProcessElevation::Unknown => "unknown",
    }
}

/// 本文が複数行の場合、2 行目以降をタブ 1 個で字下げします。
///
/// 改行として扱うのは `\n`、`\r\n`、`\r` 単独の 3 種類です。Windows 以外で
/// 作られたログや古い機器の出力には `\r` 単独の改行が現れ得るため、これを
/// 改行として扱わないと、本文の 2 行目以降が字下げされず、次のレコードの
/// 先頭行と見分けられなくなります（モジュール doc コメント「レコードの書式と
/// 機械的な分割」の行の読み分け）。
///
/// 挿入するのはタブだけで、**本文のバイト列そのものは書き換えません**
/// （`DIAG-003`、`DIAG-004`）。`\r\n` は改行 1 個として扱い、`\r` と `\n` の
/// 間へタブを差し込みません。
fn indent_continuation(message: &str) -> String {
    let mut result = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    while let Some(character) = chars.next() {
        result.push(character);
        match character {
            '\n' => result.push('\t'),
            // CRLF は 2 文字で改行 1 個。字下げは後続の LF 側で行う。
            '\r' if chars.peek() != Some(&'\n') => result.push('\t'),
            _ => {}
        }
    }
    result
}

/// `diag_trace!`〜`diag_error!` マクロの内部実装です。
///
/// 利用者はこのマクロを直接呼び出さず、`diag_trace!`・`diag_debug!`・
/// `diag_info!`・`diag_warn!`・`diag_error!` を使ってください。
#[doc(hidden)]
#[macro_export]
macro_rules! __hakutaku_diag_log {
    (@start $diag:expr, $severity:expr, module = $module:expr, operation = $operation:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@rest $diag, $severity, $module, $operation, None, None, $($rest)*)
    };
    (@rest $diag:expr, $severity:expr, $module:expr, $operation:expr, $source:expr, $code:expr, source_id = $source_id:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@rest $diag, $severity, $module, $operation, Some($source_id), $code, $($rest)*)
    };
    (@rest $diag:expr, $severity:expr, $module:expr, $operation:expr, $source:expr, $code:expr, error_code = $error_code:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@rest $diag, $severity, $module, $operation, $source, Some($error_code), $($rest)*)
    };
    (@rest $diag:expr, $severity:expr, $module:expr, $operation:expr, $source:expr, $code:expr, $($fmt:tt)*) => {{
        let hakutaku_diag_message = format!($($fmt)*);
        $diag.log(&$crate::Record {
            severity: $severity,
            module: $module,
            operation: $operation,
            source_id: $source,
            error_code: $code,
            location: concat!(file!(), ":", line!()),
            message: hakutaku_diag_message.as_str(),
        });
    }};
}

/// TRACE 重要度で診断ログへ記録します（`DIAG-005`）。
///
/// `diag_trace!(diag, module = "...", operation = "...", "本文 {}", x)` の形式で
/// 呼び出します。`source_id = ...`・`error_code = ...` は任意（順不同）で
/// 指定できます。`location` は `file!():line!()` から自動的に埋まります。
#[macro_export]
macro_rules! diag_trace {
    ($diag:expr, module = $module:expr, operation = $operation:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@start $diag, $crate::Severity::Trace, module = $module, operation = $operation, $($rest)*)
    };
}

/// DEBUG 重要度で診断ログへ記録します（`DIAG-005`）。[`diag_trace!`] と同じ書式です。
#[macro_export]
macro_rules! diag_debug {
    ($diag:expr, module = $module:expr, operation = $operation:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@start $diag, $crate::Severity::Debug, module = $module, operation = $operation, $($rest)*)
    };
}

/// INFO 重要度で診断ログへ記録します（`DIAG-005`）。[`diag_trace!`] と同じ書式です。
#[macro_export]
macro_rules! diag_info {
    ($diag:expr, module = $module:expr, operation = $operation:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@start $diag, $crate::Severity::Info, module = $module, operation = $operation, $($rest)*)
    };
}

/// WARN 重要度で診断ログへ記録します（`DIAG-005`）。[`diag_trace!`] と同じ書式です。
#[macro_export]
macro_rules! diag_warn {
    ($diag:expr, module = $module:expr, operation = $operation:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@start $diag, $crate::Severity::Warn, module = $module, operation = $operation, $($rest)*)
    };
}

/// ERROR 重要度で診断ログへ記録します（`DIAG-005`）。[`diag_trace!`] と同じ書式です。
#[macro_export]
macro_rules! diag_error {
    ($diag:expr, module = $module:expr, operation = $operation:expr, $($rest:tt)*) => {
        $crate::__hakutaku_diag_log!(@start $diag, $crate::Severity::Error, module = $module, operation = $operation, $($rest)*)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "hakutaku-diagnostics-unit-{label}-{}-{count}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn rotation_policy_default_matches_diag_002() {
        let policy = RotationPolicy::default();
        assert_eq!(policy.max_file_bytes, DEFAULT_MAX_FILE_BYTES);
        assert_eq!(policy.max_generations, DEFAULT_MAX_GENERATIONS);
        assert_eq!(DEFAULT_MAX_FILE_BYTES, 10 * 1024 * 1024);
        assert_eq!(DEFAULT_MAX_GENERATIONS, 5);
    }

    #[test]
    fn generation_path_naming() {
        let dir = Path::new("C:/example/logs");
        assert_eq!(generation_path(dir, 0), dir.join("hakutaku.log"));
        assert_eq!(generation_path(dir, 1), dir.join("hakutaku.1.log"));
        assert_eq!(generation_path(dir, 4), dir.join("hakutaku.4.log"));
    }

    #[test]
    fn indent_continuation_marks_second_line_and_later() {
        assert_eq!(indent_continuation("single"), "single");
        assert_eq!(indent_continuation("line1\nline2"), "line1\n\tline2");
        assert_eq!(
            indent_continuation("line1\nline2\nline3"),
            "line1\n\tline2\n\tline3"
        );
    }

    // 受け入れ条件: `\r` 単独の改行も継続行として字下げし、`\r\n` は改行 1 個
    // として扱う（`DIAG-005` の「1 レコードであることが分かる形式」、Issue #41）。
    #[test]
    fn indent_continuation_treats_lone_cr_and_crlf_as_line_breaks() {
        assert_eq!(indent_continuation("line1\r\nline2"), "line1\r\n\tline2");
        assert_eq!(indent_continuation("line1\rline2"), "line1\r\tline2");
        assert_eq!(
            indent_continuation("line1\rline2\r\nline3\nline4"),
            "line1\r\tline2\r\n\tline3\n\tline4"
        );
        // 末尾の改行の後ろにも字下げが入る（`\n` のときと同じ扱い）。
        assert_eq!(indent_continuation("line1\r"), "line1\r\t");
    }

    // 受け入れ条件: 字下げはタブを差し込むだけで、本文の文字そのものを
    // 書き換えない（`DIAG-003`、`DIAG-004`）。
    #[test]
    fn indent_continuation_only_inserts_tabs() {
        let message = "本文\r\n2 行目\r3 行目\n4 行目";
        let indented = indent_continuation(message);
        let stripped: String = indented.chars().filter(|c| *c != '\t').collect();
        assert_eq!(stripped, message);
    }

    // 受け入れ条件: 本文に区切り文字 ` | ` が含まれてもエスケープせず、
    // 先頭から 8 回の分割で 9 個目に本文全体が入る（`DIAG-003`、`DIAG-004`、
    // モジュール doc コメント「レコードの書式と機械的な分割」）。
    #[test]
    fn format_record_keeps_separator_in_message_and_places_it_last() {
        let message = "本文 | の中に | 区切りが | あります";
        let line = format_record(
            &Record {
                severity: Severity::Info,
                module: "test::format",
                operation: "format.separator",
                source_id: None,
                error_code: None,
                location: "lib.rs:0",
                message,
            },
            ProcessElevation::Normal,
        );

        let trimmed = line.strip_suffix('\n').expect("行末は改行");
        let fields: Vec<&str> = trimmed.splitn(9, " | ").collect();
        assert_eq!(fields.len(), 9, "欄は 9 個（本文が最後尾）");
        assert_eq!(fields[8], message, "本文は無加工のまま最後尾の欄に入る");
        assert_eq!(fields[1], "INFO");
        assert_eq!(fields[2], "test::format");
        assert_eq!(fields[3], "format.separator");
        assert_eq!(fields[4], "src=-");
        assert_eq!(fields[5], "code=-");
        assert_eq!(fields[6], "proc=normal");
        assert_eq!(fields[7], "at=lib.rs:0");

        // すべての区切りで分割すると欄数が増えるため使えないことも固定する。
        assert!(trimmed.split(" | ").count() > 9);
    }

    #[test]
    fn unavailable_diagnostics_never_active_and_never_panics() {
        let reason = DiagnosticsUnavailable {
            target: PathBuf::from("C:/example/logs/hakutaku.log"),
            reason: "テスト用の理由".to_string(),
            os_error_code: Some(5),
        };
        let diagnostics = Diagnostics::unavailable(reason);

        assert!(!diagnostics.is_active());
        assert!(diagnostics.log_path().is_none());
        assert!(diagnostics.unavailable_reason().is_some());

        // 無効時は何もしない。panic しないことを確認する。
        diagnostics.log(&Record {
            severity: Severity::Error,
            module: "test",
            operation: "test.op",
            source_id: None,
            error_code: None,
            location: "lib.rs:0",
            message: "no-op",
        });
    }

    #[test]
    fn open_creates_directory_and_writes_header_and_record() {
        let dir = unique_temp_dir("open");
        assert!(!dir.exists());

        let (diagnostics, unavailable) =
            Diagnostics::open(&dir, RotationPolicy::default(), ProcessElevation::Normal);
        assert!(unavailable.is_none());
        assert!(diagnostics.is_active());

        diag_info!(
            diagnostics,
            module = "test::mod",
            operation = "test.op",
            "本文です"
        );

        let path = diagnostics.log_path().expect("アクティブなら Some");
        let content = std::fs::read_to_string(path).expect("読み取れる");
        assert!(content.starts_with("# Hakutaku 診断ログ"));
        assert!(content.contains("SEC-005"));
        assert!(content.contains("本文です"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnostics_type_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Diagnostics>();
    }
}
