//! ログ解析プロファイルの4段階解決（`LOG-021`、`LOG-013`）。
//!
//! `tasks/phase-05-log-parsing-core.md`「プロファイルの対応付け」節が定める
//! 優先順で、対象ファイルへ適用するプロファイルを決めます。
//!
//! 1. 開く際にユーザーが指定したプロファイル（手動指定）
//! 2. 設定内の絶対パス完全一致
//! 3. `priority` が高い順の glob パターン
//! 4. 内容による自動判定（このモジュールの外、`crates/format-detection` 等が担当）
//!
//! パス照合そのもの（正規化、大文字小文字不区別、glob の階層規則）は
//! `hakutaku_config`（[`hakutaku_config::paths_equivalent`]・
//! [`hakutaku_config::glob_match`]・[`hakutaku_config::is_glob_pattern`]）を
//! 再利用します。依存の向きは `core-services → config` であり、逆向きの依存
//! （`config` が `core-services` を参照すること）はありません。

use std::path::Path;

use hakutaku_config::LogProfileConfig;

/// プロファイル解決の結果（`LOG-021` の4段階）。
///
/// `Result` ではなく単一の列挙にしているのは、[`ResolutionOutcome::ManualNotFound`]・
/// [`ResolutionOutcome::Ambiguous`] のいずれも、呼び出し側（P07 の UI）が利用者へ
/// 選択を促すための通常の応答であり、プログラミングエラー（`panic` すべき状態）
/// ではないためです。`crates/core-services/src/notification/outcome.rs` の
/// `TaskOutcome`（`Completed` / `Failed` / `Cancelled` を1つの列挙にまとめている）
/// と同じ設計判断です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionOutcome {
    /// 第1段階: ユーザーが指定したプロファイル名が見つかった。
    Manual(LogProfileConfig),
    /// ユーザーがプロファイル名を指定したが、`profiles` の中に一致する名前が
    /// なかった（作業指示の「名前不一致はエラー」に対応）。
    ManualNotFound {
        /// ユーザーが指定した（見つからなかった）プロファイル名。
        requested: String,
    },
    /// 第2段階: 設定内の絶対パス完全一致（大文字・小文字は区別しない）。
    ExactMatch(LogProfileConfig),
    /// 第3段階: `priority` が最大の glob 一致が一意に決まった。
    Glob(LogProfileConfig),
    /// 第3段階で、同一の最大 `priority` を持つ複数の glob パターンが同時に
    /// 一致し、一意に決められなかった。
    ///
    /// `LOG-022`（複数の書式・プロファイルが同時に一致して一意に決められない
    /// 場合、貪欲マッチで推測しない）と同じ思想を glob 解決にも適用したもの
    /// です。`crates/config` の起動時検証はパターン文字列そのものが完全一致
    /// する「明確な重複」だけを弾くため（設計判断は
    /// `crates/config/src/load.rs` の `validate_no_duplicate_patterns` を参照）、
    /// パターン文字列が異なる glob どうしが実際の対象パスに対して同時一致する
    /// 状況は起動時検証をすり抜けます。この場合の一意化は、この関数（解決時）
    /// が `Ambiguous` を返すことで担います。
    Ambiguous {
        /// 一致した（＝最大 priority を共有する）候補一覧。
        ///
        /// `profiles` に現れた順序を保ちます（呼び出し側が安定した表示順で
        /// 選択肢を提示できるようにするため）。
        candidates: Vec<LogProfileConfig>,
    },
    /// 第4段階: どのプロファイルにも一致しなかった。内容による自動判定
    /// （書式の自動判定。P05-1/P05-5 の対象）へ処理を委ねる。
    NoMatch,
}

impl ResolutionOutcome {
    /// 診断ログ（`DIAG-005`）表示用に、解決経路を短い日本語ラベルで返す。
    ///
    /// 「解決の経路（手動 / 完全一致 / glob / 該当なし）を戻り値に含め、
    /// 診断情報に使える形にする」という設計方針に対応します。
    #[must_use]
    pub fn route_label(&self) -> &'static str {
        match self {
            ResolutionOutcome::Manual(_) => "手動指定",
            ResolutionOutcome::ManualNotFound { .. } => "手動指定（該当プロファイルなし）",
            ResolutionOutcome::ExactMatch(_) => "絶対パス完全一致",
            ResolutionOutcome::Glob(_) => "glob 一致",
            ResolutionOutcome::Ambiguous { .. } => "曖昧（同一優先度の glob が複数一致）",
            ResolutionOutcome::NoMatch => "自動判定へ委譲",
        }
    }
}

