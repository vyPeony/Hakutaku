//! デコード（バイト列 → 文字列）です。[`decode`] が公開入口です。

use crate::decision::{DecidedEncoding, SelectedEncoding};

/// 不正位置の報告件数の上限です。
///
/// 大量の不正バイトを含む入力で無制限に位置を溜め込まないための上限です。
/// 上限に達した場合、[`DecodeOutcome::invalid_positions_truncated`] が
/// `true` になり、それ以降の不正位置は一覧に含まれません。
pub const MAX_INVALID_POSITIONS: usize = 100;

/// UTF-8 経路で [`decode`] が確保し得る、入力1バイトあたりの最悪バイト数です
/// （[`max_decode_peak_bytes`] の係数。導出根拠は同関数の doc コメント）。
const UTF8_PEAK_BYTES_PER_INPUT_BYTE: usize = 6;

/// Windows コードページ経路で [`decode`] が確保し得る、入力1バイトあたりの
/// 最悪バイト数です（[`max_decode_peak_bytes`] の係数。導出根拠は同関数の
/// doc コメント）。
const WINDOWS_PEAK_BYTES_PER_INPUT_BYTE: usize = 8;

/// [`max_decode_peak_bytes`] が、入力長に比例しない分として加算するバイト数です。
///
/// 内訳は次の2つです。
///
/// - **不正位置一覧**（`Vec<usize>`）: 最大 [`MAX_INVALID_POSITIONS`] 件を倍々
///   成長で溜めるため、確保済み容量は最大 `100.next_power_of_two()` = 128 要素
///   です。さらに [`decode`] が BOM 分を加算して別の `Vec` へ集める区間では
///   変換前後の2本が同時に生きるため、2倍を見込みます
/// - **極小の入力での端数**: `String` と `Vec` は最小確保単位（`RawVec` の
///   `MIN_NON_ZERO_CAP`。1バイト要素では8）を下回る容量を確保しないため、
///   入力長への比例だけでは数バイトの入力で実際の確保量を下回ります。この
///   固定分がその端数を吸収します
const FIXED_OVERHEAD_BYTES: usize =
    MAX_INVALID_POSITIONS.next_power_of_two() * std::mem::size_of::<usize>() * 2;

/// [`decode`] の結果です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeOutcome {
    /// デコードされた文字列。不正バイトは置換文字（UTF-8 は U+FFFD、Windows
    /// コードページはそのコードページの既定文字）を含み得ます。
    pub text: String,
    /// 実際に使われた文字コード（`decided.encoding` と同じ値）。
    pub selected_encoding: SelectedEncoding,
    /// 検出した不正バイト位置（デコード対象へ渡した `bytes` の先頭からの絶対
    /// バイトオフセット。BOM を除去した場合もその分を加算済みで、`bytes`
    /// そのものに対して直接使えます）。上限 [`MAX_INVALID_POSITIONS`] 件まで。
    pub invalid_positions: Vec<usize>,
    /// `invalid_positions` が上限に達し、以降の位置を打ち切ったか。
    pub invalid_positions_truncated: bool,
}

