//! `hakutaku-diagnostics` の統合テストです。
//!
//! 受け入れ条件（`tasks/phase-01-bootstrap-webview2.md`）を
//! 直接検証します。マスキングをしないこと（`DIAG-003`、`DIAG-004`）、
//! `DIAG-005` の全項目が 1 行に含まれること、`logs` がない状態からの自動
//! 作成（`DIAG-001`、`DIAG-006`）、ローテーション（`DIAG-002`）、書き込み
//! 不可時の無効化（`DIAG-006`）、昇格プロセスの判別（`DIAG-007`）を扱います。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hakutaku_diagnostics::{
    diag_error, diag_info, Diagnostics, ProcessElevation, Record, RotationPolicy, Severity,
};

/// テストごとに衝突しない一時ディレクトリを用意します。
///
/// 依存クレートを追加できない制約があるため、`tempfile` は使わず
/// `std::env::temp_dir()` とプロセス ID・カウンター・ナノ秒時刻で
/// 一意な名前を組み立てます。テスト終了時にベストエフォートで削除します。
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "hakutaku-diagnostics-it-{label}-{}-{count}-{nanos}",
            std::process::id()
        ));
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 後片付けはベストエフォート。失敗しても他のテストへ影響させない。
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `logs` フォルダが無い状態から `open` すると、フォルダが作成され、
/// 見出し（`SEC-005` の明記を含む）とレコードが書き込まれる（`DIAG-001`、`DIAG-006`）。
#[test]
fn creates_logs_directory_and_writes_header_and_record_when_missing() {
    let temp = TempDir::new("create");
    let logs_dir = temp.path().join("logs");
    assert!(!logs_dir.exists(), "事前条件: logs はまだ存在しない");

    let (diagnostics, unavailable) = Diagnostics::open(
        &logs_dir,
        RotationPolicy::default(),
        ProcessElevation::Normal,
    );

    assert!(
        unavailable.is_none(),
        "書き込み可能な環境では失敗しないはず"
    );
    assert!(diagnostics.is_active());
    assert!(logs_dir.is_dir(), "logs フォルダが作成されている");

    diag_info!(
        diagnostics,
        module = "test::bootstrap",
        operation = "bootstrap.start",
        "起動を開始しました"
    );

    let log_path = diagnostics.log_path().expect("アクティブなら Some");
    assert_eq!(log_path, logs_dir.join("hakutaku.log"));

    let content = fs::read_to_string(log_path).expect("ログファイルを読み取れる");

    // SEC-005 の明記を含む見出しがファイル冒頭にあること。
    assert!(content.starts_with("# Hakutaku 診断ログ"));
    assert!(content.contains("SEC-005"));
    assert!(content.contains("機密データ"));

    // レコードが記録されていること。
    assert!(content.contains("bootstrap.start"));
    assert!(content.contains("起動を開始しました"));
}

