//! 参考指標（`PrivateUsage` 合計）の計測とプロセス特定（`PERF-011`、P02-4）。
//!
//! # 位置づけ（参考指標であり合否判定に使わない）
//!
//! ここで計測する値は、**`PERF-008` の予算（Rust コアプロセスのヒープ確保量の
//! 合計）とは別の、参考指標**です。`PERF-011` が定める「Rust コアプロセスと
//! Hakutaku 専用の Tauri／WebView2 子プロセス群の
//! `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` 合計」を計測しますが、**この値は
//! 合否判定に使いません**。予算超過の判定（予約の拒否）は
//! [`crate::MemoryBudget::reserve`] が担い、この値の役割は「予算値 + 1 GiB を
//! 超えないことを性能試験で確認する」（`PERF-011`）ための観測手段の提供です。
//!
//! **「+ 1 GiB」は暫定値です**（[`REFERENCE_INDICATOR_MARGIN_BYTES`]）。
//! WebView2 の複数プロセス構成を前提とした暫定的なマージンであり、P12・P13 の
//! 実測結果を見て再確定される予定です
//! （`tasks/phase-02-memory-accounting.md` 「後続 Issue / ADR 候補」）。
//!
//! # プロセス特定の方法（Hakutaku 専用の子孫プロセス群）
//!
//! `CreateToolhelp32Snapshot`（`TH32CS_SNAPPROCESS`）でシステム全体のプロセス
//! 一覧（PID・親 PID）を取得し、自プロセス（`std::process::id()`）を根とする
//! **子孫集合**を求めます。WebView2 は複数プロセス構成（ブラウザー・GPU・
//! レンダラー・ユーティリティ等）で動作しますが、いずれも Hakutaku（Tauri／
//! WebView2 ホスト）の子プロセスとして起動されるため、この子孫集合に含まれ
//! ます。**他アプリが起動した WebView2 プロセスは自プロセスの子孫ではないため
//! 合算されません**（`PERF-011` の受け入れ条件）。
//!
//! ## PID 再利用への防御
//!
//! 親 PID が一致するというだけでは、終了した祖先プロセスの PID を OS が
//! 再利用した無関係なプロセスを誤って子孫に含めてしまう可能性があります。
//! これを防ぐため、[`compute_descendant_pids`] は「子と判定するプロセスの
//! 生成時刻（`GetProcessTimes`）が、直接の親の生成時刻以降であること」を追加
//! の条件とします。子の生成時刻が親より古い場合、実際には無関係なプロセスが
//! 親 PID の再利用によって偽の親子関係を持っているとみなし、子孫から除外し
//! ます（詳細は [`compute_descendant_pids`] の doc コメントを参照）。
//!
//! **既知の制約:** 生成時刻を確認できないプロセス（`OpenProcess` が失敗する
//! 権限差のあるプロセス）は、安全側に倒して子孫集合から除外します。通常、
//! 自プロセスが起動した子孫プロセスへは同一ユーザーの権限でアクセスできるため
//! 実運用上の影響は小さいと見込みますが、理論上はこの経路で正当な子孫を
//! 見逃す可能性があります。
//!
//! # 計測の範囲（呼び出し側の責務との分担）
//!
//! [`measure_private_usage`] は**1回の計測だけ**を行います。定期計測の
//! スケジューリング（間隔、実行タイミング）は呼び出し側（P04 以降）の責務
//! です。同様に、取得時刻の記録も [`PrivateUsageSample`] は持たず、呼び出し側
//! が管理します。
//!
//! 参考指標の超過検知（しきい値との比較とイベント発火）は
//! [`crate::MemoryBudget::check_reference_indicator`] が担います。「進行中の
//! 追加読み込みをキャンセルして警告する」（計画正本 5.2）というキャンセル接続
//! は P06 以降の対象外で、ここではイベント発火（警告の経路）までです。

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

/// `PERF-011` のマージン: 参考指標のしきい値に加算する値（暫定値）。
///
/// 1 GiB。WebView2 の複数プロセス構成を前提とした暫定的な値であり、要件 ID を
/// 持ちません。P12・P13 の実測結果を見て再確定される予定です
/// （`tasks/phase-02-memory-accounting.md` 「後続 Issue / ADR 候補」）。
pub const REFERENCE_INDICATOR_MARGIN_BYTES: usize = 1024 * 1024 * 1024;

