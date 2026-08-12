//! P05-6 試験データ一式（`tasks/phase-05-log-parsing-core.md` 作業項目8）。
//!
//! `crates/core-services::load_file_into_registry`（読み込み〜表示集合登録までの
//! 統合パイプライン）を、実際のファイル読み込みから通しで検証します。
//!
//! # 合成データのみ（実データ禁止）
//!
//! `tests/fixtures/` 配下のログはすべてこのテストのために作成した合成データ
//! です（実運用のログではありません）。CP932・CP1252・不正バイト列は
//! テキストとしてリポジトリへコミットできないため（`git` 管理下で意図しない
//! 改行・エンコーディング変換を受けるおそれがあるうえ、`fixtures` ディレクトリに
//! バイナリを置かない方針のため）、このファイル内でバイト列を直接組み立てて
//! 一時ファイルへ書き出しています。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// `tests/fixtures/` 配下のファイルへの絶対パスを返します。
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// CP932・CP1252・不正バイト列のような、テキストとしてコミットできない入力を
/// テストコード内で組み立てて書き出すための一時ファイルです。
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn write_bytes(label: &str, contents: &[u8]) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "hakutaku-core-services-fixtures-test-{label}-{}-{count}-{nanos}.log",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("テスト用ファイルを作成できません");
        TempFile { path }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// ファイルを読み込み、表示集合とその全項目（先頭から `max_items` 件）を返す
/// ヘルパーです。
fn load(
    path: &Path,
    profiles: &[hakutaku_config::LogProfileConfig],
) -> (
    hakutaku_core::DisplaySetHandle,
    hakutaku_core::LoadSummary,
    Vec<hakutaku_core::ItemDto>,
) {
    let mut registry = hakutaku_core::DisplaySetRegistry::new();
    let (handle, summary) = hakutaku_core::load_file_into_registry(
        &mut registry,
        path,
        "test.log".to_string(),
        profiles,
    )
    .expect("読み込みは成功するはず");

    let response = registry
        .fetch_range(
            handle.display_set_id,
            hakutaku_core::RangeRequest {
                start: 0,
                max_items: 100,
                expected_generation: handle.generation,
            },
        )
        .expect("範囲取得は成功するはず");

    (handle, summary, response.items)
}

// ---------------------------------------------------------------------
// 6書式（LOG-DT-001〜006）。
// ---------------------------------------------------------------------

// 受け入れ条件: LOG-DT-001（ミリ秒精度）が解析でき、比較キー・表示が仕様の
// 表と一致する。
#[test]
fn log_dt_001_fixture_parses_with_millisecond_precision() {
    let (handle, summary, items) = load(&fixture("log_dt_001.log"), &[]);

    assert_eq!(handle.total_items, 2);
    assert_eq!(summary.detected_datetime_format, Some("LOG-DT-001"));
    assert!(!summary.fell_back_to_raw_display);
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23.456")
    );
    assert_eq!(
        items[1].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:24.001")
    );
    // LOG-004: ログレベルの有無に関わらず解析が成立する（現状は常に付随情報なし）。
    assert_eq!(&*items[0].raw_text, "2026/07/28 15:12:23.456 起動しました");
}

// 受け入れ条件: LOG-DT-002（`-` 日付区切り・`:` ミリ秒区切り）が解析できる。
#[test]
fn log_dt_002_fixture_parses_with_millisecond_precision() {
    let (handle, summary, items) = load(&fixture("log_dt_002.log"), &[]);

    assert_eq!(handle.total_items, 2);
    assert_eq!(summary.detected_datetime_format, Some("LOG-DT-002"));
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23.456")
    );
}

// 受け入れ条件: LOG-DT-003（1/100秒精度）が解析でき、`.45` が450ミリ秒相当の
// 精度のまま表示される（LOG-025）。
#[test]
fn log_dt_003_fixture_parses_with_centisecond_precision() {
    let (handle, summary, items) = load(&fixture("log_dt_003.log"), &[]);

    assert_eq!(handle.total_items, 2);
    assert_eq!(summary.detected_datetime_format, Some("LOG-DT-003"));
    // LOG-024: 表示は元の精度（1/100秒2桁）のまま、450ミリ秒に書き換わって
    // 見えない。
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23.45")
    );
}

