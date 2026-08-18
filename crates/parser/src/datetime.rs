//! 6 書式（`LOG-DT-001`〜`006`）の日時解析、精度保持、比較キー、曖昧性検出
//! （P05-1、`tasks/phase-05-log-parsing-core.md` の「既知の日時書式」表）。
//!
//! # P04 の [`crate::ParsedTimestamp`] との関係
//!
//! P04 の `parse_line`／[`crate::ParsedTimestamp`]（`LOG-DT-001` の1書式・常に
//! ミリ秒精度）は、`crates/core-services` の呼び出し側（`loader.rs`）を壊さない
//! ため、フィールド・挙動を一切変えずに残しています。このモジュールは並行する
//! 新しい公開 API（[`DateTimeMatch`] 系）を追加するだけで、既存 API を置き換え
//! ません。呼び出し側をこの新 API へ差し替える作業は P05-6 の担当です。
//!
//! # 曖昧性検出と境界条件の設計（`LOG-022`、Issue #36）
//!
//! 自動判定（[`parse_datetime_auto`]）は 6 書式すべてを独立に試し、成立した
//! 書式の比較キーまたは消費長が異なる場合に [`AutoParseOutcome::Ambiguous`] を
//! 返します。愚直に「6書式を無条件に先頭一致で試す」だけだと、`LOG-DT-006`
//! （秒を持たない）が常に他の全書式の単なる短い接頭辞として成立してしまい、
//! `LOG-DT-001` のような曖昧でないはずの入力まで曖昧判定になってしまいます。
//! これを避けるため、各書式には「一致箇所の直後に続く文字を見て候補から
//! 除外する」境界条件（[`boundary_ok`]）を持たせています。
//!
//! 境界条件は、次の一つの原則で決めます（Issue #36 で6書式に対して対称化
//! しました。それ以前は `001`／`002` に境界条件が無く、4桁以上の小数秒が黙って
//! 3桁へ切り詰められていました）。
//!
//! > **わずかに合致しない入力を、黙って部分一致で成立させない。** 一致箇所の
//! > 直後に「その書式では読み取れない時刻の桁」が続く場合、その書式は候補に
//! > しません。ただし、その桁を読み取れる別の既知書式が同時に成立する場合は
//! > 候補に残し、判断を曖昧性検出（`LOG-022`）へ委ねます。
//!
//! 除外の結果どの書式も成立しなければ、その行は「日時なし」になります。直前に
//! 日時付き行があれば継続行として結合され（`LOG-014`）、無ければ日時未確定の
//! 独立した項目になります。**精度を黙って切り捨てた解析結果を返すより、解析
//! しないことを選ぶ**という判断です（`LOG-024`「元の精度を失わない」）。
//!
//! 書式ごとの内訳は次のとおりです。
//!
//! - `LOG-DT-001`／`002`（3桁ミリ秒）・`LOG-DT-003`／`004`（2桁の1/100秒）:
//!   直後がさらに数字であれば除外します。入力がその書式より細かい小数秒
//!   （マイクロ秒など）を持つことを意味し、採用すると残りの桁を黙って
//!   捨てることになるためです。
//! - `LOG-DT-005`（秒まで、小数なし）: 直後が数字（秒の桁の続き）または `.`
//!   （この文脈で小数点区切り以外の意味を持たない）であれば除外します。直後が
//!   `:` の場合は、そこから続く数字の桁数で分けます。**ちょうど2桁**のときだけ
//!   候補に残します（`LOG-DT-004` が必ず同時に成立するため、どちらの読み方が
//!   正しいかを推測せず曖昧性検出へ委ねられる）。1桁または3桁以上のときは、
//!   その桁を読み取れる既知書式が無く、採用すれば黙って捨てることになるため
//!   除外します。数字が続かない `:`（本文中の区切り）は、失う桁が無いため
//!   候補に残します。
//! - `LOG-DT-006`（分まで）: 直後が `:`、`.`、数字のいずれかであれば除外します。
//!   `:` は他の5書式すべてで秒の区切りとして使われ、`.` と数字は分より細かい
//!   桁の始まりであるため、いずれも `006` の独立した候補にはしません。
//!
//! この規則により、`LOG-DT-004` は成立するたびに必ず `LOG-DT-005` とも同時に
//! 成立します（`004` の小数点区切りは常に `:` であり、続く数字がちょうど2桁
//! なので `005` の除外条件に該当しないため）。これは意図した設計です。`LOG-DT-004`
//! 単独では「曖昧でない」自動判定結果を作れません（仕様の例そのものが表す
//! 性質のため）。単一書式を指定した解析（[`parse_datetime_with_format`]）では
//! 曖昧性の比較を行わないため、`LOG-DT-004` 単体の解析は通常どおり成功します。

use super::is_valid_calendar_date;

/// 既知の日時書式（`LOG-DT-001`〜`006`）です。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogDateTimeFormat {
    /// `YYYY/MM/DD HH:mm:ss.SSS`（ミリ秒）。
    LogDt001,
    /// `YYYY-MM-DD HH:mm:ss:SSS`（ミリ秒）。
    LogDt002,
    /// `YYYY/MM/DD HH:mm:ss.SS`（1/100秒）。
    LogDt003,
    /// `YYYY/MM/DD HH:mm:ss:SS`（1/100秒）。
    LogDt004,
    /// `YYYY/MM/DD HH:mm:ss`（秒）。
    LogDt005,
    /// `YYYY/MM/DD HH:mm`（分）。
    LogDt006,
}

impl LogDateTimeFormat {
    /// 既知の6書式すべてを、要件 ID の昇順（`LOG-DT-001`〜`006`）で並べた一覧
    /// です。書式選択 UI の選択肢を組み立てる用途（P07）を想定して
    /// います。
    ///
    /// 呼び出し側が6書式を自前の定数表として持つと、書式を追加したときに
    /// 更新漏れが起きます。この一覧を単一の出所にすることでそれを防ぎます。
    pub const ALL: [LogDateTimeFormat; 6] = [
        LogDateTimeFormat::LogDt001,
        LogDateTimeFormat::LogDt002,
        LogDateTimeFormat::LogDt003,
        LogDateTimeFormat::LogDt004,
        LogDateTimeFormat::LogDt005,
        LogDateTimeFormat::LogDt006,
    ];

