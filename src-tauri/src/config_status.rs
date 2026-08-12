//! `get_config_status` Tauri コマンド（`CFG-015`〜`CFG-017`、P03-2）。
//!
//! 起動時に読み込んだ設定状態（[`crate::bootstrap::config::ConfigState`]、Tauri の
//! managed state として保持している）を、フロントエンド向けの応答型へ変換して
//! 公開する。`crates/config` の型（`hakutaku_config::HakutakuConfig` 等）を
//! そのまま公開せず、UI 表示に必要な情報（起動経路とエラー一覧）だけを持つ
//! 専用の型（[`ConfigStatusResponse`]）にする。
//!
//! 実際のバナー表示・エラー一覧表示は `src/main.js` が行う。ここでは値の
//! 受け渡しだけを担当する。

use serde::Serialize;
use tauri::State;

use crate::bootstrap::config::{ConfigRoute, ConfigState};

/// MiB 単位の `u32` をバイト単位の `u64` へ変換する。
///
/// `crate::bootstrap::config` の非公開ヘルパー（`mib_to_bytes`）と同じ考え方
/// （`u32::MAX` MiB でも `u64` の範囲に十分収まるが、将来の変更に備えて
/// `saturating_mul` で明示的に安全側へ倒す）をこのモジュールでも踏襲する。
const BYTES_PER_MIB: u64 = 1024 * 1024;

fn mib_to_bytes(mib: u32) -> u64 {
    u64::from(mib).saturating_mul(BYTES_PER_MIB)
}

/// フロントエンドへ公開する起動経路
/// （`tasks/phase-03-configuration.md` の「設定の三つの起動経路」）。
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigStatusRoute {
    /// 正常起動。通知なし。
    Loaded,
    /// 既定値起動（`CFG-015`）。非致命的な通知を表示する。
    Missing,
    /// 安全モード（`CFG-016`）。警告バナーとエラー一覧を表示する。
    Invalid,
}

impl From<ConfigRoute> for ConfigStatusRoute {
    fn from(route: ConfigRoute) -> Self {
        match route {
            ConfigRoute::Loaded => ConfigStatusRoute::Loaded,
            ConfigRoute::Missing => ConfigStatusRoute::Missing,
            ConfigRoute::Invalid => ConfigStatusRoute::Invalid,
        }
    }
}

/// 安全モード（`Invalid`）のエラー1件。
///
/// `CFG-016` の表示要件（ファイル名・行・列・理由）に、項目パスを加えたもの。
/// 行・列は特定できない場合があるため `Option`（構文エラーの一部など。
/// `hakutaku_config::ConfigError` と同じ）。
#[derive(Clone, Debug, Serialize)]
pub struct ConfigStatusError {
    pub file_name: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub item_path: String,
    pub reason: String,
}

impl From<&hakutaku_config::ConfigError> for ConfigStatusError {
    fn from(error: &hakutaku_config::ConfigError) -> Self {
        ConfigStatusError {
            file_name: error.file_name.clone(),
            line: error.line,
            column: error.column,
            item_path: error.item_path.clone(),
            reason: error.reason.clone(),
        }
    }
}

/// フロントエンドが保持する行の上限（`CFG-022`）。
///
/// `crates/config` の `FrontendConfig`（`max_rows` と `max_mib`）から導出する。
/// フロントエンドはこの値をハードコードせず、起動時に取得したこの応答から
/// 読み取る（`tasks/phase-04-vertical-slice.md` 作業項目6）。`route` が
/// `Missing` / `Invalid` の場合も、`ConfigState::config` が組み込み既定値
/// （[`hakutaku_config::HakutakuConfig::default`]）であるため、常に有効な値が
/// 入る。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct FrontendRetentionLimits {
    /// 保持行数の上限。
    pub max_rows: u32,
    /// 保持バイト数の上限（`max_mib` を MiB からバイトへ変換した値）。
    pub max_bytes: u64,
}

impl From<&hakutaku_config::FrontendConfig> for FrontendRetentionLimits {
    fn from(frontend: &hakutaku_config::FrontendConfig) -> Self {
        FrontendRetentionLimits {
            max_rows: frontend.max_rows,
            max_bytes: mib_to_bytes(frontend.max_mib),
        }
    }
}

/// `get_config_status` コマンドの応答。
#[derive(Clone, Debug, Serialize)]
pub struct ConfigStatusResponse {
    pub route: ConfigStatusRoute,
    /// `route` が `Invalid` のときだけ非空。
    pub errors: Vec<ConfigStatusError>,
    /// フロントエンドの保持上限（`CFG-022`）。P04-2。
    pub frontend_retention: FrontendRetentionLimits,
    /// 設定で事前定義されたデータソースの表示名一覧（`CFG-003`／`PROD-006`、
    /// P07-1）。
    ///
    /// `SEC-012` に従い、パスは含めません。名前だけを渡し、共通シェルの
    /// 参照対象一覧はこの名前を左ペインへ表示します。実際に開く際は
    /// `crate::targets::open_config_data_source(name)` を名前で呼び出し、
    /// パスの解決は Rust 側（`ConfigState`）だけで行います。`route` が
    /// `Invalid`（安全モード）の場合、`ConfigState::config` が組み込み既定値
    /// （データソースは空）になるため、ここも自動的に空になります
    /// （`CFG-016` の「設定由来のデータソースを無効化する」要件）。
    pub data_source_names: Vec<String>,
}

