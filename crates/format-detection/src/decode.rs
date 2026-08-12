//! デコード（バイト列 → 文字列）です。[`decode`] が公開入口です。

use crate::decision::{DecidedEncoding, SelectedEncoding};

/// 不正位置の報告件数の上限です。
///
/// 大量の不正バイトを含む入力で無制限に位置を溜め込まないための上限です。
/// 上限に達した場合、[`DecodeOutcome::invalid_positions_truncated`] が
/// `true` になり、それ以降の不正位置は一覧に含まれません。
pub const MAX_INVALID_POSITIONS: usize = 100;

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
}