    /// 要件 ID（`LOG-DT-001` など）を返します。
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            LogDateTimeFormat::LogDt001 => "LOG-DT-001",
            LogDateTimeFormat::LogDt002 => "LOG-DT-002",
            LogDateTimeFormat::LogDt003 => "LOG-DT-003",
            LogDateTimeFormat::LogDt004 => "LOG-DT-004",
            LogDateTimeFormat::LogDt005 => "LOG-DT-005",
            LogDateTimeFormat::LogDt006 => "LOG-DT-006",
        }
    }

    /// 書式パターン文字列（診断・UI 表示向け）を返します。
    #[must_use]
    pub fn pattern(&self) -> &'static str {
        match self {
            LogDateTimeFormat::LogDt001 => "YYYY/MM/DD HH:mm:ss.SSS",
            LogDateTimeFormat::LogDt002 => "YYYY-MM-DD HH:mm:ss:SSS",
            LogDateTimeFormat::LogDt003 => "YYYY/MM/DD HH:mm:ss.SS",
            LogDateTimeFormat::LogDt004 => "YYYY/MM/DD HH:mm:ss:SS",
            LogDateTimeFormat::LogDt005 => "YYYY/MM/DD HH:mm:ss",
            LogDateTimeFormat::LogDt006 => "YYYY/MM/DD HH:mm",
        }
    }

    /// 要件 ID の文字列から書式へ戻します（[`Self::id`] の逆変換）。既知の
    /// 6書式のいずれにも一致しなければ `None` です。
    ///
    /// 設定ファイル（`hakutaku_config::DateTimeFormatSetting`）ではなく、
    /// 実行時に外から渡される ID 文字列——UI の書式選択（P07）が
    /// Tauri コマンド経由で送る値——を受け取るための入口です。呼び出し側は
    /// `None` を「利用者が選べる範囲の外にある値」として扱い、推測で
    /// どれかの書式へ寄せないでください（`LOG-022` の「貪欲に推測しない」）。
    ///
    /// 綴りの対応表を [`Self::id`] と二重に持つと片方だけ更新される事故が
    /// 起きるため、[`Self::ALL`] を `id()` で線形に走査します。要素数6の
    /// 走査であり、呼び出しは1ファイルにつき1回のため速度は問題になりません。
    #[must_use]
    pub fn from_id(id: &str) -> Option<LogDateTimeFormat> {
        Self::ALL.iter().copied().find(|format| format.id() == id)
    }
}

/// 解析できた日時の元の精度です（`LOG-024`: 表示時に失わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Precision {
    /// ミリ秒（`LOG-DT-001`／`002`）。
    Millisecond,
    /// 1/100秒（`LOG-DT-003`／`004`）。`LOG-025` によりミリ秒へ10倍展開済み。
    Centisecond,
    /// 秒（`LOG-DT-005`）。
    Second,
    /// 分（`LOG-DT-006`）。
    Minute,
}

/// ミリ秒精度へ正規化した比較キーです（`LOG-024`）。
///
/// 書式に含まれない下位桁は 0 として補完済みの値から構築されます。`Ord` を
/// 実装しているため、P09 の時系列マージがそのまま全順序比較に使えます。
///
/// **タイムゾーン変換を行いません。** 既知の6書式はいずれもタイムゾーン表記を
/// 持たず、Hakutaku はそれらを端末のローカル時刻として解釈します（`LOG-012`）。
/// この型は、そのローカル壁時計の値をそのまま数値の一次元へ写して大小比較を
/// できるようにするだけのもので、絶対時刻（UTC の瞬間）を表しません。異なる
/// タイムゾーンで記録されたログを混在させても、この値は記録された壁時計の
/// 順序で並びます（`LOG-016`: 時刻ずれ補正を行わない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComparisonKey(i64);

impl ComparisonKey {
    /// 年月日時分秒とミリ秒精度に正規化済みのミリ秒値から比較キーを構築します。
    fn from_parts(
        year: u32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        millisecond: u32,
    ) -> Self {
        let days = days_from_civil(i64::from(year), i64::from(month), i64::from(day));
        let millis_of_day = i64::from(hour) * 3_600_000
            + i64::from(minute) * 60_000
            + i64::from(second) * 1_000
            + i64::from(millisecond);
        ComparisonKey(days * 86_400_000 + millis_of_day)
    }

    /// 内部値を返します。診断・デバッグ表示、およびテストでの検証向けです。
    ///
    /// 値は、解析した**ローカル壁時計の**年月日時分秒ミリ秒を、
    /// `1970-01-01T00:00:00.000` を起点とする経過ミリ秒として換算したものです。
    /// タイムゾーン変換を行わないため（[`ComparisonKey`] の doc コメント）、
    /// 同じ壁時計の値を UTC として解釈した場合の Unix 時刻と数値は一致しますが、
    /// 意味は「その瞬間の絶対時刻」ではありません。呼び出し側はこの値を比較・
    /// 差分計算にだけ使い、絶対時刻として外部へ渡さないでください。
    #[must_use]
    pub fn as_millis_since_epoch(&self) -> i64 {
        self.0
    }
}

/// グレゴリオ暦の年月日から、1970-01-01 を基準とした通日（エポック日数）への
/// 変換です。Howard Hinnant の `days_from_civil` アルゴリズム
/// （<http://howardhinnant.github.io/date_algorithms.html>）に基づく整数演算
/// のみの実装で、外部クレート（`chrono` 等）に依存しません。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (month + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// 1件の日時解析結果です（元の精度・原文を保持したまま、正規化した比較キーを
/// 別フィールドとして持ちます。`LOG-024`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateTimeMatch {
    /// 一致した書式。
    pub format: LogDateTimeFormat,
    /// 元の精度（表示時に失わない。`LOG-024`）。
    pub precision: Precision,
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// ミリ秒。書式に含まれない場合は 0、1/100秒書式は10倍展開済み（`LOG-025`）。
    pub millisecond: u16,
    /// ミリ秒精度へ正規化した比較キー（`LOG-024`）。
    pub comparison_key: ComparisonKey,
    /// 一致した原文のバイト長（曖昧性判定の「消費長」に使う）。
    ///
    /// 原文そのもの（かつての `matched_text`）は保持しません。呼び出し側は
    /// 解析対象の文字列を手元に持っており、必要なら `text[..matched_len]` で
    /// 同じ内容を借用できるため、解析1件ごとに `String` を確保する価値が
    /// ありませんでした。表示用の文字列は元の精度を保って
    /// 再構成する [`DateTimeMatch::to_display_string`] を使います。
    pub matched_len: usize,
}

