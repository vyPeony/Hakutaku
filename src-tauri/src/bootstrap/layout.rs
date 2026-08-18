//! 実行時フォルダの配置（P01-3）。
//!
//! Hakutaku は実行ファイルと同じフォルダの直下だけに実行時データを置きます。
//! このモジュールは、その位置の解決（[`Layout::discover`] / [`Layout::from_exe_dir`]）、
//! フォルダの作成と書き込み確認（`ensure_*`）、`temp` の清掃（[`Layout::purge_temp`]）を
//! 提供します。
//!
//! 満たす要件:
//!
//! - `DIST-013`: WebView2 のユーザーデータフォルダを実行ファイル直下の `WebView2` に固定する。
//!   既定の `<実行ファイル名>.exe.WebView2` は使わない。
//! - `DIST-014`: `WebView2` を作成・書き込みできない場合、理由・対象パス・必要な権限を
//!   呼び出し側へ返す。**別の場所へは自動フォールバックしない。**
//! - `SEC-006`: 一時ファイルは `temp` 配下に限定し、起動時に残存ファイルを清掃する。
//! - `SEC-009`: 実行時に作成・書き込みするフォルダを `logs`・`temp`・`WebView2` に限定する。
//!   `%LOCALAPPDATA%` などユーザープロファイル配下は一切参照しない。
//!
//! `SEC-009` は「導入フォルダごと退避・削除すれば、Hakutaku が残したデータをすべて
//! 処分できる」ことまでを求めています。フォルダ**自体**がリンク（シンボリックリンク・
//! ジャンクション）へ差し替えられていると、書き込みはリンク先へ透過的に抜け、この保証が
//! 崩れます。`fs::create_dir_all` は既存のジャンクションを「既存ディレクトリ」として
//! 成功扱いにするため、`ensure_*` の作成・書き込み確認だけではこれを検出できません。
//! そのため [`Layout::ensure_runtime_folders_are_real_directories`] で 4 フォルダを
//! 起動前に検査し、1 つでもリンクなら起動を拒否します（Issue #42）。
//!
//! 本体コードは `std::env::current_exe()` の親ディレクトリだけを基準にし、
//! `std::env::temp_dir()` や `%LOCALAPPDATA%` 相当のユーザープロファイル参照は行いません
//! （`SEC-009`）。テストコードでのみ、確認用の作業ディレクトリとして
//! `std::env::temp_dir()` を使用します。
//!
//! このファイルは他の `bootstrap` サブモジュール（`notify` / `diagnostics` など）に
//! 依存しません。失敗は [`DirectoryFailure`] / [`LayoutError`] として返すだけで、
//! 通知や診断ログへの記録は呼び出し側（統合担当）が行います。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 実行ファイル直下に置く診断ログフォルダの固定名（`DIAG-001`）。
const LOGS_DIR_NAME: &str = "logs";
/// 実行ファイル直下に置く一時フォルダの固定名（`SEC-006`）。
const TEMP_DIR_NAME: &str = "temp";
/// WebView2 のユーザーデータフォルダの固定名（`DIST-013`）。
const WEBVIEW2_DATA_DIR_NAME: &str = "WebView2";
/// Fixed Version Runtime の配置先フォルダの固定名（`DIST-008`）。
const WEBVIEW2_RUNTIME_DIR_NAME: &str = "WebView2Runtime";
/// 設定ファイルの固定名。実行ファイル直下に置く。
const CONFIG_FILE_NAME: &str = "hakutaku.yaml";

/// 書き込み確認に失敗した場合・作成に失敗した場合に、利用者へ伝えるべき対処。
///
/// `DIST-014` は「作成・書き込みできない場合、理由、対象パス、必要な権限を通知して
/// 起動を中止する。別の場所へ自動フォールバックしない」ことを求めている。
/// この文言はその「必要な権限・対処」を表す。
const REQUIRED_PRIVILEGE_MESSAGE: &str = "導入フォルダへの書き込み権限が必要です。別の場所へは作成しません。書き込み可能なフォルダへ Hakutaku 一式を移動するか、管理者に権限の付与を依頼してください。";

/// 実行時フォルダ自体がリンクだったために起動を拒否した場合に、利用者へ伝えるべき対処
/// （`SEC-009`、Issue #42）。
///
/// 「リンクを実体のフォルダへ戻す」ことが唯一の対処であり、Hakutaku 側で
/// リンクを削除したり、リンク先へ書き込みを続けたりはしない。
const REPARSE_POINT_REMEDY_MESSAGE: &str = "このリンクを削除し、同じ名前の実体のフォルダを作り直すか、リンクごと削除してから Hakutaku を再起動してください（Hakutaku が起動時に作り直します）。リンク先に必要なデータが残っている場合は、削除の前に手元へ移してください。これらのフォルダをリンクにして別の場所へ逃がす運用はできません。";

