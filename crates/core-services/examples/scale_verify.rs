//! P06-4 実規模検証ハーネス。
//!
//! `tasks/phase-06-large-file-loading.md` の受け入れ条件のうち、規模に関する
//! 項目（1 GB 単一ファイル、合計 2 GB 以内の複数ファイル、超過時の拒否と
//! 既存表示の維持、メモリ会計の機能）を、実際のファイルを使って手動検証する
//! ための example バイナリです。プロダクトコード（`crates/core-services/src`）は
//! 変更せず、公開 API（`register_source`・`DisplaySetRegistry`・
//! `SourceBudget`・`hakutaku_memory_accounting`）だけを呼び出します。
//!
//! # 使い方
//!
//! 環境変数 `SCALE_FILES` に、登録したいファイルの絶対パスをセミコロン
//! （`;`）区切りで並べて渡します。指定した順に `register_source` を呼び、
//! 各段階の会計値・所要時間を日本語ラベルで標準出力へ出します。
//!
//! ```text
//! SCALE_FILES="C:\a.log;C:\b.log" cargo run --release -p hakutaku-core --example scale_verify
//! ```
//!
//! グローバルアロケータに `hakutaku_memory_accounting::CountingAllocator` を
//! 設置しているため、`allocated_bytes` / `peak_bytes` はこのバイナリ自身の
//! ヒープ確保量を反映します（`crates/memory-accounting` の doc コメント
//! 「予算の定義」参照）。
//!
//! 登録が成功した対象については、所要時間に続けて**段階別内訳**
//! （[`hakutaku_core::LoadStageTimings`]）も出します。I/O・デコード・解析・バッチ
//! 登録・その他の累計と、合計に対する割合が並ぶので、「どの段階が支配的か」を
//! 環境変数の追加なしに毎回確認できます。この内訳は release ビルドで常時有効
//! であり、計時点はチャンク境界にしか置いていません（オーバーヘッドが実測に
//! 現れない根拠は `LoadStageTimings` の doc コメント）。
//!
//! # 読み込み中のロック競合の計測
//!
//! 環境変数 `SCALE_CONTENTION=1` を併せて指定すると、通常の登録計測の代わりに
//! **読み込み中の範囲取得がどれだけ待たされるか**を計測します（`SCALE_FILES`
//! の先頭1件だけを使います）。
//!
//! ```text
//! $env:SCALE_CONTENTION="1"; $env:SCALE_FILES="C:\bench-3m.log"
//! cargo run --release -p hakutaku-core --example scale_verify
//! ```
//!
//! レジストリを `std::sync::Mutex` で包み（GUI 層＝`src-tauri` と同じ形）、
//! 読み込みを別スレッドで走らせながら、監視スレッドが一定間隔で
//! 「ロック取得 → `fetch_range`」を試みて待ち時間を記録します。次の2通りを
//! 同じファイルで続けて計測し、直接比較できるようにしています。
//!
//! - **改善前**: `register_source`（`&mut DisplaySetRegistry` を受け取る形）を
//!   ロックを保持したまま呼ぶ。読み込みが終わるまでロックが解放されない
//! - **改善後**: `register_source_with_access`。確定したバッチを
//!   登録する瞬間だけロックを取り直す
//!
//! # 再読み込み経路の計測
//!
//! 環境変数 `SCALE_RELOAD=1` を併せて指定すると、**再読み込み**
//! （`reload_source` → `stream_decode_and_index`）の会計と所要時間を計測します
//! （`SCALE_FILES` の先頭1件だけを使います）。
//!
//! ```text
//! $env:SCALE_RELOAD="1"; $env:SCALE_FILES="C:\near-1gib.log"
//! cargo run --release --locked -p hakutaku-core --example scale_verify
//! ```
//!
//! 手順は「作業用コピーの作成 → 登録 → コピーへ1行追記 → `reload_source` →
//! 会計スナップショット」です。**追記は `%TEMP%` の作業用コピーに対してだけ
//! 行い、指定された元ファイルは読み取りしかしません**（試験データの再生成
//! コストが高いため）。再読み込み中は `allocated_bytes` を短い間隔で標本化し、
//! 一時バッファ（`PendingItem` の `Vec`）の再確保による山を捉えます。
//!
//! # 読み込み完了時の統合ビュー同期の計測
//!
//! 環境変数 `SCALE_MERGED=1` を併せて指定すると、**統合表示 ON のまま新しい
//! 対象を開いたときの、読み込み完了時同期のコスト**を計測します
//! （`SCALE_FILES` の先頭2件を使います）。
//!
//! ```text
//! $env:SCALE_MERGED="1"; $env:SCALE_FILES="C:\bench-3m.log;C:\budget-c.log"
//! cargo run --release --locked -p hakutaku-core --example scale_verify
//! ```
//!
//! 「1件目を登録 → 統合表示を ON → 2件目を `register_source_with_access` で
//! 登録」という手順を、統合表示 OFF と ON で1回ずつ実行します。読み込み中の
//! レジストリ借用を1回ずつ計時し、**最後の借用（読み込み終了時の最終確定）**の
//! 差を取ることで、完了時同期（参加ソース全体の再マージ）の増分が分かります。
//! 併せて、統合表示集合の件数が両ソースの合計と一致することを確認します。
//!
//! # 範囲取得のアクセスパターン計測
//!
//! 環境変数 `SCALE_SCROLL=1` を併せて指定すると、**範囲取得のデコード済み
//! チャンクキャッシュがどれだけ効くか**を、3つのアクセスパターンで計測します
//! （`SCALE_FILES` の先頭1件だけを使います）。
//!
//! ```text
//! $env:SCALE_SCROLL="1"; $env:SCALE_FILES="C:\bench-3m.log"
//! cargo run --release --locked -p hakutaku-core --example scale_verify
//! ```
//!
//! パターンを分けているのは、GUI の経路とそれ以外で前提が違うためです。
//! `src/log_view.js` の `CHUNK_SIZE`（512）は
//! [`hakutaku_core::MAX_ITEMS_PER_RESPONSE`] と一致させてあり、GUI からの範囲
//! 取得は常に512件境界へ整列しています。一方、コピー（`copy_selection`）や
//! 統合表示集合は任意の開始位置・件数で要求します。
//!
//! 1. **整列前方スクロール**: 512件境界を先頭から順に [`SCROLL_FETCHES`] 回。
//!    毎回まだ読んでいないチャンクなので、キャッシュの照合規則によらず必ず
//!    ミスになります（前方スクロールの初回コストはキャッシュでは減りません）
//! 2. **整列往復スクロール**: キャッシュ容量に収まる範囲（8チャンク）を往復。
//!    同じ要求の繰り返しなので、完全一致だけでもヒットします
//! 3. **非整列サブ範囲**: 表示済みチャンクの内側から、任意の開始位置・件数で
//!    取得（コピー選択に相当）。**包含判定が効くのはここです**
//!
//! 各パターンでキャッシュのヒット率、ソース再オープン回数（
//! [`hakutaku_core::FetchPathMetrics`]）、所要時間の平均・最大を出力します。
//! 併せて、取得した本文・識別子から求めた検査値を出力するので、実装の前後で
//! 取得内容が1バイトも変わっていないことを突き合わせられます。

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hakutaku_core::{
    fetch_path_metrics, register_source, register_source_with_access, reload_source,
    reset_fetch_path_metrics, DisplaySetHandle, DisplaySetRegistry, FetchRangeError, LoadControl,
    LoadStageTimings, RangeRequest, RegistryAccess, ReloadOutcome, SourceBudget,
    MAX_SINGLE_FILE_BYTES, MAX_SOURCE_COUNT, MAX_TOTAL_BYTES,
};
use hakutaku_memory_accounting::{
    allocated_bytes, global_budget, peak_bytes, AccountingEvent, DEFAULT_BUDGET_BYTES,
};

