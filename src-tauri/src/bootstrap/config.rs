//! 起動時の設定読み込み（`CFG-015`〜`CFG-017`、P03-2）。
//!
//! `hakutaku_config::load_config` を起動手順の中で一度だけ呼び、結果を
//! [`ConfigState`] としてまとめる。[`crate::bootstrap::run`] がこの値を
//! [`crate::bootstrap::Bootstrap`] 経由で `src-tauri/src/lib.rs` へ渡し、
//! そこで Tauri の managed state として保持する。フロントエンドへは
//! `get_config_status` コマンド（`src-tauri/src/config_status.rs`）が、この
//! クレートの型をそのまま公開せず専用の応答型へ変換して渡す。
//!
//! # `CFG-020`（診断ログのローテーション）の適用順序について
//!
//! [`hakutaku_diagnostics::Diagnostics::open`] はローテーション設定
//! （`RotationPolicy`）を開いた時点で確定させ、開いた**後**に変更する API を
//! 提供していない（`crates/diagnostics/src/lib.rs` の `ActiveState` 参照）。
//! 一方、これまでの起動手順（P01）は、実行時フォルダの位置解決（`layout`）の
//! 直後に診断ログを開いていた。
//!
//! `hakutaku.yaml` の読み込み（[`ConfigState::load`]）自体はファイル I/O と
//! YAML 解析だけで完結し、診断ログにもファイルシステムの他の初期化にも依存
//! しない。そこで本フェーズでは、**診断ログを開く前に設定を読み込む**よう
//! `bootstrap::run` の手順を並べ替えた（
//! 「(a) 設定を診断ログ open より前に読む」を採用し、
//! 「(b) 次回起動から適用」は選ばなかった）。これにより、`hakutaku.yaml` の
//! `diagnostics.rotate_mib` / `diagnostics.keep_generations`
//! （[`ConfigState::rotation_policy`]）を**その起動から**診断ログへ反映できる。
//!
//! 設定読み込みは `load_config` が返す [`hakutaku_config::LoadOutcome`] を
//! そのまま変換するだけで、ファイル I/O 以外に失敗しうる処理（panic しうる
//! 処理）を含まない。そのため、診断ログを開く前に読んでも「設定読み込み自体の
//! 失敗が診断ログ無しで黙って落ちる」ことはない。診断ログを開けなかった場合の
//! 通知（`notify::diagnostics_unavailable`）は従来どおり `bootstrap::run` が
//! 表示し、設定読み込みの結果（経路・エラー件数）は診断ログを開いた**後**に
//! 記録する（`bootstrap::run` の手順5）。

use std::path::Path;

use hakutaku_config::{ConfigError, HakutakuConfig, LoadOutcome};

/// 1 MiB をバイトに換算する定数。
const BYTES_PER_MIB: u64 = 1024 * 1024;

/// 起動経路（`tasks/phase-03-configuration.md` の「設定の三つの起動経路」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigRoute {
    /// 正常起動。`hakutaku.yaml` があり、構文と値がすべて妥当だった。
    Loaded,
    /// 既定値起動（`CFG-015`）。`hakutaku.yaml` が存在しない。
    Missing,
    /// 安全モード（`CFG-016`）。`hakutaku.yaml` は存在するが構文または値が不正。
    Invalid,
}

/// 起動時の設定読み込み結果。Tauri の managed state として保持する。
#[derive(Clone, Debug)]
pub struct ConfigState {
    /// 起動経路。
    pub route: ConfigRoute,
    /// 適用する設定値。
    ///
    /// `Missing` / `Invalid` の場合は組み込み既定値（[`HakutakuConfig::default`]）。
    /// `Invalid`（安全モード）の場合、`hakutaku_config::load_config` は検証に
    /// 1件でも失敗すると部分的な設定値を返さない（`crates/config/src/load.rs`
    /// の `load_config` の実装）ため、データソース・ログ解析プロファイルを
    /// 含む全項目が確実に組み込み既定値（データソース・プロファイルは空）になる
    /// （`CFG-016` の「設定由来のデータソース・プロファイルを無効化する」要件）。
    pub config: HakutakuConfig,
    /// 安全モード（`Invalid`）のときだけ非空。ファイル名・行・列・項目・理由。
    pub errors: Vec<ConfigError>,
}

impl ConfigState {
    /// `config_path`（実行ファイルと同じフォルダの `hakutaku.yaml`。`CFG-014`）を
    /// 読み込む。
    ///
    /// 診断ログに依存しない（呼び出し順序の制約はモジュール doc コメントを参照）。
    /// 結果の記録は呼び出し側（`bootstrap::run`）が診断ログを開いた後に行う。
    ///
    /// # コードページ存在確認の注入（Issue #39）
    ///
    /// `log_profiles[].ansi_codepage` の番号が実行環境に存在するかは Win32 の
    /// `GetCPInfoExW` にしか答えられず、`hakutaku_config` は Win32 に依存しない。
    /// 判定を持つのは形式判定層（`hakutaku_core::codepage_available` が再公開）
    /// であるため、両方を知っているこの起動経路で結び付け、他の検証エラーと
    /// 同じ一覧（`CFG-016` の一括提示）へ合流させる。
    #[must_use]
    pub fn load(config_path: &Path) -> Self {
        match hakutaku_config::load_config_with_codepage_check(
            config_path,
            &hakutaku_core::codepage_available,
        ) {
            LoadOutcome::Loaded(config) => ConfigState {
                route: ConfigRoute::Loaded,
                config,
                errors: Vec::new(),
            },
            LoadOutcome::Missing => ConfigState {
                route: ConfigRoute::Missing,
                config: HakutakuConfig::default(),
                errors: Vec::new(),
            },
            LoadOutcome::Invalid(errors) => ConfigState {
                route: ConfigRoute::Invalid,
                config: HakutakuConfig::default(),
                errors: errors.into_iter().collect(),
            },
        }
    }

