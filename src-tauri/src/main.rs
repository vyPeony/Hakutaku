#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Rust コアプロセス（この Tauri バイナリ）のグローバルアロケータを計装する
/// （`TECH-006`、`PERF-008`）。
///
/// `PERF-008` が求める予算は「Rust コアプロセスのヒープ確保量の合計」であり、
/// Hakutaku 自身の確保だけでなく利用する全クレートの内部確保を含む。これを
/// 計装で漏れなく捕捉するには、実行ファイルのエントリポイントである
/// `main.rs` に `#[global_allocator]` を設置し、プロセスの起動直後から全確保
/// 経路を通す必要がある（ADR-0003、`crates/memory-accounting`）。
#[global_allocator]
static GLOBAL_ALLOCATOR: hakutaku_memory_accounting::CountingAllocator =
    hakutaku_memory_accounting::CountingAllocator;

fn main() {
    hakutaku_lib::run();
}