impl From<&ConfigState> for ConfigStatusResponse {
    fn from(state: &ConfigState) -> Self {
        ConfigStatusResponse {
            route: ConfigStatusRoute::from(state.route),
            errors: state.errors.iter().map(ConfigStatusError::from).collect(),
            frontend_retention: FrontendRetentionLimits::from(&state.config.frontend),
            data_source_names: state
                .config
                .data_sources
                .iter()
                .map(|data_source| data_source.name.clone())
                .collect(),
        }
    }
}

/// 起動時の設定読み込み結果を返す（フロントエンドの起動時通知用）。
///
/// `Loaded` → 通知なし。`Missing` → 非致命的な通知（`CFG-015`）。`Invalid` →
/// 安全モードの警告とエラー一覧（`CFG-016`）。
#[tauri::command]
pub fn get_config_status(state: State<'_, ConfigState>) -> ConfigStatusResponse {
    ConfigStatusResponse::from(state.inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hakutaku_config::{ConfigError, HakutakuConfig};

    #[test]
    fn loaded_route_converts_with_no_errors() {
        let state = ConfigState {
            route: ConfigRoute::Loaded,
            config: HakutakuConfig::default(),
            errors: Vec::new(),
        };

        let response = ConfigStatusResponse::from(&state);

        assert!(matches!(response.route, ConfigStatusRoute::Loaded));
        assert!(response.errors.is_empty());
        assert_eq!(response.frontend_retention.max_rows, 10_000);
        assert_eq!(response.frontend_retention.max_bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn missing_route_converts_with_no_errors() {
        let state = ConfigState {
            route: ConfigRoute::Missing,
            config: HakutakuConfig::default(),
            errors: Vec::new(),
        };

        let response = ConfigStatusResponse::from(&state);

        assert!(matches!(response.route, ConfigStatusRoute::Missing));
        assert!(response.errors.is_empty());
        // Missing でも config は組み込み既定値のため、保持上限は既定値になる。
        assert_eq!(response.frontend_retention.max_rows, 10_000);
        assert_eq!(response.frontend_retention.max_bytes, 64 * 1024 * 1024);
    }

    #[test]
    fn invalid_route_carries_file_line_column_item_and_reason() {
        let state = ConfigState {
            route: ConfigRoute::Invalid,
            config: HakutakuConfig::default(),
            errors: vec![ConfigError {
                file_name: "C:\\App\\hakutaku.yaml".to_string(),
                line: Some(3),
                column: Some(5),
                item_path: "memory.budget_mib".to_string(),
                reason: "1 以上の整数である必要があります".to_string(),
            }],
        };

        let response = ConfigStatusResponse::from(&state);

        assert!(matches!(response.route, ConfigStatusRoute::Invalid));
        assert_eq!(response.errors.len(), 1);
        let error = &response.errors[0];
        assert_eq!(error.file_name, "C:\\App\\hakutaku.yaml");
        assert_eq!(error.line, Some(3));
        assert_eq!(error.column, Some(5));
        assert_eq!(error.item_path, "memory.budget_mib");
        assert_eq!(error.reason, "1 以上の整数である必要があります");
    }

    // frontend_retention は route に関わらず config.frontend から導出される
    // （route が Missing / Invalid でも config は組み込み既定値のため有効な値になる）。
    // ここでは既定値以外の値（hakutaku.yaml で明示指定された場合を模す）で
    // max_mib → max_bytes の変換が正しいことを検証する。
    #[test]
    fn frontend_retention_converts_max_mib_to_max_bytes() {
        let config = HakutakuConfig {
            frontend: hakutaku_config::FrontendConfig {
                max_rows: 5_000,
                max_mib: 32,
            },
            ..HakutakuConfig::default()
        };
        let state = ConfigState {
            route: ConfigRoute::Loaded,
            config,
            errors: Vec::new(),
        };

        let response = ConfigStatusResponse::from(&state);

        assert_eq!(response.frontend_retention.max_rows, 5_000);
        assert_eq!(response.frontend_retention.max_bytes, 32 * 1024 * 1024);
    }

    // 受け入れ条件（CFG-003／PROD-006）: 設定で事前定義したデータソースの
    // 名前一覧が公開される。パスは含まれない（SEC-012）。
    #[test]
    fn data_source_names_are_exposed_without_paths_when_loaded() {
        let config = HakutakuConfig {
            data_sources: vec![
                hakutaku_config::DataSourceConfig {
                    name: "端末A".to_string(),
                    path: std::path::PathBuf::from("C:\\Device\\a.log"),
                },
                hakutaku_config::DataSourceConfig {
                    name: "端末B".to_string(),
                    path: std::path::PathBuf::from("C:\\Device\\b.log"),
                },
            ],
            ..HakutakuConfig::default()
        };
        let state = ConfigState {
            route: ConfigRoute::Loaded,
            config,
            errors: Vec::new(),
        };

        let response = ConfigStatusResponse::from(&state);

        assert_eq!(response.data_source_names, vec!["端末A", "端末B"]);
    }

    // CFG-016: 安全モードでは config.data_sources が組み込み既定値（空）に
    // なるため、data_source_names も自動的に空になる。
    #[test]
    fn data_source_names_are_empty_when_invalid_route_uses_builtin_defaults() {
        let state = ConfigState {
            route: ConfigRoute::Invalid,
            config: HakutakuConfig::default(),
            errors: vec![ConfigError {
                file_name: "hakutaku.yaml".to_string(),
                line: Some(1),
                column: Some(1),
                item_path: "memory.budget_mib".to_string(),
                reason: "不正な値です".to_string(),
            }],
        };

        let response = ConfigStatusResponse::from(&state);

        assert!(response.data_source_names.is_empty());
    }
}
