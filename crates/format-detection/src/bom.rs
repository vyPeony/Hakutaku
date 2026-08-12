//! BOM（バイト順マーク）検出です（`ENC-003`、`ENC-005`、`ENC-006`）。
//!
//! 判定対象はファイル先頭の固定バイト列のみであり、内容全体は走査しません。
//! UTF-8 BOM（`EF BB BF`）と UTF-16 LE/BE の BOM（`FF FE` / `FE FF`）を区別
//! できれば十分なため（UTF-16 は `ENC-006` により未対応形式として扱うだけで、
//! それ以上の解釈はしない）、UTF-32 の BOM（`FF FE 00 00` 等）は個別には扱わず、
//! 前方一致する UTF-16 LE BOM として検出されます。UTF-32 は仕様上そもそも
//! 対応対象に含まれていません。

/// 検出した BOM の種類です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BomKind {
    /// UTF-8 BOM（`EF BB BF`）。
    Utf8,
    /// UTF-16 リトルエンディアン BOM（`FF FE`）。`ENC-006` により未対応。
    Utf16Le,
    /// UTF-16 ビッグエンディアン BOM（`FE FF`）。`ENC-006` により未対応。
    Utf16Be,
}

/// 検出結果です。`len` は BOM 自体のバイト数（除去する際に読み飛ばす長さ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DetectedBom {
    pub(crate) kind: BomKind,
    pub(crate) len: usize,
}

/// `bytes` の先頭が既知の BOM と一致するかを調べます。
///
/// 3バイトの UTF-8 BOM を先に確認するため、`FF FE` や `FE FF` との誤認は
/// 起きません（UTF-8 BOM の先頭バイト `EF` はどちらとも一致しないため）。
pub(crate) fn detect(bytes: &[u8]) -> Option<DetectedBom> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(DetectedBom {
            kind: BomKind::Utf8,
            len: 3,
        });
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some(DetectedBom {
            kind: BomKind::Utf16Le,
            len: 2,
        });
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Some(DetectedBom {
            kind: BomKind::Utf16Be,
            len: 2,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ条件: UTF-8 BOM あり／なしの判定（ENC-003、ENC-005 第2段階）。
    #[test]
    fn detects_utf8_bom() {
        let bytes = [0xEF, 0xBB, 0xBF, b'a', b'b'];
        let bom = detect(&bytes).expect("UTF-8 BOM を検出できるはず");
        assert_eq!(bom.kind, BomKind::Utf8);
        assert_eq!(bom.len, 3);
    }

    #[test]
    fn no_bom_returns_none() {
        let bytes = *b"abc";
        assert!(detect(&bytes).is_none());
    }

    // 受け入れ条件: UTF-16 LE/BE の BOM 検出（ENC-006 の前提）。
    #[test]
    fn detects_utf16_le_bom() {
        let bytes = [0xFF, 0xFE, 0x41, 0x00];
        let bom = detect(&bytes).expect("UTF-16 LE BOM を検出できるはず");
        assert_eq!(bom.kind, BomKind::Utf16Le);
        assert_eq!(bom.len, 2);
    }

    #[test]
    fn detects_utf16_be_bom() {
        let bytes = [0xFE, 0xFF, 0x00, 0x41];
        let bom = detect(&bytes).expect("UTF-16 BE BOM を検出できるはず");
        assert_eq!(bom.kind, BomKind::Utf16Be);
        assert_eq!(bom.len, 2);
    }

    #[test]
    fn short_input_without_bom_does_not_panic() {
        assert!(detect(&[]).is_none());
        assert!(detect(&[0xEF]).is_none());
        assert!(detect(&[0xFF]).is_none());
    }
}