/// 1回の `PrivateUsage` 計測結果です（`PERF-011`、参考指標）。
///
/// 呼び出し時点のスナップショットであり、取得時刻は持ちません。定期計測の
/// スケジューリングと時刻の管理は呼び出し側（P04 以降）の責務です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateUsageSample {
    /// 自プロセスと子孫プロセス全体の `PrivateUsage` 合計（バイト）。
    ///
    /// 個々のプロセスの `PrivateUsage`（`usize`）を `saturating_add` で積算
    /// します。理論上の `usize` 溢れは実際のメモリ量では起こり得ないため、
    /// 素通しの加算失敗より安全側（値を頭打ちにする）を優先しています。
    pub total_private_usage_bytes: usize,
    /// プロセスごとの内訳（`PrivateUsage` を取得できたプロセスのみ）。
    pub processes: Vec<ProcessPrivateUsage>,
    /// アクセスできずスキップしたプロセス数（権限差など）。
    pub skipped_count: usize,
}

/// 内訳の1エントリ（1プロセス分）です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPrivateUsage {
    /// プロセス ID。
    pub pid: u32,
    /// 実行ファイル名（`CreateToolhelp32Snapshot` の `szExeFile`。パスを含ま
    /// ずファイル名のみ）。取得できなかった場合は空文字列。
    pub image_file_name: String,
    /// このプロセスの `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage`（バイト）。
    pub private_usage_bytes: usize,
}

/// [`measure_private_usage`] 全体が失敗した場合のエラーです。
///
/// 個々のプロセスへアクセスできない（権限差）場合はエラーにせずスキップして
/// 続行します（[`PrivateUsageSample::skipped_count`]）。ここでのエラーは、
/// 計測の土台となる操作そのものが失敗した場合だけを表します。
#[cfg(windows)]
#[derive(Debug)]
pub enum MeasurePrivateUsageError {
    /// プロセス一覧のスナップショット取得に失敗した
    /// （`CreateToolhelp32Snapshot`）。
    SnapshotFailed(windows::core::Error),
    /// 自プロセスの生成時刻取得に失敗した（`GetProcessTimes`）。子孫判定の
    /// 安全性（PID 再利用への防御）の基盤となる値のため、取得できない場合は
    /// 計測全体を失敗として扱う。
    OwnProcessTimesFailed(windows::core::Error),
}

#[cfg(windows)]
impl fmt::Display for MeasurePrivateUsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MeasurePrivateUsageError::SnapshotFailed(error) => {
                write!(f, "プロセス一覧の取得に失敗しました: {error}")
            }
            MeasurePrivateUsageError::OwnProcessTimesFailed(error) => {
                write!(f, "自プロセスの生成時刻の取得に失敗しました: {error}")
            }
        }
    }
}

#[cfg(windows)]
impl std::error::Error for MeasurePrivateUsageError {}

/// プロセススナップショットの1エントリです（子孫判定に必要な最小限の情報）。
///
/// テスト容易性のため、実際の Win32 呼び出し（`win32` モジュール、Windows
/// 専用）とは独立してこの型と [`compute_descendant_pids`] を定義しています。
/// プロセス列挙のスナップショット（PID・親 PID・生成時刻のリスト）を入力に
/// 取る純粋関数として子孫集合の計算を切り出しているため、決定的な単体テスト
/// が書けます。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessSnapshotEntry {
    /// プロセス ID。
    pub(crate) pid: u32,
    /// 親プロセス ID（`CreateToolhelp32Snapshot` が報告する値。終了した
    /// プロセスの PID が再利用されている可能性がある）。
    pub(crate) parent_pid: u32,
    /// プロセス生成時刻。`FILETIME`（100ナノ秒単位、単調増加のカウンタ）を
    /// `u64` へ変換した値で、大小比較にだけ使う。
    pub(crate) creation_time: u64,
}

