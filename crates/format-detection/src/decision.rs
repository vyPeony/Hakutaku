//! 文字コード判定（`ENC-005` の4段階）です。[`detect_encoding`] が公開入口です。

use std::fmt;

use crate::bom::{self, BomKind};

/// BOM なし UTF-8 の妥当性確認に使う先頭バイト数（`ENC-005` 第3段階）。
///
/// 判定は `bytes` の先頭からこのバイト数まで（`bytes` がこれより短い場合は
/// `bytes` 全体）でだけ行います。ファイルの後半にだけ不正なバイト列がある
/// 場合、この判定では検出できません（既知の限界）。実際のデコード
/// （[`crate::decode`]）は全バイトを対象に行い、そちらで不正位置を報告します。
pub const UTF8_AUTO_DETECT_PREFIX_BYTES: usize = 64 * 1024;

/// 判定・デコードで最終的に選ばれた文字コードです。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedEncoding {
    /// UTF-8。
    Utf8,
    /// 任意の Windows コードページ（例: 932 = Shift_JIS 系、1252 = 西欧言語）。
    Windows(u32),
}

impl fmt::Display for SelectedEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectedEncoding::Utf8 => write!(f, "utf-8"),
            SelectedEncoding::Windows(codepage) => write!(f, "windows-{codepage}"),
        }
    }
}

/// 選択された判定経路です（診断情報用。`ENC-005`、`DIAG-005`）。
///
/// 呼び出し側（読み込み経路）が、なぜその文字コードが選ばれたかを診断ログへ記録する
/// 際に使います。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionRoute {
    /// UTF-8 BOM を検出した（`ENC-005` 第1段階）。
    Utf8Bom,
    /// BOM はないが、先頭 [`UTF8_AUTO_DETECT_PREFIX_BYTES`] が妥当な UTF-8
    /// だった（`ENC-005` 第2段階）。
    Utf8ValidatedNoBom,
    /// プロファイルの明示指定（`encoding` 名前指定または `ansi_codepage`）が
    /// 使われた。
    ProfileSpecified(ProfileSpecifiedKind),
    /// 明示指定がなく、BOM も妥当な UTF-8 判定もされなかったため、実行環境の
    /// Windows ANSI コードページ（`GetACP`）へフォールバックした
    /// （`ENC-005` 第4段階）。
    EnvironmentAnsi,
}

/// [`DetectionRoute::ProfileSpecified`] の内訳です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSpecifiedKind {
    /// `encoding` の名前指定（例: `utf-8`、`windows-932`）。
    NamedEncoding,
    /// `ansi_codepage` の明示指定。
    AnsiCodepage,
}

/// UTF-16 BOM の LE/BE 区別です（[`UnsupportedEncoding`] で使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Utf16BomKind {
    /// リトルエンディアン（`FF FE`）。
    Le,
    /// ビッグエンディアン（`FE FF`）。
    Be,
}

/// UTF-16 の BOM を検出し、未対応形式と判定した結果です（`ENC-006`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedEncoding {
    /// 検出した BOM の種類。
    pub bom: Utf16BomKind,
}

/// 判定中に検出した警告です。矛盾があっても判定処理自体は継続し、警告として
/// 呼び出し側へ伝えます（診断ログへの実際の出力は呼び出し側 = 読み込み経路の責務）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodingWarning {
    /// 警告の種別（機械可読な区分）。
    pub kind: EncodingWarningKind,
    /// 呼び出し側がそのまま診断ログへ出せる、分かりやすい日本語メッセージ。
    pub message: String,
}

/// [`EncodingWarning`] の種別です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingWarningKind {
    /// 検出した BOM と、プロファイルの明示指定が矛盾している
    /// （`tasks/phase-05-log-parsing-core.md` の暫定設計 4.3）。
    BomConflictsWithExplicitSetting,
}