/// [`decode`] の失敗です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// 指定されたコードページが実行環境に存在しない
    /// （`GetCPInfoExW` での検証、または変換失敗）。
    UnknownCodepage(u32),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::UnknownCodepage(codepage) => {
                write!(f, "コードページ {codepage} は実行環境に存在しません")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// エンコーディング別の内部デコード実装が返す、BOM 分のオフセットを加算する
/// **前**の生の結果です。[`decode`] が `decided.bom_len` を加算してから
/// [`DecodeOutcome`] へ変換します。
#[derive(Debug)]
pub(crate) struct RawDecodeResult {
    pub(crate) text: String,
    pub(crate) invalid_offsets: Vec<usize>,
    pub(crate) truncated: bool,
}

/// `bytes` を `decided` が示す文字コードでデコードします。
///
/// `decided.bom_len` バイトを読み飛ばしてからデコードします（`decided` は
/// [`crate::detect_encoding`] が返した値をそのまま渡す想定）。デコードできない
/// バイト列は置換文字へ変換しつつ処理を継続し、**元の `bytes` は変更・破棄
/// しません**（呼び出し側が引き続き `bytes` を保持できる設計。実際に元バイト
/// 列を保持し続けるバッファの設計は P05-6 の対象です）。
///
/// # 不正位置の特定粒度
///
/// - [`SelectedEncoding::Utf8`]: `str::from_utf8` の判定に基づく、バイト単位の
///   正確な位置です。
/// - [`SelectedEncoding::Windows`]: `MultiByteToWideChar` を使った、
///   `WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES`（Windows 専用モジュール
///   `crate::win32` で定義）バイト単位の近似です。詳細は同定数の doc コメント
///   を参照してください。
pub fn decode(bytes: &[u8], decided: &DecidedEncoding) -> Result<DecodeOutcome, DecodeError> {
    let bom_len = decided.bom_len.min(bytes.len());
    let content = &bytes[bom_len..];

    let raw = match decided.encoding {
        SelectedEncoding::Utf8 => decode_utf8_lossy_with_positions(content),
        SelectedEncoding::Windows(codepage) => decode_windows_codepage(content, codepage)?,
    };

    let invalid_positions = raw
        .invalid_offsets
        .into_iter()
        .map(|offset| offset + bom_len)
        .collect();

    Ok(DecodeOutcome {
        text: raw.text,
        selected_encoding: decided.encoding,
        invalid_positions,
        invalid_positions_truncated: raw.truncated,
    })
}

/// `input_len` バイトを [`decode`] したときに、実行中**同時に生存し得る**追加
/// ヒープ確保量の最悪合計（バイト）を見積もります（`PERF-008`・`PERF-010`、
/// Issue #32）。
///
/// # 何のために公開しているか
///
/// `PERF-010` は大規模な確保の**前**に予約・拒否することを求めますが、
/// [`decode`] が内部で確保する量は呼び出し側（読み込み経路、オンデマンド
/// 読み出し経路）から見えず、予約量を決められません。呼び出し側はこの値を
/// `hakutaku_memory_accounting::MemoryBudget::reserve` へ渡して確保前に予約し、
/// 予約が通ったら [`decode`] を呼び、実際に得た [`DecodeOutcome::text`] の容量を
/// `ReservationToken::mark_allocated` で実確保へ振り替えます（ADR-0003）。
/// 本クレートはメモリ会計クレートに依存しない（判定・デコードの純粋なロジックに
/// 閉じる）ため、見積もりだけを純関数として提供します。
///
/// `input_len` には [`decode`] へ渡す `bytes` の長さを渡してください。BOM 分
/// （`decided.bom_len`、最大3バイト）は差し引きません（差し引かない方が過大側
/// = 安全側で、しかも呼び出し側が長さを取り違える余地がないため）。
///
/// # 見積もりの前提: 何を「同時に生存する量」と数えるか
///
/// 計装アロケータは `realloc` の**差分だけ**を `allocated_bytes` へ計上します
/// （ADR-0003 の会計契約）。したがって再確保の瞬間に旧容量と新容量が同時に
/// 生きる分は会計値へ現れず、ここで数えるのは「ある時点で同時に確保されている
/// 容量の合計」の最大値です。
///
/// 見積もりの土台は、`String` と `Vec` の伸長が償却的な倍々成長
/// （新容量 = `max(旧容量 × 2, 必要量)`）である点です。伸長が起きた時点の旧容量は
/// 必ずその時点の長さ未満なので、**伸長を経た確保済み容量は最終長の2倍を
/// 超えません**。以下の各経路はこの性質だけを使い、標準ライブラリが初期容量を
/// どう見積もるかには依存しません。
///
/// # 経路別の導出
///
/// ## UTF-8（[`SelectedEncoding::Utf8`]）: `input_len` × 6
///
/// この経路が確保するのは出力 `String` だけです。`decode_utf8_lossy_with_positions`
/// は不正区間1つにつき U+FFFD（UTF-8 で3バイト）を1つだけ push しますが、区間長は
/// 最小1バイトなので、**入力1バイトが出力3バイトへ膨らむ**のが最悪です（妥当な
/// 部分はそのまま複写するので1対1）。よって最終長は最悪 `input_len` × 3、
/// 確保済み容量は倍々成長の性質からその2倍の `input_len` × 6 を超えません。
///
/// ## Windows コードページ（[`SelectedEncoding::Windows`]）: `input_len` × 8
///
/// `crate::win32::decode_windows_codepage` は、中間の UTF-16 バッファと最終の
/// `String` を**同時に**生かします（`String::from_utf16_lossy(&wide)` は `wide` を
/// 借用したまま `String` を組み立てるため）。合計がこの経路の最悪ピークです。
///
/// - **中間の `Vec<u16>`**: `MultiByteToWideChar` が返す UTF-16 コード単位数
///   ぴったりで確保します（`truncate` は容量を縮めません）。単位を1つ生成する
///   のに入力を最低1バイト消費する（SBCS は1バイト→1単位、DBCS は2バイト
///   →1単位、UTF-8・GB18030 の4バイト列はサロゲートペアで2単位、不正バイトは
///   1バイト→置換1単位）ため、単位数は `input_len` を超えません。1単位2バイトで
///   `input_len` × 2 バイト
/// - **最終の `String`**: 1コード単位は UTF-8 で最大3バイトなので最終長は
///   `input_len` × 3 を超えず、確保済み容量は倍々成長の性質からその2倍の
///   `input_len` × 6 バイト
///
/// 不正位置の近似特定に使うチャンク単位の一時バッファ
/// （`WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES` バイトごと。Windows 専用モジュール
/// `crate::win32` で定義）は、1本ずつ確保して破棄し、しかもチャンク長は
/// `input_len` 以下なので、中間バッファ分の見積もりに収まります。
///
/// ## 入力長に比例しない固定分
///
/// 上記に `FIXED_OVERHEAD_BYTES` を加算します（内訳は同定数の doc コメント）。
///
/// # オーバーフロー
///
/// 算術はすべて飽和演算です。飽和した場合の戻り値は [`usize::MAX`] 近傍と
/// なり、予約側（`MemoryBudget::reserve` は `checked_add` でオーバーフローする
/// 要求を拒否します）で必ず拒否されるため、安全側へ倒れます。
#[must_use]
pub fn max_decode_peak_bytes(decided: &DecidedEncoding, input_len: usize) -> usize {
    let bytes_per_input_byte = match decided.encoding {
        SelectedEncoding::Utf8 => UTF8_PEAK_BYTES_PER_INPUT_BYTE,
        SelectedEncoding::Windows(_) => WINDOWS_PEAK_BYTES_PER_INPUT_BYTE,
    };
    input_len
        .saturating_mul(bytes_per_input_byte)
        .saturating_add(FIXED_OVERHEAD_BYTES)
}

/// UTF-8 として `bytes` をデコードします。不正なバイト列は U+FFFD へ置換し、
/// 各不正区間の最初のバイトの絶対オフセット（`bytes` の先頭からのバイト数）を
/// 記録します。
///
/// 標準ライブラリの `String::from_utf8_lossy` と同じ判定（`str::from_utf8` が
/// 返す `Utf8Error` の `valid_up_to` / `error_len`）を使いますが、標準ライブラリ
/// は不正位置を公開しないため、位置を記録しながら同等のループを自前で実装して
/// います。
fn decode_utf8_lossy_with_positions(bytes: &[u8]) -> RawDecodeResult {
    const REPLACEMENT: char = '\u{FFFD}';

    let mut text = String::with_capacity(bytes.len());
    let mut invalid_offsets = Vec::new();
    let mut truncated = false;
    let mut remaining = bytes;
    let mut consumed = 0usize;

    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                text.push_str(valid);
                remaining = &[];
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                // str::from_utf8 の契約により、[..valid_up_to] は妥当な UTF-8
                // であることが保証されている。
                let valid_part = std::str::from_utf8(&remaining[..valid_up_to])
                    .expect("valid_up_to までは妥当な UTF-8 であることが保証されている");
                text.push_str(valid_part);

                let invalid_offset = consumed + valid_up_to;
                // error_len が None の場合、`remaining` の末尾でマルチバイト
                // 文字が入力切れになっている（この関数はバイト列全体を対象と
                // した最終判定であり、続きの入力が来る余地はない）ことを表す。
                // 残り全部を1つの不正区間として扱う。
                let skip = err.error_len().unwrap_or(remaining.len() - valid_up_to);

                if skip > 0 {
                    if invalid_offsets.len() < MAX_INVALID_POSITIONS {
                        invalid_offsets.push(invalid_offset);
                    } else {
                        truncated = true;
                    }
                    text.push(REPLACEMENT);
                }

                consumed += valid_up_to + skip;
                remaining = &remaining[valid_up_to + skip..];
            }
        }
    }

    RawDecodeResult {
        text,
        invalid_offsets,
        truncated,
    }
}

