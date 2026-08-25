//! `copy_selection` コマンド（P10、COPY-001〜006／CFG-018）。
//!
//! 上限判定・整形ロジックは一切持たず、すべて `hakutaku_core::assemble_copy`
//! に委譲します（`src-tauri` は解析ロジックを持たないという計画書「作業項目8:
//! 層境界の確認」と同じ方針）。このモジュール固有の責務は次の2つだけです。
//!
//! 1. `hakutaku_core::assemble_copy` の応答（`CopyOutcome`）を、フロントエンド
//!    向けの応答型（[`CopySelectionResponse`]）へ変換すること。
//! 2. 生成に成功した場合だけ、Win32 のクリップボード API（[`set_unicode_text`]）
//!    でクリップボードへ書き込むこと（COPY-002、初期リリースは ADR-0009 に
//!    従い CF_UNICODETEXT のみ。コピー内容の形式そのものは Issue #85 の
//!    ADR-0011 で「常に原文そのまま」へ変えた）。
//!
//! `SEC-004`／`COPY-006`（明示的な操作時のみコピーする）は、この関数自体が
//! 「明示的に呼ばれたときだけ実行される」ことで満たされます。スクロールや
//! 選択変更ではこのコマンドを呼ばないことは `src/log_view.js` 側の責務です。
//! 本文はいかなる場合も診断ログへ記録しません（バイト数・行数だけを記録）。

use std::sync::{Arc, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::State;

use hakutaku_diagnostics::{diag_info, diag_warn, Diagnostics};

use crate::bootstrap::config::ConfigState;
use crate::log_view::DisplaySetRegistryState;
use crate::targets::UserFacingErrorDto;

/// フロントエンドが指定するコピー範囲（`hakutaku_core::CopyRange` の
/// serde 化。Issue #85）。JS 側は `{ start, count }` の配列で渡します。
///
/// 受け入れ条件（`start` 昇順・互いに素・`count` が1以上・表示集合の範囲内）
/// は `hakutaku_core::assemble_copy` が検証します。ここでは形だけを受け取り、
/// 判定は持ちません（このモジュールの責務はモジュール doc コメントの2点だけ）。
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct CopyRangeArg {
    pub start: u64,
    pub count: u64,
}

impl From<CopyRangeArg> for hakutaku_core::CopyRange {
    fn from(value: CopyRangeArg) -> Self {
        hakutaku_core::CopyRange {
            start: value.start,
            count: value.count,
        }
    }
}

/// `copy_selection` の正常系の応答です。
///
/// 既定（`#[serde(rename_all = "snake_case")]` 付きの外部タグ表現）により、
/// JSON は `{ "copied": { "bytes": .., "lines": .. } }` または
/// `{ "rejected": { "limit_bytes": .., "limit_lines": .., "selected_lines": ..,
/// "selected_bytes": .., "error": { .. } } }` になります。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopySelectionResponse {
    Copied {
        bytes: u64,
        lines: u64,
    },
    /// 上限超過による拒否（`COPY-005`／`CFG-018`）。異常ではなく正規の応答
    /// ですが、**利用者が選択を絞り込まない限りコピーできない**ため、
    /// `ERR-002` の利用者向けエラーとして扱います（Issue #47）。
    ///
    /// `error` は5要素を持つ DTO です。上限値・選択量（`limit_*`・
    /// `selected_*`）は、フロントエンドが日本語の単位表記（`16.0 MB` など）へ
    /// 整形して文面へ差し込むため、DTO とは別に数値のまま残しています。
    /// 文面の組み立ては `src/log_view.js` の `formatCopyRejectionMessage` が
    /// 行い、そこで `error.reason` と `error.next_action` を使います。
    Rejected {
        limit_bytes: u64,
        limit_lines: u64,
        selected_lines: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        selected_bytes: Option<u64>,
        error: UserFacingErrorDto,
    },
}