impl EncodingWarning {
    fn bom_conflict(detected: BomKind, explicit: SelectedEncoding) -> Self {
        let bom_label = match detected {
            BomKind::Utf8 => "UTF-8",
            BomKind::Utf16Le => "UTF-16 LE",
            BomKind::Utf16Be => "UTF-16 BE",
        };
        EncodingWarning {
            kind: EncodingWarningKind::BomConflictsWithExplicitSetting,
            message: format!(
                "検出した BOM（{bom_label}）と、プロファイルで明示指定された文字コード\
                （{explicit}）が矛盾しています。明示指定を優先し、暗黙には切り替えません。"
            ),
        }
    }
}

/// プロファイル側のエンコーディング設定です。
///
/// `hakutaku-config` の `LogProfileConfig::encoding`
/// （`hakutaku_config::EncodingSetting::Auto` / `Named(String)`）と
/// `LogProfileConfig::ansi_codepage` に対応する呼び出し側の入力です。
/// 本クレートは責務分離のため `hakutaku-config` に依存しません（判定ロジックを
/// 設定パーサーから独立させる設計判断）。呼び出し側がこの構造体へ変換して
/// 渡します。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileEncodingSetting {
    /// `encoding` の名前指定（`Auto` の場合は `None`）。
    /// 解釈できる形式は [`detect_encoding`] の doc コメントを参照してください。
    pub named: Option<String>,
    /// `ansi_codepage` の明示指定（未指定は `None`）。
    pub ansi_codepage: Option<u32>,
}

impl ProfileEncodingSetting {
    /// 自動判定（`encoding: auto` かつ `ansi_codepage` 未指定）を表します。
    pub fn auto() -> Self {
        Self::default()
    }

    /// `encoding` の名前指定を表します。
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            named: Some(name.into()),
            ansi_codepage: None,
        }
    }

    /// `ansi_codepage` の明示指定を表します。
    pub fn ansi_codepage(codepage: u32) -> Self {
        Self {
            named: None,
            ansi_codepage: Some(codepage),
        }
    }

    /// いずれかの明示指定（`named` または `ansi_codepage`）を持つか。
    fn is_explicit(&self) -> bool {
        self.named.is_some() || self.ansi_codepage.is_some()
    }
}

/// `encoding` の名前指定を解釈できなかったエラーです。
///
/// `utf-8` と `windows-<コードページ番号>`（例: `windows-932`）以外の名前は
/// すべてこのエラーになります（大文字・小文字は区別しません）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidEncodingNameError {
    /// 解釈できなかった元の名前。
    pub name: String,
}

impl fmt::Display for InvalidEncodingNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "文字コード名 '{}' を解釈できません。'utf-8' または 'windows-<コードページ番号>'\
            （例: 'windows-932'）の形式で指定してください",
            self.name
        )
    }
}

impl std::error::Error for InvalidEncodingNameError {}

/// `encoding` の名前指定を [`SelectedEncoding`] へ解釈します。
///
/// 受理する形式は次の2つです（大文字・小文字を区別しません、前後の空白は
/// 無視します）。
///
/// - `utf-8`
/// - `windows-<コードページ番号>`（数字1文字以上、先頭ゼロを含めて可）
fn parse_named_encoding(name: &str) -> Result<SelectedEncoding, InvalidEncodingNameError> {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized == "utf-8" {
        return Ok(SelectedEncoding::Utf8);
    }
    if let Some(digits) = normalized.strip_prefix("windows-") {
        if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            if let Ok(codepage) = digits.parse::<u32>() {
                if codepage > 0 {
                    return Ok(SelectedEncoding::Windows(codepage));
                }
            }
        }
    }
    Err(InvalidEncodingNameError {
        name: name.to_string(),
    })
}

