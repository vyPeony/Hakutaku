//! 整合性再確認のためのファイルオープン回数が、チャンク数ではなく周期に
//! 比例することを確かめる試験です。
//!
//! # なぜ独立した試験バイナリなのか
//!
//! 観測に使うカウンタ（[`hakutaku_data_source::snapshot_verify_metrics`]）は
//! プロセス全体で共有されます。単体試験（`#[cfg(test)]`）へ置くと、同じ
//! プロセスで並行に走る他の試験の再確認まで数えてしまい、期待値が定まりません。
//! 試験バイナリを分けると、この中の試験だけがカウンタを触る状態を作れます。
//! **そのため、このファイルへ試験を増やすときは、カウンタを使う試験が同時に
//! 2つ以上走らないようにしてください**（このファイルの試験は1つに保つのが
//! 最も安全です）。

use std::sync::atomic::{AtomicU64, Ordering};

use hakutaku_data_source::{
    open_and_snapshot, reset_snapshot_verify_metrics, snapshot_verify_metrics,
    stream_snapshotted_bytes_chunked, ChunkedReadRequest, IoThrottle, PATH_VERIFY_CHUNK_INTERVAL,
};
use hakutaku_memory_accounting::MemoryBudget;

struct TempFile {
    path: std::path::PathBuf,
}

impl TempFile {
    fn create(label: &str, contents: &[u8]) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "hakutaku-data-source-verify-counts-{label}-{}-{count}-{nanos}.log",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("テスト用ファイルを作成できません");
        TempFile { path }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// 受け入れ条件: 読み込み経路がパスを開き直す回数が、チャンクごと（従来）から
// 「PATH_VERIFY_CHUNK_INTERVAL チャンクごと＋読み切った直後の1回」へ減る。
// 縮小の検知は毎チャンク行われ続けるが、そちらはファイルのオープンを伴わない。
#[test]
fn path_reopens_follow_the_interval_instead_of_the_chunk_count() {
    const CHUNK_BYTES: u64 = 1_000;
    const CHUNK_COUNT: u64 = 20;

    let contents = vec![b'a'; (CHUNK_BYTES * CHUNK_COUNT) as usize];
    let file = TempFile::create("interval", &contents);
    let (handle, snapshot) = open_and_snapshot(&file.path).expect("開けるはず");
    let budget = MemoryBudget::new(usize::MAX);
    let throttle = IoThrottle::unlimited();

    reset_snapshot_verify_metrics();
    let summary = stream_snapshotted_bytes_chunked(
        ChunkedReadRequest {
            file: handle,
            path: &file.path,
            snapshot: &snapshot,
            budget: &budget,
            chunk_bytes: CHUNK_BYTES,
            throttle: &throttle,
            eager_bytes: snapshot.snapshot_end,
            is_cancelled: &|| false,
        },
        |_, _, _| {},
    )
    .expect("読み込みは成功するはず");
    let metrics = snapshot_verify_metrics();

    assert_eq!(summary.bytes_read, CHUNK_BYTES * CHUNK_COUNT);

    // 周期の先頭に当たるチャンク（0起点で 0・8・16）＋読み切った直後の1回。
    let expected_path_reopens = CHUNK_COUNT.div_ceil(PATH_VERIFY_CHUNK_INTERVAL) + 1;
    assert_eq!(
        metrics.path_verifications, expected_path_reopens,
        "パス再オープンは周期＋最終確認の回数に一致するはず"
    );
    assert!(
        metrics.path_verifications < CHUNK_COUNT,
        "チャンク数({CHUNK_COUNT})より少ないことがこの変更の主目的（実測値: {}）",
        metrics.path_verifications
    );

    // 残りのチャンク境界は、オープンを伴わないハンドルへの問い合わせで縮小を
    // 確認している（＝確認の回数自体は減っていない）。
    assert_eq!(
        metrics.path_verifications + metrics.handle_verifications,
        CHUNK_COUNT + 1,
        "全チャンクの読み込み前と読み切った直後に、必ず何らかの確認が入るはず"
    );
}