/// 上限超過によるコピーの拒否（`COPY-005`）を、`ERR-002` の5要素を持つ
/// 利用者向けエラーとして組み立てます（Issue #47）。
///
/// # 各要素の決め方
///
/// - **対象**: コピーはファイルではなく利用者の選択範囲に対する操作のため、
///   選択範囲そのものを対象として示します。`SEC-012` によりフロントエンドへ
///   パスを渡さないこのコマンドでは、ファイル名も持っていません
/// - **発生位置**: 付けません（`None`）。判定は選択範囲全体の合計に対して
///   行われ、特定の1行で失敗したわけではないためです
///   （`docs/requirements/functional.md` の `ERR-002`）
/// - **理由**・**次の操作**: 上限値と選択量の数値は含めません。桁区切りと
///   単位（`16.0 MB`）の整形はフロントエンドの責務であり、応答には数値の
///   まま別フィールドで載せています（[`CopySelectionResponse::Rejected`]）
/// - **継続可否**: 継続可能（`true`）。クリップボードは変更しておらず
///   （`COPY-005`）、閲覧も選択もそのまま続けられます
///
/// 行数超過とバイト数超過を文面で区別しないため、拒否の実値
/// （[`hakutaku_core::CopyRejection`]）は引数に取りません。利用者が取る操作
/// （選択を絞り込む）はどちらでも同じで、どちらの上限に当たったかは応答に
/// 載せる上限値と選択量の対比から読み取れます。
fn copy_rejection_error() -> UserFacingErrorDto {
    UserFacingErrorDto::from(&hakutaku_core::notification::UserFacingError::new(
        "選択範囲のコピー",
        "選択範囲が上限を超えているため、コピーを中止しました",
        "選択範囲を絞り込んでください。",
    ))
}

/// `copy_selection` が失敗した理由です。
///
/// `unknown_display_set`・`generation_mismatch` は既存の範囲取得コマンド
/// （`fetch_log_range`）と同じ意味・同じ表現（`src-tauri/src/log_view.rs` の
/// `FetchLogRangeError` を参照）です。
///
/// # `ERR-002` の5要素は種別ごとにフロントエンドが組み立てる（Issue #47）
///
/// この型は `UserFacingErrorDto` を持ちません。ここに並ぶのはいずれも
/// 「コピーが成立しなかった」異常系であり、利用者へ示すべき理由と次の操作が
/// 種別ごとに大きく異なる（表示を取り直す、対象を開き直す、選択し直す、
/// 対象の状態を確認する）ため、文面の組み立ては受け側
/// （`src/log_view.js` の `handleCopySelectionError`）が種別で分岐して行い、
/// 上部バナー1枚で伝えます（表示方式は Issue #49 の裁定どおり）。
///
/// 一方、**上限超過による拒否**（`CFG-018`）は異常系ではなく `COPY-005` が
/// 定める正規の応答であり、[`CopySelectionResponse::Rejected`] として正常系で
/// 返ります。そちらは利用者が選択を絞り込まない限りコピーできない失敗のため、
/// `ERR-002` の5要素を持つ DTO を含めています（[`copy_rejection_error`]）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CopySelectionError {
    UnknownDisplaySet,
    GenerationMismatch {
        expected: u64,
        current: u64,
    },
    /// UI 側では通常発生しない防御的エラー（選択範囲が受け入れ条件を満たさ
    /// ない。Issue #85）。`reason` は利用者向けの日本語の理由文
    /// （`hakutaku_core::InvalidSelectionReason`）。
    InvalidSelection {
        reason: String,
    },
    /// `PERF-008`。`CFG-018` の上限内でも、他の用途でメモリ予算が逼迫して
    /// いる場合に発生し得る。
    MemoryReservationRejected {
        reason: String,
    },
    /// Win32 クリップボード API の呼び出しに失敗した（生成自体は成功した後の失敗）。
    ClipboardWriteFailed {
        reason: String,
    },
    /// `COPY-005`（Issue #37）。コピーの最中に対象のファイルが削除・置換された
    /// 等で本文を読み出せず、中身の抜けた内容を渡さないためにコピー全体を
    /// 中止した。クリップボードは変更していない。
    SourceUnavailable,
}