#[global_allocator]
static GLOBAL: hakutaku_memory_accounting::CountingAllocator =
    hakutaku_memory_accounting::CountingAllocator;

/// 範囲取得スモークテストで1回に要求する件数（`MAX_ITEMS_PER_RESPONSE` と
/// 同じ暫定値。作業指示「先頭・中間・末尾の各512件取得」）。
const SMOKE_FETCH_ITEMS: u32 = 512;

fn format_bytes_u64(bytes: u64) -> String {
    format!(
        "{bytes} バイト（約 {:.3} MiB）",
        bytes as f64 / (1024.0 * 1024.0)
    )
}

fn format_bytes_usize(bytes: usize) -> String {
    format_bytes_u64(bytes as u64)
}

/// `hakutaku_memory_accounting::global_budget()` と `CountingAllocator` の
/// 現在値をまとめて1行で出す。
fn print_memory_accounting_snapshot(prefix: &str) {
    let budget = global_budget();
    println!(
        "{prefix} allocated_bytes={} / peak_bytes={} / outstanding_reserved_bytes={} / \
         budget_bytes={} / prefetch_paused={}",
        format_bytes_usize(allocated_bytes()),
        format_bytes_usize(peak_bytes()),
        format_bytes_usize(budget.outstanding_reserved_bytes()),
        format_bytes_usize(budget.budget_bytes()),
        budget.prefetch_paused(),
    );
}

/// 読み込みの段階別内訳（[`LoadStageTimings`]）を出す。
///
/// 各段階の秒数に加えて合計に対する割合を出すのは、「どの段階が支配的か」が
/// この計測の目的だからです（絶対値だけだと、ファイルサイズの違う実測どうしを
/// 見比べられません）。
fn print_stage_timings(prefix: &str, timings: &LoadStageTimings) {
    let total = timings.total.as_secs_f64();
    let share = |part: std::time::Duration| {
        if total <= 0.0 {
            0.0
        } else {
            part.as_secs_f64() * 100.0 / total
        }
    };
    println!("{prefix} 合計 {total:.3} 秒");
    for (label, part) in [
        ("I/O（チャンク読み取り）", timings.io_read),
        ("デコード（判定・変換）", timings.decode),
        ("解析（行分割・日時・継続行）", timings.parse),
        ("バッチ登録（レジストリ借用）", timings.deliver),
        (
            "その他（オープン・上限判定・整合性再確認ほか）",
            timings.other(),
        ),
    ] {
        println!(
            "    {label}: {:.3} 秒（{:.1}%）",
            part.as_secs_f64(),
            share(part)
        );
    }
}

/// `SourceBudget`（PERF-004〜006、ファイル単位・合計サイズ・ファイル数の上限）の
/// 現在値を1行で出す。
fn print_source_budget_snapshot(prefix: &str, budget: &SourceBudget) {
    println!(
        "{prefix} 登録ファイル数={} / 合計サイズ={} / 単一ファイル上限={} / \
         ファイル数上限={} / 合計サイズ上限={}",
        budget.count(),
        format_bytes_u64(budget.total_bytes()),
        format_bytes_u64(MAX_SINGLE_FILE_BYTES),
        MAX_SOURCE_COUNT,
        format_bytes_u64(MAX_TOTAL_BYTES),
    );
}

fn smoke_fetch(
    registry: &mut DisplaySetRegistry,
    handle: &DisplaySetHandle,
    label: &str,
    start: u64,
) {
    let request = RangeRequest {
        start,
        max_items: SMOKE_FETCH_ITEMS,
        expected_generation: handle.generation,
    };
    let begin = Instant::now();
    let result = registry.fetch_range(handle.display_set_id, request);
    let elapsed = begin.elapsed();
    match result {
        Ok(response) => {
            println!(
                "    [{label}] start={start} 取得件数={} truncated={} 所要時間={:.3} ms",
                response.items.len(),
                response.truncated,
                elapsed.as_secs_f64() * 1000.0,
            );
        }
        Err(error) => {
            println!(
                "    [{label}] start={start} エラー: {error}（所要時間={:.3} ms）",
                elapsed.as_secs_f64() * 1000.0
            );
        }
    }
}

/// 監視スレッドがロック取得と範囲取得を試みる間隔です。
///
/// UI のポーリング間隔（`src/shell.js` の 500ms）より十分に細かくし、読み込み
/// 中の待ち時間の山を取りこぼさないようにしています。
const CONTENTION_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// `Mutex` 越しにレジストリを借りる [`RegistryAccess`] 実装です（`src-tauri` の
/// `PerBatchRegistryLock` と同じ形）。
struct MutexRegistryAccess(Arc<Mutex<DisplaySetRegistry>>);

impl RegistryAccess for MutexRegistryAccess {
    fn with_registry<R>(&mut self, borrow: impl FnOnce(&mut DisplaySetRegistry) -> R) -> R {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        borrow(&mut guard)
    }
}

/// 読み込み中のロック競合の計測結果。
struct ContentionResult {
    load_elapsed: Duration,
    max_lock_wait: Duration,
    lock_count: usize,
    partial_observations: usize,
    fetch_ok: usize,
    max_fetch_elapsed: Duration,
    total_items: u64,
    reserved_bytes: usize,
    generation: u64,
}