/// 実行ファイル直下に固定される実行時フォルダの位置（`SEC-009`、`DIST-013`）。
///
/// すべてのパスは [`Layout::discover`] または [`Layout::from_exe_dir`] の時点で
/// 実行ファイルの親ディレクトリ（`exe_dir`）を基準に確定し、以後変化しない。
#[derive(Clone, Debug)]
pub struct Layout {
    exe_dir: PathBuf,
    logs_dir: PathBuf,
    temp_dir: PathBuf,
    webview2_data_dir: PathBuf,
    webview2_runtime_dir: PathBuf,
    config_path: PathBuf,
}

impl Layout {
    /// 実行ファイルの位置から `Layout` を解決する。
    ///
    /// `std::env::current_exe()` の親ディレクトリを `exe_dir` とする。
    /// フォルダの作成は行わない（作成は `ensure_*` の役割）。
    ///
    /// シンボリックリンクの解決に失敗するなどして実行ファイルの位置を取得できない場合、
    /// または親ディレクトリを求められない場合は [`LayoutError::ExecutablePathUnavailable`]
    /// を返す。
    pub fn discover() -> Result<Self, LayoutError> {
        let exe_path = std::env::current_exe().map_err(|err| {
            LayoutError::ExecutablePathUnavailable(format!(
                "実行ファイルの位置を取得できません: {err}"
            ))
        })?;

        let exe_dir = exe_path.parent().ok_or_else(|| {
            LayoutError::ExecutablePathUnavailable(format!(
                "実行ファイル「{}」の親ディレクトリを取得できません。",
                exe_path.display()
            ))
        })?;

        Ok(Self::from_exe_dir(exe_dir.to_path_buf()))
    }

    /// `exe_dir` を明示指定して `Layout` を組み立てる。テスト用の入口。
    ///
    /// 統合担当が `discover()` を経由しない構成（テスト、将来の検証用途）で
    /// 使うことを想定し、`#[cfg(test)]` には限定しない。
    pub fn from_exe_dir(exe_dir: PathBuf) -> Self {
        let logs_dir = exe_dir.join(LOGS_DIR_NAME);
        let temp_dir = exe_dir.join(TEMP_DIR_NAME);
        let webview2_data_dir = exe_dir.join(WEBVIEW2_DATA_DIR_NAME);
        let webview2_runtime_dir = exe_dir.join(WEBVIEW2_RUNTIME_DIR_NAME);
        let config_path = exe_dir.join(CONFIG_FILE_NAME);

        Self {
            exe_dir,
            logs_dir,
            temp_dir,
            webview2_data_dir,
            webview2_runtime_dir,
            config_path,
        }
    }

    /// 実行ファイルが存在するフォルダ。
    pub fn exe_dir(&self) -> &Path {
        &self.exe_dir
    }

    /// 診断ログの保存先（`<exe_dir>/logs`、`DIAG-001`）。
    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// 一時ファイルの保存先（`<exe_dir>/temp`、`SEC-006`）。
    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    /// WebView2 のユーザーデータフォルダ（`<exe_dir>/WebView2`、`DIST-013`）。
    pub fn webview2_data_dir(&self) -> &Path {
        &self.webview2_data_dir
    }

    /// Fixed Version Runtime の配置先（`<exe_dir>/WebView2Runtime`、`DIST-008`）。
    pub fn webview2_runtime_dir(&self) -> &Path {
        &self.webview2_runtime_dir
    }

    /// 設定ファイルの位置（`<exe_dir>/hakutaku.yaml`）。
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// 4 つの実行時フォルダ自体が、リンク（シンボリックリンク・ジャンクション）へ
    /// 差し替えられていないことを確認する（`SEC-009`、Issue #42）。
    ///
    /// 差し替えを見つけた場合は [`ReparsePointRejection`] を返す。呼び出し側は理由を
    /// 通知して起動を中止する。`ensure_*` の失敗（[`DirectoryFailure`]）が
    /// 「用意できない」ことを表すのに対し、こちらは「用意はできるが、書き込むと
    /// 導入フォルダの外へ抜ける」ことを表すため、`logs` のように通常は失敗しても
    /// 続行するフォルダ（`DIAG-006`）でも起動を中止する。
    ///
    /// 検査は、どのフォルダへも書き込む前に 1 回で済ませる。`logs` を最初に検査するのは、
    /// 診断ログを開く前にその保存先自体の差し替えを検出する必要があるためである。
    ///
    /// まだ存在しないフォルダは検査対象にならない（差し替えられていないため）。
    /// 存在するが種別を確認できない場合も拒否しない（後続の `ensure_*` が
    /// [`DirectoryFailure`] として具体的な理由を返すため、確認できていない状態を
    /// 「リンクである」と断定して起動を止めない）。
    pub fn ensure_runtime_folders_are_real_directories(&self) -> Result<(), ReparsePointRejection> {
        for (folder_name, purpose, path) in self.runtime_folders() {
            if is_reparse_point_path(path) {
                return Err(ReparsePointRejection {
                    target: path.to_path_buf(),
                    folder_name,
                    purpose,
                    remedy: REPARSE_POINT_REMEDY_MESSAGE.to_string(),
                });
            }
        }

        Ok(())
    }

