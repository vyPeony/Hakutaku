//! Win32 API 呼び出しの実装本体です（Windows 専用）。
//!
//! 実行環境の ANSI コードページ取得（`GetACP`）、コードページの存在確認
//! （`GetCPInfoExW`）、コードページ変換（`MultiByteToWideChar`）を、`windows`
//! クレート経由で直接呼び出します。純粋な判定ロジック（`crate::decision`、
//! `crate::bom`）とは独立したモジュールに分離し、Windows に依存しない部分を
//! 任意のプラットフォームでテストできるようにしています
//! （`crates/memory-accounting/src/private_usage.rs` と同じ分離方針）。

use windows::Win32::Globalization::{
    GetACP, GetCPInfoExW, MultiByteToWideChar, CPINFOEXW, MB_ERR_INVALID_CHARS,
    MULTI_BYTE_TO_WIDE_CHAR_FLAGS,
};

use crate::decode::{DecodeError, RawDecodeResult, MAX_INVALID_POSITIONS};

/// Windows コードページのデコードで、不正位置をチャンク単位の近似で特定する
/// 際のチャンクサイズ（バイト）。
///
/// # 位置特定の粒度（既知の限界）
///
/// `MultiByteToWideChar` はバイト単位の不正位置を返さないため、`bytes` を
/// このバイト数ごとのチャンクへ分割し、チャンクごとに厳密モード
/// （`MB_ERR_INVALID_CHARS`）で変換を試みて、失敗したチャンクの先頭オフセット
/// を不正位置として報告します。**バイト単位の厳密な位置特定は行いません**
/// （`tasks/phase-05-log-parsing-core.md` 作業項目4「位置特定の粒度はチャンク
/// 単位の近似でもよい」を採用した暫定設計）。
///
/// したがって、この経路が報告する位置は**常にこの定数の倍数**（0、4096、8192…）
/// であり、1チャンク内に不正バイトがいくつあっても1件しか報告されません。
/// UTF-8 経路（`crate::decode` の `decode_utf8_lossy_with_positions`）は
/// 標準ライブラリの判定に基づくバイト単位の正確な位置を報告するため、**同じ
/// [`crate::DecodeOutcome::invalid_positions`] でも経路によって粒度が異なります**。
/// 呼び出し側がこの一覧を利用者へ表示する際は、Windows コードページ経路の値を
/// バイト単位の正確な位置として扱わないでください（Issue #38 で明文化。粒度の
/// 統一そのものは行っていません）。
///
/// CP932 のような DBCS（2バイト文字）コードページでは、チャンク境界がリード
/// バイトとトレイルバイトの間をちょうど分断した場合、実際には正当な2バイト
/// 文字であっても前後どちらかのチャンクが「不正」と判定され得ます。この場合、
/// 報告される位置は実際の不正バイトの位置からチャンク境界1つ分ずれることが
/// あります。ログの1行が数キロバイトを超えることは通常ないため、実運用上の
/// 影響は小さいと見込みますが、この近似は解消していません。
pub const WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES: usize = 4096;

/// 実行環境の既定 ANSI コードページ（`GetACP`）を取得します（`ENC-005` 第4段階）。
pub(crate) fn environment_ansi_codepage() -> u32 {
    // SAFETY: GetACP は引数を取らず、現在のシステム既定 ANSI コードページ番号を
    // 返すだけの単純な問い合わせであり、失敗しない（Win32 API の契約）。
    unsafe { GetACP() }
}

/// `codepage` が実行環境で使用可能かを確認します（`GetCPInfoExW`）。
///
/// デコード時の判定（[`decode_windows_codepage`]）に加えて、設定の起動時検証
/// （`crates/config` の `ansi_codepage`。`CFG-016`、Issue #39）も
/// [`crate::codepage_available`] 経由でこの判定を使います。
pub(crate) fn codepage_exists(codepage: u32) -> bool {
    let mut info = CPINFOEXW::default();
    // SAFETY: info はこのスタックフレーム上の有効な CPINFOEXW への可変参照で
    // あり、GetCPInfoExW はその範囲内にしか書き込まない（Win32 API の契約）。
    // dwFlags は MSDN の仕様上、常に 0（予約値）を渡す。
    let result = unsafe { GetCPInfoExW(codepage, 0, &mut info) };
    result.is_ok()
}

