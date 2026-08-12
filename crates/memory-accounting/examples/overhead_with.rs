//! 計装あり（[`CountingAllocator`] を `#[global_allocator]` に設置した状態）
//! でのオーバーヘッド測定です（P02-5、`TECH-001` との両立確認）。
//!
//! `overhead_without.rs` と同一のワークロード（`support/workload.rs`）を使い、
//! 計装の有無だけを差分にします。
//!
//! ```text
//! cargo run --release -p hakutaku-memory-accounting --example overhead_with
//! ```

#[path = "support/workload.rs"]
mod workload;

use hakutaku_memory_accounting::{allocated_bytes, peak_bytes, CountingAllocator};

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn main() {
    let elapsed = workload::run();
    println!("[overhead_with] iterations={}", workload::ITERATIONS);
    println!("[overhead_with] elapsed={elapsed:?}");
    println!(
        "[overhead_with] allocated_bytes={} peak_bytes={}",
        allocated_bytes(),
        peak_bytes()
    );
}