impl From<hakutaku_core::CopyError> for CopySelectionError {
    fn from(error: hakutaku_core::CopyError) -> Self {
        match error {
            hakutaku_core::CopyError::Fetch(hakutaku_core::FetchRangeError::UnknownDisplaySet) => {
                CopySelectionError::UnknownDisplaySet
            }
            hakutaku_core::CopyError::Fetch(
                hakutaku_core::FetchRangeError::GenerationMismatch { expected, current },
            ) => CopySelectionError::GenerationMismatch { expected, current },
            hakutaku_core::CopyError::InvalidSelection(reason) => {
                CopySelectionError::InvalidSelection {
                    reason: reason.to_string(),
                }
            }
            hakutaku_core::CopyError::MemoryReservationRejected(reason) => {
                CopySelectionError::MemoryReservationRejected {
                    reason: reason.to_string(),
                }
            }
            hakutaku_core::CopyError::SourceUnavailable => CopySelectionError::SourceUnavailable,
        }
    }
}

/// 選択範囲をクリップボードへコピーします。
///
/// `hakutaku_core::assemble_copy` が上限内と判定した場合だけ、実際に
/// クリップボードへ書き込みます（`COPY-005`: 拒否時はクリップボードに一切
/// 触れません）。`display_set_id`・`generation` の意味は `fetch_log_range` と
/// 同じです。`ranges` は表示集合内のインデックス範囲の集合で、飛び飛びの
/// 選択（Ctrl+クリック）をそのまま表します（Issue #85）。表示外の範囲を
/// 含んでいても構いません。時系列統合表示の表示集合（P09-1）も
/// `fetch_log_range` と同じくそのまま指定できます（Issue #37）。
#[tauri::command]
pub fn copy_selection(
    registry: State<'_, DisplaySetRegistryState>,
    config: State<'_, ConfigState>,
    diagnostics: State<'_, Arc<Diagnostics>>,
    display_set_id: u32,
    generation: u64,
    ranges: Vec<CopyRangeArg>,
) -> Result<CopySelectionResponse, CopySelectionError> {
    let diagnostics_ref: &Diagnostics = diagnostics.inner();

    // CFG-018: 最大バイト数（MiB）・最大行数。設定を変更すればこの判定へ
    // 反映される（ConfigState は起動時に読み込んだ検証済みの値）。
    let clipboard_config = config.config.clipboard;
    let limits = hakutaku_core::CopyLimits {
        max_bytes: u64::from(clipboard_config.max_copy_mib).saturating_mul(1024 * 1024),
        max_lines: u64::from(clipboard_config.max_copy_lines),
    };

    // 診断ログ用の合計行数。`assemble_copy` の検証を通る前の生の値なので、
    // 溢れないよう飽和加算で数える（本文は記録しない。SEC-004／LOG-024）。
    let selected_lines = ranges
        .iter()
        .fold(0u64, |total, range| total.saturating_add(range.count));
    let selection = hakutaku_core::CopySelection {
        ranges: ranges
            .into_iter()
            .map(hakutaku_core::CopyRange::from)
            .collect(),
    };

    let assembled = {
        let mut registry_guard = registry.0.lock().unwrap_or_else(PoisonError::into_inner);
        hakutaku_core::assemble_copy(
            &mut registry_guard,
            display_set_id,
            generation,
            &selection,
            limits,
            hakutaku_memory_accounting::global_budget(),
        )
    };

    let outcome = match assembled {
        Ok(outcome) => outcome,
        Err(error) => {
            // COPY-005（Issue #37）: 本文を読み出せずに中止した場合だけ、原因
            // 調査の手がかりとして記録する（他の失敗理由は、そのまま応答として
            // フロントエンドの通知になる。本文はここでも記録しない）。
            if matches!(error, hakutaku_core::CopyError::SourceUnavailable) {
                diag_warn!(
                    diagnostics_ref,
                    module = "clipboard",
                    operation = "clipboard.copy_source_unavailable",
                    "選択範囲の本文を読み出せなかったためコピーを中止しました\
                     （COPY-005）: display_set_id={}, 選択行数={}",
                    display_set_id,
                    selected_lines
                );
            }
            return Err(CopySelectionError::from(error));
        }
    };

    match outcome {
        hakutaku_core::CopyOutcome::Copied(buffer) => {
            set_unicode_text(&buffer.text)
                .map_err(|reason| CopySelectionError::ClipboardWriteFailed { reason })?;

            // LOG-024／SEC-004: 本文は記録せず、バイト数・行数だけを記録する。
            diag_info!(
                diagnostics_ref,
                module = "clipboard",
                operation = "clipboard.copy",
                "選択範囲をクリップボードへコピーしました（COPY-002）: バイト数={}, 行数={}",
                buffer.bytes,
                buffer.lines
            );

            Ok(CopySelectionResponse::Copied {
                bytes: buffer.bytes,
                lines: buffer.lines,
            })
        }
        hakutaku_core::CopyOutcome::Rejected(rejection) => {
            diag_info!(
                diagnostics_ref,
                module = "clipboard",
                operation = "clipboard.copy_rejected",
                "選択範囲が上限を超えたためコピーを拒否しました（COPY-005）: \
                 上限バイト数={}, 上限行数={}, 選択行数={}, 判明バイト数={:?}",
                rejection.limit_bytes,
                rejection.limit_lines,
                rejection.selected_lines,
                rejection.selected_bytes
            );

            Ok(CopySelectionResponse::Rejected {
                limit_bytes: rejection.limit_bytes,
                limit_lines: rejection.limit_lines,
                selected_lines: rejection.selected_lines,
                selected_bytes: rejection.selected_bytes,
                error: copy_rejection_error(),
            })
        }
    }
}