/// 自プロセス（`root_pid`）を根とする子孫プロセスの PID 集合を求めます
/// （`root_pid` 自身は含みません）。
///
/// # アルゴリズム
///
/// 幅優先探索で、根から子・孫と世代ごとに辿ります。各世代を確定させてから
/// 次の世代を探索するため、常に「直接の親」の生成時刻が確定した状態で子の
/// 判定ができます。
///
/// # PID 再利用への防御
///
/// 子候補（`entry.parent_pid` が現在の探索対象と一致するエントリ）のうち、
/// **生成時刻が親（探索対象）の生成時刻より前**のものは除外します。実際の
/// 親子関係では子は親が起動した後に生成されるため、これより古い生成時刻を
/// 持つエントリは、終了した祖先の PID を再利用した無関係なプロセスが偶然
/// 同じ親 PID を指しているとみなせます（`tasks/phase-02-memory-accounting.md`
/// 「Hakutaku 専用の子プロセス群」の特定方法についての注意）。判定は根の生成
/// 時刻ではなく、**直接の親**の生成時刻を基準に行います（多世代にわたる
/// PID 再利用を検知するため）。
///
/// 境界値は「以上（`>=`）」で許可します。実際の親子関係で生成時刻が完全に
/// 一致することは通常ありませんが、`FILETIME` の解像度（100ナノ秒）の限界で
/// 理論上一致し得るため、正当な子を誤って除外しないよう安全側に倒しています。
///
/// # 他アプリのプロセスが合算されない理由
///
/// 探索は `entries` の中で `parent_pid` の連鎖を辿れる範囲だけに及びます。
/// 無関係な親を持つプロセス（他アプリのプロセスやその WebView2 子プロセス）
/// は根から辿り着けないため、生成時刻の一致・不一致に関わらず自然に除外され
/// ます。
pub(crate) fn compute_descendant_pids(
    entries: &[ProcessSnapshotEntry],
    root_pid: u32,
    root_creation_time: u64,
) -> HashSet<u32> {
    let mut children_by_parent: HashMap<u32, Vec<&ProcessSnapshotEntry>> = HashMap::new();
    for entry in entries {
        children_by_parent
            .entry(entry.parent_pid)
            .or_default()
            .push(entry);
    }

    let mut descendants: HashSet<u32> = HashSet::new();
    let mut frontier: VecDeque<(u32, u64)> = VecDeque::new();
    frontier.push_back((root_pid, root_creation_time));

    while let Some((parent_pid, parent_creation_time)) = frontier.pop_front() {
        let Some(children) = children_by_parent.get(&parent_pid) else {
            continue;
        };

        for child in children {
            // 自己参照（root を子として指すエントリ）や既に確定済みの PID は
            // 無限ループを避けるため無視する。
            if child.pid == root_pid || descendants.contains(&child.pid) {
                continue;
            }
            if child.creation_time < parent_creation_time {
                // PID 再利用への防御: 直接の親より古い生成時刻を持つ「子」は
                // 除外する（上記 doc コメントを参照）。
                continue;
            }
            descendants.insert(child.pid);
            frontier.push_back((child.pid, child.creation_time));
        }
    }

    descendants
}