/// `DIAG-005` の全項目（時刻、重要度、モジュール、操作種別、ソース ID、
/// エラーコード、内部位置）が 1 行に含まれる。`src=`・`code=` が無い場合は `-`。
#[test]
fn record_line_contains_all_diag_005_fields() {
    let temp = TempDir::new("fields");
    let logs_dir = temp.path().join("logs");
    let (diagnostics, _) = Diagnostics::open(
        &logs_dir,
        RotationPolicy::default(),
        ProcessElevation::Normal,
    );
    assert!(diagnostics.is_active());

    // すべての項目を指定した場合。
    diagnostics.log(&Record {
        severity: Severity::Warn,
        module: "bootstrap::runtime",
        operation: "runtime.resolve",
        source_id: Some("SRC-1"),
        error_code: Some("HKT-W2-0003"),
        location: "src-tauri/src/bootstrap/runtime.rs:128",
        message: "Runtime を解決できませんでした",
    });

    // source_id / error_code を省略した場合。
    diagnostics.log(&Record {
        severity: Severity::Info,
        module: "bootstrap::layout",
        operation: "layout.ensure_logs",
        source_id: None,
        error_code: None,
        location: "src-tauri/src/bootstrap/layout.rs:42",
        message: "logs フォルダを確認しました",
    });

    let log_path = diagnostics.log_path().unwrap().to_path_buf();
    let content = fs::read_to_string(&log_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    let full_line = lines
        .iter()
        .find(|line| line.contains("runtime.resolve"))
        .expect("1 行目のレコードが見つかる");
    assert!(full_line.contains(" | WARN | "));
    assert!(full_line.contains("bootstrap::runtime"));
    assert!(full_line.contains("runtime.resolve"));
    assert!(full_line.contains("src=SRC-1"));
    assert!(full_line.contains("code=HKT-W2-0003"));
    assert!(full_line.contains("proc=normal"));
    assert!(full_line.contains("at=src-tauri/src/bootstrap/runtime.rs:128"));
    assert!(full_line.contains("Runtime を解決できませんでした"));
    // タイムスタンプが RFC3339 相当（ミリ秒 + オフセット）であることの簡易確認。
    assert!(full_line.contains('T'));
    assert!(full_line.contains('+') || full_line.contains('Z'));

    let omitted_line = lines
        .iter()
        .find(|line| line.contains("layout.ensure_logs"))
        .expect("2 行目のレコードが見つかる");
    assert!(omitted_line.contains("src=-"));
    assert!(omitted_line.contains("code=-"));
    assert!(omitted_line.contains(" | INFO | "));
}

/// フルパスや `password=...` のような実値がマスキングされずそのまま
/// 記録される（`DIAG-003`、`DIAG-004`）。
#[test]
fn does_not_mask_full_paths_or_credential_like_values() {
    let temp = TempDir::new("no-mask");
    let logs_dir = temp.path().join("logs");
    let (diagnostics, _) = Diagnostics::open(
        &logs_dir,
        RotationPolicy::default(),
        ProcessElevation::Normal,
    );

    let sensitive_message =
        r"参照元: C:\Users\test\data\image001.dcm, password=hunter2, token=abcdef123456";

    diag_error!(
        diagnostics,
        module = "test::parser",
        operation = "parser.read",
        error_code = "HKT-P-0001",
        "{}",
        sensitive_message
    );

    let log_path = diagnostics.log_path().unwrap().to_path_buf();
    let content = fs::read_to_string(&log_path).unwrap();

    assert!(content.contains(r"C:\Users\test\data\image001.dcm"));
    assert!(content.contains("password=hunter2"));
    assert!(content.contains("token=abcdef123456"));
    assert!(!content.contains("***"), "伏字化されていない");
    assert!(!content.contains("[REDACTED]"), "伏字化されていない");
}

/// 小さい `RotationPolicy` を与えるとローテーションが起き、世代数の上限を
/// 超えたファイルが削除される（`DIAG-002`）。
#[test]
fn rotates_and_prunes_generations_beyond_limit() {
    let temp = TempDir::new("rotate");
    let logs_dir = temp.path().join("logs");

    // 1 レコードで確実に超える程度の小さい上限にする。
    let policy = RotationPolicy {
        max_file_bytes: 200,
        max_generations: 3,
    };
    let (diagnostics, unavailable) = Diagnostics::open(&logs_dir, policy, ProcessElevation::Normal);
    assert!(unavailable.is_none());

    // 十分な回数書き込み、複数世代のローテーションを発生させる。
    for i in 0..40 {
        diag_info!(
            diagnostics,
            module = "test::rotation",
            operation = "rotation.write",
            "ローテーション確認用のレコードです番号={}",
            i
        );
    }

    assert!(
        diagnostics.is_active(),
        "ローテーションだけでは無効化されない"
    );

    let current = logs_dir.join("hakutaku.log");
    let gen1 = logs_dir.join("hakutaku.1.log");
    let gen2 = logs_dir.join("hakutaku.2.log");
    let gen3 = logs_dir.join("hakutaku.3.log");

    assert!(current.is_file(), "現行ファイルが存在する");
    assert!(gen1.is_file(), "第1世代が存在する");
    assert!(
        gen2.is_file(),
        "第2世代が存在する（max_generations=3 は現行含め3個）"
    );
    assert!(
        !gen3.exists(),
        "max_generations=3 を超える世代（.3.log）は削除されている"
    );
}

/// `rotates_and_prunes_generations_beyond_limit` はローテーションの発生と
/// 世代削除だけを検証しており、退避後ファイルの見出し内容までは確認していない
/// （後続のレビューで特定した検証の穴）。見出し定数 `LOG_HEADER` は
/// クレート非公開のため、既存テスト（`creates_logs_directory_and_writes_header_and_record_when_missing`
/// など）と同じ代表的な部分文字列（冒頭行、マスキングしない方針、`SEC-005`）で照合する。
// 受け入れ条件: ローテーション後の新しい現行 hakutaku.log と、退避された
// hakutaku.1.log の両方の冒頭が、マスキングしない方針と SEC-005 の責任範囲の
// 明記を含む見出しになっている。どの世代のファイルを単独で開いても方針が
// 読めることを保証する（DIAG-002、SEC-005）。
#[test]
fn rotated_files_all_start_with_required_header() {
    let temp = TempDir::new("rotate-header");
    let logs_dir = temp.path().join("logs");

    // rotates_and_prunes_generations_beyond_limit と同じ小さい上限にして、
    // 現行ファイルと第1世代の両方に確実にローテーションを経由させる。
    let policy = RotationPolicy {
        max_file_bytes: 200,
        max_generations: 3,
    };
    let (diagnostics, unavailable) = Diagnostics::open(&logs_dir, policy, ProcessElevation::Normal);
    assert!(unavailable.is_none());

    for i in 0..40 {
        diag_info!(
            diagnostics,
            module = "test::rotation_header",
            operation = "rotation.write",
            "ローテーション見出し確認用のレコードです番号={}",
            i
        );
    }

    assert!(
        diagnostics.is_active(),
        "ローテーションだけでは無効化されない"
    );

    let current = logs_dir.join("hakutaku.log");
    let gen1 = logs_dir.join("hakutaku.1.log");
    assert!(
        current.is_file(),
        "ローテーション後の新しい現行ファイルが存在する"
    );
    assert!(gen1.is_file(), "退避された第1世代が存在する");

    // 現行ファイルと退避後ファイルの両方について、単独で開いても方針が
    // 分かる見出しから始まっていることを確認する。
    for path in [&current, &gen1] {
        let content = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{} を読み取れる: {error}", path.display()));
        assert!(
            content.starts_with("# Hakutaku 診断ログ"),
            "{} の冒頭が見出しで始まる",
            path.display()
        );
        assert!(
            content.contains("マスキングせずそのまま記録します"),
            "{} にマスキングしない方針の明記がある",
            path.display()
        );
        assert!(
            content.contains("SEC-005"),
            "{} に SEC-005（責任範囲）の明記がある",
            path.display()
        );
        assert!(
            content.contains("機密データ"),
            "{} に機密データが含まれ得る旨の明記がある",
            path.display()
        );
    }
}

/// 作成できないパス（既存ファイルと同名のディレクトリを要求する）を渡すと、
/// 無効な `Diagnostics` と理由が返り、`log()` を呼んでも panic しない（`DIAG-006`）。
#[test]
fn open_returns_unavailable_when_logs_dir_cannot_be_created() {
    let temp = TempDir::new("blocked");
    fs::create_dir_all(temp.path()).expect("親ディレクトリを用意できる");

    // "logs" という名前のファイルを先に作っておき、同名ディレクトリの
    // create_dir_all を失敗させる。
    let blocked_path = temp.path().join("logs");
    fs::write(&blocked_path, b"this is a file, not a directory").expect("ファイルを作成できる");

    let (diagnostics, unavailable) = Diagnostics::open(
        &blocked_path,
        RotationPolicy::default(),
        ProcessElevation::Normal,
    );

    assert!(!diagnostics.is_active());
    assert!(diagnostics.log_path().is_none());
    assert!(unavailable.is_some(), "失敗理由が返る");
    assert!(diagnostics.unavailable_reason().is_some());

    let reason = diagnostics.unavailable_reason().unwrap();
    assert_eq!(reason.target, blocked_path);
    assert!(!reason.reason.is_empty());

    // 別の保存先へフォールバックしていないことも確認する
    // （target が呼び出し時に渡したパスのままであること）。
    assert_eq!(unavailable.unwrap().target, blocked_path);

    // log() を呼んでも panic しない。
    diagnostics.log(&Record {
        severity: Severity::Error,
        module: "test::blocked",
        operation: "blocked.op",
        source_id: None,
        error_code: None,
        location: "tests/diagnostics.rs:0",
        message: "無効化されているため出力されないはず",
    });
}

/// `ProcessElevation::Elevated` のとき `proc=elevated` が記録される（`DIAG-007`）。
#[test]
fn records_elevated_process_marker() {
    let temp = TempDir::new("elevated");
    let logs_dir = temp.path().join("logs");
    let (diagnostics, _) = Diagnostics::open(
        &logs_dir,
        RotationPolicy::default(),
        ProcessElevation::Elevated,
    );

    diag_info!(
        diagnostics,
        module = "test::elevation",
        operation = "elevation.check",
        "昇格プロセスからの出力です"
    );

    let log_path = diagnostics.log_path().unwrap().to_path_buf();
    let content = fs::read_to_string(&log_path).unwrap();
    assert!(content.contains("proc=elevated"));
    assert!(!content.contains("proc=normal"));
    assert!(!content.contains("proc=unknown"));
}

/// `Diagnostics` は複数スレッドから安全に共有できる（`Sync + Send`）。
#[test]
fn diagnostics_can_be_shared_across_threads() {
    use std::sync::Arc;
    use std::thread;

    let temp = TempDir::new("threads");
    let logs_dir = temp.path().join("logs");
    let (diagnostics, _) = Diagnostics::open(
        &logs_dir,
        RotationPolicy::default(),
        ProcessElevation::Normal,
    );
    let diagnostics = Arc::new(diagnostics);

    let mut handles = Vec::new();
    for i in 0..8 {
        let diagnostics = Arc::clone(&diagnostics);
        handles.push(thread::spawn(move || {
            diag_info!(
                diagnostics,
                module = "test::threads",
                operation = "threads.write",
                "スレッド {} からの書き込み",
                i
            );
        }));
    }

    for handle in handles {
        handle.join().expect("パニックしていない");
    }

    assert!(diagnostics.is_active());
    let log_path = diagnostics.log_path().unwrap().to_path_buf();
    let content = fs::read_to_string(&log_path).unwrap();
    assert!(content.contains("threads.write"));
}

/// 見出し（`LOG_HEADER`）はクレート非公開のため、実際に 1 度開いてファイル長
/// から求めます。「見出し + N レコード分」でローテーション上限を組み立てる
/// テストが、見出しの文言変更で壊れないようにするためです。
fn header_len_bytes() -> u64 {
    let temp = TempDir::new("header-len");
    let logs_dir = temp.path().join("logs");
    let (diagnostics, unavailable) = Diagnostics::open(
        &logs_dir,
        RotationPolicy::default(),
        ProcessElevation::Normal,
    );
    assert!(unavailable.is_none());
    assert!(diagnostics.is_active());
    fs::metadata(logs_dir.join("hakutaku.log"))
        .expect("見出しだけのファイルが存在する")
        .len()
}

// 受け入れ条件: ローテーション判定は自プロセスの書き込み量ではなく実ファイル
// サイズで行う。他プロセスの追記でファイルが max_file_bytes を超えた場合も、
// 次の書き込みで退避される（DIAG-002、DIAG-007、Issue #41）。
#[test]
fn rotates_on_actual_file_size_including_other_process_appends() {
    let temp = TempDir::new("rotate-actual-size");
    let logs_dir = temp.path().join("logs");

    // 自プロセスのレコードだけでは到達しない上限にする。
    let policy = RotationPolicy {
        max_file_bytes: header_len_bytes() + 4096,
        max_generations: 3,
    };
    let (diagnostics, unavailable) = Diagnostics::open(&logs_dir, policy, ProcessElevation::Normal);
    assert!(unavailable.is_none());

    diag_info!(
        diagnostics,
        module = "test::multiprocess",
        operation = "multiprocess.write",
        "自プロセスの 1 件目"
    );

    let current = logs_dir.join("hakutaku.log");
    let gen1 = logs_dir.join("hakutaku.1.log");
    assert!(
        !gen1.exists(),
        "自プロセス分だけでは上限に届かず、まだ退避されない"
    );

    // 別プロセスによる追記を模擬する（同じファイルを追記モードで開いて書く）。
    const OTHER_PROCESS_MARKER: &str = "OTHER-PROCESS-APPEND";
    {
        use std::io::Write as _;
        let mut other = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current)
            .expect("別プロセスと同じ追記モードで開ける");
        let padding = "x".repeat(8192);
        writeln!(other, "{OTHER_PROCESS_MARKER}{padding}").expect("追記できる");
        other.flush().expect("フラッシュできる");
    }

    diag_info!(
        diagnostics,
        module = "test::multiprocess",
        operation = "multiprocess.write",
        "自プロセスの 2 件目"
    );

    assert!(
        diagnostics.is_active(),
        "ローテーションだけでは無効化されない"
    );
    assert!(
        gen1.is_file(),
        "他プロセスの追記を含む実サイズで上限を超えたため退避されている"
    );

    let rotated = fs::read_to_string(&gen1).expect("退避後ファイルを読み取れる");
    assert!(
        rotated.contains(OTHER_PROCESS_MARKER),
        "他プロセスの追記は退避後ファイルへ残る"
    );

    let content = fs::read_to_string(&current).expect("新しい現行ファイルを読み取れる");
    assert!(content.starts_with("# Hakutaku 診断ログ"));
    assert!(
        !content.contains(OTHER_PROCESS_MARKER),
        "新しい現行ファイルは見出しから始まり、退避前の内容を含まない"
    );
}

