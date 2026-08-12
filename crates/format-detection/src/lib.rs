#![deny(unsafe_op_in_unsafe_fn)]

//! Hakutaku の文字コード判定層です（P05-3、`tasks/phase-05-log-parsing-core.md`
//! 「文字コード」節、`ENC-001`〜`007`）。
//!
//! # このクレートの位置づけ
//!
//! ログファイルの生バイト列から、**どの文字コードとして読むべきか**（[`detect_encoding`]）
//! と、**実際に文字列へ変換する**（[`decode`]）処理を担う、コアに閉じた文字コード
//! 判定・デコード層です。プロファイル解決（`LOG-021`。どのファイルにどの
//! [`ProfileEncodingSetting`] を適用するか）や、実際のファイル読み込み経路への
//! 接続、不正バイトの元データ保持は本クレートの対象外であり、いずれも P05-6
//! （読み込み経路の実装）が行います。「後続課題」節も参照してください。
//!
//! # `ENC-005` の4段階判定
//!
//! [`detect_encoding`] は次の順で文字コードを決定します。
//!
//! 1. **明示指定の確認。** プロファイルの `encoding`（[`ProfileEncodingSetting::named`]）
//!    または `ansi_codepage`（[`ProfileEncodingSetting::ansi_codepage`]）のいずれかが
//!    指定されていれば、その指定を最優先で使用します。
//! 2. **UTF-8 BOM。** 明示指定がない場合、先頭が UTF-8 BOM（`EF BB BF`）なら UTF-8
//!    とみなします。
//! 3. **BOM なし UTF-8 の妥当性確認。** 先頭 [`UTF8_AUTO_DETECT_PREFIX_BYTES`]
//!    バイトが妥当な UTF-8 なら UTF-8 とみなします。
//! 4. **環境の Windows ANSI コードページ。** 上記のいずれにも該当しない場合、
//!    実行環境の既定 ANSI コードページ（`GetACP`）を使用します。
//!
//! ## 明示指定と BOM が矛盾する場合の設計判断（暫定設計、4.3）
//!
//! 要件の字面（`ENC-005`）だけを読むと「プロファイルの `ansi_codepage`」は
//! UTF-8 BOM／BOM なし UTF-8 妥当性確認より**後**の段階に見えますが、本実装は
//! 「明示指定（`encoding` 名前指定または `ansi_codepage`）があれば、BOM の
//! 有無に関わらず常に明示指定を優先する」という `tasks/phase-05-log-parsing-core.md`
//! の暫定設計（4.3）を採用しています。BOM と明示指定が矛盾する場合
//! （例: UTF-8 BOM があるのに `ansi_codepage: 932` が指定されている）は、
//! [`EncodingDecision`] に矛盾の警告（[`EncodingWarning`]）を含めつつ、
//! **暗黙に別のエンコーディングへ切り替えません**。警告を診断ログへ実際に
//! 出力するのは呼び出し側（読み込み経路、P05-6）の責務です。
//!
//! # 既知の限界: BOM なし ANSI が偶然 UTF-8 として妥当になる場合
//!
//! **BOM なしの ANSI バイト列が、たまたま妥当な UTF-8 バイト列としても解釈できる
//! 場合があります。** この場合、`auto`（明示指定なし）判定は UTF-8 と誤判定し、
//! 誤った文字化けした表示になります。これは実装上解消できない、仕様に内在する
//! 既知の限界です（`tasks/phase-05-log-parsing-core.md` 「リスクと未決事項」）。
//! 既知の生成元については、`auto` に依存せずプロファイルで `encoding` または
//! `ansi_codepage` を明示することを推奨します。
//!
//! # 対応対象外: UTF-16
//!
//! UTF-16 LE/BE の BOM（`FF FE` / `FE FF`）を検出した場合、明示指定がなければ
//! [`EncodingDecision::Unsupported`] を返します（`ENC-006`）。明示指定がある
//! 場合は、他の BOM 矛盾と同様に警告を返しつつ明示指定を使用します（上記の
//! 「明示指定と BOM が矛盾する場合の設計判断」と同じ扱い）。
//!
//! # デコードと不正バイトの扱い
//!
//! [`decode`] はデコードできないバイト列を置換文字（U+FFFD）へ変換しつつ処理を
//! 継続し、**元バイト列を破棄せず**、不正位置の一覧（上限
//! [`MAX_INVALID_POSITIONS`] 件）を [`DecodeOutcome`] へ含めます。実際に元バイト
//! 列を保持し続ける実装（読み込みバッファの設計）は P05-6 の対象です。
//!
//! 不正位置の特定粒度はエンコーディングにより異なります。
//!
//! - UTF-8: 標準ライブラリ（`str::from_utf8`）の判定に基づく、**バイト単位の
//!   正確な位置**です。
//! - 任意の Windows コードページ: `MultiByteToWideChar` を使い、
//!   [`WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES`] バイト単位の**近似**です。詳細な限界
//!   は同定数の doc コメントを参照してください。
//!
//! # 依存（`windows` クレートのみ、Windows 専用）
//!
//! 実行環境の ANSI コードページ取得（`GetACP`）、コードページの存在確認
//! （`GetCPInfoExW`）、コードページ変換（`MultiByteToWideChar`）はすべて
//! `windows` クレート経由で Win32 API を直接呼び出します（新規外部クレートを
//! 追加しない方針。`encoding_rs` 等は使用しません）。これらは
//! `[target.'cfg(windows)'.dependencies]` で宣言しており、本リポジトリの
//! ビルド対象は `.cargo/config.toml` で `x86_64-pc-windows-msvc` に固定して
//! いるため実質的に常に有効ですが、型としては Windows 以外でもコンパイルが
//! 通るよう `#[cfg(windows)]` で切り分けています。
//!
//! # 後続課題（P05-6 への引き継ぎ）
//!
//! 本クレートが提供するのは判定・デコードの純粋なロジックまでです。次は P05-6
//! （読み込み経路の実装）の対象です。
//!
//! - [`detect_encoding`] の呼び出しタイミング（ファイルを開いた直後、または
//!   プロファイル解決後）と、結果のプロファイル・データソースへの保持
//! - [`EncodingWarning`] を実際の診断ログ（`hakutaku-diagnostics`）へ出力する経路
//! - [`DecodeOutcome::invalid_positions`] を利用者向け表示（対象ファイル、位置、
//!   選択された文字コード）へつなぐ経路
//! - 不正バイトを含む元データ（デコード前のバイト列）を、表示・再デコードの
//!   ために実際に保持し続けるバッファ設計と、`hakutaku-memory-accounting` の
//!   予約 API との接続
//! - [`SelectedEncoding::Windows`] を選んだ場合の、メモリ予約（デコード後の
//!   `String` バッファ確保前の `MemoryBudget::reserve` 呼び出し）

mod bom;
mod decision;
mod decode;
#[cfg(windows)]
mod win32;

pub use decision::{
    detect_encoding, DecidedEncoding, DetectionRoute, EncodingDecision, EncodingWarning,
    EncodingWarningKind, InvalidEncodingNameError, ProfileEncodingSetting, ProfileSpecifiedKind,
    SelectedEncoding, UnsupportedEncoding, Utf16BomKind, UTF8_AUTO_DETECT_PREFIX_BYTES,
};
pub use decode::{decode, DecodeError, DecodeOutcome, MAX_INVALID_POSITIONS};
#[cfg(windows)]
pub use win32::WINDOWS_CODEPAGE_SCAN_CHUNK_BYTES;

/// 形式判定層が担う責務の表示名です。
pub const RESPONSIBILITY: &str = "形式判定";

#[cfg(test)]
mod tests {
    use super::RESPONSIBILITY;

    #[test]
    fn responsibility_is_explicit() {
        assert_eq!(RESPONSIBILITY, "形式判定");
    }
}
