//! `overhead_with` / `overhead_without` の両方で使う共通ワークロードです
//! （P02-5「計装のオーバーヘッド測定」）。
//!
//! 計装あり・なしの条件で「この1点（`#[global_allocator]` の設置有無）以外
//! すべて揃える」ため、確保・解放パターンそのものをこのファイルへ集約します。
//! `examples/` 直下ではなく `examples/support/` サブディレクトリへ置くことで、
//! Cargo のサンプル自動検出（`examples/*.rs` を独立したサンプルとして扱う規則）
//! の対象から外し、`fn main` を持たない共有モジュールとして
//! `#[path = "support/workload.rs"] mod workload;` から読み込めるようにして
//! います（`examples/*/main.rs` 形式のディレクトリ例とも異なるため、単独の
//! サンプルとしては検出されません）。

use std::time::{Duration, Instant};

/// 1回の呼び出しで行う反復回数です。1反復あたり Vec・String・Box の確保を
/// それぞれ複数回（拡張の realloc を含む）行うため、総確保・解放回数は
/// 数百万〜一千万オーダーになります。
pub const ITERATIONS: u32 = 2_000_000;

/// さまざまなサイズの `Vec<u8>` / `String` / `Box<[u8]>` の確保・解放・realloc
/// を [`ITERATIONS`] 回繰り返し、所要時間を返します。
///
/// サイズを反復ごとに変えているのは、単一サイズに特化した最適化（例えば
/// 特定サイズのフリーリスト）だけに依存した測定にならないようにするためです。
pub fn run() -> Duration {
    let start = Instant::now();

    for i in 0..ITERATIONS {
        let size = 16 + (i as usize % 256);

        // Vec<u8>: 確保 → 拡張（realloc）→ 解放。
        let mut v: Vec<u8> = Vec::with_capacity(size);
        v.resize(size, (i % 251) as u8);
        v.reserve(size); // 容量を超える要求で realloc（拡張）を誘発する。
        v.push(1);
        std::hint::black_box(&v);
        drop(v);

        // String: 確保 → 追記（容量超過で realloc）→ 解放。
        let mut s = String::with_capacity(size / 2);
        for _ in 0..4 {
            s.push('a');
        }
        s.push_str("overhead-measurement-workload");
        std::hint::black_box(&s);
        drop(s);

        // Box<[u8]>: 確保して即解放。
        let b: Box<[u8]> = vec![0u8; size / 2 + 1].into_boxed_slice();
        std::hint::black_box(&b);
        drop(b);
    }

    start.elapsed()
}