    /// 検査対象の実行時フォルダを、固定名・役割・絶対パスの組で返す。
    ///
    /// 役割の文言は利用者向けの通知文へそのまま載せる。`WebView2`（ユーザーデータの
    /// 保存先）と `WebView2Runtime`（Fixed Version Runtime の配置先）は名前が似ており
    /// 取り違えられやすいため、どちらのフォルダの話なのかが文面だけで分かるようにする。
    fn runtime_folders(&self) -> [(&'static str, &'static str, &Path); 4] {
        [
            (LOGS_DIR_NAME, "診断ログの保存先", self.logs_dir.as_path()),
            (
                TEMP_DIR_NAME,
                "一時ファイルの保存先",
                self.temp_dir.as_path(),
            ),
            (
                WEBVIEW2_DATA_DIR_NAME,
                "WebView2 のユーザーデータ（閲覧・実行状態）の保存先",
                self.webview2_data_dir.as_path(),
            ),
            (
                WEBVIEW2_RUNTIME_DIR_NAME,
                "Fixed Version WebView2 Runtime 本体の配置先",
                self.webview2_runtime_dir.as_path(),
            ),
        ]
    }

    /// `logs` フォルダを作成し、書き込めることを確認する（`DIAG-001`、`DIAG-006`）。
    ///
    /// 作成・書き込みに失敗しても panic せず [`DirectoryFailure`] を返す。
    /// `logs` を諦めて動作を継続するかどうかは呼び出し側（診断ログ担当）が判断する。
    pub fn ensure_logs(&self) -> Result<&Path, DirectoryFailure> {
        ensure_directory_writable(&self.logs_dir)?;
        Ok(&self.logs_dir)
    }

    /// `temp` フォルダを作成し、書き込めることを確認する（`SEC-006`）。
    pub fn ensure_temp(&self) -> Result<&Path, DirectoryFailure> {
        ensure_directory_writable(&self.temp_dir)?;
        Ok(&self.temp_dir)
    }

    /// `WebView2` フォルダを作成し、書き込めることを確認する（`DIST-013`、`DIST-014`）。
    ///
    /// 失敗した場合、呼び出し側は**別の場所へフォールバックせず**起動を中止し、
    /// 返された [`DirectoryFailure`] の内容（対象パス・理由・必要な権限）を
    /// ネイティブダイアログで通知する（`DIST-014`）。
    pub fn ensure_webview2_data(&self) -> Result<&Path, DirectoryFailure> {
        ensure_directory_writable(&self.webview2_data_dir)?;
        Ok(&self.webview2_data_dir)
    }

    /// `temp` 配下の残存ファイル・フォルダを削除する（`SEC-006`）。
    ///
    /// - `temp` 自体は残す。
    /// - 個々のエントリの削除に失敗しても [`TempPurgeReport::failures`] に積むだけで、
    ///   起動は止めない。
    /// - `temp` が存在しない場合は「削除 0 件」の成功として扱う。
    /// - シンボリックリンクやジャンクションは、リンク先をたどらずリンク自体だけを
    ///   削除する。これにより `temp` の外にあるファイルを誤って削除しない。
    pub fn purge_temp(&self) -> TempPurgeReport {
        let mut report = TempPurgeReport::default();

        let entries = match fs::read_dir(&self.temp_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // temp がまだ作られていない場合、清掃対象がないため成功として扱う。
                return report;
            }
            Err(err) => {
                report.failures.push(TempPurgeFailure {
                    target: self.temp_dir.clone(),
                    reason: format!(
                        "フォルダ「{}」を読み取れません: {}",
                        self.temp_dir.display(),
                        err
                    ),
                });
                return report;
            }
        };

        for entry in entries {
            match entry {
                Ok(entry) => remove_entry_tree(&entry.path(), &mut report),
                Err(err) => report.failures.push(TempPurgeFailure {
                    target: self.temp_dir.clone(),
                    reason: format!("temp 配下のエントリを読み取れません: {err}"),
                }),
            }
        }

        report
    }
}

/// ディレクトリを作成し、実際に書き込めるかまで確認する。
///
/// 書き込み確認は、対象ディレクトリ内に一意な名前の一時ファイルを作成してから
/// 削除する方式で行う。確認用ファイルが残らないようにする。
/// 削除に失敗した場合でも、書き込み確認自体（ファイル作成）には成功しているため
/// エラーとはしない（`temp` 配下であれば次回起動時の [`Layout::purge_temp`] で
/// 回収できる。`logs`・`WebView2` では通常発生しない一過性の事象として扱う）。
fn ensure_directory_writable(dir: &Path) -> Result<(), DirectoryFailure> {
    if let Err(err) = fs::create_dir_all(dir) {
        return Err(DirectoryFailure {
            target: dir.to_path_buf(),
            action: DirectoryAction::Create,
            reason: format!("フォルダ「{}」を作成できません: {}", dir.display(), err),
            os_error_code: err.raw_os_error(),
            required_privilege: REQUIRED_PRIVILEGE_MESSAGE.to_string(),
        });
    }

    let probe_path = dir.join(probe_file_name());
    match fs::write(&probe_path, b"hakutaku-write-check") {
        Ok(()) => {
            let _ = fs::remove_file(&probe_path);
            Ok(())
        }
        Err(err) => Err(DirectoryFailure {
            target: dir.to_path_buf(),
            action: DirectoryAction::Write,
            reason: format!("フォルダ「{}」へ書き込めません: {}", dir.display(), err),
            os_error_code: err.raw_os_error(),
            required_privilege: REQUIRED_PRIVILEGE_MESSAGE.to_string(),
        }),
    }
}