/// 読み込みを別スレッドで実行しながら、レジストリのロック取得と範囲取得に
/// かかる時間を計測します（モジュール doc コメント「読み込み中のロック競合の
/// 計測」参照）。
///
/// `split_lock` が真なら改善後（バッチ境界で借り直す）、偽なら改善前
/// （読み込み中ずっと保持する）の形で読み込みます。
fn measure_contention(path: &Path, label: &str, split_lock: bool) -> ContentionResult {
    let registry = Arc::new(Mutex::new(DisplaySetRegistry::new()));
    let finished = Arc::new(AtomicBool::new(false));

    let loader_registry = Arc::clone(&registry);
    let loader_finished = Arc::clone(&finished);
    let loader_path = path.to_path_buf();
    let loader_label = label.to_string();
    let loader = std::thread::spawn(move || {
        let budget = SourceBudget::new();
        let started = Instant::now();
        let outcome = if split_lock {
            register_source_with_access(
                &mut MutexRegistryAccess(Arc::clone(&loader_registry)),
                &budget,
                &loader_path,
                loader_label,
                &[],
                &LoadControl::none(),
            )
            .map(|outcome| (outcome.handle, outcome.summary))
        } else {
            let mut guard = loader_registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            register_source(&mut guard, &budget, &loader_path, loader_label, &[])
        };
        let load_elapsed = started.elapsed();
        loader_finished.store(true, Ordering::SeqCst);
        (outcome, load_elapsed)
    });

    let mut max_lock_wait = Duration::ZERO;
    let mut max_fetch_elapsed = Duration::ZERO;
    let mut seen_totals: Vec<u64> = Vec::new();
    let mut lock_count = 0usize;
    let mut fetch_ok = 0usize;
    while !finished.load(Ordering::SeqCst) {
        let begin = Instant::now();
        let mut guard = registry.lock().unwrap_or_else(PoisonError::into_inner);
        let waited = begin.elapsed();
        lock_count += 1;
        // GUI 層の fetch_log_range と同じ形（ロックを取ってから fetch_range）。
        if let Some(summary) = guard.list_sources().first() {
            let request = RangeRequest {
                start: 0,
                max_items: SMOKE_FETCH_ITEMS,
                // 伸長では世代が進まないため、読み込み中は常に初回の世代。
                expected_generation: 1,
            };
            let display_set_id = summary.display_set_id;
            let fetch_begin = Instant::now();
            let response = guard.fetch_range(display_set_id, request);
            let fetch_elapsed = fetch_begin.elapsed();
            if let Ok(response) = response {
                fetch_ok += 1;
                seen_totals.push(response.total_items);
                max_fetch_elapsed = max_fetch_elapsed.max(fetch_elapsed);
            }
        }
        drop(guard);
        max_lock_wait = max_lock_wait.max(waited);
        std::thread::sleep(CONTENTION_POLL_INTERVAL);
    }

    let (outcome, load_elapsed) = loader.join().expect("読み込みスレッドは正常終了するはず");
    let (handle, summary) = outcome.expect("読み込みは成功するはず");
    ContentionResult {
        load_elapsed,
        max_lock_wait,
        lock_count,
        partial_observations: seen_totals
            .iter()
            .filter(|seen| **seen < handle.total_items)
            .count(),
        fetch_ok,
        max_fetch_elapsed,
        total_items: handle.total_items,
        reserved_bytes: summary.reserved_bytes,
        generation: handle.generation,
    }
}

fn print_contention_result(title: &str, result: &ContentionResult) {
    println!("--- {title} ---");
    println!(
        "  読み込み所要時間: {:.3} 秒",
        result.load_elapsed.as_secs_f64()
    );
    println!(
        "  ロック取得の最長待ち時間: {:.1} ms",
        result.max_lock_wait.as_secs_f64() * 1000.0
    );
    println!(
        "  読み込み中のロック取得回数: {} / うち範囲取得成功 {} 回 / 途中経過（伸長中）の観測 {} 回",
        result.lock_count, result.fetch_ok, result.partial_observations
    );
    println!(
        "  範囲取得（512件）自体の最長所要時間: {:.1} ms",
        result.max_fetch_elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "  最終結果: total_items={} / reserved_bytes={} / generation={}",
        result.total_items, result.reserved_bytes, result.generation
    );
}

/// 再読み込み中の `allocated_bytes` を標本化する間隔です。
///
/// 一時バッファの再確保（旧容量と新容量が同時に生きる瞬間）は、数百 MB の
/// memcpy を伴うため数十 ms 続きます。この間隔なら、その山を必ず数回踏みます。
const RELOAD_SAMPLE_INTERVAL: Duration = Duration::from_millis(2);

/// 再読み込み経路の計測結果です。
struct ReloadMeasurement {
    elapsed: Duration,
    outcome_label: String,
    total_items_before: u64,
    total_items_after: Option<u64>,
    allocated_before: usize,
    allocated_after: usize,
    /// 再読み込み中に標本化した `allocated_bytes` の最大値。
    sampled_max: usize,
    /// 再読み込み直前・直後の `peak_bytes`。プロセス起動からの単調増加値なので、
    /// 直後の値が直前より大きければ、その差は再読み込みが押し上げた分である。
    peak_before: usize,
    peak_after: usize,
    outstanding_reserved_after: usize,
}

