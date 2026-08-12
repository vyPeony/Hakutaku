//! グローバルアロケータ計装の統合テストです。
//!
//! `#[global_allocator]` はプロセス（テストバイナリ）全体に対して一度だけ設定
//! できるため、実際に [`CountingAllocator`] を計装として動かした状態での検証
//! （`allocated_bytes` / `peak_bytes` の実測、`Vec` の確保・解放・再確保）は、
//! このファイル（1つの統合テストバイナリ）にまとめています。
//!
//! `cargo test` は既定で同一テストバイナリ内のテスト関数を複数スレッドで並行
//! 実行します。ここでのテストはプロセス全体で共有される計装値を検証するため、
//! 他テストの確保・解放が計装値に混ざるとテストが flaky になり得ます。
//! そのため、各テストの先頭で [`serialize`] を呼んでロックを取得し、このファイル
//! 内のテストを実質的に直列実行させています。それでも、テストハーネス自体が
//! 保有する他スレッド（このロックの外側で動く付随的な処理）による、ごく僅かな
//! 確保・解放の混入までは排除できません。実測では数百バイト程度の増減が
//! 双方向（増える方向にも減る方向にも）に観測されたため、比較は絶対値や
//! 一方向の不等式ではなく、[`assert_close`] による**双方向**の許容誤差
//! （[`NOISE_MARGIN_BYTES`]）付きで行い、flaky にならないようにしています。

use std::sync::{Arc, Mutex, MutexGuard};

use hakutaku_memory_accounting::{
    allocated_bytes, peak_bytes, AccountingEvent, CountingAllocator, MemoryBudget,
};

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

/// テスト間の計装値の混入を避けるための許容誤差（バイト）。
///
/// 直列化していても、テストハーネスやランタイム内部の付随的な確保・解放が
/// わずかに紛れ得るため、実測との比較にはこの余裕を持たせる。想定する確保量
/// （数百 KiB〜数 MiB オーダー）に対して、誤検出を防ぎつつ実装の不具合は検出
/// できる大きさにしている。
const NOISE_MARGIN_BYTES: usize = 256 * 1024;

/// このバイナリ内の統合テストを直列化するためのロックを取得します。
fn serialize() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `actual` が `expected` の [`NOISE_MARGIN_BYTES`] 以内であることを確認する。
///
/// 単純な `>=` や `<=` の一方向比較だと、テスト間に紛れ込む僅かな解放
/// （バックグラウンドの付随的な処理など）によって `actual` が `expected` を
/// 僅かに下回るだけで flaky に失敗し得る。双方向の差分（`abs_diff`）で
/// 判定することで、増減どちらのノイズにも同じ許容誤差で対応する。
fn assert_close(actual: usize, expected: usize, message: &str) {
    let diff = actual.abs_diff(expected);
    assert!(
        diff <= NOISE_MARGIN_BYTES,
        "{message}: actual={actual} expected={expected} diff={diff} margin={NOISE_MARGIN_BYTES}"
    );
}

// 受け入れ条件: グローバルアロケータの計装により、Rust プロセス内の確保量を
// 取得できる。未解放確保量の現在値を取得できる。
// 受け入れ条件: 予約トークンの外で利用クレートが行った確保も allocated_bytes に
// 計上される（ここでは std の Vec 経由の確保で代表させる。予約を一切経由して
// いない点が「予約の外の確保」に相当する）。
#[test]
fn allocated_bytes_tracks_vec_allocation_and_deallocation() {
    let _guard = serialize();

    let before = allocated_bytes();
    let amount = 4 * 1024 * 1024; // 4 MiB
    let data = vec![0u8; amount];
    assert_eq!(data.len(), amount);

    let during = allocated_bytes();
    assert_close(during, before + amount, "確保後は要求量だけ増えているはず");

    drop(data);
    let after = allocated_bytes();
    assert_close(after, before, "drop 後は確保前の水準に戻るはず");
}