// --- Win32 クリップボード書き込み（COPY-002、ADR-0009: 初期リリースは
// CF_UNICODETEXT のみ） ---

use windows::Win32::Foundation::{GlobalFree, HANDLE};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// クリップボードへ `text` を UTF-16（`CF_UNICODETEXT`）として設定します。
///
/// 失敗時は利用者向けの日本語理由文字列を返します。`OpenClipboard` に成功した
/// 後は、以降どの経路で終わっても必ず `CloseClipboard` を呼びます（失敗時の
/// クローズ保証。書き込み処理を `write_after_open` へ切り出し、この関数が
/// クローズを一本化して保証します）。
fn set_unicode_text(text: &str) -> Result<(), String> {
    // SAFETY: OpenClipboard(None) は、特定のウィンドウではなく「現在のタスク」に
    // 紐づけてクリップボードを開く。この呼び出しはウィンドウハンドルを一切
    // 引き回さない Tauri コマンドハンドラから行うため、既知の有効なハンドルを
    // 持たない None を渡すことが正しい（Win32 の仕様上、hWndNewOwner は
    // 省略可能）。戻り値の Result で成否を判定するだけで、他の前提条件はない。
    unsafe { OpenClipboard(None) }
        .map_err(|error| format!("クリップボードを開けません（{error}）"))?;

    let write_result = write_after_open(text);

    // SAFETY: 直前の OpenClipboard(None) が成功しているため、対応する
    // CloseClipboard を呼ぶことは常に安全（開いた分は必ず閉じる）。
    // write_after_open の成否に関わらずここで必ず1回だけ呼ぶことで、
    // クリップボードを開いたままにしない（失敗時のクローズ保証）。
    let close_result = unsafe { CloseClipboard() }
        .map_err(|error| format!("クリップボードを閉じられません（{error}）"));

    // 書き込みの失敗理由（原因が特定しやすい）を優先して報告する。
    // close_result のエラーは、write が成功していた場合にだけ表面化する。
    write_result?;
    close_result
}

