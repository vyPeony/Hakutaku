//! 起動時検証のエラー表現（`CFG-016`）。
//!
//! 構文エラー・値検証エラーのいずれも、ファイル名・行・列・項目パス・理由を持つ
//! [`ConfigError`] として表現する。`saphyr` の型（`Marker` など）はここには現れず、
//! 単純な数値へ変換済みである（ADR-0004 の封じ込め方針）。

use std::fmt;

/// 設定ファイルの検証で見つかった1件のエラー。
///
/// `Display` は「ファイル名:行:列 項目: 理由」の日本語形式で出力する
/// （`CFG-016` が求める「ファイル名・行・列・理由」の表示に対応する）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// 設定ファイルの表示名（通常は読み込みに使った絶対パスの表示形式）。
    pub file_name: String,
    /// 問題箇所の行番号（1始まり）。特定できない場合は `None`。
    pub line: Option<usize>,
    /// 問題箇所の列番号（1始まり）。特定できない場合は `None`。
    pub column: Option<usize>,
    /// 項目パス（例: `memory.budget_mib`、`log_profiles[0].name`）。
    ///
    /// 特定の項目に紐づかないエラー（構文エラー、最上位がマッピングでない等）
    /// では空文字列を使う。
    pub item_path: String,
    /// エラーの理由（日本語）。
    pub reason: String,
}

impl fmt::Display for ConfigError {
    /// 「ファイル名:行:列 項目: 理由」の日本語形式で出力する。
    ///
    /// 行・列が特定できない場合は `?` を表示する。項目パスが空の場合は項目部分を省く。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let line = self
            .line
            .map_or_else(|| "?".to_string(), |value| value.to_string());
        let column = self
            .column
            .map_or_else(|| "?".to_string(), |value| value.to_string());
        if self.item_path.is_empty() {
            write!(f, "{}:{line}:{column}: {}", self.file_name, self.reason)
        } else {
            write!(
                f,
                "{}:{line}:{column} {}: {}",
                self.file_name, self.item_path, self.reason
            )
        }
    }
}

/// 検証で見つかった全エラー。
///
/// 起動時検証は最初の1件で止めず、**全項目を走査してエラーを収集する**
/// （利用者が一度の起動確認ですべてのエラーに気づけるようにするため）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigErrors(Vec<ConfigError>);

impl ConfigErrors {
    /// エラーの一覧から構築する。
    pub(crate) fn new(errors: Vec<ConfigError>) -> Self {
        Self(errors)
    }

    /// 1件もエラーが無ければ `true`。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// エラー件数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// エラーをスライスとして参照する。
    #[must_use]
    pub fn as_slice(&self) -> &[ConfigError] {
        &self.0
    }

    /// 借用イテレータ。
    pub fn iter(&self) -> std::slice::Iter<'_, ConfigError> {
        self.0.iter()
    }
}

impl fmt::Display for ConfigErrors {
    /// 各エラーを1行ずつ、「ファイル名:行:列 項目: 理由」の形式で出力する。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}

impl IntoIterator for ConfigErrors {
    type Item = ConfigError;
    type IntoIter = std::vec::IntoIter<ConfigError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a ConfigErrors {
    type Item = &'a ConfigError;
    type IntoIter = std::slice::Iter<'a, ConfigError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigError, ConfigErrors};

    #[test]
    fn display_includes_item_path_when_present() {
        // 項目パスがある場合は「ファイル名:行:列 項目: 理由」の形式になる。
        let error = ConfigError {
            file_name: "C:\\Device\\hakutaku.yaml".to_string(),
            line: Some(3),
            column: Some(5),
            item_path: "memory.budget_mib".to_string(),
            reason: "1 以上の整数である必要があります".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "C:\\Device\\hakutaku.yaml:3:5 memory.budget_mib: 1 以上の整数である必要があります"
        );
    }

    #[test]
    fn display_omits_item_path_when_empty() {
        // 構文エラーなど、項目パスを特定できない場合は項目部分を省く。
        let error = ConfigError {
            file_name: "C:\\Device\\hakutaku.yaml".to_string(),
            line: Some(1),
            column: Some(1),
            item_path: String::new(),
            reason: "YAML の構文エラーです".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "C:\\Device\\hakutaku.yaml:1:1: YAML の構文エラーです"
        );
    }

    #[test]
    fn display_uses_question_mark_when_position_unknown() {
        // 行・列を特定できない場合（例: ファイル読み取り失敗）は `?` を表示する。
        let error = ConfigError {
            file_name: "C:\\Device\\hakutaku.yaml".to_string(),
            line: None,
            column: None,
            item_path: String::new(),
            reason: "読み取れませんでした".to_string(),
        };
        assert_eq!(
            error.to_string(),
            "C:\\Device\\hakutaku.yaml:?:?: 読み取れませんでした"
        );
    }

    #[test]
    fn config_errors_display_joins_with_newlines() {
        let errors = ConfigErrors::new(vec![
            ConfigError {
                file_name: "hakutaku.yaml".to_string(),
                line: Some(1),
                column: Some(1),
                item_path: "a".to_string(),
                reason: "reason1".to_string(),
            },
            ConfigError {
                file_name: "hakutaku.yaml".to_string(),
                line: Some(2),
                column: Some(1),
                item_path: "b".to_string(),
                reason: "reason2".to_string(),
            },
        ]);
        assert_eq!(
            errors.to_string(),
            "hakutaku.yaml:1:1 a: reason1\nhakutaku.yaml:2:1 b: reason2"
        );
        assert_eq!(errors.len(), 2);
        assert!(!errors.is_empty());
    }
}