    /// 診断ログのローテーション設定（`CFG-020`）。
    ///
    /// 経路によらず `self.config.diagnostics` から導出する。`Missing` /
    /// `Invalid` では `self.config` が組み込み既定値であるため、結果的に
    /// `hakutaku_diagnostics::RotationPolicy::default()` と同じ値になる。
    /// 適用順序の制約はモジュール doc コメントを参照。
    #[must_use]
    pub fn rotation_policy(&self) -> hakutaku_diagnostics::RotationPolicy {
        hakutaku_diagnostics::RotationPolicy {
            max_file_bytes: mib_to_bytes(self.config.diagnostics.rotate_mib),
            max_generations: self.config.diagnostics.keep_generations,
        }
    }

    /// メモリ予算をバイト単位で返す（`CFG-007`）。
    ///
    /// `budget_mib`（`u32`、単位 MiB）から `usize`（単位バイト）への変換は、
    /// 一度 `u64` で乗算してからオーバーフローの有無を明示的に確認する
    /// （[`mib_to_bytes`] の `saturating_mul`。実際には `u32::MAX` MiB でも
    /// `u64` の範囲に十分収まるためオーバーフローしないが、将来の変更に
    /// 備えて明示的に確認する）。`usize` への変換も `try_from` を使い、収まら
    /// ない場合は [`hakutaku_memory_accounting::DEFAULT_BUDGET_BYTES`] へ
    /// フォールバックする（Hakutaku は x86_64 のみを対象とするため
    /// （`src-tauri/build.rs` の対象アーキテクチャ検証）実際には到達しない）。
    #[must_use]
    pub fn memory_budget_bytes(&self) -> usize {
        let bytes = mib_to_bytes(self.config.memory.budget_mib);
        usize::try_from(bytes).unwrap_or(hakutaku_memory_accounting::DEFAULT_BUDGET_BYTES)
    }
}