/// `OpenClipboard` 成功後の実処理です（`EmptyClipboard` → メモリ確保 →
/// UTF-16 への変換・書き込み → `SetClipboardData`）。呼び出し元
/// （[`set_unicode_text`]）が `CloseClipboard` の呼び出しを保証します。
fn write_after_open(text: &str) -> Result<(), String> {
    // SAFETY: 呼び出し元 set_unicode_text が OpenClipboard の成功を保証している。
    unsafe { EmptyClipboard() }
        .map_err(|error| format!("クリップボードを空にできません（{error}）"))?;

    // NUL 終端の UTF-16（wide）バッファへ変換する（CF_UNICODETEXT の契約）。
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = utf16.len() * std::mem::size_of::<u16>();

    // SAFETY: GMEM_MOVEABLE はクリップボードへ渡すハンドルとして Win32 が
    // 要求するフラグ（固定アドレスの GMEM_FIXED はクリップボード用途には
    // 使えない）。byte_len は NUL 終端を含む utf16 バッファの実サイズであり、
    // 0 になることはない（空文字列でも終端の2バイトを含む）。
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) }
        .map_err(|error| format!("クリップボード用メモリを確保できません（{error}）"))?;

    // SAFETY: handle は直前の GlobalAlloc が返した有効な所有ハンドル。
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        // ロックに失敗した場合、まだ SetClipboardData に渡していないため
        // このプロセスが所有者のままである。自分で解放する（リーク防止）。
        // SAFETY: handle は GlobalAlloc が返した所有ハンドルであり、他へ
        // 渡していないため、ここで解放する権利と責任がある。
        let _ = unsafe { GlobalFree(Some(handle)) };
        return Err("クリップボード用メモリをロックできません".to_string());
    }

    // SAFETY: ptr は GlobalLock が返した、byte_len バイト以上書き込み可能な
    // 領域（GlobalAlloc(GMEM_MOVEABLE, byte_len) で確保した領域そのもの）。
    // コピー元（utf16.as_ptr()）は utf16.len() 要素（= byte_len バイト）を
    // 持つ別のヒープ確保であり、コピー先と領域が重ならない
    // （copy_nonoverlapping の前提を満たす）。
    unsafe {
        std::ptr::copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), ptr.cast::<u8>(), byte_len);
    }

    // SAFETY: handle は直前に GlobalLock でロック済みの同じハンドル。
    // GlobalUnlock は「ロック参照カウントが1から0になった」通常の成功時にも
    // Win32 の仕様上 FALSE を返す（windows-rs はこれを Err として表現する）。
    // ここでは単一ロック・単一アンロックしか行わないため、この戻り値は
    // 実際のエラーではなく無視してよい（MSDN の既知の挙動）。
    let _ = unsafe { GlobalUnlock(handle) };

    // SAFETY: uformat は Win32 が定義する CF_UNICODETEXT 定数。hmem は直前まで
    // NUL 終端 UTF-16 データを書き込み済みの、ロック解除後のグローバルメモリ
    // ハンドルである。SetClipboardData 成功後はシステムがこのハンドルの
    // 所有権を引き継ぐため、以降このプロセスから GlobalFree してはならない
    // （成功時に解放しないことが正しい）。
    let set_result =
        unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(handle.0))) };

    match set_result {
        Ok(_) => Ok(()),
        Err(error) => {
            // SetClipboardData が失敗した場合、システムは所有権を引き継がない。
            // 呼び出し元（このプロセス）がまだ所有者のままなので解放する。
            // SAFETY: handle はまだこのプロセスが所有する GlobalAlloc の
            // ハンドルであり、SetClipboardData が失敗した（Err）ため、他の
            // 誰にも渡っていない。
            let _ = unsafe { GlobalFree(Some(handle)) };
            Err(format!(
                "クリップボードへの書き込みに失敗しました（{error}）"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 受け入れ条件（Issue #85）: フロントエンドが送る `{ start, count }` の
    // 配列が、コア層の範囲へそのまま（順序も値も変えずに）渡る。
    #[test]
    fn copy_range_args_convert_in_order_without_changing_values() {
        let args = vec![
            CopyRangeArg { start: 0, count: 2 },
            CopyRangeArg { start: 5, count: 1 },
        ];
        let converted: Vec<hakutaku_core::CopyRange> = args
            .into_iter()
            .map(hakutaku_core::CopyRange::from)
            .collect();
        assert_eq!(
            converted,
            vec![
                hakutaku_core::CopyRange { start: 0, count: 2 },
                hakutaku_core::CopyRange { start: 5, count: 1 },
            ]
        );
    }

    // 受け入れ条件（Issue #85）: JS 側が渡す JSON（`{ "start": .., "count": .. }`）
    // をそのまま受け取れる（フィールド名の読み替えを入れていないこと）。
    #[test]
    fn copy_range_arg_deserializes_from_the_frontend_json_shape() {
        let arg: CopyRangeArg =
            serde_json::from_str(r#"{"start":3,"count":4}"#).expect("解釈できるはず");
        assert_eq!(arg.start, 3);
        assert_eq!(arg.count, 4);
    }

    #[test]
    fn copy_selection_error_conversion_maps_fetch_variants() {
        let unknown = CopySelectionError::from(hakutaku_core::CopyError::Fetch(
            hakutaku_core::FetchRangeError::UnknownDisplaySet,
        ));
        assert!(matches!(unknown, CopySelectionError::UnknownDisplaySet));

        let mismatch = CopySelectionError::from(hakutaku_core::CopyError::Fetch(
            hakutaku_core::FetchRangeError::GenerationMismatch {
                expected: 1,
                current: 2,
            },
        ));
        match mismatch {
            CopySelectionError::GenerationMismatch { expected, current } => {
                assert_eq!(expected, 1);
                assert_eq!(current, 2);
            }
            other => panic!("GenerationMismatch を期待したが {other:?} だった"),
        }
    }

    // 受け入れ条件（Issue #85）: 選択範囲の検証で拒否した失敗は、理由の文言を
    // 添えた別種別としてフロントエンドへ届く（利用者へ「選び直す」と案内でき、
    // 原因の特定にも使えるようにするため）。
    #[test]
    fn copy_selection_error_conversion_carries_the_invalid_selection_reason() {
        let converted = CopySelectionError::from(hakutaku_core::CopyError::InvalidSelection(
            hakutaku_core::InvalidSelectionReason::NoRanges,
        ));
        match &converted {
            CopySelectionError::InvalidSelection { reason } => {
                assert_eq!(
                    reason,
                    &hakutaku_core::InvalidSelectionReason::NoRanges.to_string()
                );
            }
            other => panic!("InvalidSelection を期待したが {other:?} だった"),
        }
        let json = serde_json::to_string(&converted).expect("直列化できるはず");
        assert!(
            json.starts_with(r#"{"kind":"invalid_selection","reason":"#),
            "フロントエンド（src/log_view.js）が分岐に使う kind と reason を含むはず: {json}"
        );
    }

    // 受け入れ条件（COPY-005、Issue #37）: 本文を読み出せずに中止した失敗は、
    // 上限超過の拒否とも世代不一致とも別の種別としてフロントエンドへ届く
    // （利用者へ出す文言と次の操作が異なるため）。
    #[test]
    fn copy_selection_error_conversion_maps_source_unavailable() {
        let converted = CopySelectionError::from(hakutaku_core::CopyError::SourceUnavailable);
        assert!(matches!(converted, CopySelectionError::SourceUnavailable));
        let json = serde_json::to_string(&converted).expect("直列化できるはず");
        assert_eq!(json, r#"{"kind":"source_unavailable"}"#);
    }

    // 受け入れ条件（`ERR-002`、Issue #47）: 上限超過による拒否が、対象を開く
    // 操作の失敗と同じ5要素（対象・発生位置・理由・継続可否・次の操作）を
    // 持つ利用者向けエラーを伴ってフロントエンドへ届く。
    #[test]
    fn copy_rejection_carries_all_five_user_facing_elements() {
        let error = copy_rejection_error();

        assert_eq!(error.target, "選択範囲のコピー");
        assert!(
            error.reason.contains("上限"),
            "上限を超えたことが理由として示されるはず: {}",
            error.reason
        );
        assert!(
            error.continuable,
            "クリップボードは変更しておらず、閲覧も選択も続けられるはず（COPY-005）"
        );
        assert!(
            !error.next_action.is_empty(),
            "利用者が次に取れる操作が示されるはず"
        );
    }

    // 受け入れ条件（`ERR-002`、Issue #47）: 上限判定は選択範囲全体の合計に
    // 対して行われ、特定の1行で失敗したわけではないため、発生位置は付けない
    // （表示側は「（特定できません）」を示す）。
    #[test]
    fn copy_rejection_error_omits_the_location_element() {
        assert_eq!(copy_rejection_error().location, None);
    }

    // 受け入れ条件（Issue #47）: 拒否応答は5要素の DTO（`error`）と、
    // フロントエンドが単位付きで整形するための数値（上限値・選択量）の
    // 両方を持つ。数値を DTO へ畳み込むと桁区切り・単位の整形先が Rust 側へ
    // 移り、既存の通知文（`src/log_view.js`）の情報量が落ちる。
    #[test]
    fn copy_selection_rejected_response_carries_both_the_dto_and_the_raw_numbers() {
        let response = CopySelectionResponse::Rejected {
            limit_bytes: 16 * 1024 * 1024,
            limit_lines: 100_000,
            selected_lines: 250_000,
            selected_bytes: Some(20 * 1024 * 1024),
            error: copy_rejection_error(),
        };
        let json = serde_json::to_value(&response).expect("直列化できるはず");
        let rejected = &json["rejected"];

        assert_eq!(rejected["limit_lines"], 100_000);
        assert_eq!(rejected["selected_lines"], 250_000);
        assert_eq!(rejected["selected_bytes"], 20 * 1024 * 1024);
        assert_eq!(rejected["error"]["continuable"], true);
        assert!(
            rejected["error"]["reason"].is_string() && rejected["error"]["next_action"].is_string(),
            "受け側が文面へ差し込む理由と次の操作が含まれるはず: {json}"
        );
        assert!(
            rejected["error"]["location"].is_null(),
            "発生位置は付けない: {json}"
        );
    }

    // 受け入れ条件（COPY-002）: 実際に Win32 クリップボードへ書き込み、
    // 同じ内容が読み戻せる。CI・開発機いずれも Windows 実行が前提のため
    // （.cargo/config.toml で x86_64-pc-windows-msvc に固定）、実際の
    // OpenClipboard/SetClipboardData を呼ぶ統合的な検証として実施する。
    //
    // クリップボードはプロセス外のグローバル資源であり、他のテストと並行
    // 実行されると競合し得るため、この1テストだけで書き込み・読み戻し・
    // 内容確認まで完結させる（他テストからクリップボードへは触れない）。
    #[test]
    fn set_unicode_text_round_trips_through_the_real_clipboard() {
        let sample = "Hakutaku コピー確認用テキスト\n2行目\tタブ";
        set_unicode_text(sample).expect("クリップボードへ書き込めるはず");

        let read_back = read_unicode_text_for_test();
        assert_eq!(read_back, sample);
    }

    /// テスト専用: クリップボードから `CF_UNICODETEXT` を読み戻す。
    fn read_unicode_text_for_test() -> String {
        use windows::Win32::Foundation::HGLOBAL;
        use windows::Win32::System::DataExchange::GetClipboardData;
        use windows::Win32::System::Memory::{GlobalSize, GlobalUnlock};

        // SAFETY: テスト専用の読み戻し。set_unicode_text と同じ規則
        // （Open は必ず Close する）。
        unsafe { OpenClipboard(None) }.expect("開けるはず");

        let result = {
            // SAFETY: 直前に OpenClipboard が成功している。
            let data_handle = unsafe { GetClipboardData(u32::from(CF_UNICODETEXT.0)) }
                .expect("CF_UNICODETEXT が設定されているはず");
            let hglobal = HGLOBAL(data_handle.0);
            // SAFETY: hglobal は GetClipboardData が返した、システムが所有する
            // 有効なハンドル（このプロセスからは解放しない）。
            let size = unsafe { GlobalSize(hglobal) };
            // SAFETY: hglobal は上と同じ有効なハンドル。
            let ptr = unsafe { GlobalLock(hglobal) };
            assert!(!ptr.is_null(), "ロックできるはず");

            // SAFETY: ptr は size バイトの読み取り可能領域（GlobalLock の
            // 契約）。CF_UNICODETEXT は u16 単位の NUL 終端文字列であり、
            // size は u16 の整数倍である前提（Win32 の仕様）。
            let len_u16 = size / std::mem::size_of::<u16>();
            let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), len_u16) };
            let nul_pos = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
            let text = String::from_utf16(&slice[..nul_pos]).expect("有効な UTF-16 のはず");

            // SAFETY: hglobal は直前に GlobalLock でロック済み。
            let _ = unsafe { GlobalUnlock(hglobal) };
            text
        };

        // SAFETY: 冒頭の OpenClipboard が成功しているため、対応する
        // CloseClipboard を呼ぶことは常に安全。
        unsafe { CloseClipboard() }.expect("閉じられるはず");

        result
    }
}