/// Win32 API 呼び出しの実装本体です（`measure_private_usage` の内部実装）。
///
/// 子孫判定の純粋関数（[`compute_descendant_pids`]）とは独立したモジュールに
/// 分離し、Win32 に依存しない部分（`crate::private_usage` の上位スコープ）を
/// 任意のプラットフォームでテストできるようにしています。
#[cfg(windows)]
mod win32 {
    use std::collections::HashMap;

    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_VM_READ,
    };

    use super::{
        compute_descendant_pids, MeasurePrivateUsageError, PrivateUsageSample, ProcessPrivateUsage,
        ProcessSnapshotEntry,
    };

    /// [`super::measure_private_usage`] の実装本体です。
    pub(super) fn measure() -> Result<PrivateUsageSample, MeasurePrivateUsageError> {
        let root_pid = std::process::id();
        let root_creation_time = own_process_creation_time()?;

        let (entries, image_file_names) = enumerate_process_snapshot()?;
        let descendant_pids = compute_descendant_pids(&entries, root_pid, root_creation_time);

        // 決定的な並びにするため PID 昇順にソートし、自プロセスを先頭へ置く。
        let mut target_pids: Vec<u32> = descendant_pids.into_iter().collect();
        target_pids.sort_unstable();
        target_pids.insert(0, root_pid);

        let mut processes = Vec::with_capacity(target_pids.len());
        let mut total_private_usage_bytes: usize = 0;
        let mut skipped_count = 0usize;

        for pid in target_pids {
            match private_usage_bytes(pid) {
                Some(bytes) => {
                    total_private_usage_bytes = total_private_usage_bytes.saturating_add(bytes);
                    let image_file_name = image_file_names.get(&pid).cloned().unwrap_or_default();
                    processes.push(ProcessPrivateUsage {
                        pid,
                        image_file_name,
                        private_usage_bytes: bytes,
                    });
                }
                None => {
                    // アクセスできないプロセス（権限差）はスキップして続行する。
                    skipped_count += 1;
                }
            }
        }

        Ok(PrivateUsageSample {
            total_private_usage_bytes,
            processes,
            skipped_count,
        })
    }

    /// 自プロセスの生成時刻を取得する。`GetCurrentProcess` の擬似ハンドルは
    /// 常に有効で、`CloseHandle` は不要（呼んではいけない）。
    fn own_process_creation_time() -> Result<u64, MeasurePrivateUsageError> {
        // SAFETY: GetCurrentProcess は現在のプロセスを指す擬似ハンドルを返す
        // だけの操作であり、失敗しない。閉じる必要はない（CloseHandle の対象
        // にしない）。
        let pseudo_handle = unsafe { GetCurrentProcess() };

        read_creation_time(pseudo_handle).map_err(MeasurePrivateUsageError::OwnProcessTimesFailed)
    }

    /// システム全体のプロセス一覧（PID・親 PID・生成時刻）と、実行ファイル名の
    /// 対応表を取得する。
    ///
    /// 生成時刻を取得できなかったプロセス（アクセス不可、または列挙後に終了
    /// したプロセス）は `entries` から除外する（安全側。子孫判定で誤って時刻
    /// 検証をすり抜けさせないため）。実行ファイル名の対応表には、時刻を取得
    /// できたかどうかに関わらず、スナップショットに含まれる全プロセスを登録
    /// する。
    fn enumerate_process_snapshot(
    ) -> Result<(Vec<ProcessSnapshotEntry>, HashMap<u32, String>), MeasurePrivateUsageError> {
        let raw = snapshot_all_processes()?;

        let mut entries = Vec::with_capacity(raw.len());
        let mut image_file_names = HashMap::with_capacity(raw.len());

        for (pid, parent_pid, image_file_name) in raw {
            image_file_names.insert(pid, image_file_name);
            if let Some(creation_time) = process_creation_time(pid) {
                entries.push(ProcessSnapshotEntry {
                    pid,
                    parent_pid,
                    creation_time,
                });
            }
        }

        Ok((entries, image_file_names))
    }

    /// `CreateToolhelp32Snapshot` でシステム全体のプロセス一覧を取得し、
    /// (PID, 親 PID, 実行ファイル名) の組へ変換する。
    fn snapshot_all_processes() -> Result<Vec<(u32, u32, String)>, MeasurePrivateUsageError> {
        // SAFETY: TH32CS_SNAPPROCESS はプロセス一覧のスナップショットを要求
        // するフラグであり、第二引数の 0 は「全プロセスを対象にする」ことを
        // 意味する（プロセス指定はスレッド・モジュール・ヒープのスナップ
        // ショット取得時のみ使う、Win32 API の契約）。
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(MeasurePrivateUsageError::SnapshotFailed)?;
        let _guard = SnapshotHandleGuard(snapshot);

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut results = Vec::new();

        // SAFETY: entry は dwSize を正しく設定済みのローカル変数であり、
        // Process32FirstW はこの構造体のサイズを超えて書き込まない（Win32
        // API の契約）。snapshot は直前に取得した有効なハンドル。
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();

        while has_entry {
            results.push((
                entry.th32ProcessID,
                entry.th32ParentProcessID,
                wide_to_string(&entry.szExeFile),
            ));

            // SAFETY: snapshot・entry は直前のループと同じ有効な値であり、
            // Process32NextW も entry のサイズを超えて書き込まない。
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }

        Ok(results)
    }

    /// `CreateToolhelp32Snapshot` が返すハンドルを、関数を抜けるすべての経路
    /// （早期 return・`?` を含む）で確実に閉じるためのガードです。
    struct SnapshotHandleGuard(HANDLE);

    impl Drop for SnapshotHandleGuard {
        fn drop(&mut self) {
            // SAFETY: self.0 は CreateToolhelp32Snapshot が返した所有ハンドル
            // であり、Drop は一度しか呼ばれないため二重解放は起きない。
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// `pid` のプロセスの生成時刻を取得する。アクセスできない（権限差、また
    /// は取得までの間に終了した）場合は `None`。
    fn process_creation_time(pid: u32) -> Option<u64> {
        // SAFETY: pid は CreateToolhelp32Snapshot から得たプロセス識別子。
        // false はハンドルを子プロセスへ継承させないことを示す。失敗（権限
        // 不足や、列挙後にプロセスが終了した競合状態）は呼び出し元へ
        // Option::None として伝え、安全側（子孫判定からの除外）に倒す。
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return None;
        };

        let creation_time = read_creation_time(handle).ok();

        // SAFETY: handle は直前の OpenProcess が成功して返した所有ハンドル
        // であり、以降使用しないためここで確実に閉じる。
        unsafe {
            let _ = CloseHandle(handle);
        }

        creation_time
    }

    /// 開いたハンドルから生成時刻を読む。ハンドルの解放は呼び出し側が行う。
    fn read_creation_time(handle: HANDLE) -> windows::core::Result<u64> {
        let mut creation_time = FILETIME::default();
        let mut exit_time = FILETIME::default();
        let mut kernel_time = FILETIME::default();
        let mut user_time = FILETIME::default();

        // SAFETY: 4つの変数はすべてこのスタックフレーム上の有効な FILETIME
        // への可変参照であり、GetProcessTimes はその範囲内にしか書き込まない
        // （Win32 API の契約）。handle は呼び出し元が所有する有効なプロセス
        // ハンドル、または `GetCurrentProcess` の擬似ハンドルである。
        let result = unsafe {
            GetProcessTimes(
                handle,
                &mut creation_time,
                &mut exit_time,
                &mut kernel_time,
                &mut user_time,
            )
        };
        result?;

        Ok(filetime_to_u64(creation_time))
    }

    /// `FILETIME`（100ナノ秒単位、単調増加のカウンタ）を `u64` へ変換する。
    fn filetime_to_u64(filetime: FILETIME) -> u64 {
        (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime)
    }

    /// `pid` の `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` を取得する。
    /// アクセスできない場合は `None`（呼び出し側でスキップ数として計上）。
    fn private_usage_bytes(pid: u32) -> Option<usize> {
        // SAFETY: pid は子孫判定を経て特定した対象プロセス（自プロセスまたは
        // その子孫）。PROCESS_VM_READ は GetProcessMemoryInfo の契約が要求
        // する追加のアクセス権であり、PROCESS_QUERY_LIMITED_INFORMATION だけ
        // では不足する（Win32 API の契約）。false はハンドルを継承させない
        // ことを示す。
        let Ok(handle) = (unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                false,
                pid,
            )
        }) else {
            return None;
        };

        let usage = read_private_usage(handle);

        // SAFETY: handle は直前の OpenProcess が成功して返した所有ハンドル
        // であり、以降使用しないためここで確実に閉じる。
        unsafe {
            let _ = CloseHandle(handle);
        }

        usage
    }

    /// 開いたハンドルから `PrivateUsage` を読む。ハンドルの解放は呼び出し側
    /// が行う。
    fn read_private_usage(handle: HANDLE) -> Option<usize> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
        let cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;

        // SAFETY: counters は cb バイトぴったりの有効なローカル変数であり、
        // GetProcessMemoryInfo はそのサイズ以内にしか書き込まない。
        // PROCESS_MEMORY_COUNTERS_EX は PROCESS_MEMORY_COUNTERS と互換な
        // 先頭部分を持つ拡張レイアウトであり、拡張構造体へのポインタを基本
        // 構造体のポインタへキャストしたうえで cb に拡張構造体のサイズを渡す
        // のは Win32 のドキュメントに従った標準的な使い方である。handle は
        // 呼び出し元が所有する有効なプロセスハンドル。
        let result = unsafe {
            GetProcessMemoryInfo(
                handle,
                &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                cb,
            )
        };

        result.ok().map(|()| counters.PrivateUsage)
    }

    /// NUL 終端の `u16` 配列（`szExeFile` 等）を `String` へ変換する。不正な
    /// UTF-16 シーケンスは置換文字へ変換する（`from_utf16_lossy`）。
    fn wide_to_string(wide: &[u16]) -> String {
        let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..len])
    }
}