/// MiB 単位の `u32` 値をバイト単位の `u64` へ変換する。
///
/// `u32::MAX`（約42億）× 1,048,576 は約 4.5 × 10^15 であり、`u64::MAX`
/// （約1.8 × 10^19）に対して十分小さいため、この乗算自体がオーバーフロー
/// することは無い。`saturating_mul` を使い、それでも明示的に安全側
/// （`u64::MAX`）へ倒す。
fn mib_to_bytes(mib: u32) -> u64 {
    u64::from(mib).saturating_mul(BYTES_PER_MIB)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一意な一時ファイルパスを作る（`std::env::temp_dir()` 配下。テスト専用の
    /// 一時領域であり、アプリ本体が実行時に書き込む対象ではない）。
    fn temp_yaml_path(label: &str) -> std::path::PathBuf {
        let unique = format!(
            "hakutaku-bootstrap-config-test-{label}-{}-{:?}.yaml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    #[test]
    fn missing_file_uses_builtin_defaults_with_no_errors() {
        let path = temp_yaml_path("missing");
        assert!(!path.exists());

        let state = ConfigState::load(&path);

        assert_eq!(state.route, ConfigRoute::Missing);
        assert_eq!(state.config, HakutakuConfig::default());
        assert!(state.errors.is_empty());
    }

    #[test]
    fn loaded_file_applies_configured_values() {
        let path = temp_yaml_path("loaded");
        std::fs::write(
            &path,
            "config_version: 1\nmemory:\n  budget_mib: 4096\ndiagnostics:\n  rotate_mib: 20\n  keep_generations: 3\n",
        )
        .unwrap();

        let state = ConfigState::load(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(state.route, ConfigRoute::Loaded);
        assert_eq!(state.config.memory.budget_mib, 4096);
        assert_eq!(state.config.diagnostics.rotate_mib, 20);
        assert_eq!(state.config.diagnostics.keep_generations, 3);
        assert!(state.errors.is_empty());
    }

    #[test]
    fn invalid_file_disables_data_sources_and_profiles_and_collects_errors() {
        // memory.budget_mib: 0 は値域外（CFG-016）で安全モードになる。
        // data_sources 自体は構文上妥当だが、安全モードでは load_config が
        // 部分的な設定値を返さないため、結果的に空になる。
        let path = temp_yaml_path("invalid");
        std::fs::write(
            &path,
            "config_version: 1\nmemory:\n  budget_mib: 0\ndata_sources:\n  - name: a\n    path: \"C:/Device/Logs\"\nlog_profiles:\n  - name: b\n    path_pattern: \"C:/Device/Logs/*.log\"\n",
        )
        .unwrap();

        let state = ConfigState::load(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(state.route, ConfigRoute::Invalid);
        assert_eq!(state.config, HakutakuConfig::default());
        assert!(state.config.data_sources.is_empty());
        assert!(state.config.log_profiles.is_empty());
        assert_eq!(state.errors.len(), 1);
        let error = &state.errors[0];
        assert_eq!(error.item_path, "memory.budget_mib");
        assert!(error.line.is_some());
        assert!(error.column.is_some());
        assert!(!error.reason.is_empty());
        assert!(!error.file_name.is_empty());
    }

    // 受け入れ条件: 実行環境に存在しないコードページ番号は、対象ファイルを開く
    // 時点まで待たずに起動時の一覧へ出る（`CFG-016`、`ENC-007`、Issue #39）。
    // 存在確認は `hakutaku_core::codepage_available` を注入して行う。
    #[test]
    fn unknown_ansi_codepage_is_reported_at_startup() {
        let path = temp_yaml_path("unknown-codepage");
        std::fs::write(
            &path,
            "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    ansi_codepage: 99999\n",
        )
        .unwrap();

        let state = ConfigState::load(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(state.route, ConfigRoute::Invalid);
        assert_eq!(state.errors.len(), 1);
        assert_eq!(state.errors[0].item_path, "log_profiles[0].ansi_codepage");
        assert!(state.errors[0].line.is_some());
    }

    // 受け入れ条件: 実行環境に存在するコードページ（CP932 / CP1252 は Windows
    // に標準で含まれる）は、注入した存在確認を通過して正常起動になる。
    #[test]
    fn known_ansi_codepage_still_loads() {
        let path = temp_yaml_path("known-codepage");
        std::fs::write(
            &path,
            "config_version: 1\nlog_profiles:\n  - name: a\n    path_pattern: \"C:/Device/Logs/*.log\"\n    ansi_codepage: 932\n",
        )
        .unwrap();

        let state = ConfigState::load(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(state.route, ConfigRoute::Loaded);
        assert_eq!(state.config.log_profiles[0].ansi_codepage, Some(932));
    }

    #[test]
    fn malformed_yaml_is_invalid_with_position() {
        let path = temp_yaml_path("malformed");
        std::fs::write(&path, "config_version: 1\nmemory:\n  budget_mib: [1\n").unwrap();

        let state = ConfigState::load(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(state.route, ConfigRoute::Invalid);
        assert_eq!(state.errors.len(), 1);
        assert!(state.errors[0].line.is_some());
    }

    #[test]
    fn rotation_policy_reflects_loaded_diagnostics_config() {
        let mut config = HakutakuConfig::default();
        config.diagnostics.rotate_mib = 20;
        config.diagnostics.keep_generations = 3;
        let state = ConfigState {
            route: ConfigRoute::Loaded,
            config,
            errors: Vec::new(),
        };

        let policy = state.rotation_policy();
        assert_eq!(policy.max_file_bytes, 20 * 1024 * 1024);
        assert_eq!(policy.max_generations, 3);
    }

    #[test]
    fn rotation_policy_falls_back_to_defaults_for_missing_route() {
        let state = ConfigState {
            route: ConfigRoute::Missing,
            config: HakutakuConfig::default(),
            errors: Vec::new(),
        };

        let policy = state.rotation_policy();
        let default_policy = hakutaku_diagnostics::RotationPolicy::default();
        assert_eq!(policy.max_file_bytes, default_policy.max_file_bytes);
        assert_eq!(policy.max_generations, default_policy.max_generations);
    }

    #[test]
    fn memory_budget_bytes_converts_mib_to_bytes() {
        let mut config = HakutakuConfig::default();
        config.memory.budget_mib = 2048;
        let state = ConfigState {
            route: ConfigRoute::Loaded,
            config,
            errors: Vec::new(),
        };

        assert_eq!(state.memory_budget_bytes(), 2048 * 1024 * 1024);
    }

    #[test]
    fn memory_budget_bytes_handles_maximum_u32_mib_without_overflow_or_panic() {
        let mut config = HakutakuConfig::default();
        config.memory.budget_mib = u32::MAX;
        let state = ConfigState {
            route: ConfigRoute::Loaded,
            config,
            errors: Vec::new(),
        };

        let expected = u64::from(u32::MAX) * 1024 * 1024;
        assert_eq!(state.memory_budget_bytes() as u64, expected);
    }

    #[test]
    fn mib_to_bytes_boundary_values() {
        assert_eq!(mib_to_bytes(0), 0);
        assert_eq!(mib_to_bytes(1), 1024 * 1024);
        assert_eq!(mib_to_bytes(2048), 2048 * 1024 * 1024);
        // オーバーフローしないことの確認（u32::MAX でも u64 に十分収まる）。
        assert_eq!(mib_to_bytes(u32::MAX), u64::from(u32::MAX) * 1024 * 1024);
    }
}