/// 書き込み確認用の一時ファイル名を一意に作る。
///
/// プロセス ID と現在時刻（UNIX エポックからのナノ秒）を組み合わせる。
/// `SystemTime::duration_since` が失敗する（システム時計がエポックより前）
/// という通常起こり得ない状況でも panic せず `0` にフォールバックする。
fn probe_file_name() -> String {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(".hakutaku-write-check-{pid}-{nanos}.tmp")
}

/// `symlink_metadata` で得たリンクが、ディレクトリ型のリンク（ディレクトリ
/// シンボリックリンクやジャンクション）かどうかを判定する。
///
/// `FileType::is_dir()` はリンクそのものの種別を返すため、`symlink_metadata` 由来の
/// 値では常に `false` になり、ディレクトリ型かどうかの判定には使えない。
/// Windows ではファイル属性 (`FILE_ATTRIBUTE_DIRECTORY`) にディレクトリ型リンクか
/// どうかが反映されるため、こちらで判定する。
#[cfg(windows)]
fn is_directory_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // winapi の FILE_ATTRIBUTE_DIRECTORY。新規に windows クレートへ依存しない。
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
}

/// Unix 系では、シンボリックリンクはリンク先の種別によらず `remove_file`
/// （`unlink`）でリンク自体だけを削除できるため、特別な判定は不要。
#[cfg(not(windows))]
fn is_directory_link(_metadata: &fs::Metadata) -> bool {
    false
}

/// `path` が存在し、かつリパースポイント（シンボリックリンク・ジャンクションなど、
/// 別の場所へ透過的に転送される仕掛け）である場合だけ `true` を返す。
///
/// 判定には `symlink_metadata`（リンクをたどらない）を使う。`metadata` ではリンク先の
/// 情報が返り、差し替えを検出できない。
///
/// 存在しない場合と種別を確認できない場合は `false` を返す。前者は差し替えようがなく、
/// 後者は「確認できていない」だけであり、リンクだと断定して起動を止めるべきではない
/// （`WebView2Runtime` 以外は後続の `ensure_*` が具体的な理由を返す）。
fn is_reparse_point_path(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => is_reparse_point(&metadata),
        Err(_) => false,
    }
}

/// `symlink_metadata` で得た情報が、リパースポイントを指しているかどうかを判定する。
///
/// `FileType::is_symlink()` はシンボリックリンクとジャンクション（マウントポイント）
/// だけを真とするため、それ以外のリパースポイント（別の場所へ内容を転送する仕掛け）を
/// 取りこぼす。`SEC-009` の保証（導入フォルダごと削除すればデータを処分できる）は
/// タグの種類によらず崩れるため、リパースポイントかどうかをファイル属性
/// （`FILE_ATTRIBUTE_REPARSE_POINT`）で直接判定する。
#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // winapi の FILE_ATTRIBUTE_REPARSE_POINT。新規に windows クレートへ依存しない。
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Unix 系にリパースポイントはない。相当する脅威はシンボリックリンクだけ。
#[cfg(not(windows))]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// [`remove_entry_tree`] の明示スタックへ積む作業単位。
enum PurgeStep {
    /// 種別を判定して削除する。フォルダなら、配下と自分自身の削除を積み直す。
    Inspect(PathBuf),
    /// 配下をすべて処理し終えたフォルダ自身を削除する。
    RemoveEmptyDir(PathBuf),
}

/// `path` 以下を削除する。`temp` の外へは出ない。
///
/// 種別の判定には `symlink_metadata`（リンクをたどらない）を使う。
/// シンボリックリンクやジャンクションは、リンク先をたどらず**リンク自体だけ**を
/// 削除する（削除前に中身へ入らない）。これにより `temp` の外にあるファイルを
/// 誤って削除しない。
///
/// 走査は再帰呼び出しではなく明示スタック（[`PurgeStep`]）で行う。`temp` 配下の
/// 深さは Hakutaku が決めるものではなく（残存物は異常終了時の中身や外部プロセスが
/// 置いたものでもあり得る）、再帰では深いツリーで通知のないスタックオーバーフローに
/// 至るため（`ERR-004` の「再帰に上限を設ける」と同じ趣旨、Issue #42）。
/// 深さの上限値を決め打ちすると、その値より深いだけの正常な残存物を清掃できなくなる。
/// 反復方式ならその恣意的な線引きが要らないため、上限方式ではなくこちらを採る。
fn remove_entry_tree(root: &Path, report: &mut TempPurgeReport) {
    let mut pending = vec![PurgeStep::Inspect(root.to_path_buf())];

    while let Some(step) = pending.pop() {
        match step {
            PurgeStep::Inspect(path) => inspect_and_remove(path, &mut pending, report),
            PurgeStep::RemoveEmptyDir(path) => match fs::remove_dir(&path) {
                Ok(()) => report.removed_entries += 1,
                Err(err) => report.failures.push(TempPurgeFailure {
                    target: path.clone(),
                    reason: format!("フォルダ「{}」を削除できません: {}", path.display(), err),
                }),
            },
        }
    }
}