// 受け入れ条件: LOG-DT-005（秒精度、ログレベル相当表記なし）が解析できる。
#[test]
fn log_dt_005_fixture_parses_with_second_precision() {
    let (handle, summary, items) = load(&fixture("log_dt_005.log"), &[]);

    assert_eq!(handle.total_items, 2);
    assert_eq!(summary.detected_datetime_format, Some("LOG-DT-005"));
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23")
    );
    // LOG-016・LOG-012: 記録された時刻がそのまま使われる（補正なし）。
    assert_eq!(
        items[1].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:24")
    );
}

// 受け入れ条件: LOG-DT-006（分精度）が解析でき、`15:12` が `15:12:00.000` に
// 書き換わって見えない（LOG-024）。
#[test]
fn log_dt_006_fixture_parses_with_minute_precision() {
    let (handle, summary, items) = load(&fixture("log_dt_006.log"), &[]);

    assert_eq!(handle.total_items, 2);
    assert_eq!(summary.detected_datetime_format, Some("LOG-DT-006"));
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12")
    );
}

// 受け入れ条件: LOG-DT-004 のみで構成されたファイルは、自動判定では常に
// LOG-DT-005 とも同時に成立するため（`crates/parser/src/datetime.rs` の設計。
// `crates/core-services/src/loader.rs` の doc コメント「既知の制約」を参照）、
// 曖昧判定となり日時未解析の生表示へ退避する（LOG-022）。このフィクスチャは
// 「曖昧な日時（HH:mm:ss:SS）」の試験データも兼ねる。
#[test]
fn log_dt_004_fixture_is_ambiguous_and_falls_back_to_raw_display() {
    let (handle, summary, items) = load(&fixture("log_dt_004_ambiguous.log"), &[]);

    assert!(summary.fell_back_to_raw_display);
    assert_eq!(summary.detected_datetime_format, None);
    // 生表示では1行=1項目のまま（結合しない）。
    assert_eq!(handle.total_items, 2);
    assert_eq!(items[0].timestamp_display, None);
    assert_eq!(&*items[0].raw_text, "2026/07/28 15:12:23:45 一行目");
    assert_eq!(&*items[1].raw_text, "2026/07/28 15:12:24:99 二行目");
}

// 受け入れ条件: 同じファイルでも、プロファイルで
// `datetime_format: LOG-DT-004` を明示すれば自動判定を行わずに解析される。
// 元の精度（1/100秒2桁）はそのまま表示される（LOG-024・LOG-025）。
#[test]
fn log_dt_004_fixture_parses_when_the_profile_specifies_the_format() {
    let profile = hakutaku_config::LogProfileConfig {
        name: "dt-004".to_string(),
        path_pattern: fixture("log_dt_004_ambiguous.log")
            .to_string_lossy()
            .into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Auto,
        ansi_codepage: None,
        datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt004,
    };

    let (handle, summary, items) = load(&fixture("log_dt_004_ambiguous.log"), &[profile]);

    assert!(
        !summary.fell_back_to_raw_display,
        "書式を明示したので曖昧判定による退避は起きない"
    );
    assert_eq!(summary.detected_datetime_format, Some("LOG-DT-004"));
    assert_eq!(summary.profile_resolution_route, "絶対パス完全一致");
    assert_eq!(handle.total_items, 2);
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23.45")
    );
    assert_eq!(
        items[1].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:24.99")
    );
}

// 受け入れ条件: 明示指定は glob 一致のプロファイルでも効く
// （絶対パス完全一致だけの機能ではない）。
#[test]
fn log_dt_004_fixture_parses_when_a_glob_profile_specifies_the_format() {
    let profile = hakutaku_config::LogProfileConfig {
        name: "dt-004-glob".to_string(),
        path_pattern: fixture("*.log").to_string_lossy().into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Auto,
        ansi_codepage: None,
        datetime_format: hakutaku_config::DateTimeFormatSetting::LogDt004,
    };

    let (_handle, summary, items) = load(&fixture("log_dt_004_ambiguous.log"), &[profile]);

    assert_eq!(summary.profile_resolution_route, "glob 一致");
    assert_eq!(summary.detected_datetime_format, Some("LOG-DT-004"));
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23.45")
    );
}