// 受け入れ条件: 退避（rename・削除）に失敗しても、その直前に書いたレコードは
// 失われず、panic せずに理由を取得できる（DIAG-002、DIAG-006、Issue #41）。
// 判定を書き込みの後に行うため、レコードの記録が退避の成否に依存しない。
#[test]
fn keeps_the_record_written_before_a_failed_rotation_and_reports_the_reason() {
    let temp = TempDir::new("rotate-failure");
    let logs_dir = temp.path().join("logs");
    fs::create_dir_all(&logs_dir).expect("logs を先に用意できる");

    // 退避上限を超える世代（max_generations=3 なら hakutaku.2.log）を
    // ディレクトリとして作り、fs::remove_file を失敗させる。ハンドル保持による
    // rename 失敗（Windows 固有で単体テストからは再現しにくい）と同じ経路
    // （rotate_generations の Err）へ入る。
    fs::create_dir(logs_dir.join("hakutaku.2.log")).expect("同名ディレクトリを作れる");

    let policy = RotationPolicy {
        max_file_bytes: 1,
        max_generations: 3,
    };
    let (diagnostics, unavailable) = Diagnostics::open(&logs_dir, policy, ProcessElevation::Normal);
    assert!(unavailable.is_none(), "開くところまでは成功する");
    assert!(diagnostics.is_active());

    const MARKER: &str = "退避に失敗しても残るレコード";
    diag_info!(
        diagnostics,
        module = "test::rotate_failure",
        operation = "rotate_failure.write",
        "{MARKER}"
    );

    let current = logs_dir.join("hakutaku.log");
    let content = fs::read_to_string(&current).expect("現行ファイルを読み取れる");
    assert!(
        content.contains(MARKER),
        "退避に失敗しても、直前に書いたレコードは書き込み済みで失われない"
    );

    // DIAG-006: 退避に失敗した時点で無効化し、理由を取得できるようにする。
    assert!(!diagnostics.is_active());
    assert!(diagnostics.log_path().is_none());
    let reason = diagnostics
        .unavailable_reason()
        .expect("無効化の理由を取得できる");
    assert!(!reason.reason.is_empty());

    // 無効化後も呼び出し側は継続できる（panic しない）。
    diag_error!(
        diagnostics,
        module = "test::rotate_failure",
        operation = "rotate_failure.write",
        "無効化後は出力されない"
    );
    let after = fs::read_to_string(&current).expect("現行ファイルを読み取れる");
    assert_eq!(content, after, "無効化後は追記されない");
}

