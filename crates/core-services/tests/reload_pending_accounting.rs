//! 再読み込み経路の一時バッファ（`PendingItem` の `Vec`）が、メモリ会計と
//! 整合して確保・解放されることの検証です（`PERF-008`・`PERF-010`）。
//!
//! # なぜ単体テストではなく統合テストか
//!
//! 検証したいのは「予約 → 実確保への振り替え → 解放」の一巡が会計値へ正しく
//! 現れることであり、実確保と解放を数えるのは
//! [`hakutaku_memory_accounting::CountingAllocator`] です。ライブラリの単体
//! テスト（`cargo test --lib`）にはこのアロケータが設置されないため
//! `allocated_bytes()` が常に 0 になり、解放の観測ができません。統合テストは
//! 独立したテストバイナリであり、グローバルアロケータを設置でき、グローバル
//! 予算（`OnceLock`）も他のテストと共有しないため、`outstanding_reserved_bytes`
//! を決定的に確認できます。

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use hakutaku_core::{
    register_source, reload_source, DisplaySetRegistry, ReloadOutcome, SourceBudget,
};
use hakutaku_memory_accounting::{allocated_bytes, global_budget};

#[global_allocator]
static GLOBAL: hakutaku_memory_accounting::CountingAllocator =
    hakutaku_memory_accounting::CountingAllocator;

/// 一時バッファの漏れを検出できる程度に大きく、テストの実行時間を延ばさない
/// 行数です。この行数なら一時バッファは
/// `20_000 * size_of::<PendingItem>()`（64ビット環境で約 960 KB）になり、
/// 解放されなければ下の許容差をはっきり超えます。
const LINE_COUNT: usize = 20_000;

/// 再読み込み前後の `allocated_bytes` の差として許容する量です。
///
/// 再読み込みが正味で増やしてよいのは、追記した1件分の常駐（索引・行番号・
/// 項目で `RESIDENT_BYTES_PER_ITEM`）と、経路上の小さな確保（パス文字列、
/// チャンク読み込みの作業バッファ、オンデマンド読み出しのキャッシュなど）
/// だけです。一時バッファ1本分（約 960 KB）よりはるかに小さいこの値を超えたら、
/// `PendingItem` の `Vec` が解放されずに残っている（＝会計と実態がずれている）
/// ことを意味します。
const ALLOWED_GROWTH_BYTES: usize = 256 * 1024;

struct TempFile {
    path: std::path::PathBuf,
}

impl TempFile {
    fn create_text(label: &str, contents: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "hakutaku-reload-pending-accounting-{label}-{}-{count}.log",
            std::process::id()
        ));
        std::fs::write(&path, contents.as_bytes()).expect("テスト用ファイルを作成できません");
        TempFile { path }
    }

    fn append_line(&self, line: &str) {
        let mut appender = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .expect("追記のために開けるはず");
        appender.write_all(line.as_bytes()).expect("追記できるはず");
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// 受け入れ条件（`PERF-010`）: 再読み込みが完走した後、一時バッファ
// のために行った予約が残らず（`outstanding_reserved_bytes` が 0）、確保した
// 一時バッファも解放されて会計の実確保量へ残らない。
#[test]
fn reload_releases_the_pending_buffer_and_leaves_no_outstanding_reservation() {
    let mut contents = String::new();
    for index in 0..LINE_COUNT {
        contents.push_str(&format!(
            "2026/07/28 15:{:02}:{:02}.000 メッセージ{index}\n",
            (index / 60) % 60,
            index % 60
        ));
    }
    let file = TempFile::create_text("reload", &contents);

    let mut registry = DisplaySetRegistry::new();
    let budget = SourceBudget::new();
    let (handle, _summary) = register_source(
        &mut registry,
        &budget,
        &file.path,
        "reload.log".to_string(),
        &[],
    )
    .expect("初回登録は成功するはず");
    assert_eq!(handle.total_items, LINE_COUNT as u64);
    assert_eq!(
        global_budget().outstanding_reserved_bytes(),
        0,
        "初回登録の予約はすべて振り替え済みのはず"
    );

    // 追記してから再読み込みする（`stream_decode_and_index` を通る経路）。
    file.append_line("2026/07/28 16:00:00.000 追記行\n");
    let allocated_before_reload = allocated_bytes();

    let outcome = reload_source(&mut registry, &budget, handle.source_id, &[])
        .expect("登録済みなので None にはならないはず");
    let ReloadOutcome::Reloaded { total_items, .. } = outcome else {
        panic!("追記後の再読み込みは Reloaded になるはず: {outcome:?}");
    };
    assert_eq!(
        total_items,
        LINE_COUNT as u64 + 1,
        "追記した1行が項目として増えるはず"
    );

    assert_eq!(
        global_budget().outstanding_reserved_bytes(),
        0,
        "再読み込み完了後に予約残が0へ戻るはず（振り替え漏れ・返却漏れがない）"
    );

    let allocated_after_reload = allocated_bytes();
    let growth = allocated_after_reload.saturating_sub(allocated_before_reload);
    assert!(
        growth < ALLOWED_GROWTH_BYTES,
        "再読み込み後も一時バッファが残っている可能性がある（増分 {growth} バイト）"
    );
}
