//! ADR-0008 の `source_ordinal` 決定規則のうち、単一の `insert_source` 呼び出し
//! 順（`crate::registry::DisplaySetRegistry` が「後から追加は末尾」として
//! 自然に実装する部分）**以外**の規則を実装する純粋関数群です（P09-1）。
//!
//! # 現状の呼び出し口について
//!
//! 現時点の `src-tauri::file_dialog` はネイティブダイアログでの単一ファイル
//! 選択のみに対応しており（`IFileOpenDialog` を `FOS_ALLOWMULTISELECT` なしで
//! 使用）、「同一操作でのアドホック複数選択」を行う UI 経路はまだ存在しません。
//! そのため本モジュールの関数は、`crate::registry::DisplaySetRegistry` の
//! 状態変更（`insert_source` の呼び出し順 = `source_ordinal`）へまだ直接
//! 配線されていません。ADR-0008 が確定させた規則を先行して実装し、複数選択
//! UI・重複オープン防止 UI（いずれも本フェーズの対象外）が追加された時点で
//! そのまま呼び出せるようにするための下地です（`tasks/phase-09-timeline-merge.md`
//! 「対象外」・後続課題）。
//!
//! 設定由来（`CFG-003`）のソースは、現状は名前を指定して1件ずつ開く経路
//! （`src-tauri::targets::open_config_data_source`）しかないため、
//! 「設定に記載された順」は `insert_source` の呼び出し順（= 利用者が設定一覧を
//! 上から順に開く操作）にそのまま従います。一括で開く経路が追加された場合は、
//! 呼び出し側が設定の記載順で `insert_source` を呼ぶだけで規則を満たせます
//! （本モジュールへの依存は不要です）。

use std::path::{Path, PathBuf};

use hakutaku_data_source::FileIdentity;

/// 同一操作でのアドホック複数選択（ADR-0008）向けに、`paths` を挿入すべき
/// 順序へ並べ替えた添字列（`paths` への添字の並び）を返します。`paths` 自体は
/// 変更しません。
///
/// 正規化した絶対パスを第1キー ordinal case-insensitive、第2キー ordinal
/// （大文字・小文字を区別）で比較します（ADR-0008「同一操作でのアドホック
/// 複数選択」）。OS のファイル選択ダイアログが返す列挙順に依存しないための
/// 規則であり、`paths` を渡す前の並び順は結果に影響しません。
///
/// # 大文字小文字の比較について
///
/// 「ordinal case-insensitive」（Windows のパス比較で一般的な大文字小文字を
/// 区別しない比較）の近似として [`str::to_lowercase`]（Unicode の単純ケース
/// フォールディング）を使います。ASCII 範囲（`a`〜`z`／`A`〜`Z`、ドライブ
/// レターや典型的なフォルダ名の大半）では厳密な ordinal 比較と一致します。
#[must_use]
pub fn plan_adhoc_batch_order(paths: &[PathBuf]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..paths.len()).collect();
    indices.sort_by(|&a, &b| {
        let key_a = normalized_ordinal_key(&paths[a]);
        let key_b = normalized_ordinal_key(&paths[b]);
        key_a.0.cmp(&key_b.0).then_with(|| key_a.1.cmp(&key_b.1))
    });
    indices
}

/// (大文字小文字を区別しない正規化キー, 大文字小文字を区別する正規化キー)。
///
/// 区切り文字を `\` へ揃え（`/` と `\` の混在を同一パスとして扱う）ますが、
/// `..`・`.` の解決や短縮パス展開などの完全な正規化は行いません
/// （`CFG-010` が対象とするローカル絶対パスは、通常この程度の正規化で
/// 決定的な比較に十分であり、完全なファイルシステム正規化はファイル
/// アクセスを伴うため意図的に避けています）。
fn normalized_ordinal_key(path: &Path) -> (String, String) {
    let normalized = path.to_string_lossy().replace('/', "\\");
    (normalized.to_lowercase(), normalized)
}