/// 決定済みの文字コードと、そこに至った経路・警告・BOM 除去情報です。
///
/// [`crate::decode`] はこの構造体を受け取ってデコードします。
/// [`EncodingDecision::Unsupported`]（UTF-16 の BOM）はここに含まれないため、
/// デコードできない状態のまま `decode` へ渡してしまう心配がありません
/// （型で防いでいます）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecidedEncoding {
    /// 選択された文字コード。
    pub encoding: SelectedEncoding,
    /// 選択された判定経路。
    pub route: DetectionRoute,
    /// 検出した UTF-8 BOM のバイト数（BOM がない、または明示指定と矛盾して
    /// 除去しなかった場合は 0）。[`crate::decode`] はこのバイト数を読み飛ばして
    /// からデコードします。
    pub bom_len: usize,
    /// 判定中に検出した警告（矛盾など）。空なら警告なし。
    pub warnings: Vec<EncodingWarning>,
}

/// [`detect_encoding`] の結果です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingDecision {
    /// 文字コードが決定した（通常経路）。
    Decided(DecidedEncoding),
    /// UTF-16 LE/BE の BOM を検出し、未対応形式と判定した（`ENC-006`）。
    /// このバリアントは [`crate::decode`] へ渡せません（型シグネチャ上、
    /// [`DecidedEncoding`] のみを受け付けるため）。
    Unsupported(UnsupportedEncoding),
}

/// `bytes` の先頭バイト列とプロファイル設定（`profile`）から、`ENC-005` の
/// 4段階で文字コードを判定します。
///
/// # 引数
///
/// - `bytes`: 判定対象のバイト列（ファイル先頭からの生バイト。BOM 判定と
///   UTF-8 妥当性確認は `bytes` の**先頭部分**だけを見るため、ファイル全体を
///   渡す必要はありません。ただし [`UTF8_AUTO_DETECT_PREFIX_BYTES`] 以上は
///   渡してください。それより短い場合は渡された範囲全体で判定します）
/// - `profile`: プロファイルの文字コード設定
///
/// # 戻り値
///
/// - `Ok(EncodingDecision::Decided(_))`: 文字コードが決定した
/// - `Ok(EncodingDecision::Unsupported(_))`: UTF-16 の BOM を検出した
///   （`ENC-006`）
/// - `Err(_)`: `profile.named` の名前を解釈できなかった
///
/// 判定順序と明示指定・BOM の優先関係は、このモジュールの doc コメントおよび
/// クレートルートの doc コメントを参照してください。
pub fn detect_encoding(
    bytes: &[u8],
    profile: &ProfileEncodingSetting,
) -> Result<EncodingDecision, InvalidEncodingNameError> {
    let detected_bom = bom::detect(bytes);

    if profile.is_explicit() {
        let (encoding, kind) = if let Some(name) = profile.named.as_deref() {
            (
                parse_named_encoding(name)?,
                ProfileSpecifiedKind::NamedEncoding,
            )
        } else {
            // is_explicit() が真かつ named が None なので、ansi_codepage は必ず Some。
            let codepage = profile
                .ansi_codepage
                .expect("is_explicit() が true のため named か ansi_codepage のいずれかは Some");
            (
                SelectedEncoding::Windows(codepage),
                ProfileSpecifiedKind::AnsiCodepage,
            )
        };
        return Ok(EncodingDecision::Decided(build_explicit_decision(
            encoding,
            kind,
            detected_bom,
        )));
    }

    // ここから auto 判定（明示指定なし）。BOM → UTF-8 妥当性確認 → 環境 ANSI。
    if let Some(detected) = detected_bom {
        return Ok(match detected.kind {
            BomKind::Utf8 => EncodingDecision::Decided(DecidedEncoding {
                encoding: SelectedEncoding::Utf8,
                route: DetectionRoute::Utf8Bom,
                bom_len: detected.len,
                warnings: Vec::new(),
            }),
            BomKind::Utf16Le => EncodingDecision::Unsupported(UnsupportedEncoding {
                bom: Utf16BomKind::Le,
            }),
            BomKind::Utf16Be => EncodingDecision::Unsupported(UnsupportedEncoding {
                bom: Utf16BomKind::Be,
            }),
        });
    }

    if is_valid_utf8_prefix(bytes) {
        return Ok(EncodingDecision::Decided(DecidedEncoding {
            encoding: SelectedEncoding::Utf8,
            route: DetectionRoute::Utf8ValidatedNoBom,
            bom_len: 0,
            warnings: Vec::new(),
        }));
    }

    Ok(EncodingDecision::Decided(DecidedEncoding {
        encoding: SelectedEncoding::Windows(environment_ansi_codepage()),
        route: DetectionRoute::EnvironmentAnsi,
        bom_len: 0,
        warnings: Vec::new(),
    }))
}

