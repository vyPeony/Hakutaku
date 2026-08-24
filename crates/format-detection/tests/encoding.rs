//! `hakutaku_format_detection` の受け入れ確認（P05-3、
//! `tasks/phase-05-log-parsing-core.md` 「文字コード」節）。
//!
//! `crate::decision` / `crate::decode` / `crate::win32` 内の単体テストが
//! 判定・デコードそれぞれの内部ロジックを個別に確認するのに対し、この
//! ファイルは**公開 API（`hakutaku_format_detection::` 経由）を通した
//! 一連の流れ**（判定 → デコード）を、`ENC-0xx` の受け入れ条件に対応付けて
//! 確認する。

use hakutaku_format_detection::{
    decode, detect_encoding, DecidedEncoding, DecodeError, DetectionRoute, EncodingDecision,
    EncodingWarningKind, ProfileEncodingSetting, ProfileSpecifiedKind, SelectedEncoding,
    UnsupportedEncoding, Utf16BomKind,
};

/// 「日本語のログです。」の CP932（Shift_JIS 系）バイト列
/// （`[System.Text.Encoding]::GetEncoding(932).GetBytes(...)` で採取）。
const JAPANESE_LOG_LINE_CP932: [u8; 18] = [
    0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA, // 日本語
    0x82, 0xCC, // の
    0x83, 0x8D, 0x83, 0x4F, // ログ
    0x82, 0xC5, 0x82, 0xB7, // です
    0x81, 0x42, // 。
];

fn expect_decided(decision: EncodingDecision) -> DecidedEncoding {
    match decision {
        EncodingDecision::Decided(decided) => decided,
        EncodingDecision::Unsupported(unsupported) => {
            panic!("Decided を期待しましたが Unsupported({unsupported:?}) でした")
        }
    }
}

// 受け入れ条件: コードページ 932 で生成した BOM なしログを、`ansi_codepage: 932`
// があれば正しく表示できる（ENC-001、ENC-004、ENC-007）。実行環境の ANSI が
// 932 かどうかに関わらず、明示指定により正しくデコードされることを確認する。
#[test]
fn ansi_codepage_932_profile_decodes_japanese_log_regardless_of_environment() {
    let profile = ProfileEncodingSetting::ansi_codepage(932);
    let decision = detect_encoding(&JAPANESE_LOG_LINE_CP932, &profile).unwrap();
    let decided = expect_decided(decision);
    assert_eq!(decided.encoding, SelectedEncoding::Windows(932));
    assert_eq!(
        decided.route,
        DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::AnsiCodepage)
    );

    let outcome = decode(&JAPANESE_LOG_LINE_CP932, &decided).unwrap();
    assert_eq!(outcome.text, "日本語のログです。");
    assert!(outcome.invalid_positions.is_empty());
}

// 受け入れ条件: `auto` で `ansi_codepage` 未指定の場合に限り、実行環境の
// Windows ANSI へフォールバックする（ENC-005）。判定経路を確認する
// （環境依存の実際の値そのものは断定しない）。
#[test]
fn auto_without_ansi_codepage_falls_back_to_environment_ansi_route() {
    // UTF-8 としても妥当な UTF-8 BOM としても解釈できない、明らかに非 UTF-8 な
    // バイト列（0xFF は UTF-8 の先頭バイトとして常に不正）。
    let bytes = [0xFF, 0x30, 0x30];
    let decision = detect_encoding(&bytes, &ProfileEncodingSetting::auto()).unwrap();
    let decided = expect_decided(decision);
    assert_eq!(decided.route, DetectionRoute::EnvironmentAnsi);
    assert!(matches!(decided.encoding, SelectedEncoding::Windows(_)));
}