impl DateTimeMatch {
    /// 元の精度を保った表示用文字列を返します（`LOG-024`: `15:12` が
    /// `15:12:00.000` に書き換わって見えないようにするための表示）。
    #[must_use]
    pub fn to_display_string(&self) -> String {
        match self.precision {
            Precision::Millisecond => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
                self.year,
                self.month,
                self.day,
                self.hour,
                self.minute,
                self.second,
                self.millisecond
            ),
            Precision::Centisecond => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:02}",
                self.year,
                self.month,
                self.day,
                self.hour,
                self.minute,
                self.second,
                self.millisecond / 10
            ),
            Precision::Second => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                self.year, self.month, self.day, self.hour, self.minute, self.second
            ),
            Precision::Minute => format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}",
                self.year, self.month, self.day, self.hour, self.minute
            ),
        }
    }
}

/// 自動判定（[`parse_datetime_auto`]）の結果です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoParseOutcome {
    /// 一意に決まった（該当書式がただ一つ、または複数書式が同じ比較キー・
    /// 同じ消費長になった場合を含む）。
    Matched(DateTimeMatch),
    /// どの書式にも一致しなかった。
    NoMatch,
    /// 複数の書式が異なる結果（比較キーまたは消費長）で同時に成立した
    /// （`LOG-022`）。成立した書式すべてを含みます。
    Ambiguous(Vec<DateTimeMatch>),
}

/// 小数点以下の区切り文字と桁数です。
struct Fraction {
    sep: u8,
    digits: u8,
}

/// 1書式の構造定義（区切り文字・桁数・精度の対応）です。
struct FormatSpec {
    format: LogDateTimeFormat,
    /// 年月日の区切り文字（`/` または `-`）。
    date_sep: u8,
    /// 秒を持つか（`LOG-DT-006` のみ `false`）。
    has_seconds: bool,
    /// 秒未満の小数部（`None` なら `LOG-DT-005`／`006`）。
    fraction: Option<Fraction>,
}

impl FormatSpec {
    /// 固定長の合計バイト数です（`YYYY/MM/DD HH:mm` の16文字を基本に、
    /// 秒・小数部の有無で加算します）。
    fn total_len(&self) -> usize {
        const BASE_LEN: usize = 16; // "YYYY/MM/DD HH:mm"
        if !self.has_seconds {
            return BASE_LEN;
        }
        const WITH_SECONDS_LEN: usize = BASE_LEN + 3; // ":ss"
        match &self.fraction {
            None => WITH_SECONDS_LEN,
            Some(fraction) => WITH_SECONDS_LEN + 1 + usize::from(fraction.digits),
        }
    }

    /// この書式が表す元の時刻精度です。
    fn precision(&self) -> Precision {
        match &self.fraction {
            Some(fraction) if fraction.digits == 3 => Precision::Millisecond,
            Some(fraction) if fraction.digits == 2 => Precision::Centisecond,
            Some(_) => unreachable!("書式定義は2桁または3桁の小数部のみを持つ"),
            None if self.has_seconds => Precision::Second,
            None => Precision::Minute,
        }
    }
}

/// 既知の6書式の定義です（表の順、`LOG-DT-001`〜`006`）。
const FORMAT_SPECS: [FormatSpec; 6] = [
    FormatSpec {
        format: LogDateTimeFormat::LogDt001,
        date_sep: b'/',
        has_seconds: true,
        fraction: Some(Fraction {
            sep: b'.',
            digits: 3,
        }),
    },
    FormatSpec {
        format: LogDateTimeFormat::LogDt002,
        date_sep: b'-',
        has_seconds: true,
        fraction: Some(Fraction {
            sep: b':',
            digits: 3,
        }),
    },
    FormatSpec {
        format: LogDateTimeFormat::LogDt003,
        date_sep: b'/',
        has_seconds: true,
        fraction: Some(Fraction {
            sep: b'.',
            digits: 2,
        }),
    },
    FormatSpec {
        format: LogDateTimeFormat::LogDt004,
        date_sep: b'/',
        has_seconds: true,
        fraction: Some(Fraction {
            sep: b':',
            digits: 2,
        }),
    },
    FormatSpec {
        format: LogDateTimeFormat::LogDt005,
        date_sep: b'/',
        has_seconds: true,
        fraction: None,
    },
    FormatSpec {
        format: LogDateTimeFormat::LogDt006,
        date_sep: b'/',
        has_seconds: false,
        fraction: None,
    },
];