// 受け入れ条件: keep_generations（max_generations）が 1 のときは世代退避を
// 行わず、現行ファイルだけを保持する（DIAG-002、CFG-020 の分岐の現行挙動固定）。
#[test]
fn single_generation_policy_keeps_only_the_current_file() {
    let temp = TempDir::new("single-generation");
    let logs_dir = temp.path().join("logs");

    // 見出し + 2 レコード程度で上限に達する大きさにする。
    let policy = RotationPolicy {
        max_file_bytes: header_len_bytes() + 400,
        max_generations: 1,
    };
    let (diagnostics, unavailable) = Diagnostics::open(&logs_dir, policy, ProcessElevation::Normal);
    assert!(unavailable.is_none());

    for index in 0..20 {
        diag_info!(
            diagnostics,
            module = "test::single_generation",
            operation = "single_generation.write",
            "世代1件の確認用レコード{:03}",
            index
        );
    }

    assert!(
        diagnostics.is_active(),
        "ローテーションだけでは無効化されない"
    );

    let current = logs_dir.join("hakutaku.log");
    assert!(current.is_file(), "現行ファイルは存在する");
    assert!(
        !logs_dir.join("hakutaku.1.log").exists(),
        "max_generations=1 では退避ファイルを作らない"
    );

    let content = fs::read_to_string(&current).expect("現行ファイルを読み取れる");
    assert!(content.starts_with("# Hakutaku 診断ログ"));
    assert!(
        !content.contains("レコード000"),
        "上限に達した時点で古い内容は退避されず削除される"
    );
}

