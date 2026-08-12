//! 進捗・エラー・キャンセルの共通契約（P04-6）。
//!
//! # 位置づけ
//!
//! このモジュールは `tasks/phase-04-vertical-slice.md` の実装単位 P04-6
//! （「進捗・エラー・キャンセルの共通契約の定義」）を実装します。P06
//! （大容量読み込み、`tasks/phase-06-large-file-loading.md`）と P07（共通
//! シェル UI、`tasks/phase-07-shell-ui.md`）の両方が依存する契約であり、
//! **この契約の型だけに依存すれば、P06 と P07 は互いの完成を待たずに並行
//! 着手できます**（`tasks/README.md` の「開始依存と完了依存を分ける」）。
//!
//! - **P06 が発行側です。** 読み込み・索引構築の進捗を [`ProgressSink`]
//!   経由で通知し、キャンセルを [`CancellationToken`] で検出し、処理単位の
//!   結果を [`TaskOutcome`] で終えます。
//! - **P07 が受信側です。** `src-tauri` 経由で [`ProgressSink`] を実装し、
//!   受け取った通知を Tauri イベントへ変換して WebView 側の UI へ表示し
//!   ます。
//!
//! **このモジュールは型と契約の定義、および契約の性質を検証する単体テスト
//! だけを持ちます。** 実際の読み込み進捗の発行（P06）、UI 表示（P07）、
//! Tauri イベントへの変換（`src-tauri`）は、いずれもこのモジュールの対象外
//! です。GUI 非依存を保つため、このクレート自体も Tauri へ依存しません
//! （`crates/core-services/tests/tauri_independence.rs` が検査します）。
//!
//! # 構成
//!
//! - [`TaskId`]: 進捗・キャンセルの対象となる処理単位を識別する ID
//! - [`Progress`] / [`ProgressUnit`] / [`ProgressSink`] / [`ProgressThrottle`]:
//!   進捗の契約と、通知単位（間引き）の規約
//! - [`UserFacingError`]: 利用者向けエラーの共通型（`ERR-002`）
//! - [`CancellationToken`]: キャンセル要求の共有と検出
//! - [`TaskOutcome`]: 処理単位の最終結果（`Completed` / `Failed` / `Cancelled`）
//!
//! # serde を追加しない設計判断
//!
//! この契約は GUI 非依存かつ純粋な Rust の型として定義し、`serde` などの
//! 直列化クレートへ依存しません。Tauri イベントや DTO への変換（直列化可能
//! な形への変換）は `src-tauri` 側の責務とし、コア層（このクレート）は型の
//! 意味だけを持ちます。外部依存クレートを追加しないという P04-6 の
//! 実装方針にも合致します。

mod cancellation;
mod error;
mod outcome;
mod progress;
mod task_id;

pub use cancellation::CancellationToken;
pub use error::UserFacingError;
pub use outcome::TaskOutcome;
pub use progress::{
    Progress, ProgressSink, ProgressThrottle, ProgressUnit, DEFAULT_MIN_NOTIFY_AMOUNT_BYTES,
    DEFAULT_MIN_NOTIFY_INTERVAL,
};
pub use task_id::TaskId;
