//! 同一マッピング内で同じキーが2回以上書かれた箇所の検出（`CFG-016`、Issue #39）。
//!
//! # なぜ専用の走査が必要か
//!
//! `saphyr` はマッピングを「キーのノードを鍵とする連結ハッシュマップ」として
//! 組み立てる。[`saphyr::MarkedYaml`] の同値判定と `Hash` は位置（`Span`）を
//! **無視して値だけで比較する**ため、同じキーを2回書くと1件へ潰れる。潰れた
//! 結果は「先に書いたキーのノード」と「後に書いた値のノード」の組であり、先に
//! 書いた値は木のどこにも残らない。つまり [`crate::load::load_config`] が受け取る
//! 木を後からいくら調べても、二重定義があったかどうかは分からない。
//!
//! 誤設定を黙って既定値（この場合は「後勝ちの値」）へ倒さないことは `CFG-016`
//! の趣旨そのものであるため、検出手段を別に用意する必要がある。
//!
//! # 実現方法（なぜもう一度読み直すのか）
//!
//! 低レベルの構文解析イベントを直接受け取れば1回の走査で検出できるが、それには
//! `saphyr-parser` への直接依存が必要になる（`saphyr` はイベント API を再公開
//! していない）。依存を増やさずに済ませるため、ここでは `saphyr` が公開する
//! [`saphyr::LoadableYamlNode`] トレイトを、**ノードごとに一意な識別子で同値
//! 判定する**最小のノード型 [`ScanNode`] へ実装し、同じキーが潰れない木をもう
//! 一度組み立てる。設定ファイルは実行ファイルと同じフォルダーに置かれる小さな
//! ファイル（`CFG-014`）で、読み直すのは起動時の1回だけであるため、2回解析する
//! 費用は問題にならない。
//!
//! # このモジュールが返さないもの
//!
//! [`ScanNode`] は位置情報を持たない。スカラーの位置は
//! `LoadableYamlNode::with_span` からしか受け取れないが、その引数型
//! （`saphyr_parser::Span`）を `saphyr` が再公開しておらず、直接依存なしでは
//! メソッドの型を書けないためである。したがってこのモジュールが返すのは
//! 「どのマッピング（項目パス）に、どのキーが2回以上現れたか」だけであり、
//! 表示に使う行・列は呼び出し側が [`saphyr::MarkedYaml`] の木から取る。

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use saphyr::{AnnotatedMapping, LoadableYamlNode, Scalar, Tag, Yaml};

/// [`ScanNode`] へ与える一意な識別子の発番元。
///
/// 値そのものに意味はなく、「異なるノードは決して等しくならない」ことだけを
/// 保証するために使う。並行して読み込む経路は無いが、`static` を安全に共有
/// するために原子的に加算する。
static NEXT_NODE_ID: AtomicU64 = AtomicU64::new(0);

/// 検出結果。キーは項目パス（`""` は最上位、`log_profiles[0]` のような添字表記を
/// 含む）、値はそのマッピングで2回以上現れたキー名（出現順、重複なし）。
pub(crate) type DuplicateKeys = HashMap<String, Vec<String>>;

/// `source`（`hakutaku.yaml` の内容）を読み直し、重複キーの一覧を返す。
///
/// 構文エラーで読めない場合は空を返す。呼び出し側は同じ内容を先に
/// [`saphyr::MarkedYaml`] として読み込んでおり、構文エラーはそちらで位置つきの
/// 検証エラーとして報告済みだからである（同じ理由を二重に積まない）。
///
/// 走査するのは先頭のドキュメントだけである。2件目以降は
/// [`crate::load::load_config`] が別の検証エラー（`---` 区切りの複数ドキュメント）
/// として報告し、設定値としては採用しないため。
pub(crate) fn collect_duplicate_keys(source: &str) -> DuplicateKeys {
    let mut duplicates = DuplicateKeys::new();
    let Ok(documents) = ScanNode::load_from_str(source) else {
        return duplicates;
    };
    if let Some(root) = documents.first() {
        collect(root, "", &mut duplicates);
    }
    duplicates
}