/// 明示指定（`named` または `ansi_codepage`）が使われる場合の [`DecidedEncoding`]
/// を組み立てます。BOM が検出されていて、かつそれが明示指定と一致しない場合は
/// 警告を追加し、BOM を除去しません（暗黙の切り替えを避ける。doc コメント
/// 「明示指定と BOM が矛盾する場合の設計判断」を参照）。
fn build_explicit_decision(
    encoding: SelectedEncoding,
    kind: ProfileSpecifiedKind,
    detected_bom: Option<bom::DetectedBom>,
) -> DecidedEncoding {
    let mut warnings = Vec::new();
    let bom_len = match detected_bom {
        Some(detected) if bom_matches(detected.kind, encoding) => detected.len,
        Some(detected) => {
            warnings.push(EncodingWarning::bom_conflict(detected.kind, encoding));
            0
        }
        None => 0,
    };
    DecidedEncoding {
        encoding,
        route: DetectionRoute::ProfileSpecified(kind),
        bom_len,
        warnings,
    }
}

/// 検出した BOM の種類が、選択された文字コードと整合するか。
///
/// UTF-8 BOM と `SelectedEncoding::Utf8` の組だけが「整合」です。
/// `SelectedEncoding::Windows(_)` は BOM を持つ概念自体がないため、どんな BOM
/// を検出してもここでは「矛盾」として扱います。
fn bom_matches(bom: BomKind, encoding: SelectedEncoding) -> bool {
    matches!((bom, encoding), (BomKind::Utf8, SelectedEncoding::Utf8))
}

/// `bytes` の先頭 [`UTF8_AUTO_DETECT_PREFIX_BYTES`] バイト（`bytes` がそれより
/// 短い場合は `bytes` 全体）が妥当な UTF-8 かどうかを確認します。
///
/// 判定範囲の末尾でマルチバイト文字が途切れている場合（`Utf8Error::error_len`
/// が `None`）は、判定範囲を打ち切ったことによる見かけ上の不完全さであり
/// 実際に不正とは限らないため、妥当として扱います（寛容側に倒す）。これは
/// 判定用の軽量ヒューリスティックであり、実際のデコード（[`crate::decode`]）は
/// バイト列全体を対象に厳密な判定を行います。
fn is_valid_utf8_prefix(bytes: &[u8]) -> bool {
    let limit = bytes.len().min(UTF8_AUTO_DETECT_PREFIX_BYTES);
    match std::str::from_utf8(&bytes[..limit]) {
        Ok(_) => true,
        Err(err) => err.error_len().is_none(),
    }
}

/// 実行環境の Windows ANSI コードページ（`GetACP`）を取得します
/// （`ENC-005` 第4段階）。
#[cfg(windows)]
fn environment_ansi_codepage() -> u32 {
    crate::win32::environment_ansi_codepage()
}