/// 一致箇所の直後に続くバイト列から、その書式を候補として採用してよいかを
/// 判定します。判断の原則と書式ごとの内訳は、モジュール冒頭のドキュメント
/// 「曖昧性検出と境界条件の設計」を参照してください。
///
/// `rest` は一致箇所の直後から行末までのバイト列です（一致箇所が行末で
/// 終わっていれば空）。直後の1バイトだけでは `LOG-DT-005` の `:` を判断
/// できない——続く数字がちょうど2桁なら `LOG-DT-004` も同時に成立し、それ
/// 以外の桁数なら読み取れる書式が無い——ため、1バイトではなく残り全体を
/// 受け取ります。
fn boundary_ok(format: LogDateTimeFormat, rest: &[u8]) -> bool {
    let next_byte = rest.first().copied();
    match format {
        // 小数秒を持つ4書式は、直後がさらに数字なら「入力の方が細かい精度を
        // 持つ」ことの証拠なので候補にしない（Issue #36）。
        LogDateTimeFormat::LogDt001
        | LogDateTimeFormat::LogDt002
        | LogDateTimeFormat::LogDt003
        | LogDateTimeFormat::LogDt004 => !matches!(next_byte, Some(byte) if byte.is_ascii_digit()),
        LogDateTimeFormat::LogDt005 => match next_byte {
            // 秒の桁がまだ続いている（`15:12:234` など）。どの既知書式でも
            // 読み取れない桁であり、採用すれば黙って捨てることになる。
            Some(byte) if byte.is_ascii_digit() => false,
            // `.` は小数点区切りとして一義的。`.SSS`／`.SS` は 001／003 が
            // 読み取り、それ以外の桁数はどの書式でも読み取れない。
            Some(b'.') => false,
            // `:` は小数点区切り（`LOG-DT-004`）にも本文の区切りにもなり得る
            // ため、続く数字の桁数で判断する。0桁（本文の区切り）は失う桁が
            // 無いので許容し、2桁は 004 も成立して曖昧性検出（`LOG-022`）が
            // 推測せず利用者へ返すので許容する。それ以外は除外する。
            Some(b':') => {
                let digits = leading_digit_run(&rest[1..]);
                digits == 0 || digits == 2
            }
            _ => true,
        },
        // 分までの書式は、秒の区切り（`:`）・小数点（`.`）・分の桁の続き
        // （数字）のいずれが来ても、より細かい桁を捨てることになるため
        // 独立した候補にしない。
        LogDateTimeFormat::LogDt006 => {
            !matches!(next_byte, Some(byte) if byte == b':' || byte == b'.' || byte.is_ascii_digit())
        }
    }
}

/// 先頭から連続する ASCII 数字の個数を返します（[`boundary_ok`] が
/// 「直後に何桁の数字が続くか」で書式を分けるために使います）。
fn leading_digit_run(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

/// 年月日時分（共通部分、`YYYY/MM/DD HH:mm` の12桁）が数字であるべき相対位置。
const BASE_DIGIT_POSITIONS: [usize; 12] = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15];