/// `SCALE_RELOAD=1` のときに実行する、再読み込み経路の計測。
///
/// **元データは書き換えません。** 指定されたファイルを `%TEMP%` の作業用コピーへ
/// 複製し、追記も再読み込みもコピーに対してだけ行い、最後にコピーを削除します
/// （試験データの再生成コストが高いため、原本を汚さないことを優先する）。
fn run_reload_mode(source_path: &Path, label: &str) {
    println!("=== 再読み込み経路の計測 ===");
    println!("元ファイル（読み取りのみ）: {}", source_path.display());

    let work_path = std::env::temp_dir().join(format!(
        "hakutaku-scale-reload-work-{}.log",
        std::process::id()
    ));
    println!("作業用コピー: {}", work_path.display());
    let copy_begin = Instant::now();
    match std::fs::copy(source_path, &work_path) {
        Ok(bytes) => println!(
            "コピー完了: {}（{:.3} 秒）",
            format_bytes_u64(bytes),
            copy_begin.elapsed().as_secs_f64()
        ),
        Err(error) => {
            eprintln!("作業用コピーを作成できません: {error}");
            std::process::exit(1);
        }
    }

    let source_budget = SourceBudget::new();
    let mut registry = DisplaySetRegistry::new();

    println!("\n--- 初回登録 ---");
    let begin = Instant::now();
    let registered = register_source(
        &mut registry,
        &source_budget,
        &work_path,
        label.to_string(),
        &[],
    );
    let register_elapsed = begin.elapsed();
    let (handle, summary) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            println!("初回登録に失敗しました: {error}");
            let _ = std::fs::remove_file(&work_path);
            return;
        }
    };
    println!("所要時間: {:.3} 秒", register_elapsed.as_secs_f64());
    println!("total_items: {}", handle.total_items);
    println!(
        "予約振替量（reserved_bytes、常駐分）: {}",
        format_bytes_usize(summary.reserved_bytes)
    );
    print_memory_accounting_snapshot("  会計（メモリ、登録直後）:");

    // 追記は作業用コピーに対してだけ行う（1行で足りる。純粋な追記として
    // 検知されれば `stream_decode_and_index` を通る経路に入る）。
    println!("\n--- 1行追記してから再読み込み ---");
    {
        use std::io::Write;
        let mut appender = std::fs::OpenOptions::new()
            .append(true)
            .open(&work_path)
            .expect("作業用コピーへ追記できるはず");
        appender
            .write_all("2026/08/11 00:00:00.000 再読み込み計測用の追記行\n".as_bytes())
            .expect("追記できるはず");
    }

    let stop_sampling = Arc::new(AtomicBool::new(false));
    let sampled_max = Arc::new(AtomicUsize::new(allocated_bytes()));
    let sampler = {
        let stop_sampling = Arc::clone(&stop_sampling);
        let sampled_max = Arc::clone(&sampled_max);
        std::thread::spawn(move || {
            while !stop_sampling.load(Ordering::Relaxed) {
                sampled_max.fetch_max(allocated_bytes(), Ordering::Relaxed);
                std::thread::sleep(RELOAD_SAMPLE_INTERVAL);
            }
            sampled_max.fetch_max(allocated_bytes(), Ordering::Relaxed);
        })
    };

    let allocated_before = allocated_bytes();
    let peak_before = peak_bytes();
    let begin = Instant::now();
    let outcome = reload_source(&mut registry, &source_budget, handle.source_id, &[]);
    let elapsed = begin.elapsed();
    let allocated_after = allocated_bytes();
    let peak_after = peak_bytes();

    stop_sampling.store(true, Ordering::Relaxed);
    sampler.join().expect("標本化スレッドは正常終了するはず");

    let (outcome_label, total_items_after) = match &outcome {
        Some(ReloadOutcome::Reloaded { total_items, .. }) => {
            ("Reloaded（成功）".to_string(), Some(*total_items))
        }
        Some(ReloadOutcome::RejectedOverLimit(rejection)) => {
            (format!("RejectedOverLimit: {rejection}"), None)
        }
        Some(ReloadOutcome::Changed(kind)) => (format!("Changed: {kind:?}"), None),
        Some(ReloadOutcome::SharingViolation) => ("SharingViolation".to_string(), None),
        Some(ReloadOutcome::Failed(error)) => (format!("Failed: {error:?}"), None),
        None => ("None（未登録）".to_string(), None),
    };

    let measurement = ReloadMeasurement {
        elapsed,
        outcome_label,
        total_items_before: handle.total_items,
        total_items_after,
        allocated_before,
        allocated_after,
        sampled_max: sampled_max.load(Ordering::Relaxed),
        peak_before,
        peak_after,
        outstanding_reserved_after: global_budget().outstanding_reserved_bytes(),
    };
    print_reload_measurement(&measurement);
    print_memory_accounting_snapshot("  会計（メモリ、再読み込み直後）:");
    print_source_budget_snapshot("  会計（ソース予算、再読み込み直後）:", &source_budget);

    // 表示集合を落として、常駐分と一時バッファの解放が会計に現れることを見る。
    drop(registry);
    print_memory_accounting_snapshot("  会計（メモリ、レジストリ破棄後）:");

    let _ = std::fs::remove_file(&work_path);
    println!("作業用コピーを削除しました（元ファイルは未変更）。");
    println!("=== 計測完了 ===");
}

fn print_reload_measurement(result: &ReloadMeasurement) {
    println!("--- 再読み込みの結果 ---");
    println!("  結果: {}", result.outcome_label);
    println!("  所要時間: {:.3} 秒", result.elapsed.as_secs_f64());
    match result.total_items_after {
        Some(after) => println!(
            "  total_items: {} → {}（増分 {}）",
            result.total_items_before,
            after,
            after as i64 - result.total_items_before as i64
        ),
        None => println!(
            "  total_items: {}（再読み込みが成立しなかったため据え置き）",
            result.total_items_before
        ),
    }
    println!(
        "  allocated_bytes: 直前 {} → 直後 {}",
        format_bytes_usize(result.allocated_before),
        format_bytes_usize(result.allocated_after),
    );
    println!(
        "  再読み込み中の allocated_bytes 最大（{} ms 間隔の標本）: {}",
        RELOAD_SAMPLE_INTERVAL.as_millis(),
        format_bytes_usize(result.sampled_max),
    );
    println!(
        "  peak_bytes: 直前 {} → 直後 {}（差 {}）",
        format_bytes_usize(result.peak_before),
        format_bytes_usize(result.peak_after),
        format_bytes_usize(result.peak_after.saturating_sub(result.peak_before)),
    );
    println!(
        "  outstanding_reserved_bytes（直後）: {}",
        format_bytes_usize(result.outstanding_reserved_after),
    );
}

/// レジストリの借用時間を1回ずつ記録する [`RegistryAccess`] 実装です
/// （読み込み完了時の統合ビュー同期の計測用）。読み込み中の借用はバッチ登録、
/// 最後の1回は読み込み終了時の最終確定（統合表示 ON ならその同期を含む）に
/// 対応します。
struct TimingRegistryAccess<'a> {
    registry: &'a mut DisplaySetRegistry,
    borrows: Vec<Duration>,
}

impl RegistryAccess for TimingRegistryAccess<'_> {
    fn with_registry<R>(&mut self, borrow: impl FnOnce(&mut DisplaySetRegistry) -> R) -> R {
        let started = Instant::now();
        let result = borrow(self.registry);
        self.borrows.push(started.elapsed());
        result
    }
}

