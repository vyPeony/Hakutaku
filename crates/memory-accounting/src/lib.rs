#![deny(unsafe_op_in_unsafe_fn)]

//! Hakutaku のメモリ会計クレートです（P02、ADR-0003 準拠）。
//!
//! # このクレートの位置づけ
//!
//! `PERF-008`（メモリ上限は Rust コアプロセスのヒープ確保量の合計に対する予算と
//! する。WebView2 プロセス群は含まない）と `PERF-010`（メモリ会計をコア設計の
//! 一部として初期実装から組み込み、大規模読み込み前の予約・拒否を必須とする）を
//! 実装する、コアに閉じたメモリ会計サービスです。計画正本は
//! `tasks/phase-02-memory-accounting.md`、確保を予約トークンへ帰属させる方式の
//! 決定は `docs/architecture/decisions/0003-memory-reservation-attribution.md`
//! （ADR-0003）です。**このクレートは ADR-0003 の会計契約に厳密に従います。**
//! 実装を変更する場合は、まず ADR-0003 を読み直してください。
//!
//! # 予算の定義（`PERF-008`）
//!
//! ここでいう「メモリ確保量」は、**Rust コアプロセス（`src-tauri` の実行ファイル
//! そのもの）のヒープ確保量の合計**です。Hakutaku 自身の確保だけでなく、利用する
//! すべてのクレートの内部確保を含みます。WebView2 は複数プロセス構成の別プロセス
//! 群として動作するため、その確保量はこの予算に含まれません。WebView2 を含む
//! 参考指標（`PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` の合計、`PERF-011`）は
//! 下記「参考指標」節（`private_usage` モジュール）が計測します。
//!
//! # 二つの計装（会計契約、ADR-0003）
//!
//! - [`allocated_bytes`] / [`peak_bytes`]: [`CountingAllocator`] がグローバル
//!   アロケータとして計装する、実際のヒープ確保量です。**予約の有無に関わらず、
//!   成功した全確保を無条件に計上します。** ここに予約トークンへの帰属判定は
//!   持ち込みません。アロケータ内部では原子操作以外を一切行いません
//!   （確保・ロック取得・ログ出力・panic の禁止。再入防止）。
//! - [`MemoryBudget`] / [`ReservationToken`]: 大きな確保の**前**に予約し、
//!   `allocated_bytes + outstanding_reserved_bytes + 要求量 <= 予算` を
//!   `outstanding_reserved_bytes` に対する CAS ループで原子的に判定して、予算を
//!   超える場合は拒否します。実確保の直後に
//!   [`ReservationToken::mark_allocated`] を呼ぶことで、予約トークンを所有する
//!   コード自身が予約から実確保へ明示的に振り替え、二重計上を避けます
//!   （ADR-0003 が採用した「明示的な確保 API」方式）。
//!
//! この2系統の値の関係が会計契約の中心です。詳細は [`MemoryBudget`] の
//! doc コメントと ADR-0003 を参照してください。
//!
//! # ソフトしきい値（`threshold` モジュール、P02-3）
//!
//! [`MemoryBudget`] は、予算に対する割合（既定 [`DEFAULT_SOFT_THRESHOLD_PERCENT`]、
//! **暫定設計で要件 ID を持ちません**）でソフトしきい値を判定します。判定は
//! [`MemoryBudget::reserve`] / [`ReservationToken::mark_allocated`] の操作時と、
//! 明示的な確認関数 [`MemoryBudget::check_soft_threshold`] でだけ行い、ADR-0003
//! に従ってアロケータの `alloc` / `dealloc` 経路では判定しません。到達は
//! エッジ検出で扱い、登録された解放処理（[`MemoryBudget::
//! register_release_handler`]）の呼び出しと先読み停止フラグ
//! （[`MemoryBudget::prefetch_paused`]）の設定を、超過中に繰り返しません。
//! **実際に解放する対象（索引、ログ本文バッファ、表示範囲）の登録は本クレートの
//! 対象外です**（P06・P08）。
//!
//! # 参考指標（`PrivateUsage` 合計、`PERF-011`、`private_usage` モジュール、P02-4）
//!
//! [`measure_private_usage`]（Windows 専用）は、自プロセスとその子孫プロセス
//! （Hakutaku 専用の Tauri／WebView2 子プロセス群）の
//! `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` 合計を1回計測します。**この値は
//! `PERF-008` の予算とは別の参考指標であり、合否判定には使いません。**
//! [`MemoryBudget::check_reference_indicator`] が、この合計を予算値 +
//! [`REFERENCE_INDICATOR_MARGIN_BYTES`]（1 GiB、暫定値）と比較し、超えた場合に
//! [`AccountingEvent::ReferenceIndicatorExceeded`] を発火します。詳細な設計
//! 判断（子孫プロセスの特定方法、PID 再利用への防御）は `private_usage`
//! モジュールの doc コメントを参照してください。
//!
//! # 会計イベント（`AccountingEvent`、`DIAG-005`）
//!
//! 予約の拒否・ソフトしきい値到達・参考指標の超過は [`AccountingEvent`] として、
//! [`MemoryBudget::set_event_sink`] で登録した通知先へ届きます。**このクレートは
//! `hakutaku-diagnostics` に依存しません**（コアの層を薄く保つ設計判断）。診断
//! ログへの実際の記録は、通知先を登録する呼び出し側（`src-tauri`）の責務です。
//!
//! # このクレートが扱わないもの
//!
//! 参考指標の定期計測のスケジューリングと、計測手段そのものの診断ログクレート
//! への配線は、それぞれ呼び出し側（P04 以降）と `src-tauri` 側の役割です。

mod allocator;
mod budget;
mod private_usage;
mod threshold;

pub use allocator::{allocated_bytes, peak_bytes, CountingAllocator};
pub use budget::{
    global_budget, set_global_budget_bytes, MarkAllocatedError, MemoryBudget, ReservationRejected,
    ReservationToken, DEFAULT_BUDGET_BYTES,
};
#[cfg(windows)]
pub use private_usage::measure_private_usage;
#[cfg(windows)]
pub use private_usage::MeasurePrivateUsageError;
pub use private_usage::{
    PrivateUsageSample, ProcessPrivateUsage, REFERENCE_INDICATOR_MARGIN_BYTES,
};
pub use threshold::{AccountingEvent, InvalidSoftThresholdPercent, DEFAULT_SOFT_THRESHOLD_PERCENT};