// 受け入れ条件: 未解放確保量のピーク値を取得できる（処理後に解放されて現在値が
// 下がっても、一時的な超過をピーク値で検出できる）。
#[test]
fn peak_bytes_records_high_water_mark_even_after_release() {
    let _guard = serialize();

    let peak_before = peak_bytes();
    let current_before = allocated_bytes();

    let amount = 8 * 1024 * 1024; // 8 MiB
    let data = vec![0u8; amount];
    assert_eq!(data.len(), amount);

    let peak_during = peak_bytes();
    assert!(
        peak_during + NOISE_MARGIN_BYTES >= current_before + amount,
        "確保中のピークは、少なくとも確保直前の現在値 + 要求量に近いはず: \
         current_before={current_before} peak_during={peak_during} amount={amount}"
    );

    drop(data);

    let current_after = allocated_bytes();
    assert!(
        current_after < peak_during,
        "drop 後の現在値はピークより小さいはず（一時的な超過をピークで検出できる）: \
         current_after={current_after} peak_during={peak_during}"
    );
    // ピーク値そのものは drop しても下がらない。
    assert!(peak_bytes() >= peak_during);
    assert!(peak_bytes() >= peak_before);
}

// 受け入れ条件: realloc（Vec の拡張・縮小等）で差分だけが正しく増減する。
#[test]
fn realloc_via_vec_reserve_and_shrink_accounts_only_the_delta() {
    let _guard = serialize();

    let initial_capacity = 1024 * 1024; // 1 MiB
    let mut data: Vec<u8> = vec![0; initial_capacity];

    let after_initial = allocated_bytes();

    // 拡張: reserve_exact で、実際に増えた容量（バイト数。u8 なので容量 = バイト
    // 数）をそのまま実測できる形で増やす。
    let grow_by = 512 * 1024; // 512 KiB
    data.reserve_exact(grow_by);
    let capacity_after_grow = data.capacity();
    assert!(capacity_after_grow >= initial_capacity + grow_by);
    let grown_amount = capacity_after_grow - initial_capacity;

    let after_grow = allocated_bytes();
    assert_close(
        after_grow,
        after_initial + grown_amount,
        "拡張分だけ増えているはず（realloc の増分計上）",
    );

    // 縮小: 長さを初期容量まで戻してから shrink_to_fit で詰める。
    data.resize(initial_capacity, 0);
    data.shrink_to_fit();
    let capacity_after_shrink = data.capacity();
    assert!(capacity_after_shrink <= capacity_after_grow);
    let shrunk_amount = capacity_after_grow.saturating_sub(capacity_after_shrink);

    let after_shrink = allocated_bytes();
    assert_close(
        after_shrink,
        after_grow.saturating_sub(shrunk_amount),
        "縮小分だけ減っているはず（realloc の減分計上）",
    );

    drop(data);
}

// 受け入れ条件: 実確保時に予約から実確保へ振り替えられ、二重計上されない。
// グローバルアロケータの計装（allocated_bytes）と、独立した MemoryBudget
// インスタンスの outstanding_reserved_bytes を組み合わせ、「予約 → 実確保 →
// mark_allocated」の一連の流れで合計計上量が二重にならないことを確認する
// （ADR-0003 の会計契約の中心）。
#[test]
fn reservation_and_real_allocation_do_not_double_count() {
    let _guard = serialize();

    // テスト専用の独立インスタンス。global_budget() は使わず、他のテストや
    // 将来コードの予約と干渉しないようにする。
    let budget = MemoryBudget::new(64 * 1024 * 1024); // 64 MiB
    let allocated_before = allocated_bytes();

    let amount = 2 * 1024 * 1024; // 2 MiB
    let token = budget
        .reserve(amount)
        .expect("予算内なので予約は成功するはず");
    assert_eq!(budget.outstanding_reserved_bytes(), amount);

    // 予約の下で実際に確保する。
    let data = vec![0u8; amount];
    assert_eq!(data.len(), amount);
    let allocated_during = allocated_bytes();
    assert_close(
        allocated_during,
        allocated_before + amount,
        "予約の下での実確保は allocated_bytes に計上されるはず",
    );

    // 実確保の直後に振り替える。
    token
        .mark_allocated(amount)
        .expect("予約量ちょうどの振り替えは成功するはず");

    // 振り替え後は予約側が 0 になり、実確保側だけに計上されている
    // （二重計上されていない）。
    assert_eq!(budget.outstanding_reserved_bytes(), 0);
    let allocated_after_mark = allocated_bytes();
    assert_close(
        allocated_after_mark,
        allocated_before + amount,
        "振り替え後も allocated_bytes 側の計上量は変わらない（二重計上されない）",
    );

    drop(data);
    drop(token);
    assert_eq!(budget.outstanding_reserved_bytes(), 0);
}