/// [`remove_entry_tree`] のエントリ 1 件分の処理。
///
/// フォルダの場合は、自分自身の削除を先に積んでから配下を積む。スタックは後入れ先出し
/// のため、この順序により配下がすべて処理された後に自分自身が削除される
/// （空でないフォルダに対する `remove_dir` は失敗するため、この順序でなければならない）。
fn inspect_and_remove(path: PathBuf, pending: &mut Vec<PurgeStep>, report: &mut TempPurgeReport) {
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) => {
            report.failures.push(TempPurgeFailure {
                target: path,
                reason: format!("種別を確認できません: {err}"),
            });
            return;
        }
    };

    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        // 注意: `symlink_metadata` で得た FileType の `is_dir()` は、リンクに対しては
        // 常に false になる（リンク先の種別ではなく、リンクそのものの種別を返すため）。
        // そのためディレクトリ型のシンボリックリンクやジャンクションを判定するには、
        // Windows のファイル属性（`FILE_ATTRIBUTE_DIRECTORY`）を直接見る必要がある。
        // 実機（Windows 10 Pro 19045）で確認済み: ジャンクションに `remove_file` を
        // 呼ぶと `ERROR_ACCESS_DENIED` になり、`remove_dir` はリンクだけを削除し
        // リンク先を残す。
        let result = if is_directory_link(&metadata) {
            fs::remove_dir(&path)
        } else {
            fs::remove_file(&path)
        };

        match result {
            Ok(()) => report.removed_entries += 1,
            Err(err) => report.failures.push(TempPurgeFailure {
                target: path.clone(),
                reason: format!("リンク「{}」を削除できません: {}", path.display(), err),
            }),
        }
        return;
    }

    if !file_type.is_dir() {
        match fs::remove_file(&path) {
            Ok(()) => report.removed_entries += 1,
            Err(err) => report.failures.push(TempPurgeFailure {
                target: path.clone(),
                reason: format!("ファイル「{}」を削除できません: {}", path.display(), err),
            }),
        }
        return;
    }

    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(err) => {
            // 中身を確認できないフォルダは、削除も試みない（中身が残ったままの
            // `remove_dir` は必ず失敗し、同じ原因で失敗が2件記録されるだけになる）。
            report.failures.push(TempPurgeFailure {
                target: path.clone(),
                reason: format!("フォルダ「{}」を読み取れません: {}", path.display(), err),
            });
            return;
        }
    };

    pending.push(PurgeStep::RemoveEmptyDir(path.clone()));

    for entry in entries {
        match entry {
            Ok(entry) => pending.push(PurgeStep::Inspect(entry.path())),
            Err(err) => report.failures.push(TempPurgeFailure {
                target: path.clone(),
                reason: format!("配下のエントリを読み取れません: {err}"),
            }),
        }
    }
}

/// フォルダの作成・書き込みに失敗した理由（`DIST-014`）。
///
/// 通知文（`bootstrap::notify::webview2_data_unavailable` など）の組み立てに使う。
#[derive(Clone, Debug)]
pub struct DirectoryFailure {
    /// 対象の絶対パス。
    pub target: PathBuf,
    /// 作成に失敗したのか、書き込みに失敗したのか。
    pub action: DirectoryAction,
    /// 日本語の理由。OS のエラー文言を含めてよい。
    pub reason: String,
    /// `std::io::Error::raw_os_error()` の値。
    pub os_error_code: Option<i32>,
    /// 利用者が取るべき対処（日本語）。「別の場所へは作成しない」ことを含む。
    pub required_privilege: String,
}

/// [`DirectoryFailure`] がどの操作で発生したか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryAction {
    /// フォルダの作成に失敗した。
    Create,
    /// フォルダへの書き込み確認に失敗した。
    Write,
}

/// 実行時フォルダ自体がリンクへ差し替えられていたために起動を拒否する理由
/// （`SEC-009`、Issue #42）。
///
/// [`DirectoryFailure`] と違い、呼び出し側に選択の余地はない。リンク先への書き込みを
/// 続ければ `SEC-009` の保証（導入フォルダごと削除すればデータを処分できる）が崩れ、
/// リンクを Hakutaku 側で削除・差し替えれば利用者のデータを壊しかねないため、
/// 通知して起動を中止する以外の継続手段を用意しない。
#[derive(Clone, Debug)]
pub struct ReparsePointRejection {
    /// 対象フォルダの絶対パス。
    pub target: PathBuf,
    /// 対象フォルダの固定名（`logs`／`temp`／`WebView2`／`WebView2Runtime`）。
    pub folder_name: &'static str,
    /// そのフォルダの役割（利用者向けの短い説明）。
    pub purpose: &'static str,
    /// 利用者が取るべき対処（日本語）。
    pub remedy: String,
}