#[cfg(windows)]
fn decode_windows_codepage(bytes: &[u8], codepage: u32) -> Result<RawDecodeResult, DecodeError> {
    crate::win32::decode_windows_codepage(bytes, codepage)
}

/// Windows 以外でのビルド用の代替実装です。
///
/// 本リポジトリのビルド対象は `.cargo/config.toml` で `x86_64-pc-windows-msvc`
/// に固定されているため、この関数が実際に呼ばれることはありません
/// （[`SelectedEncoding::Windows`] は Win32 の `MultiByteToWideChar` 前提の型で
/// あり、Windows 以外では変換手段を持たないため、型としてコンパイルが通る
/// ようにするための代替実装です）。
#[cfg(not(windows))]
fn decode_windows_codepage(_bytes: &[u8], codepage: u32) -> Result<RawDecodeResult, DecodeError> {
    Err(DecodeError::UnknownCodepage(codepage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::DetectionRoute;

    fn decided_utf8(bom_len: usize) -> DecidedEncoding {
        DecidedEncoding {
            encoding: SelectedEncoding::Utf8,
            route: DetectionRoute::Utf8ValidatedNoBom,
            bom_len,
            warnings: Vec::new(),
        }
    }

    // 受け入れ条件: 不正バイト列で位置と選択文字コードが返り、置換文字で継続する。
    #[test]
    fn utf8_decode_reports_invalid_position_and_continues_with_replacement() {
        let bytes = b"abc\xFFdef";
        let outcome = decode(bytes, &decided_utf8(0)).unwrap();
        assert_eq!(outcome.text, "abc\u{FFFD}def");
        assert_eq!(outcome.invalid_positions, vec![3]);
        assert!(!outcome.invalid_positions_truncated);
        assert_eq!(outcome.selected_encoding, SelectedEncoding::Utf8);
    }

    #[test]
    fn utf8_decode_of_valid_bytes_has_no_invalid_positions() {
        let bytes = "日本語".as_bytes();
        let outcome = decode(bytes, &decided_utf8(0)).unwrap();
        assert_eq!(outcome.text, "日本語");
        assert!(outcome.invalid_positions.is_empty());
    }

    // BOM を除去した場合でも、不正位置は元の `bytes`（BOM を含む）基準の絶対
    // オフセットで報告される。
    #[test]
    fn utf8_decode_invalid_position_accounts_for_removed_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"ab\xFFcd");
        let outcome = decode(&bytes, &decided_utf8(3)).unwrap();
        assert_eq!(outcome.text, "ab\u{FFFD}cd");
        // BOM 3バイト + "ab" 2バイト = オフセット5。
        assert_eq!(outcome.invalid_positions, vec![5]);
    }

    #[test]
    fn utf8_decode_caps_invalid_positions_at_limit() {
        let bytes = vec![0xFFu8; MAX_INVALID_POSITIONS + 10];
        let outcome = decode(&bytes, &decided_utf8(0)).unwrap();
        assert_eq!(outcome.invalid_positions.len(), MAX_INVALID_POSITIONS);
        assert!(outcome.invalid_positions_truncated);
    }

    // ---------------------------------------------------------------
    // max_decode_peak_bytes（`PERF-010` の予約量見積もり、Issue #32）。
    // ---------------------------------------------------------------

    fn decided_windows(codepage: u32) -> DecidedEncoding {
        DecidedEncoding {
            encoding: SelectedEncoding::Windows(codepage),
            route: DetectionRoute::ProfileSpecified(
                crate::decision::ProfileSpecifiedKind::AnsiCodepage,
            ),
            bom_len: 0,
            warnings: Vec::new(),
        }
    }

    // 受け入れ条件（`PERF-010`）: UTF-8 経路の見積もりが、入力長 × 6 + 固定分に
    // なる（出力 `String` は入力1バイトあたり最大3バイトへ膨らみ、倍々成長で
    // 確保済み容量は最終長の2倍を超えない）。
    #[test]
    fn max_decode_peak_bytes_for_utf8_is_six_times_input_plus_fixed_overhead() {
        for input_len in [0usize, 1, 7, 4096, 64 * 1024] {
            assert_eq!(
                max_decode_peak_bytes(&decided_utf8(0), input_len),
                input_len * 6 + FIXED_OVERHEAD_BYTES,
                "入力長 {input_len} の UTF-8 経路の見積もり"
            );
        }
    }

    // 受け入れ条件（`PERF-010`）: Windows コードページ経路の見積もりが、入力長
    // × 8 + 固定分になる（中間 UTF-16 バッファの2倍と、最終 `String` の6倍が
    // 同時に生きる）。
    #[test]
    fn max_decode_peak_bytes_for_windows_codepage_is_eight_times_input_plus_fixed_overhead() {
        for input_len in [0usize, 1, 7, 4096, 64 * 1024] {
            assert_eq!(
                max_decode_peak_bytes(&decided_windows(932), input_len),
                input_len * 8 + FIXED_OVERHEAD_BYTES,
                "入力長 {input_len} の Windows コードページ経路の見積もり"
            );
        }
    }

    // 受け入れ条件（`PERF-010`）: 極端な入力長でもパニックせず飽和する
    // （飽和値は予約側の `checked_add` / 予算判定で必ず拒否されるため安全側）。
    #[test]
    fn max_decode_peak_bytes_saturates_instead_of_overflowing() {
        for decided in [decided_utf8(0), decided_windows(932)] {
            // 係数を掛けた時点で必ずあふれる領域では usize::MAX へ張り付く。
            for input_len in [usize::MAX, usize::MAX - 1, usize::MAX / 2] {
                assert_eq!(
                    max_decode_peak_bytes(&decided, input_len),
                    usize::MAX,
                    "飽和するはず（encoding={:?}、input_len={input_len}）",
                    decided.encoding
                );
            }

            // あふれない領域では飽和させず、入力長より大きい比例値を返す
            // （飽和を理由に一律 usize::MAX を返して常に拒否させることはしない）。
            let modest = usize::MAX / 16;
            let estimate = max_decode_peak_bytes(&decided, modest);
            assert!(
                estimate > modest && estimate < usize::MAX,
                "あふれない入力長では比例値を返すはず（encoding={:?}、estimate={estimate}）",
                decided.encoding
            );
        }
    }

    // 受け入れ条件（`PERF-010`）: UTF-8 経路の実測が見積もりを超えない。最悪比率
    // （全バイトが不正 = 1バイトが U+FFFD の3バイトへ膨らむ）を含めて確認する。
    #[test]
    fn max_decode_peak_bytes_covers_actual_utf8_decode() {
        let worst_case = vec![0xFFu8; MAX_INVALID_POSITIONS + 10];
        let samples: [&[u8]; 4] = [
            b"abc\xFFdef",
            "日本語のログ行です".as_bytes(),
            &worst_case,
            b"",
        ];
        for bytes in samples {
            let decided = decided_utf8(0);
            let estimate = max_decode_peak_bytes(&decided, bytes.len());
            let outcome = decode(bytes, &decided).unwrap();
            assert!(
                outcome.text.len() <= estimate,
                "出力長 {} が見積もり {} を超えた（入力長 {}）",
                outcome.text.len(),
                estimate,
                bytes.len()
            );
            assert!(
                outcome.text.capacity() <= estimate,
                "確保済み容量 {} が見積もり {} を超えた（入力長 {}）",
                outcome.text.capacity(),
                estimate,
                bytes.len()
            );
        }
    }

    // 受け入れ条件（`PERF-010`）: Windows コードページ経路の実測が見積もりを
    // 超えない（CP932 の日本語、CP1252 の高位バイト、CP932 として不正な組）。
    // `MultiByteToWideChar` を呼ぶため Windows でのみ実行する。
    #[cfg(windows)]
    #[test]
    fn max_decode_peak_bytes_covers_actual_windows_codepage_decode() {
        let invalid_cp932 = [0x4Fu8, 0x4B, 0x3A, 0x81, 0x30, 0x3A, 0x45, 0x4E, 0x44];
        let samples: [(&[u8], u32); 4] = [
            (&[0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA], 932),
            (&[0x63, 0x61, 0x66, 0xE9, 0x20, 0x80], 1252),
            (&invalid_cp932, 932),
            (b"", 932),
        ];
        for (bytes, codepage) in samples {
            let decided = decided_windows(codepage);
            let estimate = max_decode_peak_bytes(&decided, bytes.len());
            let outcome = decode(bytes, &decided).unwrap();
            assert!(
                outcome.text.len() <= estimate,
                "出力長 {} が見積もり {} を超えた（コードページ {codepage}、入力長 {}）",
                outcome.text.len(),
                estimate,
                bytes.len()
            );
            assert!(
                outcome.text.capacity() <= estimate,
                "確保済み容量 {} が見積もり {} を超えた（コードページ {codepage}、入力長 {}）",
                outcome.text.capacity(),
                estimate,
                bytes.len()
            );
        }
    }
}