/// 現在のプロセスと、その子孫プロセス（Hakutaku 専用の Tauri／WebView2 子
/// プロセス群）の `PrivateUsage` 合計を1回計測します（`PERF-011`）。
///
/// 定期計測のスケジューリングは呼び出し側（P04 以降）の責務です。この関数は
/// 1回の計測だけを提供します。プロセス特定の方法と PID 再利用への防御は、
/// このモジュールの doc コメントと [`compute_descendant_pids`] を参照して
/// ください。
#[cfg(windows)]
pub fn measure_private_usage() -> Result<PrivateUsageSample, MeasurePrivateUsageError> {
    win32::measure()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, parent_pid: u32, creation_time: u64) -> ProcessSnapshotEntry {
        ProcessSnapshotEntry {
            pid,
            parent_pid,
            creation_time,
        }
    }

    // 受け入れ条件: 子孫判定の単体テスト（複数世代の子孫が正しく含まれる）。
    #[test]
    fn compute_descendant_pids_includes_multi_generation_descendants() {
        let entries = [
            entry(200, 100, 20), // 子（root=100 の子）。
            entry(300, 200, 30), // 孫（200 の子）。
        ];
        let descendants = compute_descendant_pids(&entries, 100, 10);
        assert_eq!(descendants, HashSet::from([200, 300]));
    }

    // 受け入れ条件: 他アプリのプロセスが合算されない（無関係な親を持つ
    // プロセスは子孫集合の純粋関数テストで除外される）。
    #[test]
    fn compute_descendant_pids_excludes_processes_with_unrelated_parent() {
        let entries = [
            entry(200, 100, 20), // 正当な子。
            entry(500, 999, 25), // 他アプリのプロセス（無関係な親 999）。
            entry(600, 500, 26), // その子（他アプリの WebView2 相当）。
        ];
        let descendants = compute_descendant_pids(&entries, 100, 10);
        assert_eq!(
            descendants,
            HashSet::from([200]),
            "無関係な親を持つプロセス（500, 600）は含まれないはず"
        );
    }

    // 受け入れ条件（PID 再利用への防御）: 祖先より生成時刻が古い「子」は
    // 除外される。
    #[test]
    fn compute_descendant_pids_excludes_child_older_than_root() {
        let entries = [
            entry(150, 100, 50), // root（生成時刻100）より古い（50）ため除外。
        ];
        let descendants = compute_descendant_pids(&entries, 100, 100);
        assert!(
            descendants.is_empty(),
            "親より古い生成時刻を持つ「子」は PID 再利用とみなして除外するはず"
        );
    }

    // 受け入れ条件（PID 再利用への防御、多世代）: 判定は「直接の親」の生成
    // 時刻を基準に行う（root の生成時刻ではない）。
    #[test]
    fn compute_descendant_pids_validates_against_immediate_parent_not_root() {
        let entries = [
            entry(200, 100, 150), // root（時刻100）の正当な子。
            entry(300, 200, 120), // 直接の親（200, 時刻150）より古いため除外。
                                  // root（時刻100）より新しいだけでは通らない。
        ];
        let descendants = compute_descendant_pids(&entries, 100, 100);
        assert_eq!(
            descendants,
            HashSet::from([200]),
            "300 は直接の親200より古いため除外され、root基準では通らないはず"
        );
    }

    // 境界値: 子の生成時刻が親とちょうど同じ場合は許可する（FILETIME の
    // 解像度限界を考慮した安全側の判断。doc コメント参照）。
    #[test]
    fn compute_descendant_pids_allows_equal_creation_time_boundary() {
        let entries = [entry(200, 100, 100)];
        let descendants = compute_descendant_pids(&entries, 100, 100);
        assert_eq!(descendants, HashSet::from([200]));
    }

    // root_pid 自身は結果に含まれない（自己参照エントリの防御）。
    #[test]
    fn compute_descendant_pids_does_not_include_root_itself() {
        let entries = [entry(100, 1, 5)];
        let descendants = compute_descendant_pids(&entries, 100, 10);
        assert!(!descendants.contains(&100));
    }

    // 受け入れ条件: 自プロセスの計測が成功し、合計が0より大きい（自プロセス
    // 自身の PrivateUsage を含むため）。実際に Win32 API を呼び出す統合的な
    // スモークテスト（このモジュールの Win32 呼び出しはモック化できる純粋な
    // ロジックへ分離できないため、実際に呼び出して確認する）。
    #[cfg(windows)]
    #[test]
    fn measure_private_usage_succeeds_and_total_is_positive_for_self() {
        let sample = measure_private_usage().expect("自プロセスの計測は成功するはず");

        assert!(
            sample.total_private_usage_bytes > 0,
            "自プロセス自身の PrivateUsage を含むため0より大きいはず"
        );
        assert!(
            !sample.processes.is_empty(),
            "少なくとも自プロセス1件は内訳に含まれるはず"
        );
        assert!(
            sample
                .processes
                .iter()
                .any(|process| process.pid == std::process::id()),
            "内訳に自プロセスの PID が含まれるはず"
        );
    }
}