// 受け入れ条件: UTF-8 BOM あり、BOM なし、任意の明示コードページについて、
// 選択された判定経路を診断情報で確認できる（ENC-005、DIAG-005）。
#[test]
fn detection_route_is_observable_for_bom_no_bom_and_explicit_codepage() {
    let with_bom = {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("ok".as_bytes());
        bytes
    };
    let decision = detect_encoding(&with_bom, &ProfileEncodingSetting::auto()).unwrap();
    assert_eq!(expect_decided(decision).route, DetectionRoute::Utf8Bom);

    let without_bom = "問題ありません".as_bytes();
    let decision = detect_encoding(without_bom, &ProfileEncodingSetting::auto()).unwrap();
    assert_eq!(
        expect_decided(decision).route,
        DetectionRoute::Utf8ValidatedNoBom
    );

    let decision = detect_encoding(
        &JAPANESE_LOG_LINE_CP932,
        &ProfileEncodingSetting::named("windows-932"),
    )
    .unwrap();
    assert_eq!(
        expect_decided(decision).route,
        DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::NamedEncoding)
    );
}

// 受け入れ条件: BOM と明示指定が矛盾する場合に警告が出て、暗黙の切り替えが
// 起きない（4.3 の暫定設計）。
#[test]
fn bom_and_explicit_setting_conflict_produces_warning_without_implicit_switch() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM。
    bytes.extend_from_slice(&JAPANESE_LOG_LINE_CP932);

    let decision = detect_encoding(&bytes, &ProfileEncodingSetting::ansi_codepage(932)).unwrap();
    let decided = expect_decided(decision);

    assert_eq!(
        decided.encoding,
        SelectedEncoding::Windows(932),
        "明示指定（ansi_codepage）が優先されるはず"
    );
    assert_eq!(decided.warnings.len(), 1);
    assert_eq!(
        decided.warnings[0].kind,
        EncodingWarningKind::BomConflictsWithExplicitSetting
    );
}

// 受け入れ条件: UTF-16 の BOM を検出すると、未対応形式として通知される
// （ENC-006）。
#[test]
fn utf16_bom_is_reported_as_unsupported() {
    let le_bytes = [0xFF, 0xFE, 0x41, 0x00];
    let decision = detect_encoding(&le_bytes, &ProfileEncodingSetting::auto()).unwrap();
    assert_eq!(
        decision,
        EncodingDecision::Unsupported(UnsupportedEncoding {
            bom: Utf16BomKind::Le
        })
    );
}

// 受け入れ条件: UTF-16 の BOM は、プロファイルで文字コードを明示指定していても
// 未対応形式として通知される（ENC-006、Issue #38）。明示指定を優先すると、
// UTF-16 の本文が「指定した文字コードで読めた」ことになり、全面的に文字化けした
// 内容が正常な表示として出てしまう。
#[test]
fn utf16_bom_is_unsupported_even_when_profile_specifies_an_encoding() {
    let le_bytes = [0xFF, 0xFE, 0x41, 0x00];
    for profile in [
        ProfileEncodingSetting::ansi_codepage(932),
        ProfileEncodingSetting::named("windows-932"),
        ProfileEncodingSetting::named("utf-8"),
    ] {
        assert_eq!(
            detect_encoding(&le_bytes, &profile).unwrap(),
            EncodingDecision::Unsupported(UnsupportedEncoding {
                bom: Utf16BomKind::Le
            }),
            "明示指定 {profile:?} でも未対応通知になるはず"
        );
    }
}

// 受け入れ条件: 明示指定（encoding 名前指定・ansi_codepage）は、UTF-8 BOM・
// BOM なし UTF-8 の妥当性確認・実行環境 ANSI のいずれよりも優先される
// （ENC-005 の「明示指定があれば最優先」。要件文書の判定順序と実装の一致を
// ここで確認する）。
#[test]
fn explicit_profile_setting_wins_over_every_auto_stage() {
    // (1) BOM なしで妥当な UTF-8 のバイト列でも、明示した CP932 が使われる。
    let valid_utf8 = "日本語のログ".as_bytes();
    let decided = expect_decided(
        detect_encoding(valid_utf8, &ProfileEncodingSetting::ansi_codepage(932)).unwrap(),
    );
    assert_eq!(decided.encoding, SelectedEncoding::Windows(932));
    assert_eq!(
        decided.route,
        DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::AnsiCodepage)
    );

    // (2) UTF-8 BOM があっても、明示した CP1252 が使われる（BOM は除去せず警告）。
    let mut bom_utf8 = vec![0xEF, 0xBB, 0xBF];
    bom_utf8.extend_from_slice(b"ok");
    let decided = expect_decided(
        detect_encoding(&bom_utf8, &ProfileEncodingSetting::named("windows-1252")).unwrap(),
    );
    assert_eq!(decided.encoding, SelectedEncoding::Windows(1252));
    assert_eq!(decided.bom_len, 0);
    assert_eq!(decided.warnings.len(), 1);

    // (3) UTF-8 として不正なバイト列でも、環境 ANSI ではなく明示指定が使われる。
    let decided = expect_decided(
        detect_encoding(
            &JAPANESE_LOG_LINE_CP932,
            &ProfileEncodingSetting::named("utf-8"),
        )
        .unwrap(),
    );
    assert_eq!(decided.encoding, SelectedEncoding::Utf8);
    assert_eq!(
        decided.route,
        DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::NamedEncoding)
    );
}