/// 既に開いている（`existing_identities` に含まれる）ファイルかどうかを
/// ファイル識別子（ボリューム連番＋ファイルインデックス）で判定します
/// （ADR-0008「同一ファイルの重複選択」）。パス文字列ではなく識別子で比較する
/// ため、ハードリンク等の別名パスも同一ファイルとして検出できます。
#[must_use]
pub fn is_already_open(existing_identities: &[FileIdentity], candidate: FileIdentity) -> bool {
    existing_identities.contains(&candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ条件: 大文字・小文字だけが異なるパスを同時に開いても、
    // source_ordinal（この関数が返す並び順そのもの）が決定的に決まる。
    #[test]
    fn plan_adhoc_batch_order_is_case_insensitive_primary_key() {
        let paths = vec![
            PathBuf::from(r"C:\logs\Log.txt"),
            PathBuf::from(r"C:\logs\alpha.txt"),
            PathBuf::from(r"C:\logs\beta.txt"),
        ];
        let order = plan_adhoc_batch_order(&paths);
        // 大文字小文字を無視した辞書順: alpha.txt, beta.txt, Log.txt
        assert_eq!(order, vec![1, 2, 0]);
    }

    // 受け入れ条件: 大文字小文字だけが異なるパス同士は、第2キー（大文字小文字
    // を区別する ordinal）で決着する（同値のまま決定不能にならない）。
    #[test]
    fn plan_adhoc_batch_order_breaks_case_only_ties_with_case_sensitive_secondary_key() {
        let paths = vec![
            PathBuf::from(r"C:\logs\log.txt"),
            PathBuf::from(r"C:\logs\Log.txt"),
            PathBuf::from(r"C:\logs\LOG.txt"),
        ];
        let order = plan_adhoc_batch_order(&paths);

        // 第1キー（小文字化）はすべて同値のため、第2キー（大文字小文字を
        // 区別する文字列比較）で決着する。Rust の文字列比較は Unicode
        // コードポイント順のため、'L' (0x4C) < 'l' (0x6C) より
        // "LOG.txt" < "Log.txt" < "log.txt" の順になる。
        assert_eq!(order, vec![2, 1, 0]);

        // 同じ入力を渡せば毎回同じ順序になる（決定性）。
        assert_eq!(plan_adhoc_batch_order(&paths), order);
    }

    // 受け入れ条件: `/` と `\` の混在も同一パスとして正規化して比較する。
    #[test]
    fn plan_adhoc_batch_order_normalizes_path_separators() {
        let paths = vec![
            PathBuf::from(r"C:/logs/b.txt"),
            PathBuf::from(r"C:\logs\a.txt"),
        ];
        let order = plan_adhoc_batch_order(&paths);
        assert_eq!(order, vec![1, 0]);
    }

    // 入力順（渡す前の並び）を変えても、正規化キーが同じなら結果は変わらない。
    #[test]
    fn plan_adhoc_batch_order_is_independent_of_input_order() {
        let paths_a = vec![
            PathBuf::from(r"C:\logs\b.txt"),
            PathBuf::from(r"C:\logs\a.txt"),
        ];
        let paths_b = vec![
            PathBuf::from(r"C:\logs\a.txt"),
            PathBuf::from(r"C:\logs\b.txt"),
        ];

        // どちらも「a.txt が先」という同じ意味の並びを返す。
        assert_eq!(paths_a[plan_adhoc_batch_order(&paths_a)[0]], paths_b[0]);
        assert_eq!(paths_b[plan_adhoc_batch_order(&paths_b)[0]], paths_b[0]);
    }

    fn identity(n: u64) -> FileIdentity {
        FileIdentity {
            volume_serial_number: 1,
            file_index: n,
        }
    }

    // 受け入れ条件: 同じファイルを重複して選択した場合の扱いが決まっており、
    // 決定的である（ファイル識別子による重複検出）。
    #[test]
    fn is_already_open_detects_duplicate_by_file_identity_not_path() {
        let existing = vec![identity(1), identity(2)];
        assert!(is_already_open(&existing, identity(1)));
        assert!(!is_already_open(&existing, identity(3)));

        // 決定的（同じ入力なら何度呼んでも同じ結果）。
        assert!(is_already_open(&existing, identity(1)));
    }

    #[test]
    fn is_already_open_returns_false_for_empty_existing_list() {
        assert!(!is_already_open(&[], identity(1)));
    }
}
