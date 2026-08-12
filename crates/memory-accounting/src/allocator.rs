//! グローバルアロケータの計装（`TECH-006`）。
//!
//! [`CountingAllocator`] は `std::alloc::System` をラップし、成功した確保・解放を
//! すべて `ALLOCATED_BYTES`（現在の未解放確保量）と `PEAK_BYTES`（観測ピーク値）
//! へ原子的に計上します。
//!
//! # 再入禁止（ADR-0003 の判断の基準）
//!
//! `GlobalAlloc` の各メソッドの内部では、原子操作（`AtomicUsize` への
//! `fetch_add` / `fetch_sub` / `compare_exchange_weak`）以外を一切行いません。
//! 確保、ロック取得、ログ出力、`panic!` はすべて禁止です。これらを行うと、
//! アロケータの呼び出し中にアロケータ自身が再入され、無限再帰やデッドロックを
//! 起こします。
//!
//! 予約トークンへの帰属判定もここでは行いません（ADR-0003 の決定）。全確保を
//! 無条件に計上するだけで、[`crate::MemoryBudget::reserve`] /
//! [`crate::ReservationToken::mark_allocated`] が別途、予約と実確保の振り替えを
//! 担当します。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 現在の未解放確保量（バイト）。
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

/// 起動からの観測ピーク値（バイト）。一度上がった値は下がらない。
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

/// `std::alloc::System` を計装するグローバルアロケータです。
///
/// バイナリの `#[global_allocator]` に設置してください（`src-tauri/src/main.rs`
/// が Rust コアプロセスの全確保を計装するために設置しています）。フィールドを
/// 持たないゼロサイズ型で、状態はすべてこのモジュールの `static` へ保持します。
pub struct CountingAllocator;

// SAFETY（型全体）: CountingAllocator は状態を持たないゼロサイズ型であり、実際の
// 確保・解放は System へそのまま委譲する。GlobalAlloc の契約（layout の妥当性は
// 呼び出し元が保証する）はそのまま System へ引き継がれるため、この実装自体が
// 追加で満たすべき不変条件はない。各メソッド内の unsafe ブロックの根拠は個別に
// 記す。
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: 呼び出し元は GlobalAlloc::alloc の契約（layout がゼロサイズ
        // でないこと等）を満たす。ここでは同じ契約を持つ System::alloc へ
        // そのまま委譲するだけで、追加の unsafe 操作は行わない。
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            // 成功した確保だけを計上する（契約どおり）。
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: alloc と同じ契約で、System::alloc_zeroed へそのまま委譲する。
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: 呼び出し元は、この ptr・layout が対応する alloc 系呼び出しで
        // 得たものであることを GlobalAlloc::dealloc の契約として保証する。
        // 同じ契約を持つ System::dealloc へそのまま委譲する。
        unsafe { System.dealloc(ptr, layout) };
        // dealloc は失敗を表現しない（契約上、呼び出し元は有効な ptr・layout を
        // 渡す）ため、常に計上する。
        record_dealloc(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: 呼び出し元は GlobalAlloc::realloc の契約（ptr は layout で
        // alloc 済み、new_size はゼロでない）を満たす。同じ契約を持つ
        // System::realloc へそのまま委譲する。
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // 成功した場合だけ、拡大・縮小の差分を計上する
            // （ADR-0003「realloc はアロケータが差分だけを allocated_bytes へ
            // 計上する」）。
            let old_size = layout.size();
            if new_size > old_size {
                record_alloc(new_size - old_size);
            } else if new_size < old_size {
                record_dealloc(old_size - new_size);
            }
        }
        new_ptr
    }
}

/// 確保の成功を計上する。原子的な加算とピーク値更新だけを行い、確保・ロック・
/// ログ出力は一切行わない（再入禁止）。
fn record_alloc(size: usize) {
    if size == 0 {
        return;
    }
    let previous = ALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
    update_peak(previous + size);
}

/// 解放を計上する。原子的な減算だけを行う。
fn record_dealloc(size: usize) {
    if size == 0 {
        return;
    }
    ALLOCATED_BYTES.fetch_sub(size, Ordering::Relaxed);
}

/// `PEAK_BYTES` を `candidate` 以上に維持する。CAS ループのみで、ロック・確保を
/// 一切行わない（再入禁止）。
fn update_peak(candidate: usize) {
    let mut observed_peak = PEAK_BYTES.load(Ordering::Relaxed);
    while candidate > observed_peak {
        match PEAK_BYTES.compare_exchange_weak(
            observed_peak,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => observed_peak = actual,
        }
    }
}

/// 現在の未解放確保量（バイト）を返します。
#[must_use]
pub fn allocated_bytes() -> usize {
    ALLOCATED_BYTES.load(Ordering::Relaxed)
}

/// 起動からの観測ピーク値（バイト）を返します。一時的に確保量が跳ね上がり、
/// その後解放されて現在値が下がった場合でも、この値でピークを検出できます。
#[must_use]
pub fn peak_bytes() -> usize {
    PEAK_BYTES.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ピーク値更新（CAS ループ）が、より大きい値でのみ更新され、より小さい
    // 値では変化しないことを確認する。`record_alloc` / `record_dealloc` 経由
    // ではなく `update_peak` を直接呼ぶことで、このプロセス内の他テストの実際の
    // アロケーション量の影響を受けないようにする（決定的なテスト）。
    #[test]
    fn update_peak_only_increases() {
        let baseline = PEAK_BYTES.load(Ordering::Relaxed);

        update_peak(baseline + 100);
        assert_eq!(PEAK_BYTES.load(Ordering::Relaxed), baseline + 100);

        // より小さい値では変化しない。
        update_peak(baseline + 10);
        assert_eq!(PEAK_BYTES.load(Ordering::Relaxed), baseline + 100);

        // より大きい値ではさらに更新される。
        update_peak(baseline + 200);
        assert_eq!(PEAK_BYTES.load(Ordering::Relaxed), baseline + 200);
    }
}