/// 1つの書式定義に基づき、行頭の日時を解析します。
///
/// 桁数固定・区切り厳密です。長さ不足、区切り不一致、数字であるべき位置に
/// 数字がない、暦として不正、または [`boundary_ok`] の境界条件を満たさない
/// 場合は `None` を返します。
fn parse_with_spec(spec: &FormatSpec, text: &str) -> Option<DateTimeMatch> {
    let bytes = text.as_bytes();
    let total_len = spec.total_len();
    if bytes.len() < total_len {
        return None;
    }
    let p = &bytes[..total_len];

    if p[4] != spec.date_sep || p[7] != spec.date_sep || p[10] != b' ' || p[13] != b':' {
        return None;
    }
    if !BASE_DIGIT_POSITIONS
        .iter()
        .all(|&index| p[index].is_ascii_digit())
    {
        return None;
    }

    let digit = |index: usize| u32::from(p[index] - b'0');
    let year = digit(0) * 1000 + digit(1) * 100 + digit(2) * 10 + digit(3);
    let month = digit(5) * 10 + digit(6);
    let day = digit(8) * 10 + digit(9);
    let hour = digit(11) * 10 + digit(12);
    let minute = digit(14) * 10 + digit(15);

    let (second, millisecond) = if spec.has_seconds {
        if p[16] != b':' {
            return None;
        }
        if !(p[17].is_ascii_digit() && p[18].is_ascii_digit()) {
            return None;
        }
        let second = digit(17) * 10 + digit(18);

        match &spec.fraction {
            Some(fraction) => {
                if p[19] != fraction.sep {
                    return None;
                }
                let start = 20usize;
                let end = start + usize::from(fraction.digits);
                if !(start..end).all(|index| p[index].is_ascii_digit()) {
                    return None;
                }
                let mut value: u32 = 0;
                for index in start..end {
                    value = value * 10 + digit(index);
                }
                // LOG-025: 秒未満2桁は1/100秒。ミリ秒へ10倍展開する。
                let millisecond = if fraction.digits == 2 {
                    value * 10
                } else {
                    value
                };
                (second, millisecond)
            }
            None => (second, 0),
        }
    } else {
        (0, 0)
    };

    if !is_valid_calendar_date(year, month, day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    if !boundary_ok(spec.format, &bytes[total_len..]) {
        return None;
    }

    let comparison_key =
        ComparisonKey::from_parts(year, month, day, hour, minute, second, millisecond);

    Some(DateTimeMatch {
        format: spec.format,
        precision: spec.precision(),
        year: year as u16,
        month: month as u8,
        day: day as u8,
        hour: hour as u8,
        minute: minute as u8,
        second: second as u8,
        millisecond: millisecond as u16,
        comparison_key,
        matched_len: total_len,
    })
}

/// 指定した1書式だけで行頭の日時を解析します。
///
/// 曖昧性の比較は行いません（他の書式が同時に成立するかどうかに関わらず、
/// 指定した書式で解析できればそのまま返します）。プロファイルで書式が
/// 一意に決まっている場合（`LOG-013`・`LOG-021`）に使う想定です。
#[must_use]
pub fn parse_datetime_with_format(format: LogDateTimeFormat, text: &str) -> Option<DateTimeMatch> {
    let spec = FORMAT_SPECS.iter().find(|spec| spec.format == format)?;
    parse_with_spec(spec, text)
}

/// 6書式すべてを試し、自動判定します（`LOG-022`）。
///
/// - どの書式にも一致しない場合 [`AutoParseOutcome::NoMatch`]。
/// - 一意に決まる場合（該当書式が一つだけ、または複数書式が同じ比較キー・
///   同じ消費長になる場合を含む） [`AutoParseOutcome::Matched`]。
/// - 複数の書式が異なる結果で同時に成立する場合、貪欲に長い方を選ばず
///   [`AutoParseOutcome::Ambiguous`]（成立した書式すべてを含む）。
#[must_use]
pub fn parse_datetime_auto(text: &str) -> AutoParseOutcome {
    let matches: Vec<DateTimeMatch> = FORMAT_SPECS
        .iter()
        .filter_map(|spec| parse_with_spec(spec, text))
        .collect();

    match matches.len() {
        0 => AutoParseOutcome::NoMatch,
        1 => AutoParseOutcome::Matched(
            matches
                .into_iter()
                .next()
                .expect("直前の match で長さ1を確認済み"),
        ),
        _ => {
            let first_key = matches[0].comparison_key;
            let first_len = matches[0].matched_len;
            let all_equivalent = matches
                .iter()
                .all(|m| m.comparison_key == first_key && m.matched_len == first_len);

            if all_equivalent {
                AutoParseOutcome::Matched(
                    matches
                        .into_iter()
                        .next()
                        .expect("空でないことを確認済み（len() >= 2）"),
                )
            } else {
                AutoParseOutcome::Ambiguous(matches)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ条件: from_id が id() の逆変換として往復し、ALL が
    // 6書式すべてを重複なく含む。UI へ返す書式一覧と、UI から戻ってくる ID の
    // 綴りが、この2つの経路で食い違わないことを保証する。
    #[test]
    fn from_id_round_trips_with_id_for_all_formats() {
        assert_eq!(LogDateTimeFormat::ALL.len(), 6);

        for format in LogDateTimeFormat::ALL {
            assert_eq!(
                LogDateTimeFormat::from_id(format.id()),
                Some(format),
                "{} は自身の ID から復元できるはず",
                format.id()
            );
            assert!(
                !format.pattern().is_empty(),
                "{} の表示用パターンが空になっている",
                format.id()
            );
        }

        let ids: std::collections::HashSet<&'static str> =
            LogDateTimeFormat::ALL.iter().map(|f| f.id()).collect();
        assert_eq!(ids.len(), 6, "ALL の要件 ID に重複がある");
    }

    // 受け入れ条件（LOG-022）: 既知の6書式以外の ID は None になる。
    // 呼び出し側が推測でどれかの書式へ寄せられないようにするため。
    #[test]
    fn from_id_rejects_unknown_ids() {
        for unknown in ["", "LOG-DT-000", "LOG-DT-007", "log-dt-001", "auto"] {
            assert_eq!(
                LogDateTimeFormat::from_id(unknown),
                None,
                "{unknown} は既知の書式ではないはず"
            );
        }
    }

    // 受け入れ条件: 6書式それぞれを単一書式指定で解析でき、比較キーが仕様の
    // 表と一致する（LOG-DT-001 の比較キーを基準に、他の書式が同じ時刻を
    // 指す場合は同じ比較キーになることで検証する）。
    #[test]
    fn parses_all_six_formats_with_matching_comparison_keys() {
        let reference =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:23.456")
                .expect("LOG-DT-001 は解析できるはず");
        assert_eq!(reference.precision, Precision::Millisecond);
        assert_eq!(reference.millisecond, 456);

        let dt002 =
            parse_datetime_with_format(LogDateTimeFormat::LogDt002, "2026-07-28 15:12:23:456")
                .expect("LOG-DT-002 は解析できるはず");
        assert_eq!(dt002.precision, Precision::Millisecond);
        assert_eq!(dt002.comparison_key, reference.comparison_key);

        let dt003 =
            parse_datetime_with_format(LogDateTimeFormat::LogDt003, "2026/07/28 15:12:23.45")
                .expect("LOG-DT-003 は解析できるはず");
        assert_eq!(dt003.precision, Precision::Centisecond);
        assert_eq!(dt003.millisecond, 450, "LOG-025: 1/100秒はミリ秒へ10倍展開");

        let dt004 =
            parse_datetime_with_format(LogDateTimeFormat::LogDt004, "2026/07/28 15:12:23:45")
                .expect("LOG-DT-004 は解析できるはず");
        assert_eq!(dt004.precision, Precision::Centisecond);
        assert_eq!(dt004.comparison_key, dt003.comparison_key);

        let dt005 = parse_datetime_with_format(LogDateTimeFormat::LogDt005, "2026/07/28 15:12:23")
            .expect("LOG-DT-005 は解析できるはず");
        assert_eq!(dt005.precision, Precision::Second);
        assert_eq!(dt005.second, 23);

        let dt006 = parse_datetime_with_format(LogDateTimeFormat::LogDt006, "2026/07/28 15:12")
            .expect("LOG-DT-006 は解析できるはず");
        assert_eq!(dt006.precision, Precision::Minute);
        assert_eq!(dt006.minute, 12);
    }

    // 受け入れ条件: 15:12:23.45 と 15:12:23.450 が同一の比較キーになる（LOG-025）。
    #[test]
    fn centisecond_and_millisecond_forty_five_share_comparison_key() {
        let centisecond =
            parse_datetime_with_format(LogDateTimeFormat::LogDt003, "2026/07/28 15:12:23.45")
                .expect("解析できるはず");
        let millisecond =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:23.450")
                .expect("解析できるはず");

        assert_eq!(
            centisecond.comparison_key, millisecond.comparison_key,
            ".45 は 450ミリ秒であり、.450 と同一の比較キーになるはず"
        );
        assert_ne!(
            centisecond.comparison_key.as_millis_since_epoch(),
            0,
            "比較キーは意味のある値を持つはず（境界値の誤検出防止）"
        );
    }

    // 受け入れ条件: 15:12 の比較キーが 15:12:00.000 相当になる（LOG-024:
    // 書式に含まれない下位桁は0補完）。
    #[test]
    fn minute_precision_comparison_key_equals_zero_padded_seconds_and_millis() {
        let minute_only =
            parse_datetime_with_format(LogDateTimeFormat::LogDt006, "2026/07/28 15:12")
                .expect("解析できるはず");
        let zero_padded =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:00.000")
                .expect("解析できるはず");

        assert_eq!(minute_only.comparison_key, zero_padded.comparison_key);
    }

    // 受け入れ条件: 精度情報が保持され、表示用文字列が元の精度のまま
    // （15:12 が 15:12:00.000 に書き換わって見えない）。
    #[test]
    fn display_string_preserves_original_precision() {
        let minute_only =
            parse_datetime_with_format(LogDateTimeFormat::LogDt006, "2026/07/28 15:12")
                .expect("解析できるはず");
        assert_eq!(minute_only.to_display_string(), "2026-07-28T15:12");

        let second_only =
            parse_datetime_with_format(LogDateTimeFormat::LogDt005, "2026/07/28 15:12:23")
                .expect("解析できるはず");
        assert_eq!(second_only.to_display_string(), "2026-07-28T15:12:23");

        let centisecond =
            parse_datetime_with_format(LogDateTimeFormat::LogDt003, "2026/07/28 15:12:23.45")
                .expect("解析できるはず");
        assert_eq!(centisecond.to_display_string(), "2026-07-28T15:12:23.45");

        let millisecond =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:23.456")
                .expect("解析できるはず");
        assert_eq!(millisecond.to_display_string(), "2026-07-28T15:12:23.456");
    }

    // 受け入れ条件: HH:mm:ss:SS（LOG-DT-004）と HH:mm:ss（LOG-DT-005）の両方が
    // 成立する入力を自動判定で推測せず、Ambiguous として返す（成立書式の一覧を
    // 含む）。計画書の例そのもの。
    #[test]
    fn auto_detects_ambiguous_input_between_log_dt_004_and_log_dt_005() {
        let outcome = parse_datetime_auto("2026/07/28 15:12:23:45");

        match outcome {
            AutoParseOutcome::Ambiguous(candidates) => {
                assert_eq!(candidates.len(), 2, "成立書式は004と005の2つのはず");
                let formats: Vec<LogDateTimeFormat> = candidates.iter().map(|c| c.format).collect();
                assert!(formats.contains(&LogDateTimeFormat::LogDt004));
                assert!(formats.contains(&LogDateTimeFormat::LogDt005));

                let dt004 = candidates
                    .iter()
                    .find(|c| c.format == LogDateTimeFormat::LogDt004)
                    .expect("004が含まれるはず");
                let dt005 = candidates
                    .iter()
                    .find(|c| c.format == LogDateTimeFormat::LogDt005)
                    .expect("005が含まれるはず");
                assert_ne!(
                    dt004.comparison_key, dt005.comparison_key,
                    "004は45/100秒を消費するが005は消費しないため比較キーが異なるはず"
                );
                assert_ne!(dt004.matched_len, dt005.matched_len);
            }
            other => panic!("Ambiguous になるはずが {other:?} だった"),
        }
    }

    // 受け入れ条件: 曖昧でないケース（LOG-DT-001 等）が通常成功する。
    #[test]
    fn auto_detects_unambiguous_log_dt_001_with_trailing_message() {
        let outcome = parse_datetime_auto("2026/07/28 15:12:23.456 起動しました");

        match outcome {
            AutoParseOutcome::Matched(m) => {
                assert_eq!(m.format, LogDateTimeFormat::LogDt001);
                assert_eq!(m.millisecond, 456);
            }
            other => panic!("Matched になるはずが {other:?} だった"),
        }
    }

    // 受け入れ条件: 曖昧でないケース（他の書式でも同様に一意に決まる）。
    #[test]
    fn auto_detects_unambiguous_cases_for_remaining_formats() {
        let cases = [
            ("2026-07-28 15:12:23:456", LogDateTimeFormat::LogDt002),
            ("2026/07/28 15:12:23.45", LogDateTimeFormat::LogDt003),
            ("2026/07/28 15:12:23", LogDateTimeFormat::LogDt005),
            ("2026/07/28 15:12", LogDateTimeFormat::LogDt006),
        ];

        for (input, expected_format) in cases {
            match parse_datetime_auto(input) {
                AutoParseOutcome::Matched(m) => {
                    assert_eq!(m.format, expected_format, "入力: {input}");
                }
                other => panic!("入力 {input} は Matched になるはずが {other:?} だった"),
            }
        }
    }

    // 受け入れ条件: 不正な暦（2/30、13月）は拒否される。
    #[test]
    fn rejects_invalid_calendar_dates() {
        assert!(
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/02/30 00:00:00.000")
                .is_none()
        );
        assert!(
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/13/01 00:00:00.000")
                .is_none()
        );
        assert!(matches!(
            parse_datetime_auto("2026/02/30 00:00:00.000"),
            AutoParseOutcome::NoMatch
        ));
    }

    // 受け入れ条件: 桁不足は拒否される。
    #[test]
    fn rejects_insufficient_digits() {
        // 月が1桁（桁数固定に反する）。
        assert!(
            parse_datetime_with_format(LogDateTimeFormat::LogDt005, "2026/7/28 15:12:23").is_none()
        );
        assert!(matches!(
            parse_datetime_auto("2026/7/28 15:12:23"),
            AutoParseOutcome::NoMatch
        ));
        // ミリ秒が2桁しかない（LOG-DT-001 としては桁不足。LOG-DT-003 としては
        // 成立するので NoMatch にはならない点に注意）。
        assert!(
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:23.45")
                .is_none()
        );
    }

    // 受け入れ条件: 区切り不一致は拒否される。
    #[test]
    fn rejects_mismatched_separators() {
        // 年月日の区切りがどの書式のものとも一致しない。
        assert!(matches!(
            parse_datetime_auto("2026.07.28 15:12:23.456"),
            AutoParseOutcome::NoMatch
        ));
        // LOG-DT-005 を "-" 区切りで指定しても一致しない（005 は "/" 固定）。
        assert!(
            parse_datetime_with_format(LogDateTimeFormat::LogDt005, "2026-07-28 15:12:23")
                .is_none()
        );
    }

    // 受け入れ条件: 比較キーの Ord（同時刻・異精度の同値、前後関係）。
    #[test]
    fn comparison_key_ord_reflects_chronological_order_and_precision_equivalence() {
        let earlier =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:23.000")
                .expect("解析できるはず");
        let later =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:24.000")
                .expect("解析できるはず");
        assert!(earlier.comparison_key < later.comparison_key);

        // 同時刻・異精度（分のみ vs ミリ秒すべて0）は同値。
        let minute_only =
            parse_datetime_with_format(LogDateTimeFormat::LogDt006, "2026/07/28 15:12")
                .expect("解析できるはず");
        let zero_padded =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:00.000")
                .expect("解析できるはず");
        assert_eq!(minute_only.comparison_key, zero_padded.comparison_key);

        // 日をまたぐ前後関係。
        let end_of_day =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 23:59:59.999")
                .expect("解析できるはず");
        let next_day_start =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/29 00:00:00.000")
                .expect("解析できるはず");
        assert!(end_of_day.comparison_key < next_day_start.comparison_key);

        // 単純な整列でも前後関係が保たれる。
        let mut keys = vec![
            later.comparison_key,
            next_day_start.comparison_key,
            earlier.comparison_key,
            end_of_day.comparison_key,
        ];
        keys.sort();
        assert_eq!(
            keys,
            vec![
                earlier.comparison_key,
                later.comparison_key,
                end_of_day.comparison_key,
                next_day_start.comparison_key,
            ]
        );
    }

    // 境界値: 空文字列・短すぎる入力はどの書式にも一致しない。
    #[test]
    fn empty_and_too_short_input_yields_no_match() {
        assert!(matches!(parse_datetime_auto(""), AutoParseOutcome::NoMatch));
        assert!(matches!(
            parse_datetime_auto("2026/07/28 15"),
            AutoParseOutcome::NoMatch
        ));
    }

    // LOG-DT-004 は成立するたびに必ず LOG-DT-005 とも同時に成立する（境界条件の
    // 設計上の帰結。モジュールドキュメント参照）。単一書式指定なら曖昧性の
    // 比較を行わないため通常どおり成功する。
    #[test]
    fn log_dt_004_succeeds_standalone_via_explicit_format_even_though_auto_is_ambiguous() {
        let explicit =
            parse_datetime_with_format(LogDateTimeFormat::LogDt004, "2026/07/28 15:12:23:45");
        assert!(
            explicit.is_some(),
            "単一書式指定では曖昧性を判定しないので成功するはず"
        );

        assert!(matches!(
            parse_datetime_auto("2026/07/28 15:12:23:45"),
            AutoParseOutcome::Ambiguous(_)
        ));
    }

    // 受け入れ条件（Issue #36 ①、`LOG-024`）: 小数4桁以上（マイクロ秒など）は
    // 3桁へ黙って切り捨てず、どの書式にも一致しない。自動判定はファイル先頭で
    // 最初に一致した行の書式を全体へ適用するため、ここで部分一致を許すと
    // ファイル全体がミリ秒へ切り詰められる。
    #[test]
    fn sub_millisecond_precision_is_rejected_instead_of_truncated() {
        assert!(
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "2026/07/28 15:12:23.4567")
                .is_none(),
            "小数4桁を3桁として部分一致させない"
        );
        assert!(parse_datetime_with_format(
            LogDateTimeFormat::LogDt002,
            "2026-07-28 15:12:23:4567"
        )
        .is_none());

        for input in [
            "2026/07/28 15:12:23.4567",
            "2026/07/28 15:12:23.4567 起動しました",
            "2026/07/28 15:12:23.456789",
            "2026-07-28 15:12:23:4567",
            "2026-07-28 15:12:23:456789 起動しました",
        ] {
            assert!(
                matches!(parse_datetime_auto(input), AutoParseOutcome::NoMatch),
                "入力 {input} は日時なし（継続行へ吸収）になるはず"
            );
        }
    }

    // 受け入れ条件（Issue #36 ②、`LOG-007`）: `YYYY/MM/DD HH:mm:ss:SSS`
    // （スラッシュ日付＋コロンミリ秒）は既知の6書式に無い。`LOG-DT-005` として
    // 部分一致させるとミリ秒を捨てた比較キー（.000）で時系列統合の並びが
    // 実際の記録順とずれるため、日時なしとして扱う。
    #[test]
    fn colon_millisecond_with_slash_date_is_rejected_instead_of_dropping_the_fraction() {
        for input in [
            "2026/07/28 15:12:23:456",
            "2026/07/28 15:12:23:456 起動しました",
        ] {
            assert!(
                matches!(parse_datetime_auto(input), AutoParseOutcome::NoMatch),
                "入力 {input} は日時なしになるはず"
            );
            assert!(
                parse_datetime_with_format(LogDateTimeFormat::LogDt005, input).is_none(),
                "入力 {input} を LOG-DT-005 として部分一致させない"
            );
        }
    }

    // 受け入れ条件（Issue #36 ③、`LOG-014`）: 小数1桁の行はどの書式にも
    // 一致せず、日時なし（＝直前の日時付き行の継続行へ吸収される）になる。
    // 区切りが `.` でも `:` でも同じ扱いになることを固定する。
    #[test]
    fn single_fraction_digit_yields_no_match_for_both_separators() {
        for input in [
            "2026/07/28 15:12:23.4",
            "2026/07/28 15:12:23.4 起動しました",
            "2026/07/28 15:12:23:4",
            "2026/07/28 15:12:23:4 起動しました",
        ] {
            assert!(
                matches!(parse_datetime_auto(input), AutoParseOutcome::NoMatch),
                "入力 {input} は日時なしになるはず"
            );
        }
    }

    // 受け入れ条件（`LOG-022`）: `:` の直後がちょうど2桁の場合だけは
    // `LOG-DT-005` を候補に残す。`LOG-DT-004` が必ず同時に成立し、どちらの
    // 読み方かを推測せず曖昧性検出で利用者へ返せるためで、境界条件の対称化
    // （Issue #36）でもこの性質を変えない。
    #[test]
    fn two_digit_colon_fraction_keeps_log_dt_004_and_005_both_as_candidates() {
        for input in [
            "2026/07/28 15:12:23:45",
            "2026/07/28 15:12:23:45 起動しました",
        ] {
            match parse_datetime_auto(input) {
                AutoParseOutcome::Ambiguous(candidates) => {
                    let formats: Vec<LogDateTimeFormat> =
                        candidates.iter().map(|c| c.format).collect();
                    assert!(
                        formats.contains(&LogDateTimeFormat::LogDt004),
                        "入力: {input}"
                    );
                    assert!(
                        formats.contains(&LogDateTimeFormat::LogDt005),
                        "入力: {input}"
                    );
                }
                other => panic!("入力 {input} は Ambiguous になるはずが {other:?} だった"),
            }
        }
    }

    // 受け入れ条件: 数字を伴わない `:`（本文中の区切り）は失う桁が無いため、
    // `LOG-DT-005` の候補から外さない。境界条件の対称化（Issue #36）が本文の
    // 書き出しまで巻き込んで日時なしにしないことの確認。
    #[test]
    fn log_dt_005_accepts_a_colon_that_is_not_a_fraction() {
        for input in [
            "2026/07/28 15:12:23: 起動しました",
            "2026/07/28 15:12:23:エラー発生",
            "2026/07/28 15:12:23::区切り",
        ] {
            match parse_datetime_auto(input) {
                AutoParseOutcome::Matched(m) => {
                    assert_eq!(m.format, LogDateTimeFormat::LogDt005, "入力: {input}");
                    assert_eq!(m.second, 23);
                }
                other => panic!("入力 {input} は Matched になるはずが {other:?} だった"),
            }
        }
    }

    // 受け入れ条件（Issue #36）: 秒の桁がさらに続く入力（`15:12:234`）を
    // `LOG-DT-005` として部分一致させない。どの既知書式でも読み取れない桁を
    // 黙って捨てないための境界条件。
    #[test]
    fn log_dt_005_rejects_a_trailing_second_digit() {
        assert!(
            parse_datetime_with_format(LogDateTimeFormat::LogDt005, "2026/07/28 15:12:234")
                .is_none()
        );
        assert!(matches!(
            parse_datetime_auto("2026/07/28 15:12:234"),
            AutoParseOutcome::NoMatch
        ));
    }

    // 受け入れ条件（Issue #36）: `LOG-DT-006`（分まで）は、直後に分より細かい
    // 桁の始まり（`.` または数字）が続く場合も候補にしない（従来から除外して
    // いた `:` と揃える）。本文が空白で始まる通常の行は従来どおり成立する。
    #[test]
    fn log_dt_006_rejects_finer_precision_that_follows() {
        for input in ["2026/07/28 15:123", "2026/07/28 15:12.34"] {
            assert!(
                parse_datetime_with_format(LogDateTimeFormat::LogDt006, input).is_none(),
                "入力 {input} を LOG-DT-006 として部分一致させない"
            );
            assert!(
                matches!(parse_datetime_auto(input), AutoParseOutcome::NoMatch),
                "入力 {input} は日時なしになるはず"
            );
        }

        assert!(matches!(
            parse_datetime_auto("2026/07/28 15:12 起動しました"),
            AutoParseOutcome::Matched(_)
        ));
    }

    // 受け入れ条件（Issue #36 の絶対条件）: 境界条件を対称化しても、6書式
    // それぞれの正しい入力は——行末で終わる場合も本文が続く場合も——従来どおり
    // 単一書式指定で解析できる。
    #[test]
    fn all_six_formats_still_match_their_own_valid_inputs() {
        let cases = [
            (LogDateTimeFormat::LogDt001, "2026/07/28 15:12:23.456"),
            (LogDateTimeFormat::LogDt002, "2026-07-28 15:12:23:456"),
            (LogDateTimeFormat::LogDt003, "2026/07/28 15:12:23.45"),
            (LogDateTimeFormat::LogDt004, "2026/07/28 15:12:23:45"),
            (LogDateTimeFormat::LogDt005, "2026/07/28 15:12:23"),
            (LogDateTimeFormat::LogDt006, "2026/07/28 15:12"),
        ];

        for (format, input) in cases {
            let at_end = parse_datetime_with_format(format, input)
                .unwrap_or_else(|| panic!("{} は {input} を解析できるはず", format.id()));
            let with_message = parse_datetime_with_format(format, &format!("{input} 起動しました"))
                .unwrap_or_else(|| panic!("{} は本文付きでも解析できるはず", format.id()));

            assert_eq!(at_end.comparison_key, with_message.comparison_key);
            assert_eq!(at_end.matched_len, input.len());
            assert_eq!(at_end.precision, with_message.precision);
        }
    }

    // 受け入れ条件（`LOG-024`、Issue #36 の付随項目）: 時刻の範囲外表記は
    // 日時として成立しない。`24:00`（翌日0時の別表記）・分60・秒60
    // （うるう秒表記 `23:59:60` を含む）はいずれも拒否し、日時なしとして
    // 扱う（`docs/domain/time-model.md` の「うるう秒と時刻の範囲外表記」）。
    #[test]
    fn out_of_range_clock_values_including_leap_second_notation_are_rejected() {
        for input in [
            // 24時制の終端表記（一部のログが日の終わりに使う）。
            "2026/07/28 24:00:00.000",
            "2026/07/28 24:00",
            // 分が60。
            "2026/07/28 23:60:00.000",
            // 秒が60（うるう秒表記）。実際にうるう秒が挿入された日を含む。
            "2026/07/28 23:59:60.000",
            "2016/12/31 23:59:60.000",
            "2016/12/31 23:59:60",
        ] {
            assert!(
                matches!(parse_datetime_auto(input), AutoParseOutcome::NoMatch),
                "入力 {input} は日時として成立しないはず"
            );
        }

        // 範囲内の終端は従来どおり成立する（拒否が広がりすぎていないことの確認）。
        assert!(matches!(
            parse_datetime_auto("2026/07/28 23:59:59.999"),
            AutoParseOutcome::Matched(_)
        ));
    }

    // 受け入れ条件（`LOG-012`、`LOG-016`）: 比較キーは、解析したローカル壁時計の
    // 値をそのままエポック起点のミリ秒へ換算した比較専用の値であり、
    // タイムゾーン変換を行わない。
    #[test]
    fn comparison_key_is_local_wall_clock_millis_without_timezone_conversion() {
        let epoch =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "1970/01/01 00:00:00.000")
                .expect("解析できるはず");
        assert_eq!(epoch.comparison_key.as_millis_since_epoch(), 0);

        let next_day =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "1970/01/02 00:00:00.000")
                .expect("解析できるはず");
        assert_eq!(next_day.comparison_key.as_millis_since_epoch(), 86_400_000);

        // エポック以前は負の値になる（符号付きのまま全順序比較に使える）。
        let before_epoch =
            parse_datetime_with_format(LogDateTimeFormat::LogDt001, "1969/12/31 23:59:59.999")
                .expect("解析できるはず");
        assert_eq!(before_epoch.comparison_key.as_millis_since_epoch(), -1);
        assert!(before_epoch.comparison_key < epoch.comparison_key);
    }
}