/// Windows 以外でのビルド用の代替実装です。
///
/// 本リポジトリのビルド対象は `.cargo/config.toml` で `x86_64-pc-windows-msvc`
/// に固定されているため、この関数が実際に呼ばれることはありません。型として
/// コンパイルが通るようにするための最小限の代替値です。
#[cfg(not(windows))]
fn environment_ansi_codepage() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // encoding 名前指定の解釈。
    // ---------------------------------------------------------------

    #[test]
    fn parses_utf8_name_case_insensitively() {
        assert_eq!(parse_named_encoding("utf-8"), Ok(SelectedEncoding::Utf8));
        assert_eq!(parse_named_encoding("UTF-8"), Ok(SelectedEncoding::Utf8));
        assert_eq!(
            parse_named_encoding("  utf-8  "),
            Ok(SelectedEncoding::Utf8)
        );
    }

    #[test]
    fn parses_windows_codepage_name() {
        assert_eq!(
            parse_named_encoding("windows-932"),
            Ok(SelectedEncoding::Windows(932))
        );
        assert_eq!(
            parse_named_encoding("Windows-1252"),
            Ok(SelectedEncoding::Windows(1252))
        );
    }

    #[test]
    fn rejects_unknown_name() {
        assert!(parse_named_encoding("shift_jis").is_err());
        assert!(parse_named_encoding("windows-").is_err());
        assert!(parse_named_encoding("windows-abc").is_err());
        assert!(parse_named_encoding("").is_err());
    }

    // ---------------------------------------------------------------
    // detect_encoding: auto（明示指定なし）。
    // ---------------------------------------------------------------

    // 受け入れ条件: UTF-8 BOM ありの判定（ENC-005 第1段階）と経路の確認。
    #[test]
    fn auto_detects_utf8_bom() {
        let bytes = [0xEF, 0xBB, 0xBF, b'a', b'b', b'c'];
        let decision = detect_encoding(&bytes, &ProfileEncodingSetting::auto()).unwrap();
        let EncodingDecision::Decided(decided) = decision else {
            panic!("Decided を期待しました");
        };
        assert_eq!(decided.encoding, SelectedEncoding::Utf8);
        assert_eq!(decided.route, DetectionRoute::Utf8Bom);
        assert_eq!(decided.bom_len, 3);
        assert!(decided.warnings.is_empty());
    }

    // 受け入れ条件: BOM なし UTF-8 の妥当性確認（ENC-005 第2段階）。
    #[test]
    fn auto_detects_bomless_valid_utf8() {
        let bytes = "日本語のログ行です".as_bytes();
        let decision = detect_encoding(bytes, &ProfileEncodingSetting::auto()).unwrap();
        let EncodingDecision::Decided(decided) = decision else {
            panic!("Decided を期待しました");
        };
        assert_eq!(decided.encoding, SelectedEncoding::Utf8);
        assert_eq!(decided.route, DetectionRoute::Utf8ValidatedNoBom);
        assert_eq!(decided.bom_len, 0);
    }

    // 受け入れ条件: auto + ansi_codepage 未指定 → 環境 ANSI へフォールバック
    // （ENC-005 第4段階）。環境依存の実際の値は断定せず、経路とバリアントだけ
    // 確認する。
    #[test]
    fn auto_falls_back_to_environment_ansi_when_not_valid_utf8() {
        // 0x81 は CP932 の先頭バイトだが、後続がなく UTF-8 としても不正な
        // バイト列（妥当な UTF-8 の先頭バイトになり得ない範囲）。
        let bytes = [0x81, 0x00, 0x82];
        let decision = detect_encoding(&bytes, &ProfileEncodingSetting::auto()).unwrap();
        let EncodingDecision::Decided(decided) = decision else {
            panic!("Decided を期待しました");
        };
        assert_eq!(decided.route, DetectionRoute::EnvironmentAnsi);
        assert!(matches!(decided.encoding, SelectedEncoding::Windows(_)));
        assert_eq!(decided.bom_len, 0);
    }

    // 受け入れ条件: UTF-16 LE/BE の BOM → Unsupported（ENC-006）。
    #[test]
    fn auto_detects_utf16_bom_as_unsupported() {
        let le = [0xFF, 0xFE, 0x41, 0x00];
        let decision = detect_encoding(&le, &ProfileEncodingSetting::auto()).unwrap();
        assert_eq!(
            decision,
            EncodingDecision::Unsupported(UnsupportedEncoding {
                bom: Utf16BomKind::Le
            })
        );

        let be = [0xFE, 0xFF, 0x00, 0x41];
        let decision = detect_encoding(&be, &ProfileEncodingSetting::auto()).unwrap();
        assert_eq!(
            decision,
            EncodingDecision::Unsupported(UnsupportedEncoding {
                bom: Utf16BomKind::Be
            })
        );
    }

    // ---------------------------------------------------------------
    // detect_encoding: 明示指定あり。
    // ---------------------------------------------------------------

    // 受け入れ条件: ansi_codepage の明示指定が、BOM なしのバイト列で使われる。
    #[test]
    fn explicit_ansi_codepage_is_used_and_route_reported() {
        let bytes = [0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA]; // 「日本語」の CP932。
        let decision =
            detect_encoding(&bytes, &ProfileEncodingSetting::ansi_codepage(932)).unwrap();
        let EncodingDecision::Decided(decided) = decision else {
            panic!("Decided を期待しました");
        };
        assert_eq!(decided.encoding, SelectedEncoding::Windows(932));
        assert_eq!(
            decided.route,
            DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::AnsiCodepage)
        );
        assert!(decided.warnings.is_empty());
    }

    // 受け入れ条件: encoding 名前指定（windows-1252）が使われる。
    #[test]
    fn explicit_named_windows_encoding_is_used() {
        let bytes = [0x63, 0x61, 0x66, 0xE9]; // "caf" + é(cp1252)。
        let decision =
            detect_encoding(&bytes, &ProfileEncodingSetting::named("windows-1252")).unwrap();
        let EncodingDecision::Decided(decided) = decision else {
            panic!("Decided を期待しました");
        };
        assert_eq!(decided.encoding, SelectedEncoding::Windows(1252));
        assert_eq!(
            decided.route,
            DetectionRoute::ProfileSpecified(ProfileSpecifiedKind::NamedEncoding)
        );
    }

    // 受け入れ条件: BOM と明示指定の矛盾で警告が返り、明示指定が使われる
    // （UTF-8 BOM があるのに ansi_codepage 指定。4.3 の暫定設計）。
    #[test]
    fn bom_conflict_with_explicit_ansi_codepage_warns_and_keeps_explicit_choice() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM。
        bytes.extend_from_slice(&[0x93, 0xFA]); // 以降は CP932 の一部。
        let decision =
            detect_encoding(&bytes, &ProfileEncodingSetting::ansi_codepage(932)).unwrap();
        let EncodingDecision::Decided(decided) = decision else {
            panic!("Decided を期待しました");
        };
        assert_eq!(decided.encoding, SelectedEncoding::Windows(932));
        assert_eq!(
            decided.bom_len, 0,
            "矛盾時は BOM を暗黙に除去しないはず（明示指定のコードページで先頭から解釈する）"
        );
        assert_eq!(decided.warnings.len(), 1);
        assert_eq!(
            decided.warnings[0].kind,
            EncodingWarningKind::BomConflictsWithExplicitSetting
        );
        assert!(decided.warnings[0].message.contains("矛盾"));
    }

    // 矛盾なし（BOM と明示指定 UTF-8 が一致）の場合は警告が出ず、BOM が除去
    // される。
    #[test]
    fn utf8_bom_matches_explicit_utf8_without_warning() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"hello");
        let decision = detect_encoding(&bytes, &ProfileEncodingSetting::named("utf-8")).unwrap();
        let EncodingDecision::Decided(decided) = decision else {
            panic!("Decided を期待しました");
        };
        assert_eq!(decided.bom_len, 3);
        assert!(decided.warnings.is_empty());
    }

    // encoding 名前指定を解釈できない場合はエラーになる。
    #[test]
    fn invalid_named_encoding_is_an_error() {
        let bytes = b"abc";
        let result = detect_encoding(bytes, &ProfileEncodingSetting::named("shift_jis"));
        assert!(result.is_err());
    }
}