// ---------------------------------------------------------------------
// 文字コード（UTF-8 BOM あり／なし）。
// ---------------------------------------------------------------------

// 受け入れ条件: UTF-8 BOM ありのログが判定され、BOM が除去された原文が表示
// される（ENC-003・ENC-005 第1段階）。
#[test]
fn utf8_bom_fixture_is_detected_and_bom_is_stripped_from_raw_text() {
    let (_handle, summary, items) = load(&fixture("utf8_bom.log"), &[]);

    assert_eq!(summary.encoding_route, "UTF-8 BOM");
    assert_eq!(summary.selected_encoding, "utf-8");
    assert_eq!(
        &*items[0].raw_text,
        "2026/07/28 15:12:23.456 UTF-8 BOMありの日本語ログです"
    );
    assert!(
        !items[0].raw_text.starts_with('\u{FEFF}'),
        "BOM 文字が原文に残っていないはず"
    );
}

// 受け入れ条件: UTF-8 BOM なしのログが妥当性確認により判定される（ENC-005
// 第2段階）。
#[test]
fn utf8_no_bom_fixture_is_detected_via_validity_check() {
    let (_handle, summary, _items) = load(&fixture("utf8_no_bom.log"), &[]);

    assert_eq!(summary.encoding_route, "UTF-8（BOM無し・妥当性確認）");
    assert_eq!(summary.selected_encoding, "utf-8");
}

// ---------------------------------------------------------------------
// 継続行（LOG-014）。
// ---------------------------------------------------------------------

// 受け入れ条件: 日時なし継続行が直前の日時付き行と一つの論理ログ項目になり、
// 改行が保たれる（LOG-014）。
#[test]
fn continuation_lines_fixture_merges_into_one_logical_item_preserving_newlines() {
    let (handle, _summary, items) = load(&fixture("continuation_lines.log"), &[]);

    // 4行 → 2論理項目（1行目+継続行2行が1件、4行目が1件）。
    assert_eq!(handle.total_items, 2);
    assert_eq!(
        &*items[0].raw_text,
        "2026/07/28 15:12:23.456 一行目の本文\n  詳細情報その1（継続行）\n  詳細情報その2（継続行）"
    );
    assert_eq!(
        items[0].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23.456"),
        "継続行を結合した項目の日時は先頭行のもの"
    );
    // 継続行を含む項目の行番号は先頭行（日時付き行）の行番号。
    assert_eq!(items[0].source_line_number, 1);
    assert_eq!(&*items[1].raw_text, "2026/07/28 15:12:24.000 二行目");
}

// 受け入れ条件: ファイル先頭の日時なし行が破棄されず、日時未確定の生データと
// して扱われる（LOG-014）。
#[test]
fn leading_lines_without_datetime_fixture_are_kept_as_independent_raw_items() {
    let (handle, _summary, items) = load(&fixture("leading_lines_without_datetime.log"), &[]);

    assert_eq!(
        handle.total_items, 3,
        "先頭の2行は破棄されず独立した項目になる"
    );
    assert_eq!(&*items[0].raw_text, "起動準備中です");
    assert_eq!(items[0].timestamp_display, None);
    assert_eq!(&*items[1].raw_text, "初期化しています");
    assert_eq!(items[1].timestamp_display, None);
    assert_eq!(
        items[2].timestamp_display.as_deref(),
        Some("2026-07-28T15:12:23.456")
    );
}

// ---------------------------------------------------------------------
// 区切りの不均一・ログレベルなし（LOG-003・LOG-004）。
// ---------------------------------------------------------------------

// 受け入れ条件: 空白数が一定でない区切り（複数空白・タブ）でも解析が成立する
// （LOG-003）。日時以降は原文のまま保持される。
#[test]
fn inconsistent_whitespace_fixture_parses_regardless_of_separator_width() {
    let (handle, _summary, items) = load(&fixture("inconsistent_whitespace.log"), &[]);

    assert_eq!(handle.total_items, 3);
    assert_eq!(
        &*items[0].raw_text,
        "2026/07/28 15:12:23.456    多めの空白の後に本文"
    );
    assert_eq!(&*items[1].raw_text, "2026/07/28 15:12:24.000 通常の空白1個");
    assert_eq!(
        &*items[2].raw_text,
        "2026/07/28 15:12:25.000\tタブ区切りの本文"
    );
    for item in &items {
        assert!(item.timestamp_display.is_some(), "全行が解析できるはず");
    }
}