/// 統合表示 ON での読み込み完了時同期の計測結果です。
struct MergedSyncMeasurement {
    merged_enabled: bool,
    load_elapsed: Duration,
    borrow_count: usize,
    borrow_total: Duration,
    borrow_max_except_final: Duration,
    finalize_borrow: Duration,
    first_total_items: u64,
    second_total_items: u64,
    /// 統合表示 ON のときだけ `Some((世代, 件数)`)。
    merged_state: Option<(u64, u64)>,
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// 統合表示集合の現在の世代と件数を取得します。
///
/// 世代は同期のたびに進むため、呼び出し側が数えるのではなく、フロントエンドと
/// 同じ自己修復経路（`generation_mismatch` 応答が返す `current`。
/// `src/log_view.js`）で現在値を問い合わせます。
fn merged_current_state(
    registry: &mut DisplaySetRegistry,
    display_set_id: u32,
) -> Option<(u64, u64)> {
    let probe = registry.fetch_range(
        display_set_id,
        RangeRequest {
            start: 0,
            max_items: 1,
            expected_generation: u64::MAX,
        },
    );
    let generation = match probe {
        Err(FetchRangeError::GenerationMismatch { current, .. }) => current,
        Ok(response) => response.generation,
        Err(error) => {
            println!("  統合表示集合の状態を取得できませんでした: {error}");
            return None;
        }
    };
    let response = registry
        .fetch_range(
            display_set_id,
            RangeRequest {
                start: 0,
                max_items: 1,
                expected_generation: generation,
            },
        )
        .ok()?;
    Some((generation, response.total_items))
}

/// `SCALE_MERGED=1` の1回分の計測です。
///
/// 「1件目を登録 →（`merged_enabled` なら統合表示を ON）→ 2件目を
/// `register_source_with_access` で登録」という手順を実行し、2件目の読み込み中の
/// **借用区間ごとの所要時間**を記録します。最後の借用が読み込み終了時の最終確定
/// であり、統合表示 ON のときだけそこに全体再マージ（読み込み完了時同期）が
/// 含まれます。ON と OFF の最終確定の差が、同期のコストそのものです。
fn measure_merged_sync(first: &Path, second: &Path, merged_enabled: bool) -> MergedSyncMeasurement {
    let budget = SourceBudget::new();
    let mut registry = DisplaySetRegistry::new();

    let (first_handle, _summary) =
        register_source(&mut registry, &budget, first, file_label(first), &[])
            .expect("1件目の登録に失敗しました");
    println!("  1件目: total_items={}", first_handle.total_items);

    let merged_display_set_id = if merged_enabled {
        let handle = registry
            .enable_merged_view()
            .expect("統合表示を開始できませんでした");
        println!(
            "  統合表示 ON（この時点では1件目のみ）: display_set_id={} generation={} total_items={}",
            handle.display_set_id, handle.generation, handle.total_items
        );
        Some(handle.display_set_id)
    } else {
        None
    };

    let mut access = TimingRegistryAccess {
        registry: &mut registry,
        borrows: Vec::new(),
    };
    let started = Instant::now();
    let outcome = register_source_with_access(
        &mut access,
        &budget,
        second,
        file_label(second),
        &[],
        &LoadControl::none(),
    )
    .expect("2件目の登録に失敗しました");
    let load_elapsed = started.elapsed();
    let borrows = std::mem::take(&mut access.borrows);

    let borrow_count = borrows.len();
    let borrow_total: Duration = borrows.iter().sum();
    let finalize_borrow = borrows.last().copied().unwrap_or(Duration::ZERO);
    // 最終確定を除いた最長。バッチ登録1回分の上限として、完了時同期の増分と
    // 比べるための目安になる。
    let borrow_max_except_final = borrows[..borrow_count.saturating_sub(1)]
        .iter()
        .copied()
        .max()
        .unwrap_or(Duration::ZERO);

    let merged_state = merged_display_set_id
        .and_then(|display_set_id| merged_current_state(&mut registry, display_set_id));

    MergedSyncMeasurement {
        merged_enabled,
        load_elapsed,
        borrow_count,
        borrow_total,
        borrow_max_except_final,
        finalize_borrow,
        first_total_items: first_handle.total_items,
        second_total_items: outcome.handle.total_items,
        merged_state,
    }
}

fn print_merged_sync_measurement(result: &MergedSyncMeasurement) {
    println!(
        "  統合表示: {}",
        if result.merged_enabled { "ON" } else { "OFF" }
    );
    println!(
        "  2件目の読み込み所要時間: {:.3} 秒",
        result.load_elapsed.as_secs_f64()
    );
    println!(
        "  レジストリ借用: 回数={} / 合計={:.1} ms / 最終確定を除く最長={:.1} ms",
        result.borrow_count,
        result.borrow_total.as_secs_f64() * 1000.0,
        result.borrow_max_except_final.as_secs_f64() * 1000.0,
    );
    println!(
        "  最終確定の借用（完了時同期を含む区間）: {:.1} ms",
        result.finalize_borrow.as_secs_f64() * 1000.0
    );
    println!(
        "  項目数: 1件目={} / 2件目={} / 合計={}",
        result.first_total_items,
        result.second_total_items,
        result.first_total_items + result.second_total_items,
    );
    match result.merged_state {
        Some((generation, total_items)) => {
            let expected = result.first_total_items + result.second_total_items;
            println!(
                "  統合表示集合: generation={generation} / total_items={total_items} / \
                 全項目との一致={}",
                if total_items == expected {
                    "一致"
                } else {
                    "不一致"
                }
            );
        }
        None => println!("  統合表示集合: なし（OFF）"),
    }
}

/// `SCALE_MERGED=1` のときに実行する、読み込み完了時の統合ビュー同期の計測。
/// `SCALE_FILES` の先頭2件を使います。
fn run_merged_sync_mode(first: &Path, second: &Path) {
    println!("=== 読み込み完了時の統合ビュー同期の計測 ===");
    println!("1件目（先に開く対象）: {}", first.display());
    println!("2件目（統合表示 ON のまま開く対象）: {}", second.display());

    println!("\n[統合表示 OFF] 完了時同期が走らない基準");
    let disabled = measure_merged_sync(first, second, false);
    print_merged_sync_measurement(&disabled);
    print_memory_accounting_snapshot("  会計（メモリ、直後）:");

    println!("\n[統合表示 ON] 完了時同期が最終確定の借用へ載る");
    let enabled = measure_merged_sync(first, second, true);
    print_merged_sync_measurement(&enabled);
    print_memory_accounting_snapshot("  会計（メモリ、直後）:");

    println!("\n=== 比較 ===");
    let delta = enabled
        .finalize_borrow
        .saturating_sub(disabled.finalize_borrow);
    println!(
        "  最終確定の借用: {:.1} ms（OFF） → {:.1} ms（ON） / 増分 {:.1} ms",
        disabled.finalize_borrow.as_secs_f64() * 1000.0,
        enabled.finalize_borrow.as_secs_f64() * 1000.0,
        delta.as_secs_f64() * 1000.0,
    );
    println!(
        "  2件目の読み込み所要時間: {:.3} 秒（OFF） → {:.3} 秒（ON）",
        disabled.load_elapsed.as_secs_f64(),
        enabled.load_elapsed.as_secs_f64(),
    );
    println!(
        "  読み込み結果の一致: total_items {}",
        if disabled.second_total_items == enabled.second_total_items {
            "一致"
        } else {
            "不一致"
        }
    );
    println!("=== 計測完了 ===");
}

// --- 範囲取得のアクセスパターン計測 ---

/// 1パターンあたりの範囲取得回数（モジュール doc コメント「範囲取得の
/// アクセスパターン計測」）。
const SCROLL_FETCHES: usize = 200;

/// 整列パターンが使うチャンクの件数。GUI の `CHUNK_SIZE`（`src/log_view.js`）と
/// 同じく [`hakutaku_core::MAX_ITEMS_PER_RESPONSE`] に合わせます。
const SCROLL_CHUNK_ITEMS: u32 = 512;

/// 往復スクロールで巡回するチャンク数。`crate::chunk_cache` の
/// `MAX_CACHED_CHUNKS`（8件、非公開の暫定値）と同じにして、「キャッシュに収まる
/// 範囲を往復する」状況を作ります。
const SCROLL_ROUNDTRIP_CHUNKS: u64 = 8;

/// FNV-1a（64ビット）の初期値。
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a（64ビット）の乗数。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 取得内容の同一性を前後比較するための検査値を更新します（FNV-1a、64ビット）。
///
/// 依存を増やさずに「本文と識別子が1バイトも変わっていない」ことを確かめる
/// ためだけのもので、暗号学的な強度は要りません。
fn fnv1a_update(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// 範囲取得1回分の要求（開始位置と件数）。
#[derive(Debug, Clone, Copy)]
struct ScrollRequest {
    start: u64,
    max_items: u32,
}

/// 1パターン分の計測結果。
struct ScrollPatternResult {
    label: &'static str,
    fetches: usize,
    items: u64,
    hits: u64,
    misses: u64,
    reopens: u64,
    average_ms: f64,
    max_ms: f64,
    /// 取得した本文・識別子から求めた検査値。
    checksum: u64,
}

/// 要求列を順に実行し、キャッシュの効き方と所要時間を計測します。
///
/// `warmup` は計測前に実行するだけで統計へ含めません（「表示済みの状態」を
/// 作るための取得を、計測対象のアクセスパターンと混ぜないため）。カウンタは
/// `warmup` の後に 0 へ戻します。
fn measure_scroll_pattern(
    registry: &mut DisplaySetRegistry,
    handle: &DisplaySetHandle,
    label: &'static str,
    warmup: &[ScrollRequest],
    measured: &[ScrollRequest],
) -> ScrollPatternResult {
    for request in warmup {
        let _ = registry.fetch_range(
            handle.display_set_id,
            RangeRequest {
                start: request.start,
                max_items: request.max_items,
                expected_generation: handle.generation,
            },
        );
    }

    reset_fetch_path_metrics();
    let mut checksum = FNV_OFFSET_BASIS;
    let mut items = 0u64;
    let mut total = Duration::ZERO;
    let mut max = Duration::ZERO;

    for request in measured {
        let begin = Instant::now();
        let result = registry.fetch_range(
            handle.display_set_id,
            RangeRequest {
                start: request.start,
                max_items: request.max_items,
                expected_generation: handle.generation,
            },
        );
        // 検査値の計算は計時の外で行う（ハッシュのコストを取得時間へ混ぜない）。
        let elapsed = begin.elapsed();
        total += elapsed;
        max = max.max(elapsed);

        match result {
            Ok(response) => {
                items += response.items.len() as u64;
                for item in &response.items {
                    checksum = fnv1a_update(checksum, &item.item_id.seq.to_le_bytes());
                    checksum = fnv1a_update(checksum, &item.source_line_number.to_le_bytes());
                    checksum = fnv1a_update(checksum, item.raw_text.as_bytes());
                }
            }
            Err(error) => {
                println!("  [{label}] start={} でエラー: {error}", request.start);
            }
        }
    }

    let metrics = fetch_path_metrics();
    ScrollPatternResult {
        label,
        fetches: measured.len(),
        items,
        hits: metrics.chunk_cache_hits,
        misses: metrics.chunk_cache_misses,
        reopens: metrics.source_reopens,
        average_ms: if measured.is_empty() {
            0.0
        } else {
            total.as_secs_f64() * 1000.0 / measured.len() as f64
        },
        max_ms: max.as_secs_f64() * 1000.0,
        checksum,
    }
}

fn print_scroll_pattern_result(result: &ScrollPatternResult) {
    let lookups = result.hits + result.misses;
    let hit_rate = if lookups == 0 {
        0.0
    } else {
        result.hits as f64 * 100.0 / lookups as f64
    };
    println!("--- {} ---", result.label);
    println!(
        "  取得回数={} / 取得項目数={} / キャッシュ ヒット={} ミス={}（ヒット率 {hit_rate:.1}%）",
        result.fetches, result.items, result.hits, result.misses,
    );
    println!(
        "  ソース再オープン回数={} / 所要時間 平均={:.3} ms 最大={:.3} ms",
        result.reopens, result.average_ms, result.max_ms,
    );
    println!("  取得内容の検査値（FNV-1a）: {:#018x}", result.checksum);
}

/// 整列前方スクロールの要求列（512件境界を先頭から順に進む）。
fn aligned_forward_requests(total_items: u64) -> Vec<ScrollRequest> {
    (0..SCROLL_FETCHES as u64)
        .map(|index| ScrollRequest {
            start: (index * u64::from(SCROLL_CHUNK_ITEMS)) % total_items,
            max_items: SCROLL_CHUNK_ITEMS,
        })
        .collect()
}

/// 整列往復スクロールの要求列（8チャンクの窓を行ったり来たりする）。
fn aligned_roundtrip_requests(base_start: u64) -> Vec<ScrollRequest> {
    // 0,1,..,7,6,..,1,0,1,.. と折り返す（周期は 2*(8-1) = 14）。
    let period = 2 * (SCROLL_ROUNDTRIP_CHUNKS - 1);
    (0..SCROLL_FETCHES as u64)
        .map(|index| {
            let phase = index % period;
            let chunk = if phase < SCROLL_ROUNDTRIP_CHUNKS {
                phase
            } else {
                period - phase
            };
            ScrollRequest {
                start: base_start + chunk * u64::from(SCROLL_CHUNK_ITEMS),
                max_items: SCROLL_CHUNK_ITEMS,
            }
        })
        .collect()
}

/// 非整列サブ範囲（コピー選択に相当）の要求列と、その前提となる表示済み
/// チャンクの取得列を返します。
///
/// 利用者が「表示中の範囲の一部を選んでコピーする」状況を模し、4つのチャンクを
/// 順に表示しながら、その内側から任意の開始位置・件数で取得します。開始位置と
/// 件数は前後比較で完全に同じ列になるよう、乱数ではなく決定的な式で散らします。
fn unaligned_subrange_requests(base_start: u64) -> (Vec<ScrollRequest>, Vec<ScrollRequest>) {
    let chunk_count = 4u64;
    let warmup: Vec<ScrollRequest> = (0..chunk_count)
        .map(|chunk| ScrollRequest {
            start: base_start + chunk * u64::from(SCROLL_CHUNK_ITEMS),
            max_items: SCROLL_CHUNK_ITEMS,
        })
        .collect();

    let per_chunk = SCROLL_FETCHES as u64 / chunk_count;
    let measured = (0..SCROLL_FETCHES as u64)
        .map(|index| {
            let chunk = index / per_chunk;
            let chunk_start = base_start + chunk * u64::from(SCROLL_CHUNK_ITEMS);
            // 選択の開始位置と件数を、チャンク内へ必ず収まる範囲で散らす
            // （包含判定が効く条件そのものを作る）。
            let offset_in_chunk = (index * 37) % 400;
            let count = 50 + (index * 13) % 60;
            ScrollRequest {
                start: chunk_start + offset_in_chunk,
                max_items: u32::try_from(count).unwrap_or(SCROLL_CHUNK_ITEMS),
            }
        })
        .collect();

    (warmup, measured)
}

/// `SCALE_SCROLL=1` のときに実行する、範囲取得のアクセスパターン計測。
/// `SCALE_FILES` の先頭1件だけを使います。
fn run_scroll_mode(path: &Path, label: &str) {
    println!("=== 範囲取得のアクセスパターン計測 ===");
    println!("対象ファイル: {}", path.display());

    let source_budget = SourceBudget::new();
    let mut registry = DisplaySetRegistry::new();

    let begin = Instant::now();
    let registered = register_source(&mut registry, &source_budget, path, label.to_string(), &[]);
    let (handle, _summary) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            println!("登録に失敗しました: {error}");
            return;
        }
    };
    println!(
        "登録完了: total_items={} / 所要時間={:.3} 秒",
        handle.total_items,
        begin.elapsed().as_secs_f64()
    );
    print_memory_accounting_snapshot("  会計（メモリ、登録直後）:");