// 受け入れ条件: 1 レコードが max_file_bytes を超える場合でも、そのレコードは
// 丸ごと記録され、書き込みの後にローテーションされる。記録の欠落や無限の
// ローテーションは起きない（DIAG-002、Issue #41）。
#[test]
fn record_larger_than_max_file_bytes_is_written_then_rotated() {
    let temp = TempDir::new("oversized-record");
    let logs_dir = temp.path().join("logs");

    // 見出しだけで既に超える極端な上限。
    let policy = RotationPolicy {
        max_file_bytes: 64,
        max_generations: 3,
    };
    let (diagnostics, unavailable) = Diagnostics::open(&logs_dir, policy, ProcessElevation::Normal);
    assert!(unavailable.is_none());

    let long_message = format!("OVERSIZED-1-{}", "あ".repeat(2000));
    diag_info!(
        diagnostics,
        module = "test::oversized",
        operation = "oversized.write",
        "{}",
        long_message
    );
    diag_info!(
        diagnostics,
        module = "test::oversized",
        operation = "oversized.write",
        "OVERSIZED-2"
    );

    assert!(
        diagnostics.is_active(),
        "1 レコードが上限を超えても無効化しない"
    );

    let current = fs::read_to_string(logs_dir.join("hakutaku.log")).expect("現行を読み取れる");
    let gen1 = fs::read_to_string(logs_dir.join("hakutaku.1.log")).expect("第1世代を読み取れる");
    let gen2 = fs::read_to_string(logs_dir.join("hakutaku.2.log")).expect("第2世代を読み取れる");

    // 書き込みの後に判定するため、現行ファイルは見出しだけの状態になる。
    assert!(current.starts_with("# Hakutaku 診断ログ"));
    assert!(!current.contains("OVERSIZED-"));
    assert!(gen1.contains("OVERSIZED-2"), "2 件目は第1世代にある");
    assert!(
        gen2.contains(&long_message),
        "上限を超える 1 レコードも切り詰められず丸ごと残る"
    );
}