/// `node` を再帰的にたどり、マッピングごとに重複キーを `duplicates` へ積む。
///
/// `path` は呼び出し側（`crate::load`）が組み立てる項目パスと同じ規則で作る
/// （最上位は空文字列、入れ子は `親.子`、配列要素は `親[添字]`）。位置情報を
/// 持たないこの木と、位置情報を持つ [`saphyr::MarkedYaml`] の木を突き合わせる
/// 鍵がこの文字列であるため、両者の組み立て規則は一致していなければならない。
fn collect(node: &ScanNode<'_>, path: &str, duplicates: &mut DuplicateKeys) {
    match &node.data {
        ScanData::Mapping(mapping) => {
            let mut seen: Vec<&str> = Vec::new();
            for (key_node, value_node) in mapping {
                // 文字列以外のキー（例: `1: a`）は対象外。スキーマが受け付ける
                // キーは文字列だけであり、型が違うことは通常の検証が別途報告する。
                let Some(key) = key_node.as_key() else {
                    continue;
                };
                if seen.contains(&key) {
                    let entry = duplicates.entry(path.to_string()).or_default();
                    // 3回以上書かれていても、利用者が直す箇所は1つ（そのキー）
                    // であるため、キー名は1件だけ積む。
                    if !entry.iter().any(|known| known == key) {
                        entry.push(key.to_string());
                    }
                } else {
                    seen.push(key);
                }
                let child_path = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                collect(value_node, &child_path, duplicates);
            }
        }
        ScanData::Sequence(items) => {
            for (index, item) in items.iter().enumerate() {
                collect(item, &format!("{path}[{index}]"), duplicates);
            }
        }
        ScanData::Scalar(_) | ScanData::Opaque | ScanData::BadValue => {}
    }
}

/// 重複キー検出のためだけに使う、位置情報を持たない YAML ノード。
///
/// 同値判定と `Hash` を [`ScanNode::id`] だけで決めるため、同じキー文字列を
/// 2回書いてもマッピングの別々の要素として残る（モジュール doc コメント参照）。
#[derive(Clone, Debug)]
struct ScanNode<'input> {
    /// 発番した一意な識別子。
    id: u64,
    /// ノードの中身。
    data: ScanData<'input>,
}

/// [`ScanNode`] が保持する中身。重複キーの検出に必要な区別だけを持つ。
#[derive(Clone, Debug)]
enum ScanData<'input> {
    /// スカラー。キー名の照合に使うため、文字列として書かれていた場合のみ
    /// その文字列を持つ。
    Scalar(Option<Cow<'input, str>>),
    /// 配列（シーケンス）。
    Sequence(Vec<ScanNode<'input>>),
    /// マッピング。
    Mapping(AnnotatedMapping<'input, ScanNode<'input>>),
    /// 上記のいずれでもないノード（エイリアスなど）。中身は見ない。
    Opaque,
    /// `saphyr` が「不正な値」として扱うノード。
    ///
    /// 読み込み器はマッピングの鍵と値を交互に受け取る際、`is_badvalue()` が真の
    /// ノードを「まだ鍵が来ていない」印として使う。そのため、この判定を返して
    /// よいのは `saphyr` が `BadValue` として渡してきたノードだけである。
    /// 例えばエイリアスを [`ScanData::BadValue`] にすると、値であるはずの
    /// エイリアスが鍵と誤認され、以降の対応がずれる。
    BadValue,
}

impl<'input> ScanNode<'input> {
    /// 新しい識別子を発番してノードを作る。
    fn new(data: ScanData<'input>) -> Self {
        Self {
            id: NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed),
            data,
        }
    }

    /// マッピングの鍵として比較できる文字列表現。文字列以外は `None`。
    fn as_key(&self) -> Option<&str> {
        match &self.data {
            ScanData::Scalar(Some(value)) => Some(value.as_ref()),
            _ => None,
        }
    }
}

impl PartialEq for ScanNode<'_> {
    /// 識別子だけで比較する（同じ内容でも別のノードなら等しくない）。
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ScanNode<'_> {}