// 受け入れ条件: 0バイトのファイルと、UTF-8 BOM を書き終える前に切れた2バイトの
// ファイル（EF BB）を、判定からデコードまでパニックせずに扱える（Issue #38）。
#[test]
fn empty_and_truncated_bom_inputs_decode_without_panicking() {
    let empty_decided =
        expect_decided(detect_encoding(&[], &ProfileEncodingSetting::auto()).unwrap());
    let outcome = decode(&[], &empty_decided).unwrap();
    assert_eq!(outcome.text, "");
    assert!(outcome.invalid_positions.is_empty());

    // `EF BB` は BOM として除去しない（3バイト目が無い以上、BOM と断定できない）。
    let truncated = [0xEF, 0xBB];
    let decided =
        expect_decided(detect_encoding(&truncated, &ProfileEncodingSetting::auto()).unwrap());
    assert_eq!(decided.bom_len, 0);
    let outcome = decode(&truncated, &decided).unwrap();
    assert_eq!(outcome.text, "\u{FFFD}");
    assert_eq!(
        outcome.invalid_positions,
        vec![0],
        "壊れた BOM の先頭が不正位置として報告されるはず"
    );

    // UTF-8 BOM だけで本文が無いファイルは、BOM を除去して空文字列になる。
    let bom_only = [0xEF, 0xBB, 0xBF];
    let decided =
        expect_decided(detect_encoding(&bom_only, &ProfileEncodingSetting::auto()).unwrap());
    assert_eq!(decided.bom_len, 3);
    let outcome = decode(&bom_only, &decided).unwrap();
    assert_eq!(outcome.text, "");
    assert!(outcome.invalid_positions.is_empty());
}

// 受け入れ条件: デコードできないバイト列で、対象ファイル、位置、選択された
// 文字コードが表示され、元バイトが破棄されない（ENC-005、4.3）。ここでは
// 「位置」と「選択された文字コード」が `DecodeOutcome` から取得できること、
// および渡した `bytes` 自体は変更されない（呼び出し側が引き続き参照できる）
// ことを確認する。
#[test]
fn undecodable_bytes_report_position_and_selected_encoding_without_discarding_original() {
    let bytes = b"before\xFFafter".to_vec();
    let original = bytes.clone();

    // 0xFF は UTF-8 としても不正なため、auto 判定に任せると環境 ANSI へ
    // フォールバックしてしまう。ここでは UTF-8 の不正位置報告そのものを
    // 検証したいので、明示的に utf-8 を指定する。
    let decided =
        expect_decided(detect_encoding(&bytes, &ProfileEncodingSetting::named("utf-8")).unwrap());

    let outcome = decode(&bytes, &decided).unwrap();
    assert_eq!(outcome.selected_encoding, SelectedEncoding::Utf8);
    assert_eq!(outcome.invalid_positions, vec![6]);
    assert!(outcome.text.contains('\u{FFFD}'));

    // 呼び出し側が渡した bytes は decode によって変更されない。
    assert_eq!(bytes, original);
}

// 受け入れ条件: 存在しないコードページのエラー。
#[test]
fn decoding_with_unknown_codepage_returns_error() {
    let decided = DecidedEncoding {
        encoding: SelectedEncoding::Windows(99_999),
        route: DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::AnsiCodepage),
        bom_len: 0,
        warnings: Vec::new(),
    };
    let result = decode(b"abc", &decided);
    assert_eq!(result.unwrap_err(), DecodeError::UnknownCodepage(99_999));
}