// 受け入れ条件: 本文に区切り文字 ` | ` が含まれてもエスケープせず記録し、
// 先頭から 8 回の分割で 9 個目が本文全体になる（DIAG-003、DIAG-004、
// crates/diagnostics/src/lib.rs のモジュール doc コメント、Issue #41）。
#[test]
fn message_containing_the_field_separator_is_recorded_without_escaping() {
    let temp = TempDir::new("separator");
    let logs_dir = temp.path().join("logs");
    let (diagnostics, _) = Diagnostics::open(
        &logs_dir,
        RotationPolicy::default(),
        ProcessElevation::Normal,
    );

    let message = r"参照元: C:\Device\Logs\a.log | level=WARN | msg=区切りを含む本文";
    diag_error!(
        diagnostics,
        module = "test::separator",
        operation = "separator.write",
        error_code = "HKT-W2-0007",
        "{}",
        message
    );

    let log_path = diagnostics.log_path().unwrap().to_path_buf();
    let content = fs::read_to_string(&log_path).unwrap();
    assert!(
        content.contains(message),
        "本文は加工されずそのまま記録される"
    );

    let line = content
        .lines()
        .find(|line| line.contains("separator.write"))
        .expect("レコード行が見つかる");
    let fields: Vec<&str> = line.splitn(9, " | ").collect();
    assert_eq!(fields.len(), 9, "欄は 9 個で、本文が最後尾の欄");
    assert_eq!(fields[8], message);
    assert_eq!(fields[1], "ERROR");
    assert_eq!(fields[5], "code=HKT-W2-0007");
    assert!(
        line.split(" | ").count() > 9,
        "すべての区切りで分割すると欄数が変わるため、固定欄数での分割が必要"
    );
}
