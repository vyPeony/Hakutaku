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