/// [`Layout::purge_temp`] の結果。
#[derive(Clone, Debug, Default)]
pub struct TempPurgeReport {
    /// 削除できたファイル・フォルダ・リンクの件数。
    pub removed_entries: usize,
    /// 削除に失敗したエントリ。起動を止めるためのものではなく、記録用。
    pub failures: Vec<TempPurgeFailure>,
}

/// `temp` 配下のエントリ 1 件の削除失敗。
#[derive(Clone, Debug)]
pub struct TempPurgeFailure {
    /// 削除できなかった対象の絶対パス。
    pub target: PathBuf,
    /// 日本語の理由。
    pub reason: String,
}

/// [`Layout::discover`] が実行ファイルの位置を解決できなかった場合のエラー。
#[derive(Clone, Debug)]
pub enum LayoutError {
    /// `std::env::current_exe()` の失敗、または親ディレクトリを取得できなかった。
    ExecutablePathUnavailable(String),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::ExecutablePathUnavailable(reason) => {
                write!(f, "実行ファイルの位置を解決できません: {reason}")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// テスト専用の一意な作業ディレクトリ。
    ///
    /// 本体コードは `std::env::temp_dir()` を使わない（`SEC-009`）が、
    /// テストコードでは実ファイルシステム上で検証するためにこれを使う。
    /// `Drop` で必ず後片付けする。
    struct TestWorkspace {
        root: PathBuf,
    }

    impl TestWorkspace {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let root = std::env::temp_dir()
                .join(format!("hakutaku-layout-test-{label}-{pid}-{nanos}-{n}"));
            fs::create_dir_all(&root).expect("テスト用ディレクトリを作成できません");
            Self { root }
        }

        fn path(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for TestWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// `link_path` に `target_dir` を指すディレクトリジャンクションを作る。
    ///
    /// Windows のディレクトリジャンクションは、シンボリックリンクと異なり
    /// 管理者権限・開発者モードなしで作成できる（`mklink /J`）。作成に失敗する環境
    /// （Windows 以外、`mklink` が使えない等）では前提を満たせないため、呼び出し側は
    /// `false` を受けてテストをスキップする。
    fn create_junction(link_path: &Path, target_dir: &Path) -> bool {
        let created = std::process::Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/J",
                &link_path.display().to_string(),
                &target_dir.display().to_string(),
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);

        created && link_path.exists()
    }

    #[test]
    fn from_exe_dir_places_runtime_folders_directly_under_exe_dir() {
        let workspace = TestWorkspace::new("basic");
        let exe_dir = workspace.path().join("app");
        let layout = Layout::from_exe_dir(exe_dir.clone());

        assert_eq!(layout.exe_dir(), exe_dir.as_path());
        assert_eq!(layout.logs_dir(), exe_dir.join("logs").as_path());
        assert_eq!(layout.temp_dir(), exe_dir.join("temp").as_path());
        assert_eq!(
            layout.webview2_data_dir(),
            exe_dir.join("WebView2").as_path()
        );
        assert_eq!(
            layout.webview2_runtime_dir(),
            exe_dir.join("WebView2Runtime").as_path()
        );
        assert_eq!(
            layout.config_path(),
            exe_dir.join("hakutaku.yaml").as_path()
        );
    }

    #[test]
    fn webview2_data_dir_is_not_the_default_exe_scoped_folder_name() {
        let workspace = TestWorkspace::new("naming");
        // 既定の WebView2 は "<実行ファイル名>.exe.WebView2" になる。
        // DIST-013 はこれを避け、フォルダ名がちょうど "WebView2" であることを求める。
        let exe_dir = workspace.path().join("Hakutaku");
        let layout = Layout::from_exe_dir(exe_dir);

        let file_name = layout
            .webview2_data_dir()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("ファイル名を取得できません");

        assert_eq!(file_name, "WebView2");
        assert!(!file_name.contains(".exe."));
        assert!(!file_name.to_lowercase().contains("hakutaku"));
    }

    #[test]
    fn ensure_logs_creates_directory_and_leaves_no_probe_file() {
        let workspace = TestWorkspace::new("ensure-logs");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());

        assert!(!layout.logs_dir().exists());
        let result = layout.ensure_logs();
        assert!(result.is_ok(), "{result:?}");
        assert!(layout.logs_dir().is_dir());

        let remaining: Vec<_> = fs::read_dir(layout.logs_dir())
            .expect("logs を読み取れません")
            .collect();
        assert!(
            remaining.is_empty(),
            "確認用ファイルが残っています: {remaining:?}"
        );
    }

    #[test]
    fn ensure_temp_creates_directory_and_leaves_no_probe_file() {
        let workspace = TestWorkspace::new("ensure-temp");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());