// ---------------------------------------------------------------------
// 文字コード（CP932・CP1252・不正バイト列）。
// テキストとしてコミットできないため、ここでバイト列を組み立てる。
// ---------------------------------------------------------------------

// 受け入れ条件: コードページ 932 で生成した BOM なしログが、`ansi_codepage: 932`
// 指定により正しくデコードされる（ENC-001・ENC-004・ENC-007。実行環境の既定
// ANSI が 932 かどうかに関わらず、明示指定により再現性を持つ）。
#[test]
fn cp932_profile_decodes_japanese_text_regardless_of_environment_ansi() {
    // ASCII の日時プレフィックスに続けて、「日本語」の CP932 バイト列を置く
    // （`crates/format-detection/src/win32.rs` のテストと同じ既知のバイト列）。
    let mut bytes = b"2026/07/28 15:12:23.456 ".to_vec();
    bytes.extend_from_slice(&[0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA]); // 日本語
    let file = TempFile::write_bytes("cp932", &bytes);

    let profile = hakutaku_config::LogProfileConfig {
        name: "cp932".to_string(),
        path_pattern: file.path.to_string_lossy().into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Auto,
        ansi_codepage: Some(932),
        datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
    };

    let (_handle, summary, items) = load(&file.path, &[profile]);

    assert_eq!(summary.selected_encoding, "windows-932");
    assert_eq!(summary.encoding_route, "プロファイル指定（ansi_codepage）");
    assert!(summary.decode_invalid_positions.is_empty());
    assert_eq!(&*items[0].raw_text, "2026/07/28 15:12:23.456 日本語");
}

// 受け入れ条件: コードページ 1252（西欧言語）で生成したログが `encoding:
// windows-1252` 指定で正しくデコードされる。
#[test]
fn cp1252_profile_decodes_western_european_text() {
    let mut bytes = b"2026/07/28 15:12:23.456 ".to_vec();
    // "café" の CP1252（é = 0xE9）。
    bytes.extend_from_slice(&[b'c', b'a', b'f', 0xE9]);
    let file = TempFile::write_bytes("cp1252", &bytes);

    let profile = hakutaku_config::LogProfileConfig {
        name: "cp1252".to_string(),
        path_pattern: file.path.to_string_lossy().into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Named("windows-1252".to_string()),
        ansi_codepage: None,
        datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
    };

    let (_handle, summary, items) = load(&file.path, &[profile]);

    assert_eq!(summary.selected_encoding, "windows-1252");
    assert_eq!(&*items[0].raw_text, "2026/07/28 15:12:23.456 café");
}

// 受け入れ条件: デコードできないバイト列で、対象ファイル・位置・選択された
// 文字コードがメタデータとして返り、元バイトが破棄されない（読み込み自体は
// 失敗させない）。UTF-8 経路での確認。
#[test]
fn undecodable_utf8_bytes_are_reported_without_failing_the_load() {
    let mut bytes = b"2026/07/28 15:12:23.456 OK:".to_vec();
    bytes.push(0xFF); // 単独では不正な UTF-8 バイト。
    bytes.extend_from_slice(b":END");
    let file = TempFile::write_bytes("invalid-utf8", &bytes);

    // 実行環境の既定 ANSI に左右されないよう UTF-8 を明示する。
    let profile = hakutaku_config::LogProfileConfig {
        name: "utf8-explicit".to_string(),
        path_pattern: file.path.to_string_lossy().into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Named("utf-8".to_string()),
        ansi_codepage: None,
        datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
    };

    let (_handle, summary, items) = load(&file.path, &[profile]);

    assert_eq!(summary.selected_encoding, "utf-8");
    assert!(!summary.decode_invalid_positions.is_empty());
    assert!(!summary.decode_invalid_positions_truncated);
    // 不正バイトは置換文字へ変換されつつ、前後の妥当な部分は保たれる。
    assert!(items[0].raw_text.starts_with("2026/07/28 15:12:23.456 OK:"));
    assert!(items[0].raw_text.ends_with(":END"));
}