/// `LOG-021` の4段階でプロファイルを解決する。
///
/// # 引数
///
/// - `manual_selection`: 開く際にユーザーが明示的に指定したプロファイル名。
///   `None` なら手動指定なしとして第2段階から評価する
/// - `target_path`: 対象ログファイルの絶対パス。呼び出し側が ADR-0005
///   （絶対ローカルパス）に従った値を渡すことを前提とし、この関数自体は
///   絶対性を検証しない（検証は `hakutaku_config::load_config` が設定読込時に
///   別途行う）。`Path` を `to_string_lossy()` で文字列化してから照合するため、
///   UTF-8 として不正なバイト列を含むパスでは近似（置換文字化け）が起きうる
///   （Windows の実運用パスでは通常発生しない）
/// - `profiles`: 設定から読み込んだプロファイル一覧（`hakutaku_config::load_config`
///   の結果）
///
/// # 各段階の判定規則
///
/// 1. **手動指定**: `manual_selection` があれば、`profiles` から同名のものを
///    先頭から探す（名前は大文字小文字を区別する通常の文字列比較。パスでは
///    ないため OS 既定の大文字小文字不区別比較は適用しない）。見つからなければ
///    [`ResolutionOutcome::ManualNotFound`]
/// 2. **絶対パス完全一致**: glob 記号を含まない `path_pattern` を持つ
///    プロファイルのうち、[`hakutaku_config::paths_equivalent`] で
///    `target_path` と一致する最初の1件を採用する。`crates/config` の起動時
///    検証が完全一致パターンの重複を弾くため（設計は
///    `crates/config/src/load.rs` を参照）、通常は高々1件しか一致しない前提
///    だが、検証を経ていない `profiles` を直接渡した場合は「最初に見つかった
///    もの」が採用される（この段階は `LOG-021` の記述どおり `Ambiguous` を
///    返さない）
/// 3. **glob**: glob 記号を含む `path_pattern` のうち
///    [`hakutaku_config::glob_match`] が一致するものを集め、その中で
///    `priority` が最大のものだけへ絞り込む。1件に絞れれば
///    [`ResolutionOutcome::Glob`]、2件以上残れば
///    [`ResolutionOutcome::Ambiguous`]
/// 4. **該当なし**: 1〜3のいずれにも一致しなければ [`ResolutionOutcome::NoMatch`]
///    （内容による自動判定へ委ねる合図）
#[must_use]
pub fn resolve_profile(
    manual_selection: Option<&str>,
    target_path: &Path,
    profiles: &[LogProfileConfig],
) -> ResolutionOutcome {
    let target = target_path.to_string_lossy();

    if let Some(requested) = manual_selection {
        return match profiles.iter().find(|profile| profile.name == requested) {
            Some(profile) => ResolutionOutcome::Manual(profile.clone()),
            None => ResolutionOutcome::ManualNotFound {
                requested: requested.to_string(),
            },
        };
    }

    let exact_match = profiles.iter().find(|profile| {
        !hakutaku_config::is_glob_pattern(&profile.path_pattern)
            && hakutaku_config::paths_equivalent(&profile.path_pattern, &target)
    });
    if let Some(profile) = exact_match {
        return ResolutionOutcome::ExactMatch(profile.clone());
    }

    let mut glob_candidates: Vec<&LogProfileConfig> = profiles
        .iter()
        .filter(|profile| hakutaku_config::is_glob_pattern(&profile.path_pattern))
        .filter(|profile| hakutaku_config::glob_match(&profile.path_pattern, &target))
        .collect();

    let Some(max_priority) = glob_candidates.iter().map(|profile| profile.priority).max() else {
        return ResolutionOutcome::NoMatch;
    };
    glob_candidates.retain(|profile| profile.priority == max_priority);

    match glob_candidates.as_slice() {
        [only] => ResolutionOutcome::Glob((*only).clone()),
        _ => ResolutionOutcome::Ambiguous {
            candidates: glob_candidates.into_iter().cloned().collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_profile, ResolutionOutcome};
    use hakutaku_config::{DateTimeFormatSetting, EncodingSetting, LogProfileConfig};
    use std::path::Path;

    /// テスト用にプロファイルを組み立てる（`encoding` は常に `Auto`、
    /// `ansi_codepage` は常に `None`、`datetime_format` は常に `Auto`。
    /// このモジュールの関心事ではないため）。
    fn profile(name: &str, path_pattern: &str, priority: i64) -> LogProfileConfig {
        LogProfileConfig {
            name: name.to_string(),
            path_pattern: path_pattern.to_string(),
            priority,
            encoding: EncodingSetting::Auto,
            ansi_codepage: None,
            datetime_format: DateTimeFormatSetting::Auto,
        }
    }

    // 受け入れ条件: 手動指定、完全一致、glob、自動判定の候補がある場合、
    // LOG-021 の優先順で同じプロファイルが選ばれる。
    #[test]
    fn four_stage_priority_picks_manual_over_exact_over_glob_over_no_match() {
        let target = Path::new(r"C:\Device\Logs\a.log");
        let profiles = vec![
            profile("glob_profile", r"C:\Device\Logs\*.log", 10),
            profile("exact_profile", r"C:\Device\Logs\a.log", 0),
            profile("manual_profile", r"C:\Other\Unrelated\*.log", 0),
        ];

        // 手動指定があれば、パスに一致するかどうかに関わらずそれが選ばれる。
        let manual = resolve_profile(Some("manual_profile"), target, &profiles);
        assert_eq!(manual, ResolutionOutcome::Manual(profiles[2].clone()));

        // 手動指定が無ければ、絶対パス完全一致が glob より優先される。
        let exact = resolve_profile(None, target, &profiles);
        assert_eq!(exact, ResolutionOutcome::ExactMatch(profiles[1].clone()));

        // 完全一致のプロファイルを取り除くと、glob 一致が採用される。
        let glob_only = vec![profiles[0].clone(), profiles[2].clone()];
        let glob = resolve_profile(None, target, &glob_only);
        assert_eq!(glob, ResolutionOutcome::Glob(glob_only[0].clone()));

        // どれにも一致しないパスは NoMatch（自動判定へ委譲）。
        let unrelated_target = Path::new(r"D:\Nothing\here.log");
        let no_match = resolve_profile(None, unrelated_target, &glob_only);
        assert_eq!(no_match, ResolutionOutcome::NoMatch);
    }

    #[test]
    fn manual_selection_with_unknown_name_returns_manual_not_found() {
        let profiles = vec![profile("known", r"C:\Device\Logs\*.log", 0)];
        let outcome = resolve_profile(
            Some("unknown"),
            Path::new(r"C:\Device\Logs\a.log"),
            &profiles,
        );
        assert_eq!(
            outcome,
            ResolutionOutcome::ManualNotFound {
                requested: "unknown".to_string()
            }
        );
    }

    // 受け入れ条件: 大文字・小文字を区別しない（完全一致）。
    #[test]
    fn exact_match_is_case_insensitive() {
        let profiles = vec![profile("a", r"C:\LOGS\a.log", 0)];
        let outcome = resolve_profile(None, Path::new(r"c:\logs\A.LOG"), &profiles);
        assert_eq!(outcome, ResolutionOutcome::ExactMatch(profiles[0].clone()));
    }

    // 受け入れ条件: 大文字・小文字を区別しない（glob）。
    #[test]
    fn glob_match_is_case_insensitive() {
        let profiles = vec![profile("a", r"C:\LOGS\*.log", 0)];
        let outcome = resolve_profile(None, Path::new(r"c:\logs\A.LOG"), &profiles);
        assert_eq!(outcome, ResolutionOutcome::Glob(profiles[0].clone()));
    }

    // 受け入れ条件: `*`・`?` は1階層内（`C:\a\*.log` は `C:\a\b\c.log` に一致
    // しない）。
    #[test]
    fn single_level_wildcard_does_not_match_nested_path() {
        let profiles = vec![profile("a", r"C:\a\*.log", 0)];
        let outcome = resolve_profile(None, Path::new(r"C:\a\b\c.log"), &profiles);
        assert_eq!(outcome, ResolutionOutcome::NoMatch);
    }

    // 受け入れ条件: `**` は複数階層（0階層を含む）に一致する。
    #[test]
    fn double_star_matches_multiple_directories_including_zero() {
        let profiles = vec![profile("a", r"C:\a\**\*.log", 0)];

        let zero_level = resolve_profile(None, Path::new(r"C:\a\c.log"), &profiles);
        assert_eq!(zero_level, ResolutionOutcome::Glob(profiles[0].clone()));

        let nested = resolve_profile(None, Path::new(r"C:\a\b\c.log"), &profiles);
        assert_eq!(nested, ResolutionOutcome::Glob(profiles[0].clone()));
    }

    // 受け入れ条件: priority の違いによる一意解決。
    #[test]
    fn higher_priority_glob_wins_when_both_match() {
        // 両方とも glob パターンで、対象パスにどちらも一致する。priority が
        // 高い方（high_glob）が一意に選ばれることを確認する。
        let low = profile("low", r"C:\a\*.log", 0);
        let high_glob = profile("high_glob", r"C:\a\a*.log", 10);
        let profiles = vec![low, high_glob.clone()];

        let outcome = resolve_profile(None, Path::new(r"C:\a\a.log"), &profiles);
        assert_eq!(outcome, ResolutionOutcome::Glob(high_glob));
    }

    // 受け入れ条件: 同一優先度・異なるパターンが同一パスへ一致した場合の
    // Ambiguous（`LOG-022` の貪欲禁止と同じ思想）。
    #[test]
    fn same_priority_different_patterns_matching_same_path_is_ambiguous() {
        let a = profile("a", r"C:\a\*.log", 5);
        let b = profile("b", r"C:\a\a*.log", 5);
        let profiles = vec![a.clone(), b.clone()];

        let outcome = resolve_profile(None, Path::new(r"C:\a\a.log"), &profiles);
        assert_eq!(
            outcome,
            ResolutionOutcome::Ambiguous {
                candidates: vec![a, b]
            }
        );
    }

    // 受け入れ条件: 完全一致指定（glob 記号なし）の扱い。glob 記号を含まない
    // path_pattern は絶対パス完全一致として扱われ、glob 段階には現れない。
    #[test]
    fn pattern_without_glob_symbols_is_treated_as_exact_match_not_glob() {
        let exact = profile("exact", r"C:\a\b.log", 0);
        // 同じ文字列に一致し得る glob パターンが priority で勝っていても、
        // 完全一致は glob より前の段階で確定するため常に完全一致が勝つ。
        let glob = profile("glob", r"C:\a\*.log", 100);
        let profiles = vec![exact.clone(), glob];

        let outcome = resolve_profile(None, Path::new(r"C:\a\b.log"), &profiles);
        assert_eq!(outcome, ResolutionOutcome::ExactMatch(exact));
    }

    #[test]
    fn route_label_distinguishes_all_variants() {
        assert_eq!(
            ResolutionOutcome::Manual(profile("a", r"C:\a\b.log", 0)).route_label(),
            "手動指定"
        );
        assert_eq!(
            ResolutionOutcome::ManualNotFound {
                requested: "x".to_string()
            }
            .route_label(),
            "手動指定（該当プロファイルなし）"
        );
        assert_eq!(
            ResolutionOutcome::ExactMatch(profile("a", r"C:\a\b.log", 0)).route_label(),
            "絶対パス完全一致"
        );
        assert_eq!(
            ResolutionOutcome::Glob(profile("a", r"C:\a\*.log", 0)).route_label(),
            "glob 一致"
        );
        assert_eq!(
            ResolutionOutcome::Ambiguous {
                candidates: Vec::new()
            }
            .route_label(),
            "曖昧（同一優先度の glob が複数一致）"
        );
        assert_eq!(ResolutionOutcome::NoMatch.route_label(), "自動判定へ委譲");
    }
}