// 受け入れ条件（暫定設計、P02-3）: しきい値判定は reserve と
// mark_allocated の操作時に行う（ADR-0003、アロケータ経路では行わない）。
// budget.rs の単体テストは CountingAllocator が設置されていないため
// allocated_bytes() が常に 0 を返す前提で書かれているが、ここでは実際に
// CountingAllocator を経由した確保を行い、reserve → 実確保 → mark_allocated
// という一連の経路全体で先読み停止フラグが正しく立つことを確認する。
#[test]
fn mark_allocated_path_triggers_soft_threshold_with_real_allocations() {
    let _guard = serialize();

    let budget = MemoryBudget::new(16 * 1024 * 1024); // 16 MiB
    budget
        .set_soft_threshold_percent(1)
        .expect("1は有効な割合のはず（境界値）");
    assert!(!budget.prefetch_paused());

    // 16 MiB の 1% ≒ 167 KiB。4 MiB の予約・実確保・振り替えで確実に跨ぐ。
    let amount = 4 * 1024 * 1024;
    let token = budget
        .reserve(amount)
        .expect("予算16 MiB内なので予約は成功するはず");

    let data = vec![0u8; amount];
    token
        .mark_allocated(amount)
        .expect("実確保量ちょうどの振り替えは成功するはず");

    assert!(
        budget.prefetch_paused(),
        "reserve → 実確保 → mark_allocated の経路全体でしきい値判定が行われ、\
         先読み停止が立つはず"
    );

    drop(data);
    drop(token);
}

// 受け入れ条件: 予約の拒否、しきい値到達が診断ログ相当の通知先（会計イベント）
// へ届く。実アロケータを使った状態で、set_event_sink に登録したテスト用の
// 通知先が両方のイベント種別を受け取れることを確認する。
#[test]
fn accounting_events_reach_registered_sink_with_real_allocations() {
    let _guard = serialize();

    let budget = MemoryBudget::new(8 * 1024 * 1024); // 8 MiB
    budget
        .set_soft_threshold_percent(1)
        .expect("1は有効な割合のはず（境界値）");

    let events: Arc<Mutex<Vec<AccountingEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_sink = Arc::clone(&events);
    assert!(budget.set_event_sink(Box::new(move |event| {
        events_sink.lock().unwrap().push(event);
    })));

    // しきい値到達（8 MiB の 1% ≒ 84 KiB を超える実確保）。
    let amount = 1024 * 1024;
    let token = budget.reserve(amount).expect("予算内なので成功するはず");
    let data = vec![0u8; amount];
    token
        .mark_allocated(amount)
        .expect("振り替えは成功するはず");
    assert!(budget.prefetch_paused());

    // 予約拒否（予算を大幅に超える要求）。
    let rejected = budget
        .reserve(1024 * 1024 * 1024)
        .expect_err("予算8 MiBを大幅に超える要求は拒否されるはず");

    let recorded = events.lock().unwrap();
    let has_threshold_event = recorded
        .iter()
        .any(|event| matches!(event, AccountingEvent::SoftThresholdReached { .. }));
    let has_rejection_event = recorded.iter().any(|event| {
        matches!(event, AccountingEvent::ReservationRejected(actual) if *actual == rejected)
    });
    assert!(has_threshold_event, "しきい値到達イベントが届くはず");
    assert!(has_rejection_event, "予約拒否イベントが届くはず");

    drop(data);
    drop(token);
}