    if handle.total_items < u64::from(SCROLL_CHUNK_ITEMS) * SCROLL_FETCHES as u64 {
        println!(
            "注意: 総項目数（{}）が {} 件に満たないため、整列前方スクロールは\
             先頭へ折り返します（同じチャンクの再取得が混ざります）。",
            handle.total_items,
            u64::from(SCROLL_CHUNK_ITEMS) * SCROLL_FETCHES as u64,
        );
    }

    // 往復・非整列パターンは、先頭付近に偏らないようファイル中ほどを使う。
    let middle_chunk = (handle.total_items / 2) / u64::from(SCROLL_CHUNK_ITEMS);
    let base_start = middle_chunk * u64::from(SCROLL_CHUNK_ITEMS);

    println!();
    let forward = measure_scroll_pattern(
        &mut registry,
        &handle,
        "整列前方スクロール（512件境界を順に進む）",
        &[],
        &aligned_forward_requests(handle.total_items),
    );
    print_scroll_pattern_result(&forward);

    println!();
    let roundtrip = measure_scroll_pattern(
        &mut registry,
        &handle,
        "整列往復スクロール（8チャンクを往復）",
        &[],
        &aligned_roundtrip_requests(base_start),
    );
    print_scroll_pattern_result(&roundtrip);

    println!();
    let (warmup, measured) = unaligned_subrange_requests(base_start);
    let subrange = measure_scroll_pattern(
        &mut registry,
        &handle,
        "非整列サブ範囲（コピー選択に相当）",
        &warmup,
        &measured,
    );
    print_scroll_pattern_result(&subrange);