impl Hash for ScanNode<'_> {
    /// [`PartialEq`] と同じ根拠（識別子）で計算する。
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<'input> LoadableYamlNode<'input> for ScanNode<'input> {
    /// マッピングの鍵にもこのノード型をそのまま使う。
    type HashKey = ScanNode<'input>;

    fn from_bare_yaml(yaml: Yaml<'input>) -> Self {
        Self::new(match yaml {
            // 読み込み器は空の入れ物だけを渡し、中身は後から詰める。
            Yaml::Sequence(_) => ScanData::Sequence(Vec::new()),
            Yaml::Mapping(_) => ScanData::Mapping(AnnotatedMapping::new()),
            Yaml::Value(Scalar::String(value)) => ScanData::Scalar(Some(value)),
            // 値として解釈する前の生の表現も、キーとしては文字列として比較する
            // （`saphyr` の既定である早期解釈では通らない経路だが、規則を
            // 揃えておく）。
            Yaml::Representation(value, _, _) => ScanData::Scalar(Some(value)),
            Yaml::Value(_) => ScanData::Scalar(None),
            // タグ（`!custom` など）は重複判定に影響しないため、中身だけを見る。
            Yaml::Tagged(_, node) => return Self::from_bare_yaml(*node),
            Yaml::Alias(_) => ScanData::Opaque,
            Yaml::BadValue => ScanData::BadValue,
        })
    }

    fn is_sequence(&self) -> bool {
        matches!(self.data, ScanData::Sequence(_))
    }

    fn is_mapping(&self) -> bool {
        matches!(self.data, ScanData::Mapping(_))
    }

    fn is_badvalue(&self) -> bool {
        matches!(self.data, ScanData::BadValue)
    }

    fn sequence_mut(&mut self) -> &mut Vec<Self> {
        match &mut self.data {
            ScanData::Sequence(items) => items,
            // 読み込み器は `is_sequence()` が真のノードにだけこれを呼ぶ
            // （`saphyr` の `LoadableYamlNode` の契約）。
            _ => panic!("配列ではないノードに対して sequence_mut を呼び出しました"),
        }
    }

    fn mapping_mut(&mut self) -> &mut AnnotatedMapping<'input, Self> {
        match &mut self.data {
            ScanData::Mapping(mapping) => mapping,
            // 読み込み器は `is_mapping()` が真のノードにだけこれを呼ぶ。
            _ => panic!("マッピングではないノードに対して mapping_mut を呼び出しました"),
        }
    }

    fn into_tagged(self, _tag: Cow<'input, Tag>) -> Self {
        // タグ付きノードも中身は変わらない。重複キーの検出にタグは不要。
        self
    }

    fn take(&mut self) -> Self {
        let mut taken_out = Self::new(ScanData::BadValue);
        core::mem::swap(&mut taken_out, self);
        taken_out
    }
}

#[cfg(test)]
mod tests {
    use super::collect_duplicate_keys;

    // 受け入れ条件: 最上位の同一キー二重定義を検出する（`CFG-016`、Issue #39）。
    #[test]
    fn detects_duplicate_top_level_key() {
        let duplicates = collect_duplicate_keys(
            "config_version: 1\nmemory:\n  budget_mib: 1\nconfig_version: 1\n",
        );
        assert_eq!(duplicates[""], vec!["config_version".to_string()]);
    }

    // 受け入れ条件: 入れ子（区分の中）の重複も、項目パスつきで検出する。
    #[test]
    fn detects_duplicate_nested_key_with_path() {
        let duplicates = collect_duplicate_keys("memory:\n  budget_mib: 1\n  budget_mib: 2\n");
        assert_eq!(duplicates["memory"], vec!["budget_mib".to_string()]);
    }

    // 受け入れ条件: 配列要素の中の重複は、添字を含む項目パスで検出する。
    #[test]
    fn detects_duplicate_key_inside_sequence_item() {
        let duplicates =
            collect_duplicate_keys("data_sources:\n  - name: a\n    name: b\n    path: c\n");
        assert_eq!(duplicates["data_sources[0]"], vec!["name".to_string()]);
    }

    // 受け入れ条件: 3回以上書かれていても、直す箇所は1つなのでキー名は1件だけ。
    #[test]
    fn reports_a_repeated_key_once() {
        let duplicates = collect_duplicate_keys("a: 1\na: 2\na: 3\n");
        assert_eq!(duplicates[""], vec!["a".to_string()]);
    }

    // 受け入れ条件: 引用符の有無は同じキーとして扱う（`saphyr` が潰す規則と
    // 一致させる。潰れるのに検出できない、という取りこぼしを作らない）。
    #[test]
    fn quoted_and_plain_keys_are_the_same_key() {
        let duplicates = collect_duplicate_keys("'a': 1\na: 2\n");
        assert_eq!(duplicates[""], vec!["a".to_string()]);
    }

    // 受け入れ条件: 重複が無ければ空。錨（アンカー）と別名（エイリアス）を
    // 使った設定を重複と誤検出しない。
    #[test]
    fn reports_nothing_without_duplicates() {
        assert!(collect_duplicate_keys("a: 1\nb:\n  c: 2\n  d: [1, 2]\n").is_empty());
        assert!(collect_duplicate_keys("a: &x 1\nb: *x\n").is_empty());
    }

    // 受け入れ条件: 構文エラーの入力では何も報告しない（同じ誤りに対する
    // 理由の二重報告を避ける。位置つきの構文エラーは load_config が出す）。
    #[test]
    fn reports_nothing_for_malformed_yaml() {
        assert!(collect_duplicate_keys("memory:\n  budget_mib: [1\n").is_empty());
    }
}