// 受け入れ条件: デコードできないバイト列（Windows コードページ経路）でも、
// 位置と選択された文字コードが返り、読み込み自体は失敗しない。
#[test]
fn undecodable_cp932_bytes_are_reported_without_failing_the_load() {
    let mut bytes = b"2026/07/28 15:12:23.456 OK:".to_vec();
    bytes.extend_from_slice(&[0x81, 0x30]); // CP932 として不正な組。
    bytes.extend_from_slice(b":END");
    let file = TempFile::write_bytes("invalid-cp932", &bytes);

    let profile = hakutaku_config::LogProfileConfig {
        name: "cp932".to_string(),
        path_pattern: file.path.to_string_lossy().into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Auto,
        ansi_codepage: Some(932),
        datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
    };

    let (_handle, summary, items) = load(&file.path, &[profile]);

    assert_eq!(summary.selected_encoding, "windows-932");
    assert!(!summary.decode_invalid_positions.is_empty());
    assert!(items[0].raw_text.starts_with("2026/07/28 15:12:23.456 OK:"));
    assert!(items[0].raw_text.ends_with(":END"));
}

// 受け入れ条件: UTF-16 の BOM を検出すると未対応形式として通知される
// （ENC-006）。読み込み自体が失敗する数少ない経路。
#[test]
fn utf16_bom_bytes_are_rejected_as_unsupported() {
    let bytes: Vec<u8> = vec![0xFF, 0xFE, b'a', 0x00, b'\n', 0x00];
    let file = TempFile::write_bytes("utf16", &bytes);
    let mut registry = hakutaku_core::DisplaySetRegistry::new();

    let error = hakutaku_core::load_file_into_registry(
        &mut registry,
        &file.path,
        "test.log".to_string(),
        &[],
    )
    .expect_err("UTF-16 は未対応のはず（ENC-006）");

    assert!(matches!(
        error,
        hakutaku_core::LoadFileError::UnsupportedEncoding(_)
    ));
    assert!(registry.is_empty(), "失敗時は表示集合を登録しないはず");
}

// ---------------------------------------------------------------------
// ファイルごとに異なるプロファイル（LOG-005）。
// ---------------------------------------------------------------------

// 受け入れ条件: ファイルごとに異なるプロファイルを適用できる。同じ実行の中で
// UTF-8 固定ファイルと CP932 固定ファイルをそれぞれ正しいプロファイルで開く。
#[test]
fn different_fixtures_apply_different_profiles_in_the_same_run() {
    let utf8_profile = hakutaku_config::LogProfileConfig {
        name: "utf8".to_string(),
        path_pattern: fixture("log_dt_001.log").to_string_lossy().into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Named("utf-8".to_string()),
        ansi_codepage: None,
        datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
    };

    let mut cp932_bytes = b"2026/07/28 15:12:23.456 ".to_vec();
    cp932_bytes.extend_from_slice(&[0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA]); // 日本語
    let cp932_file = TempFile::write_bytes("per-file-profile-cp932", &cp932_bytes);
    let cp932_profile = hakutaku_config::LogProfileConfig {
        name: "cp932".to_string(),
        path_pattern: cp932_file.path.to_string_lossy().into_owned(),
        priority: 0,
        encoding: hakutaku_config::EncodingSetting::Auto,
        ansi_codepage: Some(932),
        datetime_format: hakutaku_config::DateTimeFormatSetting::Auto,
    };

    let profiles = vec![utf8_profile, cp932_profile];

    let (_h1, summary1, items1) = load(&fixture("log_dt_001.log"), &profiles);
    assert_eq!(summary1.selected_encoding, "utf-8");
    assert_eq!(&*items1[0].raw_text, "2026/07/28 15:12:23.456 起動しました");

    let (_h2, summary2, items2) = load(&cp932_file.path, &profiles);
    assert_eq!(summary2.selected_encoding, "windows-932");
    assert_eq!(&*items2[0].raw_text, "2026/07/28 15:12:23.456 日本語");
}