    println!();
    print_memory_accounting_snapshot("会計（メモリ、計測後）:");
    println!(
        "全パターンの合計: ソース再オープン回数={}",
        forward.reopens + roundtrip.reopens + subrange.reopens
    );
}

/// `SCALE_CONTENTION=1` のときに実行する、読み込み中のロック競合の前後比較。
fn run_contention_mode(path: &Path, label: &str) {
    println!("=== 読み込み中のロック競合の計測 ===");
    println!("対象ファイル: {}", path.display());
    println!(
        "監視間隔: {} ms（ロック取得 → fetch_range を繰り返す）",
        CONTENTION_POLL_INTERVAL.as_millis()
    );

    println!("\n[改善前] register_source をロック保持のまま呼ぶ");
    let before = measure_contention(path, label, false);
    print_contention_result("改善前（読み込み中ずっとロックを保持）", &before);
    print_memory_accounting_snapshot("  会計（メモリ、直後）:");

    println!("\n[改善後] register_source_with_access（バッチ境界でロックを取り直す）");
    let after = measure_contention(path, label, true);
    print_contention_result("改善後（バッチ境界でロックを取り直す）", &after);
    print_memory_accounting_snapshot("  会計（メモリ、直後）:");

    println!("\n=== 比較 ===");
    println!(
        "  ロック取得の最長待ち時間: {:.1} ms → {:.1} ms",
        before.max_lock_wait.as_secs_f64() * 1000.0,
        after.max_lock_wait.as_secs_f64() * 1000.0
    );
    println!(
        "  読み込み所要時間: {:.3} 秒 → {:.3} 秒",
        before.load_elapsed.as_secs_f64(),
        after.load_elapsed.as_secs_f64()
    );
    println!(
        "  最終結果の一致: total_items {} / reserved_bytes {} / generation {}",
        if before.total_items == after.total_items {
            "一致"
        } else {
            "不一致"
        },
        if before.reserved_bytes == after.reserved_bytes {
            "一致"
        } else {
            "不一致"
        },
        if before.generation == after.generation {
            "一致"
        } else {
            "不一致"
        },
    );
    println!("=== 計測完了 ===");
}

