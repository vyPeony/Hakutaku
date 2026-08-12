//! 計装なし（`#[global_allocator]` 未設置、既定の `System` アロケータのまま）
//! での比較用測定です（P02-5）。
//!
//! `overhead_with.rs` と同一のワークロード（`support/workload.rs`）を使い、
//! 計装の有無だけを差分にします。
//!
//! ```text
//! cargo run --release -p hakutaku-memory-accounting --example overhead_without
//! ```

#[path = "support/workload.rs"]
mod workload;

fn main() {
    let elapsed = workload::run();
    println!("[overhead_without] iterations={}", workload::ITERATIONS);
    println!("[overhead_without] elapsed={elapsed:?}");
}