/// `flags` を使って `bytes` を `codepage` から UTF-16 へ変換します。
///
/// 失敗（不正なバイト列を含む、`codepage` が不正、または `flags` をその
/// `codepage` が受け付けない）した場合は `None` を返します。呼び出し側が厳密
/// モード／許容モードの切り替えと、失敗時のフォールバックに使います。
///
/// # 入力長の上限（`i32::MAX` バイト = 2 GiB 未満）
///
/// `MultiByteToWideChar` はバイト数を `i32` で受け取るため、`bytes.len()` が
/// [`i32::MAX`] を超えるとパニックします（`windows` クレートの束縛が
/// `try_into().unwrap()` で変換するため）。呼び出し側はこの上限を守る必要が
/// あります。読み込み経路はチャンク単位でデコードするため実際には到達しません
/// が、公開 API（[`crate::decode`]）の契約として明記しています（Issue #38）。
fn multi_byte_to_wide(
    bytes: &[u8],
    codepage: u32,
    flags: MULTI_BYTE_TO_WIDE_CHAR_FLAGS,
) -> Option<Vec<u16>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }

    // SAFETY: bytes はこの呼び出しの間だけ有効なスライス。lpwidecharstr に
    // None を渡す呼び出しは、MultiByteToWideChar の契約上「必要な文字数だけを
    // 返し、書き込みは行わない」問い合わせ専用モードであり、書き込み先
    // バッファを渡さないため安全である。
    let required = unsafe { MultiByteToWideChar(codepage, flags, bytes, None) };
    if required <= 0 {
        return None;
    }

    let mut wide = vec![0u16; required as usize];
    // SAFETY: wide は直前に required 要素ぴったりで確保した有効なバッファで
    // あり、MultiByteToWideChar は cchWideChar に wide.len() を渡すため、この
    // スライスの範囲内にしか書き込まない（Win32 API の契約）。
    let written = unsafe { MultiByteToWideChar(codepage, flags, bytes, Some(&mut wide)) };
    if written <= 0 {
        return None;
    }

    wide.truncate(written as usize);
    Some(wide)
}