        assert!(!layout.temp_dir().exists());
        let result = layout.ensure_temp();
        assert!(result.is_ok(), "{result:?}");
        assert!(layout.temp_dir().is_dir());

        let remaining: Vec<_> = fs::read_dir(layout.temp_dir())
            .expect("temp を読み取れません")
            .collect();
        assert!(
            remaining.is_empty(),
            "確認用ファイルが残っています: {remaining:?}"
        );
    }

    #[test]
    fn ensure_webview2_data_creates_directory_and_leaves_no_probe_file() {
        let workspace = TestWorkspace::new("ensure-webview2");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());

        assert!(!layout.webview2_data_dir().exists());
        let result = layout.ensure_webview2_data();
        assert!(result.is_ok(), "{result:?}");
        assert!(layout.webview2_data_dir().is_dir());

        let remaining: Vec<_> = fs::read_dir(layout.webview2_data_dir())
            .expect("WebView2 を読み取れません")
            .collect();
        assert!(
            remaining.is_empty(),
            "確認用ファイルが残っています: {remaining:?}"
        );
    }

    #[test]
    fn ensure_directory_reports_failure_when_a_file_blocks_the_path() {
        let workspace = TestWorkspace::new("blocked");
        let exe_dir = workspace.path().to_path_buf();
        // "logs" という名前のファイルを先に作り、フォルダの作成を妨げる。
        fs::write(exe_dir.join("logs"), b"not a directory").expect("準備用ファイルの作成に失敗");

        let layout = Layout::from_exe_dir(exe_dir);
        let result = layout.ensure_logs();

        match result {
            Ok(_) => panic!("失敗するはずです"),
            Err(failure) => {
                assert!(failure.target.is_absolute());
                assert_eq!(failure.target, layout.logs_dir());
                assert_eq!(failure.action, DirectoryAction::Create);
                assert!(failure.os_error_code.is_some());
                assert!(!failure.required_privilege.is_empty());
                assert!(!failure.reason.is_empty());
            }
        }
    }

    #[test]
    fn purge_temp_removes_leftovers_but_keeps_temp_itself() {
        let workspace = TestWorkspace::new("purge");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());
        layout.ensure_temp().expect("temp を用意できません");

        fs::write(layout.temp_dir().join("leftover.tmp"), b"x").expect("準備に失敗");
        let sub_dir = layout.temp_dir().join("sub");
        fs::create_dir(&sub_dir).expect("準備に失敗");
        fs::write(sub_dir.join("nested.tmp"), b"y").expect("準備に失敗");

        let report = layout.purge_temp();

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.removed_entries, 3); // leftover.tmp, sub, sub/nested.tmp
        assert!(layout.temp_dir().is_dir(), "temp 自体は残るはずです");

        let remaining: Vec<_> = fs::read_dir(layout.temp_dir())
            .expect("temp を読み取れません")
            .collect();
        assert!(remaining.is_empty(), "残存物があります: {remaining:?}");
    }

    #[test]
    fn purge_temp_does_not_touch_files_outside_temp() {
        let workspace = TestWorkspace::new("purge-scope");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());
        layout.ensure_temp().expect("temp を用意できません");

        let outside_file = workspace.path().join("outside.txt");
        fs::write(&outside_file, b"keep-me").expect("準備に失敗");

        let _ = layout.purge_temp();

        assert!(outside_file.exists(), "temp の外のファイルは残るはずです");
    }

    #[test]
    fn purge_temp_removes_directory_junction_without_deleting_its_target() {
        let workspace = TestWorkspace::new("purge-junction");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());
        layout.ensure_temp().expect("temp を用意できません");

        // ジャンクションのリンク先。temp の外（workspace 直下）に置く。
        let target_dir = workspace.path().join("junction-target");
        fs::create_dir_all(&target_dir).expect("リンク先の作成に失敗");
        fs::write(target_dir.join("keep.txt"), b"keep-me").expect("リンク先ファイルの作成に失敗");

        let link_path = layout.temp_dir().join("link-to-target");

        if !create_junction(&link_path, &target_dir) {
            eprintln!(
                "ジャンクションを作成できない環境のため \
                 purge_temp_removes_directory_junction_without_deleting_its_target をスキップします"
            );
            return;
        }

        let report = layout.purge_temp();

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(report.removed_entries >= 1);
        assert!(
            !link_path.exists(),
            "ジャンクション自体は削除されるはずです"
        );
        assert!(target_dir.exists(), "リンク先ディレクトリは残るはずです");
        assert!(
            target_dir.join("keep.txt").exists(),
            "リンク先の内容は残るはずです"
        );
    }

    #[test]
    fn purge_temp_succeeds_when_temp_directory_is_missing() {
        let workspace = TestWorkspace::new("purge-missing");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());
        assert!(!layout.temp_dir().exists());

        let report = layout.purge_temp();

        assert_eq!(report.removed_entries, 0);
        assert!(report.failures.is_empty());
    }

    // 受け入れ条件: `temp` 配下がどれだけ深くても、スタックオーバーフロー（通知のない
    // 異常終了）を起こさずに清掃を完了する（`SEC-006`、`ERR-004` の趣旨、Issue #42）。
    #[test]
    fn purge_temp_removes_a_deep_directory_tree_without_recursion() {
        let workspace = TestWorkspace::new("purge-deep");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());
        layout.ensure_temp().expect("temp を用意できません");

        // 深さを増やすほどパス解決の費用が深さの二乗で増えるため、ツリーは深くしすぎず、
        // 代わりに清掃を小さいスタック（256 KiB）のスレッドで実行して検出力を確保する。
        // 再帰実装では1段ごとに `ReadDir`（内部に約 600 バイトの検索バッファを持つ）を
        // 抱えたフレームが積まれ、この大きさのスタックは数百段で尽きる。反復実装は
        // スタック消費が深さに依存しないため通過する。
        const DEPTH: usize = 1_200;
        const PURGE_STACK_BYTES: usize = 256 * 1024;

        let mut path = layout.temp_dir().to_path_buf();
        let mut created = 0usize;
        for _ in 0..DEPTH {
            path.push("d");
            if fs::create_dir(&path).is_err() {
                // パス長の上限が異なる環境では目標の深さに届かないことがある。
                // 作れたところまでを対象に検査する。
                break;
            }
            created += 1;
        }

        assert!(
            created >= 1_000,
            "検査に足りる深さを作れません（作成できた深さ: {created}）"
        );

        let purge_layout = layout.clone();
        let report = std::thread::Builder::new()
            .stack_size(PURGE_STACK_BYTES)
            .spawn(move || purge_layout.purge_temp())
            .expect("清掃用スレッドを起動できません")
            .join()
            .expect("清掃用スレッドが異常終了しました");

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.removed_entries, created);
        assert!(layout.temp_dir().is_dir(), "temp 自体は残るはずです");

        let remaining: Vec<_> = fs::read_dir(layout.temp_dir())
            .expect("temp を読み取れません")
            .collect();
        assert!(remaining.is_empty(), "残存物があります: {remaining:?}");
    }

    // 受け入れ条件: 実体のフォルダと、まだ存在しないフォルダは起動を拒否しない
    // （`SEC-009`、Issue #42）。
    #[test]
    fn ensure_runtime_folders_are_real_directories_accepts_real_and_missing_folders() {
        let workspace = TestWorkspace::new("reparse-ok");
        let layout = Layout::from_exe_dir(workspace.path().to_path_buf());

        // 4 フォルダのいずれもまだ作られていない、初回起動の状態。
        let before = layout.ensure_runtime_folders_are_real_directories();
        assert!(before.is_ok(), "{before:?}");

        layout.ensure_logs().expect("logs を用意できません");
        layout.ensure_temp().expect("temp を用意できません");
        layout
            .ensure_webview2_data()
            .expect("WebView2 を用意できません");
        fs::create_dir_all(layout.webview2_runtime_dir()).expect("WebView2Runtime の作成に失敗");

        let after = layout.ensure_runtime_folders_are_real_directories();
        assert!(after.is_ok(), "{after:?}");
    }

    // 受け入れ条件: `logs`／`temp`／`WebView2`／`WebView2Runtime` のいずれかが
    // ジャンクションへ差し替えられている場合、対象と対処を伴って起動を拒否する
    // （`SEC-009`、Issue #42）。
    #[test]
    fn ensure_runtime_folders_are_real_directories_rejects_a_junction_for_every_folder() {
        for folder_name in [
            LOGS_DIR_NAME,
            TEMP_DIR_NAME,
            WEBVIEW2_DATA_DIR_NAME,
            WEBVIEW2_RUNTIME_DIR_NAME,
        ] {
            let workspace = TestWorkspace::new("reparse-junction");
            let exe_dir = workspace.path().join("app");
            fs::create_dir_all(&exe_dir).expect("exe_dir の作成に失敗");

            // 差し替え先は導入フォルダの外に置く。`SEC-009` が問題にするのは、
            // 導入フォルダを削除しても残る場所へ書き出されることそのもの。
            let outside_dir = workspace.path().join("outside");
            fs::create_dir_all(&outside_dir).expect("リンク先の作成に失敗");

            let link_path = exe_dir.join(folder_name);
            if !create_junction(&link_path, &outside_dir) {
                eprintln!(
                    "ジャンクションを作成できない環境のため \
                     ensure_runtime_folders_are_real_directories_rejects_a_junction_for_every_folder \
                     をスキップします"
                );
                return;
            }

            let layout = Layout::from_exe_dir(exe_dir);

            match layout.ensure_runtime_folders_are_real_directories() {
                Ok(()) => panic!("「{folder_name}」の差し替えを検出できていません"),
                Err(rejection) => {
                    assert_eq!(rejection.folder_name, folder_name);
                    assert_eq!(rejection.target, link_path);
                    assert!(rejection.target.is_absolute());
                    assert!(!rejection.purpose.is_empty());
                    assert!(!rejection.remedy.is_empty());
                }
            }

            assert!(outside_dir.is_dir(), "リンク先には手を触れないはずです");
        }
    }
}