fn main() {
    // 会計イベント（予約拒否・ソフトしきい値到達・参考指標超過）の通知先を
    // 登録する。global_budget() はプロセス全体で共有される OnceLock ベースの
    // シングルトンであり、set_event_sink は一度しか設定できない
    // （crates/memory-accounting/src/budget.rs の doc コメント参照）。
    let sink_registered = global_budget().set_event_sink(Box::new(|event| match event {
        AccountingEvent::ReservationRejected(rejection) => {
            println!("  [会計イベント] 予約拒否: {rejection}");
        }
        AccountingEvent::SoftThresholdReached {
            allocated_bytes,
            outstanding_reserved_bytes,
            budget_bytes,
            peak_bytes,
        } => {
            println!(
                "  [会計イベント] ソフトしきい値到達: allocated={} outstanding_reserved={} \
                 budget={} peak={}",
                format_bytes_usize(allocated_bytes),
                format_bytes_usize(outstanding_reserved_bytes),
                format_bytes_usize(budget_bytes),
                format_bytes_usize(peak_bytes),
            );
        }
        AccountingEvent::ReferenceIndicatorExceeded {
            total_private_usage_bytes,
            budget_bytes,
            limit_bytes,
        } => {
            println!(
                "  [会計イベント] 参考指標（PrivateUsage 合計）超過: total={} budget={} limit={}",
                format_bytes_usize(total_private_usage_bytes),
                format_bytes_usize(budget_bytes),
                format_bytes_usize(limit_bytes),
            );
        }
    }));
    println!(
        "会計イベント通知先の登録: {}",
        if sink_registered {
            "成功"
        } else {
            "既に登録済み（無視）"
        }
    );

    let files_env = env::var("SCALE_FILES").unwrap_or_default();
    let paths: Vec<PathBuf> = files_env
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect();

    if paths.is_empty() {
        eprintln!(
            "環境変数 SCALE_FILES が未設定、またはファイルパスが1件も指定されていません。\
             セミコロン区切りの絶対パスで指定してください（例: SCALE_FILES=\"C:\\a.log;C:\\b.log\"）。"
        );
        std::process::exit(1);
    }

    // 読み込み中のロック競合の前後比較モード（先頭1件だけを使う）。
    if env::var("SCALE_CONTENTION").is_ok_and(|value| value == "1") {
        let path = &paths[0];
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        run_contention_mode(path, &label);
        return;
    }

    // 読み込み完了時の統合ビュー同期の計測モード（先頭2件を使う）。
    if env::var("SCALE_MERGED").is_ok_and(|value| value == "1") {
        if paths.len() < 2 {
            eprintln!(
                "SCALE_MERGED=1 は SCALE_FILES に2件以上のファイルが必要です\
                 （1件目を開いてから統合表示を ON にし、2件目を読み込むため）。"
            );
            std::process::exit(1);
        }
        run_merged_sync_mode(&paths[0], &paths[1]);
        return;
    }

    // 範囲取得のアクセスパターン計測モード（先頭1件だけを使う）。
    if env::var("SCALE_SCROLL").is_ok_and(|value| value == "1") {
        let path = &paths[0];
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        run_scroll_mode(path, &label);
        return;
    }

    // 再読み込み経路の計測モード（先頭1件だけを使う）。
    if env::var("SCALE_RELOAD").is_ok_and(|value| value == "1") {
        let path = &paths[0];
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        run_reload_mode(path, &label);
        return;
    }

    println!("=== 検証開始 ===");
    println!(
        "既定メモリ予算（PERF-008、hakutaku_memory_accounting）: {}",
        format_bytes_usize(DEFAULT_BUDGET_BYTES)
    );
    println!(
        "ソース上限（PERF-004〜006）: 単一ファイル {} / ファイル数 {} / 合計 {}",
        format_bytes_u64(MAX_SINGLE_FILE_BYTES),
        MAX_SOURCE_COUNT,
        format_bytes_u64(MAX_TOTAL_BYTES),
    );
    println!("対象ファイル数: {}", paths.len());
    for path in &paths {
        println!("  - {}", path.display());
    }
    print_memory_accounting_snapshot("初期状態:");

    let source_budget = SourceBudget::new();
    let mut registry = DisplaySetRegistry::new();
    let log_profiles: Vec<hakutaku_config::LogProfileConfig> = Vec::new();

    // (パス, 表示ラベル, 登録に成功した場合のハンドル) の列。失敗しても
    // 後続ファイルの処理は続ける（1ファイルの拒否・失敗で検証全体を止めない）。
    let mut registered: Vec<(PathBuf, String, Option<DisplaySetHandle>)> = Vec::new();

    for path in &paths {
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        println!("\n--- ファイル登録: {} ---", path.display());

        let metadata_size = std::fs::metadata(path).map(|meta| meta.len()).ok();
        match metadata_size {
            Some(size) => println!("ファイルサイズ（メタデータ）: {}", format_bytes_u64(size)),
            None => {
                println!("ファイルサイズ（メタデータ）: 取得失敗（ファイルが存在しない可能性）")
            }
        }

        // 読み込み経路が整合性の再確認のためにファイルを開き直した回数を数える。
        // この登録の分だけを見たいので、直前に 0 へ戻す。
        hakutaku_data_source::reset_snapshot_verify_metrics();
        let begin = Instant::now();
        let outcome = register_source(
            &mut registry,
            &source_budget,
            path,
            label.clone(),
            &log_profiles,
        );
        let elapsed = begin.elapsed();
        let verify_metrics = hakutaku_data_source::snapshot_verify_metrics();

        match outcome {
            Ok((handle, summary)) => {
                println!("登録結果: 成功");
                println!("所要時間: {:.3} 秒", elapsed.as_secs_f64());
                print_stage_timings("段階別内訳:", &summary.stage_timings);
                let chunk_count = summary
                    .file_size_bytes
                    .div_ceil(hakutaku_data_source::DEFAULT_CHUNK_BYTES);
                println!(
                    "整合性再確認: パス再オープン {} 回 / ハンドル問い合わせ {} 回（チャンク数 {}）",
                    verify_metrics.path_verifications,
                    verify_metrics.handle_verifications,
                    chunk_count
                );
                println!(
                    "行数（total_items、継続行結合後の論理項目数）: {}",
                    handle.total_items
                );
                println!(
                    "行数（line_count、継続行結合前の物理行数）: {}",
                    summary.line_count
                );
                println!(
                    "読み込みバイト数（file_size_bytes）: {}",
                    format_bytes_u64(summary.file_size_bytes)
                );
                println!(
                    "予約振替量（reserved_bytes、PERF-010）: {}",
                    format_bytes_usize(summary.reserved_bytes)
                );
                println!("文字コード判定経路: {}", summary.encoding_route);
                println!("選択された文字コード: {}", summary.selected_encoding);
                println!("プロファイル解決経路: {}", summary.profile_resolution_route);
                println!("確定した日時書式: {:?}", summary.detected_datetime_format);
                println!(
                    "日時書式の決定経路: {}",
                    summary.datetime_format_route.route_label()
                );
                println!(
                    "生表示へ退避したか（LOG-022）: {}",
                    summary.fell_back_to_raw_display
                );
                println!(
                    "末尾未確定行（LOG-026）: {}",
                    summary.has_unconfirmed_trailing_line
                );
                registered.push((path.clone(), label, Some(handle)));
            }
            Err(error) => {
                println!("登録結果: 拒否/失敗");
                println!("所要時間: {:.3} 秒", elapsed.as_secs_f64());
                println!("理由: {error}");
                registered.push((path.clone(), label, None));
            }
        }

        print_memory_accounting_snapshot("  会計（メモリ、直後）:");
        print_source_budget_snapshot("  会計（ソース予算、直後）:", &source_budget);
    }

    println!(
        "\n=== 登録済みソース一覧（registry.list_sources、既存表示が維持されているかの確認） ==="
    );
    let summaries = registry.list_sources();
    if summaries.is_empty() {
        println!("  （登録済みソースなし）");
    }
    for summary in &summaries {
        println!(
            "  source_id={} display_set_id={} label={} status={:?} size={} \
             has_unconfirmed_trailing_line={} update_pending={}",
            summary.source_id,
            summary.display_set_id,
            summary.label,
            summary.status,
            format_bytes_u64(summary.size_bytes),
            summary.has_unconfirmed_trailing_line,
            summary.update_pending,
        );
    }

    println!("\n=== 範囲取得スモークテスト（先頭・中間・末尾の各512件） ===");
    for (path, _label, handle) in &registered {
        let Some(handle) = handle else {
            println!("--- {} --- 登録失敗のためスキップ", path.display());
            continue;
        };
        println!(
            "--- {} (total_items={}) ---",
            path.display(),
            handle.total_items
        );
        smoke_fetch(&mut registry, handle, "先頭", 0);
        if handle.total_items > 0 {
            let mid = handle.total_items / 2;
            smoke_fetch(&mut registry, handle, "中間", mid);
            let tail_start = handle
                .total_items
                .saturating_sub(u64::from(SMOKE_FETCH_ITEMS));
            smoke_fetch(&mut registry, handle, "末尾", tail_start);
        } else {
            println!("    項目数0のため中間・末尾のスモークはスキップ");
        }
    }

    println!("\n=== 検証終了時点の会計状態 ===");
    print_memory_accounting_snapshot("最終:");
    print_source_budget_snapshot("最終:", &source_budget);

    println!("\n=== まとめ ===");
    for (path, label, handle) in &registered {
        match handle {
            Some(handle) => println!(
                "  {} ({label}): 成功 total_items={}",
                path.display(),
                handle.total_items
            ),
            None => println!("  {} ({label}): 拒否/失敗", path.display()),
        }
    }
    println!("=== 検証完了 ===");
}