/// `bytes` を `codepage` でデコードします。
///
/// 1. `codepage` の存在を確認する（存在しなければ [`DecodeError::UnknownCodepage`]）
/// 2. まず全体を厳密モード（`MB_ERR_INVALID_CHARS`）で変換を試みる。成功すれば
///    不正バイトなしで完了
/// 3. 厳密モードが失敗した場合、[`WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES`] 単位の
///    チャンクへ分割し、チャンクごとに厳密モードを試して不正位置を近似特定する
/// 4. 許容モード（フラグなし）で全体を変換し、実際のテキストを得る
///    （不正バイトは Windows がコードページの既定文字へ置き換える）
///
/// # 厳密モードを使えないコードページ（Issue #38）
///
/// [`crate::codepage_rejection`] が理由を返すコードページ（UTF-16、および
/// `MB_ERR_INVALID_CHARS` を受け付けないコードページ）では、手順3のチャンク走査
/// を行いません。行っても全チャンクが `ERROR_INVALID_FLAGS` で失敗し、実際には
/// 妥当なバイト列であっても「全チャンクが不正位置」という実態と食い違う報告に
/// なるためです。これらのコードページは設定の起動時検証（`CFG-016`）が先に弾く
/// ので通常は到達しませんが、到達した場合は不正位置の報告をあきらめ、テキストの
/// 取得だけを試みます。
///
/// # 入力長の上限
///
/// `bytes.len()` は [`i32::MAX`] 以下である必要があります（[`multi_byte_to_wide`]
/// の doc コメント参照）。
pub(crate) fn decode_windows_codepage(
    bytes: &[u8],
    codepage: u32,
) -> Result<RawDecodeResult, DecodeError> {
    if !codepage_exists(codepage) {
        return Err(DecodeError::UnknownCodepage(codepage));
    }

    // 厳密モードが意味を持つコードページかを先に判定する。順序が逆だと、
    // 厳密モードの失敗が「不正バイトがある」のか「フラグを受け付けない」のか
    // 区別できないまま、下のチャンク走査へ進んでしまう。
    if crate::decision::codepage_rejection(codepage).is_some() {
        let wide = multi_byte_to_wide(bytes, codepage, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0))
            .ok_or(DecodeError::UnknownCodepage(codepage))?;
        return Ok(RawDecodeResult {
            text: String::from_utf16_lossy(&wide),
            invalid_offsets: Vec::new(),
            truncated: false,
        });
    }

    if let Some(wide) = multi_byte_to_wide(bytes, codepage, MB_ERR_INVALID_CHARS) {
        return Ok(RawDecodeResult {
            text: String::from_utf16_lossy(&wide),
            invalid_offsets: Vec::new(),
            truncated: false,
        });
    }

    let mut invalid_offsets = Vec::new();
    let mut truncated = false;
    for (index, chunk) in bytes.chunks(WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES).enumerate() {
        if multi_byte_to_wide(chunk, codepage, MB_ERR_INVALID_CHARS).is_none() {
            if invalid_offsets.len() < MAX_INVALID_POSITIONS {
                invalid_offsets.push(index * WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES);
            } else {
                truncated = true;
            }
        }
    }

    // 許容モード（フラグなし = 0）。不正バイトは Windows がコードページの
    // 既定文字へ置き換えるため、厳密モードが失敗した後でも通常は成功する。
    let lenient_flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0);
    let wide = multi_byte_to_wide(bytes, codepage, lenient_flags)
        .ok_or(DecodeError::UnknownCodepage(codepage))?;

    Ok(RawDecodeResult {
        text: String::from_utf16_lossy(&wide),
        invalid_offsets,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 「日本語」の CP932（Shift_JIS 系）バイト列。
    const JAPANESE_CP932: [u8; 6] = [0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA];
    // "café €" の CP1252（西欧言語）バイト列（é=0xE9、€=0x80）。
    const CAFE_EURO_CP1252: [u8; 6] = [0x63, 0x61, 0x66, 0xE9, 0x20, 0x80];

    #[test]
    fn codepage_exists_true_for_known_codepages() {
        assert!(codepage_exists(932));
        assert!(codepage_exists(1252));
    }

    // 受け入れ条件: 存在しないコードページのエラー。
    #[test]
    fn codepage_exists_false_for_unknown_codepage() {
        assert!(!codepage_exists(99_999));
    }

    // 受け入れ条件: コードページ 932 で生成したバイト列が正しくデコードされる。
    #[test]
    fn decodes_cp932_japanese_text() {
        let result = decode_windows_codepage(&JAPANESE_CP932, 932).unwrap();
        assert_eq!(result.text, "日本語");
        assert!(result.invalid_offsets.is_empty());
        assert!(!result.truncated);
    }

    // 受け入れ条件: CP1252 の高位バイトが正しくデコードされる。
    #[test]
    fn decodes_cp1252_high_bytes() {
        let result = decode_windows_codepage(&CAFE_EURO_CP1252, 1252).unwrap();
        assert_eq!(result.text, "café €");
    }

    // 受け入れ条件: 不正バイト列で位置と選択文字コードが返り、置換文字で継続する
    // （チャンク単位の近似。テスト用の小さいバイト列は1チャンクに収まるため、
    // 報告位置は先頭オフセット0になる）。
    #[test]
    fn decodes_invalid_cp932_bytes_with_approximate_position_and_continues() {
        let mut bytes = b"OK:".to_vec();
        bytes.extend_from_slice(&[0x81, 0x30]); // CP932 として不正な組（存在しないトレイルバイト）。
        bytes.extend_from_slice(b":END");

        let result = decode_windows_codepage(&bytes, 932).unwrap();
        assert!(
            result.text.starts_with("OK:"),
            "先頭の妥当な部分は保たれるはず: {}",
            result.text
        );
        assert!(
            result.text.ends_with(":END"),
            "末尾の妥当な部分は保たれるはず: {}",
            result.text
        );
        assert_eq!(
            result.invalid_offsets,
            vec![0],
            "テスト用の短いバイト列は1チャンクに収まるため、近似位置は先頭(0)になるはず"
        );
        assert!(!result.truncated);
    }

    // 受け入れ条件: 存在しないコードページのエラー。
    #[test]
    fn decode_with_unknown_codepage_is_an_error() {
        let result = decode_windows_codepage(&JAPANESE_CP932, 99_999);
        assert_eq!(result.unwrap_err(), DecodeError::UnknownCodepage(99_999));
    }

    // 実行環境の ANSI コードページは取得でき、0 より大きい（実際の値は
    // 環境依存のため断定しない）。
    #[test]
    fn environment_ansi_codepage_is_positive() {
        assert!(environment_ansi_codepage() > 0);
    }

    // ---------------------------------------------------------------
    // 特殊コードページの実挙動確認（CFG-016、ENC-006、Issue #38）。
    // ---------------------------------------------------------------

    /// 起動時検証の分類（`crate::decision::codepage_rejection`）と突き合わせる
    /// 対象のコードページ。ISO-2022 系・ISCII 系・UTF-7・Symbol・UTF-16 と、
    /// 比較のための通常のコードページを含む。
    fn probed_codepages() -> Vec<u32> {
        let mut codepages: Vec<u32> = (50220..=50229).chain(57002..=57011).collect();
        codepages.extend([
            42, 52936, 54936, 65000, 65001, 1200, 1201, 12000, 12001, 20127, 932, 1252,
        ]);
        codepages
    }

    /// `codepage` に対する `MultiByteToWideChar` の実挙動を1バイトの ASCII
    /// （どのコードページでも文字として妥当）で確かめる。戻り値は
    /// （厳密モードが成功したか, 許容モードが成功したか）。
    fn probe_modes(codepage: u32) -> (bool, bool) {
        let sample = b"A";
        let strict = multi_byte_to_wide(sample, codepage, MB_ERR_INVALID_CHARS).is_some();
        let lenient =
            multi_byte_to_wide(sample, codepage, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0)).is_some();
        (strict, lenient)
    }

    // 受け入れ条件（`CFG-016`、`ENC-006`）: 起動時検証が使う分類
    // （`codepage_supports_strict_validation` / `codepage_rejection`）が、この
    // 実行環境の `MultiByteToWideChar` の実挙動と一致する。分類は MSDN の記述を
    // 元にした固定の一覧なので、実挙動と食い違えば「起動時検証は通ったのに
    // ファイルを開くと全チャンクが不正位置になる」状態を生む。実挙動を正とし、
    // 一覧のほうを直すためのテストである。
    #[test]
    fn codepages_without_strict_mode_match_win32_behavior() {
        for codepage in probed_codepages() {
            if !codepage_exists(codepage) {
                // 実行環境に無いコードページは、そもそも存在確認
                // （`crate::codepage_available`）が起動時に弾く。厳密モードの
                // 可否は問わない。
                println!("cp={codepage:5} exists=false");
                continue;
            }
            let (strict, lenient) = probe_modes(codepage);
            let rejection = crate::decision::codepage_rejection(codepage);
            println!(
                "cp={codepage:5} exists=true  strict={strict:5} lenient={lenient:5} rejection={rejection:?}"
            );

            if strict {
                assert!(
                    crate::decision::codepage_supports_strict_validation(codepage),
                    "コードページ {codepage} は厳密モードを受け付けるのに、非対応として分類されている"
                );
            } else if lenient {
                // 許容モードは通るのに厳密モードだけ失敗する = フラグ自体を
                // 受け付けないコードページ。
                assert!(
                    !crate::decision::codepage_supports_strict_validation(codepage),
                    "コードページ {codepage} は厳密モードを受け付けないのに、対応として分類されている"
                );
            } else {
                // どちらのモードでも変換できない（UTF-16 など）。文字コードとして
                // 選べないことが分類に現れている必要がある。
                assert!(
                    rejection.is_some(),
                    "コードページ {codepage} は変換自体ができないのに、選択可能として分類されている"
                );
            }
        }
    }

    // 受け入れ条件（`ENC-006`、Issue #38）: 厳密モードを使えないコードページで
    // デコードしても、妥当なバイト列が「全チャンク不正」にならない
    // （`decode_windows_codepage` がチャンク走査を行わない）。
    #[test]
    fn decode_with_codepage_without_strict_mode_does_not_report_every_chunk_as_invalid() {
        // UTF-7（65000）は MB_ERR_INVALID_CHARS を受け付けない代表例。
        let codepage = 65000;
        if !codepage_exists(codepage) {
            return;
        }
        let bytes = b"plain ASCII line".repeat(600); // 4096 バイト超（複数チャンク）。
        let result =
            decode_windows_codepage(&bytes, codepage).expect("許容モードでの変換は成功するはず");
        assert!(
            result.invalid_offsets.is_empty(),
            "妥当な ASCII なのに不正位置が報告された: {:?}",
            result.invalid_offsets
        );
        assert!(!result.truncated);
    }
}
