//! Win32 API 呼び出しの実装本体です（Windows 専用）。
//!
//! ファイル識別子（ボリューム連番＋ファイルインデックス）・サイズ・最終更新
//! 時刻の取得（`GetFileInformationByHandle`）を、`windows` クレート経由で直接
//! 呼び出します。純粋な判定ロジック（`crate::snapshot`）とは独立したモジュール
//! に分離し、Windows に依存しない部分を任意のプラットフォームでコンパイル
//! できるようにしています（`crates/format-detection/src/win32.rs` と同じ
//! 分離方針）。

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};

use crate::snapshot::FileIdentity;

/// `file` のファイル識別子・サイズ・最終更新時刻を取得します。
///
/// 戻り値は `(識別子, サイズ（バイト）, 最終更新時刻（Windows FILETIME。
/// 1601-01-01 UTC からの100ナノ秒単位を64ビットへ結合した値）)` です。
pub(crate) fn query_file_information(file: &File) -> io::Result<(FileIdentity, u64, u64)> {
    let handle = HANDLE(file.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();

    // SAFETY: `handle` は呼び出し元が所有する `file`（`File`）から得た有効な
    // ハンドルであり、`file` の借用がこの呼び出しの間ハンドルの生存を保証する。
    // `info` はこのスタックフレーム上に確保した有効な
    // `BY_HANDLE_FILE_INFORMATION` への可変参照であり、
    // `GetFileInformationByHandle` はこの構造体のサイズ分しか書き込まない
    // （Win32 API の契約）。
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(io::Error::from)?;

    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    let identity = FileIdentity {
        volume_serial_number: info.dwVolumeSerialNumber,
        file_index,
    };
    let size_bytes = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
    let last_write_time = (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(info.ftLastWriteTime.dwLowDateTime);

    Ok((identity, size_bytes, last_write_time))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};

    /// 1601-01-01 UTC を起点とする `FILETIME` で表した 2000-01-01 UTC。
    ///
    /// 実際の更新時刻は実行時刻に依存するため、値そのものは断定できません。
    /// 「64ビットへの結合が壊れていない（上位ワードを落としていない、上下を
    /// 取り違えていない）」ことだけを、この下限との比較で確認します。
    /// 1970-01-01 の `116_444_736_000_000_000` に30年分
    /// （10,957日 × 86,400秒 × 10^7）を足した値です。
    const FILETIME_2000_01_01: u64 = 125_911_584_000_000_000;

    /// テスト用の一時ファイル（`std::env::temp_dir()` 配下）。リポジトリ内へは
    /// 何も作らず、`Drop` で必ず削除します（`crate::snapshot` のテストと同じ方式）。
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
                "hakutaku-data-source-win32-test-{label}-{}-{count}-{nanos}.log",
                std::process::id()
            ));
            std::fs::write(&path, contents).expect("テスト用ファイルを作成できません");
            TempFile { path }
        }

        fn open(&self) -> File {
            File::open(&self.path).expect("テスト用ファイルを開けるはず")
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    // 正常呼び出し: 実際の Win32 呼び出しが成功し、サイズと更新時刻を取得できる。
    #[test]
    fn returns_size_and_last_write_time_for_a_real_file() {
        let file = TempFile::create("basic", b"0123456789");
        let (identity, size_bytes, last_write_time) =
            query_file_information(&file.open()).expect("ファイル情報を取得できるはず");

        assert_eq!(size_bytes, 10, "書き込んだバイト数と一致するはず");
        assert!(
            last_write_time > FILETIME_2000_01_01,
            "最終更新時刻が FILETIME として妥当な範囲にないはず: {last_write_time}"
        );
        // ボリューム連番は NTFS では 0 にならない（0 が返るのは情報を取得
        // できていない兆候）。
        assert_ne!(
            identity.volume_serial_number, 0,
            "ボリューム連番を取得できていません"
        );
    }

    // 境界値: 空ファイルのサイズは 0（0 を「取得失敗」と取り違えない）。
    #[test]
    fn reports_zero_size_for_an_empty_file() {
        let file = TempFile::create("empty", b"");
        let (_, size_bytes, _) =
            query_file_information(&file.open()).expect("空ファイルでも取得できるはず");
        assert_eq!(size_bytes, 0);
    }

    // 境界値: 下位ワードだけでは表せない桁の書き込みでも、サイズがそのまま返る。
    // 4 GiB 超（`nFileSizeHigh` が非ゼロになる領域）の確認は、テストで 4 GiB の
    // ファイルを作ることになるため行いません（同じ結合式を使う `nFileIndexHigh`
    // 側の妥当性は下の同一性テストで間接的に確認します）。
    #[test]
    fn reports_exact_size_for_a_multi_kilobyte_file() {
        let contents = vec![b'x'; 70_000];
        let file = TempFile::create("large", &contents);
        let (_, size_bytes, _) =
            query_file_information(&file.open()).expect("ファイル情報を取得できるはず");
        assert_eq!(size_bytes, 70_000);
    }

    // 同一性: 同じファイルを開き直しても識別子は変わらない
    // （`crate::snapshot` の `SnapshotVerdict::Replaced` 判定と、
    // `hakutaku_core::is_already_open`（二重オープン検知）の前提）。
    #[test]
    fn same_file_has_the_same_identity_across_reopens() {
        let file = TempFile::create("identity-stable", b"same file");
        let (first, _, _) = query_file_information(&file.open()).expect("1回目は取得できるはず");
        let (second, _, _) = query_file_information(&file.open()).expect("2回目は取得できるはず");
        assert_eq!(first, second);
    }

    // 同一性: 別のファイルは別の識別子になる。ファイルインデックスは
    // `nFileIndexHigh`・`nFileIndexLow` の結合値であり、これが常に同じ値に
    // なる（結合式を誤って定数を返す等）と、別ファイルへの置換を検知できなくなる。
    #[test]
    fn different_files_have_different_identities() {
        let first_file = TempFile::create("identity-a", b"first");
        let second_file = TempFile::create("identity-b", b"second");
        let (first, _, _) =
            query_file_information(&first_file.open()).expect("1つ目を取得できるはず");
        let (second, _, _) =
            query_file_information(&second_file.open()).expect("2つ目を取得できるはず");
        assert_ne!(
            first, second,
            "同じ一時領域に作った別ファイルの識別子が一致しています"
        );
    }

    // エラー経路: `GetFileInformationByHandle` はファイル以外のハンドル
    // （`NUL` などの文字デバイス）に対して失敗します。`unwrap` せずに
    // `io::Error` へ変換して返すこと（呼び出し側の `crate::snapshot` が
    // `ReadFileError::Io` として利用者向けの理由を組み立てられること）を確認します。
    #[test]
    fn returns_an_error_for_a_handle_that_is_not_a_file() {
        let device = File::open("NUL").expect("NUL デバイスは開けるはず");
        let result = query_file_information(&device);
        assert!(
            result.is_err(),
            "文字デバイスに対しては失敗するはず: {result:?}"
        );
    }
}
