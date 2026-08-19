//! `WebView2Runtime` フォルダの App Container 用 ACL 確認・設定（`DIST-010`、P01-2）。
//!
//! `SEC-009` は実行時に作成・書き込みするフォルダを `logs`・`temp`・`WebView2` に
//! 限定しているが、`DIST-010` はこれとは別に「`WebView2Runtime` フォルダの
//! アクセス許可（ACL）というメタ情報だけ」を変更する明示的な例外を認めている。
//!
//! - 対象は `WebView2Runtime` **フォルダそのもの**だけであり、フォルダ内のファイル
//!   （Runtime 本体）の内容は一切変更しない（`DIST-011`）。
//! - 対象の SID は App Container（ALL APPLICATION PACKAGES）である `S-1-15-2-1`。
//! - 既に読み取り・実行が継承付きで許可されていれば何も変更せず
//!   [`AclOutcome::AlreadyAccessible`] を返す。
//!
//!   ファイルオブジェクトへ適用された ACE では、`GENERIC_READ` / `GENERIC_EXECUTE`
//!   （`0x80000000` / `0x20000000`）は Windows によって `FILE_GENERIC_READ` |
//!   `FILE_GENERIC_EXECUTE`（`0x1200A9`）へ**写像**されて格納される。そのため
//!   判定は生の `GENERIC_READ` | `GENERIC_EXECUTE` ビットとの一致では行わず、
//!   [`MapGenericMask`] へ`WebView2Runtime` と同じ `SE_FILE_OBJECT` 用の
//!   [`GENERIC_MAPPING`] を渡して写像した後の値どうしで比較する
//!   （[`map_to_file_specific_mask`]）。マジックナンバーを直書きしない。
//! - `INHERIT_ONLY_ACE` が立っている ACE は、子オブジェクトへの継承だけを目的とし
//!   フォルダ自身には権限を与えないため、判定対象から除外する
//!   （[`ace_applies_to_object_itself`]）。
//! - 判定は許可 ACE だけでなく**拒否 ACE も** DACL の並び順どおりに読む
//!   （[`evaluate_dacl`]）。Windows は DACL を先頭から順に評価し、拒否を許可より
//!   優先するため、対象 SID への拒否 ACE が必要なアクセス権と重なっている場合は、
//!   許可 ACE を追加しても有効にならない。この場合は付与を行わず
//!   [`AclOutcome::BlockedByDenyAce`] を返す（`Issue #45`）。
//! - 不足していれば ACE を追加する（[`AclOutcome::Applied`]）。付与は対象フォルダの
//!   **継承構造を変えない**。親から継承していた ACE は継承のまま残り、明示 ACE へ
//!   複製されないため、付与後も親フォルダの権限変更が対象フォルダへ反映され続ける
//!   （根拠と実機確認の範囲は [`grant_access`] の doc コメントを参照。`Issue #45`）。
//! - 現在の権限で変更できない場合は [`AclOutcome::Denied`] を返し、呼び出し側が
//!   `bootstrap::notify::acl_not_applicable` で通知する。
//! - フォルダの有無や判定自体に失敗した場合は [`AclOutcome::Undetermined`] を返す。
//!   起動は続行し、要否の確定は P13（`VER-006`）で行う。

use std::ffi::c_void;
use std::path::Path;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    LocalFree, ERROR_ACCESS_DENIED, GENERIC_EXECUTE, GENERIC_READ, HLOCAL, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW,
    EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::{
    EqualSid, GetAce, MapGenericMask, ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, ACL,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GENERIC_MAPPING, INHERIT_ONLY_ACE,
    OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};

/// App Container（ALL APPLICATION PACKAGES）を表す文字列 SID。
const APP_CONTAINER_SID: &str = "S-1-15-2-1";

/// `WebView2Runtime` フォルダの ACL 確認・設定結果。
#[derive(Clone, Debug)]
pub enum AclOutcome {
    /// 既に App Container からアクセスできる。変更していない。
    AlreadyAccessible,
    /// 対象 SID への拒否 ACE が、必要なアクセス権と重なっている。Windows の
    /// 評価順（拒否が許可より優先）では許可 ACE を追加しても有効にならないため、
    /// 付与を行わなかった。呼び出し側は「付与した」と記録せず、拒否 ACE により
    /// 有効にならない旨と、管理者による ACL の確認が必要である旨を診断ログへ
    /// 記録する（`Issue #45`）。
    BlockedByDenyAce { reason: String },
    /// 不足していたので付与した（`DIST-010`）。
    Applied,
    /// 付与が必要だが現在の権限ではできない。通知が必要。
    Denied {
        reason: String,
        required_privilege: String,
    },
    /// 判定できなかった。Runtime の使用は続行する。
    Undetermined { reason: String },
}

/// `WebView2Runtime` フォルダ**だけ**を対象に、App Container（ALL APPLICATION
/// PACKAGES、`S-1-15-2-1`）からの読み取り・実行アクセスを確認し、不足していれば
/// 付与する。**フォルダ内容（ファイル）を一切変更しない**（`DIST-011`）。
pub fn ensure_app_container_access(runtime_dir: &Path) -> AclOutcome {
    if !runtime_dir.is_dir() {
        return AclOutcome::Undetermined {
            reason: format!(
                "WebView2Runtime フォルダ「{}」が見つからないため、ACL の要否を判定できません。",
                runtime_dir.display()
            ),
        };
    }

    let path_wide = to_wide_null(&runtime_dir.to_string_lossy());

    let target_sid_guard = match convert_app_container_sid() {
        Ok(guard) => guard,
        Err(reason) => return AclOutcome::Undetermined { reason },
    };
    let target_sid = PSID(target_sid_guard.0);

    let (_descriptor_guard, dacl_ptr) = match read_dacl(&path_wide) {
        Ok(pair) => pair,
        Err(outcome) => return outcome,
    };

    let dacl_ptr = match dacl_ptr {
        // DACL が存在しない（NULL DACL）場合、その対象には制限なく全員がアクセスできる。
        // App Container からも既にアクセスできるため、変更せずそのまま返す。
        None => return AclOutcome::AlreadyAccessible,
        Some(ptr) => ptr,
    };

    match evaluate_dacl(dacl_ptr, target_sid) {
        DaclEvaluation::AlreadyAccessible => return AclOutcome::AlreadyAccessible,
        DaclEvaluation::BlockedByDenyAce => {
            return AclOutcome::BlockedByDenyAce {
                reason: deny_ace_blocked_reason(),
            }
        }
        DaclEvaluation::NeedsGrant => {}
    }

    match grant_access(&path_wide, dacl_ptr, target_sid) {
        Ok(()) => AclOutcome::Applied,
        Err(outcome) => outcome,
    }
}

/// LocalAlloc 系の Win32 API（`ConvertStringSidToSidW`・`GetNamedSecurityInfoW`・
/// `SetEntriesInAclW`）が割り当てたメモリを、スコープを抜ける際に確実に解放するガード。
struct LocalAllocGuard(*mut c_void);

impl Drop for LocalAllocGuard {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY: 保持しているポインタは LocalAlloc 系の Win32 API が割り当てた
        // メモリであり、このガード以外から解放されないことを型で保証している。
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

/// App Container 用 SID（`S-1-15-2-1`）を作成する。戻り値のガードが drop される際、
/// `LocalFree` で解放される。
fn convert_app_container_sid() -> Result<LocalAllocGuard, String> {
    convert_string_sid(APP_CONTAINER_SID, "App Container 用 SID")
}

/// 文字列 SID（`S-1-15-2-1` 形式）から SID を作成する。戻り値のガードが drop
/// される際、`LocalFree` で解放される。
///
/// 本体が使うのは [`convert_app_container_sid`] 経由の `S-1-15-2-1` だけだが、
/// テストが別の well-known SID（BATCH `S-1-5-3` など）で継承 ACE の実機挙動を
/// 検証するため、SID 文字列を引数に取る形で切り出している（`Issue #45`）。
///
/// `label` は失敗時の説明文の先頭に置く、その SID の役割名。この説明文は
/// [`AclOutcome::Undetermined`] の `reason` として利用者の目に触れるため、
/// 呼び出し側は「どの用途の SID を作れなかったか」が分かる語を渡す。
fn convert_string_sid(text: &str, label: &str) -> Result<LocalAllocGuard, String> {
    let sid_wide = to_wide_null(text);
    let mut sid_out = PSID(std::ptr::null_mut());

    // SAFETY: sid_wide はこの関数のスコープ内で生存する NUL 終端バッファであり、
    // sid_out は ConvertStringSidToSidW が書き込む出力専用の変数である。
    let result = unsafe { ConvertStringSidToSidW(PCWSTR(sid_wide.as_ptr()), &mut sid_out) };

    match result {
        Ok(()) => Ok(LocalAllocGuard(sid_out.0)),
        Err(error) => Err(format!(
            "{label}（{text}）を作成できません（{}）。",
            error.message()
        )),
    }
}

/// 対象フォルダの DACL を取得する。戻り値のセキュリティ記述子ガードが drop
/// されるまで、返した DACL ポインタは有効である。
///
/// DACL が存在しない（NULL DACL、制限なし）場合は `Ok((guard, None))` を返す。
fn read_dacl(path_wide: &[u16]) -> Result<(LocalAllocGuard, Option<*const ACL>), AclOutcome> {
    let mut dacl_ptr: *mut ACL = std::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());

    // SAFETY: path_wide はこの呼び出しの間だけ生存すればよい NUL 終端バッファ。
    // ppsidowner・ppsidgroup・ppsacl は使わないため None を渡す。dacl_ptr・
    // descriptor は出力専用の変数である。
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut dacl_ptr),
            None,
            &mut descriptor,
        )
    };

    if status.is_err() {
        return Err(AclOutcome::Undetermined {
            reason: format!(
                "WebView2Runtime フォルダのアクセス許可情報を取得できません（{}）。",
                describe_win32_error(status)
            ),
        });
    }

    let guard = LocalAllocGuard(descriptor.0);
    if dacl_ptr.is_null() {
        Ok((guard, None))
    } else {
        Ok((guard, Some(dacl_ptr as *const ACL)))
    }
}

/// [`evaluate_dacl`] が返す、DACL 走査の結果。
///
/// ACE を DACL の並び順どおりに読み、対象 SID について「必要なアクセス権を
/// 満たす許可 ACE」と「必要なアクセス権と重なる拒否 ACE」のどちらが先に
/// 見つかるかで確定する。Windows は DACL を先頭から順に評価し、拒否が許可より
/// 優先されるため、この順序どおりに読むことが判定の正しさに必要（`Issue #45`）。
enum DaclEvaluation {
    /// 必要なアクセス権を満たす許可 ACE が、重なる拒否 ACE より先に見つかった。
    /// 既にアクセスできるため、呼び出し元は変更しない。
    AlreadyAccessible,
    /// 必要なアクセス権と重なる拒否 ACE が、それを満たす許可 ACE より先に
    /// 見つかった。許可 ACE を追加してもこの拒否 ACE が先に評価されるため
    /// 有効にならない。呼び出し元は付与を行ってはならない。
    BlockedByDenyAce,
    /// 満たす許可 ACE も、重なる拒否 ACE も見つからなかった。呼び出し元は
    /// 許可 ACE を追加する必要がある（[`grant_access`]）。
    NeedsGrant,
}

/// 対象の DACL を先頭（インデックス0）から走査し、App Container 用 SID に
/// ついて「読み取り + 実行、継承あり」のアクセスが既に有効か、拒否 ACE に
/// よって有効化できないか、あるいはどちらでもないか（許可 ACE の追加が必要）
/// を判定する。
///
/// `ACCESS_ALLOWED_ACE_TYPE`・`ACCESS_DENIED_ACE_TYPE` 以外（監査 ACE など）と、
/// `INHERIT_ONLY_ACE`（子オブジェクトへの継承だけが目的で、フォルダ自身には
/// 適用されない ACE）は許可・拒否のどちらでも対象にしない。対象 SID と一致
/// しない ACE も読み飛ばす。
///
/// 許可 ACE は、単独でマスク（[`access_mask_grants_required`]）と継承フラグ
/// （[`ace_flags_have_required_inheritance`]）の両方を満たす場合だけ
/// [`DaclEvaluation::AlreadyAccessible`] とする（複数 ACE にまたがる合算は
/// 行わない。[`grant_access`] が付与する ACE 自体が単独でこの条件を満たす形で
/// 構成されているため、往復判定と整合する）。拒否 ACE は、写像後のマスクが
/// 必要なアクセス権のビットと少しでも重なれば
/// [`DaclEvaluation::BlockedByDenyAce`] とする（[`deny_mask_overlaps_required`]。
/// 拒否 ACE の継承フラグは問わない。フォルダ自身への適用可否だけが問題であり、
/// 子への継承可否は無関係）。
fn evaluate_dacl(dacl: *const ACL, target: PSID) -> DaclEvaluation {
    // SAFETY: dacl は read_dacl が GetNamedSecurityInfoW から取得した有効な
    // ポインタであり、呼び出し元（ensure_app_container_access）がガードを
    // 生存させている間だけ使われる。
    let ace_count = unsafe { (*dacl).AceCount };

    for index in 0..u32::from(ace_count) {
        let mut ace_ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: dacl は有効な ACL であり、index は AceCount 未満である。
        // ace_ptr は出力専用の変数である。
        let got = unsafe { GetAce(dacl, index, &mut ace_ptr) };
        if got.is_err() {
            // 個々の ACE を取得できない場合は、その ACE だけ読み飛ばし、
            // 残りの ACE の確認を続ける。
            continue;
        }

        // SAFETY: GetAce が返したポインタは、共通ヘッダー（ACE_HEADER）として
        // 読み取れることが Win32 の仕様で保証されている。
        let header = unsafe { *(ace_ptr as *const ACE_HEADER) };
        let ace_type = u32::from(header.AceType);
        let is_allow = ace_type == ACCESS_ALLOWED_ACE_TYPE;
        let is_deny = ace_type == ACCESS_DENIED_ACE_TYPE;
        if !is_allow && !is_deny {
            // 監査 ACE など、許可・拒否以外は対象にしない。
            continue;
        }

        if !ace_applies_to_object_itself(header.AceFlags) {
            // INHERIT_ONLY_ACE は子オブジェクトへの継承だけが目的であり、
            // フォルダ自身には適用されない（許可・拒否のどちらでも同じ）。
            continue;
        }

        // 注: ACCESS_ALLOWED_ACE と ACCESS_DENIED_ACE は Header・Mask・SidStart
        // の並びが同一の形状（Win32 の仕様）であり、SidStart から SID を読み取る
        // 手順は両者で共通化できる。SidStart は可変長 SID 領域の先頭を指す
        // フィールドであり、そのアドレスは有効な PSID として扱える。ポインタの
        // 構築自体はアドレス取得と型変換だけであり、参照外し（デリファレンス）
        // を伴わないため unsafe は不要。
        let (mask, ace_sid) = if is_allow {
            // SAFETY: AceType が ACCESS_ALLOWED_ACE_TYPE であることを確認した
            // ため、このポインタは ACCESS_ALLOWED_ACE として読み取れる。
            let ace = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };
            (ace.Mask, PSID(&ace.SidStart as *const u32 as *mut c_void))
        } else {
            // SAFETY: AceType が ACCESS_DENIED_ACE_TYPE であることを確認した
            // ため、このポインタは ACCESS_DENIED_ACE として読み取れる。
            let ace = unsafe { &*(ace_ptr as *const ACCESS_DENIED_ACE) };
            (ace.Mask, PSID(&ace.SidStart as *const u32 as *mut c_void))
        };

        // SAFETY: ace_sid は上で得た有効な SID、target は呼び出し元が保持する
        // 有効な SID である。
        if unsafe { EqualSid(ace_sid, target) }.is_err() {
            continue;
        }

        if is_allow {
            if access_mask_grants_required(mask)
                && ace_flags_have_required_inheritance(header.AceFlags)
            {
                return DaclEvaluation::AlreadyAccessible;
            }
            // この許可 ACE 単独では不足。以降の ACE（他の許可 ACE や拒否 ACE）
            // の確認を続ける。
        } else if deny_mask_overlaps_required(mask) {
            // Windows は DACL を先頭から評価し、拒否が許可より優先される。この
            // 拒否 ACE がここまでのどの許可 ACE よりも先に必要なアクセス権と
            // 重なったため、以降に満たす許可 ACE があっても有効にならない。
            // 安全側に倒し、以降の ACE は確認せずここで確定する。
            return DaclEvaluation::BlockedByDenyAce;
        }
    }

    DaclEvaluation::NeedsGrant
}

/// [`AclOutcome::BlockedByDenyAce`] の `reason` に使う、拒否 ACE により付与しても
/// 有効にならないことの説明。呼び出し元（`bootstrap::runtime`）が対象パスを
/// 前置して診断ログへ記録する。
fn deny_ace_blocked_reason() -> String {
    format!(
        "App Container 用 SID（{APP_CONTAINER_SID}）に対する拒否 ACE が、必要な読み取り + \
         実行のアクセス権と重なっているため、許可 ACE を追加しても有効になりません。\
         管理者による ACL の確認が必要です。"
    )
}

/// `SE_FILE_OBJECT`（ファイル・フォルダ）用の `GENERIC_MAPPING`。
///
/// `MapGenericMask` へ渡し、`GENERIC_READ` / `GENERIC_EXECUTE` などの汎用ビットを
/// ファイル固有の権利（`FILE_GENERIC_READ` など）へ写像するために使う。
fn file_generic_mapping() -> GENERIC_MAPPING {
    GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ.0,
        GenericWrite: FILE_GENERIC_WRITE.0,
        GenericExecute: FILE_GENERIC_EXECUTE.0,
        GenericAll: FILE_ALL_ACCESS.0,
    }
}

/// アクセスマスクに含まれる汎用ビット（`GENERIC_READ` 等）を、`SE_FILE_OBJECT`
/// 用のファイル固有ビット（`FILE_GENERIC_READ` 等）へ写像する。
///
/// 既にファイル固有のビットだけで構成されたマスクを渡した場合は変化しない。
/// ファイルオブジェクトへ実際に適用された ACE は、`GENERIC_READ` |
/// `GENERIC_EXECUTE`（`0x80000000` | `0x20000000`）ではなく、写像済みの
/// `FILE_GENERIC_READ` | `FILE_GENERIC_EXECUTE`（`0x1200A9`）として格納される
/// ため、判定・付与のどちらでもこの関数を通した値どうしで比較・使用する。
fn map_to_file_specific_mask(mask: u32) -> u32 {
    let mapping = file_generic_mapping();
    let mut mapped = mask;
    // SAFETY: mapped はここで初期化済みのローカル変数であり、その可変ポインタを
    // 渡す。mapping も同じ関数内で生存するローカル値である。MapGenericMask は
    // ビット演算だけを行い、この呼び出しの範囲外へ副作用を及ぼさない。
    unsafe {
        MapGenericMask(&mut mapped, &mapping);
    }
    mapped
}

/// 「読み取り + 実行」に必要な、ファイル固有表現（写像後）でのアクセスマスク。
///
/// マジックナンバー（`0x1200A9` など）を直書きせず、`GENERIC_READ` |
/// `GENERIC_EXECUTE` を [`map_to_file_specific_mask`] へ通して求める。
fn required_access_mask() -> u32 {
    map_to_file_specific_mask(GENERIC_READ.0 | GENERIC_EXECUTE.0)
}

/// アクセスマスクに、読み取り + 実行に相当するビットが含まれているかを判定する。
///
/// 比較対象の ACE 側マスクも [`map_to_file_specific_mask`] へ通してから比較する。
/// これにより、ACE が汎用ビット（`GENERIC_READ` 等）のまま格納されている場合と、
/// 既にファイル固有ビット（`FILE_GENERIC_READ` 等）へ写像済みの場合の両方に
/// 対応できる。
fn access_mask_grants_required(mask: u32) -> bool {
    let required = required_access_mask();
    let mapped_mask = map_to_file_specific_mask(mask);
    mapped_mask & required == required
}

/// 拒否 ACE のアクセスマスクが、必要な読み取り + 実行のアクセス権と
/// 少しでも重なっているかを判定する（`Issue #45`）。
///
/// [`access_mask_grants_required`]（許可 ACE 用。必要なビットを**すべて**
/// 満たすかを判定する）とは条件が異なる。拒否 ACE は、必要なアクセス権の
/// 一部とだけ重なっていても、その重なった部分は許可 ACE を追加しても有効に
/// ならない（Windows の評価順で拒否が許可より優先されるため）。そのため
/// 「一部でも重なる」（AND が非ゼロ）を判定条件とする。
fn deny_mask_overlaps_required(mask: u32) -> bool {
    let required = required_access_mask();
    let mapped_mask = map_to_file_specific_mask(mask);
    mapped_mask & required != 0
}

/// ACE の継承フラグに、フォルダ・オブジェクトの両方への継承
/// （`CONTAINER_INHERIT_ACE` | `OBJECT_INHERIT_ACE`）が含まれているかを判定する。
/// Win32 API を呼ばない純粋な判定。
fn ace_flags_have_required_inheritance(flags: u8) -> bool {
    let required = (CONTAINER_INHERIT_ACE.0 | OBJECT_INHERIT_ACE.0) as u8;
    flags & required == required
}

/// ACE が対象フォルダ**自身**への許可として有効かどうかを判定する。
///
/// `INHERIT_ONLY_ACE` が立っている ACE は、子オブジェクトへ継承させることだけが
/// 目的であり、フォルダ自身のアクセス制御には使われない（Win32 の仕様）。
/// マスクや継承フラグが要件を満たしていても、この ACE だけを根拠に
/// [`AclOutcome::AlreadyAccessible`] を返してはならない。Win32 API を呼ばない
/// 純粋な判定。
fn ace_applies_to_object_itself(flags: u8) -> bool {
    let inherit_only = INHERIT_ONLY_ACE.0 as u8;
    flags & inherit_only == 0
}

/// 権限不足などで ACL を変更できなかった場合に、利用者へ伝える対処の説明。
const REQUIRED_PRIVILEGE_MESSAGE: &str = "WebView2Runtime フォルダの所有者、または管理者権限を持つ利用者が、このフォルダのアクセス許可を変更する必要があります。エクスプローラーでフォルダを右クリックし、「プロパティ」→「セキュリティ」タブ→「編集」または「詳細設定」からアクセス許可を追加するか、管理者として Hakutaku を再起動して自動設定を再試行してください。";

/// App Container 用 SID へ、読み取り + 実行（継承あり）のアクセス許可を追加する。
///
/// `existing_dacl` は [`read_dacl`] が `GetNamedSecurityInfoW` から得たもので、
/// 親フォルダから継承した ACE（`INHERITED_ACE` フラグ付き）も含む。それをそのまま
/// `SetEntriesInAclW` の OldAcl として渡すため、「書き戻しで継承 ACE が対象フォルダの
/// 明示 ACE へ複製され、以後、親フォルダの権限変更が反映されなくなるのではないか」
/// という懸念があった（`Issue #45`）。
///
/// 実機で確認した結果、この複製は起きない。`SetNamedSecurityInfoW` は自動継承の
/// 規則に従い、渡された ACL のうち `INHERITED_ACE` フラグが立った ACE を明示 ACE
/// として保存せず、書き戻しの際に親から継承し直す。確認は Windows 10 22H2
/// （10.0.19045）で行い、`tests::granting_access_keeps_inherited_aces_inherited` が
/// 回帰テストとして固定している（継承 ACE が1個・継承のまま残ること、および付与後に
/// 親の権限を広げるとその変更が子へ伝播することを検証する）。
///
/// この保証は、`SetNamedSecurityInfoW` へ `DACL_SECURITY_INFORMATION` **だけ**を
/// 渡すことに依存する。`PROTECTED_DACL_SECURITY_INFORMATION` /
/// `UNPROTECTED_DACL_SECURITY_INFORMATION` を追加すると対象フォルダの保護状態
/// （`SE_DACL_PROTECTED`）自体を切り替えてしまい、Hakutaku が変更してよい範囲
/// （`DIST-010` が認める ACL の付与）を超えるため、追加しない。
fn grant_access(
    path_wide: &[u16],
    existing_dacl: *const ACL,
    target_sid: PSID,
) -> Result<(), AclOutcome> {
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_UNKNOWN,
        // 注: TrusteeForm が TRUSTEE_IS_SID の場合、ptstrName フィールドは文字列
        // ではなく PSID へのポインタとして扱われる（Win32 の仕様）。target_sid は
        // 呼び出し元が生存を保証している有効な SID であり、型変換だけで
        // 参照外しを伴わないため unsafe は不要。
        ptstrName: PWSTR(target_sid.0 as *mut u16),
    };

    let entry = EXPLICIT_ACCESS_W {
        // 判定（access_mask_grants_required）と表現を揃えるため、付与するマスクも
        // required_access_mask()（写像済みのファイル固有ビット、DIST-010）を使う。
        // これにより、付与直後に再度判定しても AlreadyAccessible になる。
        grfAccessPermissions: required_access_mask(),
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
        Trustee: trustee,
    };

    let mut new_acl: *mut ACL = std::ptr::null_mut();

    // SAFETY: entry はこの呼び出しの間だけ生存すればよいローカル値。
    // existing_dacl は呼び出し元（ensure_app_container_access）が生存させている
    // 有効な DACL ポインタ。new_acl は出力専用の変数である。
    let build_status = unsafe {
        SetEntriesInAclW(
            Some(std::slice::from_ref(&entry)),
            Some(existing_dacl),
            &mut new_acl,
        )
    };

    if build_status.is_err() {
        return Err(classify_write_failure(
            build_status,
            "新しいアクセス許可エントリを作成できません",
        ));
    }

    // 以降 new_acl は必ずこのガードを通じて解放する（成功・失敗いずれの経路でも）。
    let new_acl_guard = LocalAllocGuard(new_acl as *mut c_void);

    // 継承構造を変えないため、指定するのは DACL_SECURITY_INFORMATION だけに留める
    // （保護状態を切り替える PROTECTED / UNPROTECTED を足さない理由は、この関数の
    // doc コメントを参照。`Issue #45`）。
    // SAFETY: path_wide はこの呼び出しの間だけ生存すればよい NUL 終端バッファ。
    // new_acl は直前の SetEntriesInAclW が作成した有効な ACL である。
    let apply_status = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl as *const ACL),
            None,
        )
    };

    drop(new_acl_guard);

    if apply_status.is_err() {
        return Err(classify_write_failure(
            apply_status,
            "WebView2Runtime フォルダへのアクセス許可を設定できません",
        ));
    }

    Ok(())
}

/// ACL の書き込み（付与）に失敗した場合の結果を分類する。
/// `ERROR_ACCESS_DENIED` は権限不足として `Denied` にし、それ以外は `Undetermined` にする。
fn classify_write_failure(status: WIN32_ERROR, context: &str) -> AclOutcome {
    let reason = format!("{context}（{}）。", describe_win32_error(status));

    if status == ERROR_ACCESS_DENIED {
        AclOutcome::Denied {
            reason,
            required_privilege: REQUIRED_PRIVILEGE_MESSAGE.to_string(),
        }
    } else {
        AclOutcome::Undetermined { reason }
    }
}

/// `WIN32_ERROR` を OS のエラー文言込みの日本語の説明文へ変換する。
fn describe_win32_error(status: WIN32_ERROR) -> String {
    format!(
        "{}、OS エラーコード: {}",
        status.to_hresult().message(),
        status.0
    )
}

/// 文字列を NUL 終端の UTF-16（wide）バッファへ変換する。
fn to_wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows::Win32::Security::Authorization::{ACCESS_MODE, DENY_ACCESS};
    use windows::Win32::Security::INHERITED_ACE;
    use windows::Win32::Storage::FileSystem::FILE_WRITE_DATA;

    /// テスト専用: 継承 ACE の実機挙動を観測するための目印に使う well-known SID
    /// （BATCH）。本体が扱う `S-1-15-2-1` と衝突せず、テスト用フォルダへ付けても
    /// 実害のない良性のグループであるため選んでいる（`Issue #45`）。
    const BATCH_SID: &str = "S-1-5-3";

    // 以下はマスクの写像・継承フラグ・INHERIT_ONLY_ACE の除外という
    // 純粋なロジックだけを検証する（ファイル・レジストリ等へは一切触れない）。
    // なお map_to_file_specific_mask は MapGenericMask（Win32 API）を呼ぶが、
    // 副作用のない決定的なビット演算であるため、ここでの「純粋」はこの意味で使う。

    #[test]
    fn access_mask_satisfied_by_mapped_file_specific_bits() {
        // 実機で観測された「読み取り + 実行」の実効値（0x1200A9 相当）。
        let mapped = FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0;
        assert!(access_mask_grants_required(mapped));
    }

    #[test]
    fn access_mask_satisfied_by_generic_bits_after_mapping() {
        // ACE が汎用ビットのまま（GENERIC_READ | GENERIC_EXECUTE = 0xA0000000）
        // 格納されている場合でも、写像後に比較すれば充足すると判定できる。
        let generic = GENERIC_READ.0 | GENERIC_EXECUTE.0;
        assert!(access_mask_grants_required(generic));
    }

    #[test]
    fn access_mask_not_satisfied_by_read_only_or_execute_only() {
        assert!(!access_mask_grants_required(FILE_GENERIC_READ.0));
        assert!(!access_mask_grants_required(FILE_GENERIC_EXECUTE.0));
        assert!(!access_mask_grants_required(0));
    }

    #[test]
    fn access_mask_satisfied_by_full_access() {
        assert!(access_mask_grants_required(FILE_ALL_ACCESS.0));
    }

    #[test]
    fn inheritance_requires_both_container_and_object_flags() {
        let required = (CONTAINER_INHERIT_ACE.0 | OBJECT_INHERIT_ACE.0) as u8;
        assert!(ace_flags_have_required_inheritance(required));
        assert!(!ace_flags_have_required_inheritance(
            CONTAINER_INHERIT_ACE.0 as u8
        ));
        assert!(!ace_flags_have_required_inheritance(
            OBJECT_INHERIT_ACE.0 as u8
        ));
        assert!(!ace_flags_have_required_inheritance(0));
    }

    #[test]
    fn inherit_only_ace_is_excluded_from_applying_to_the_object_itself() {
        // マスク・継承フラグが揃っていても、INHERIT_ONLY_ACE が立っている ACE は
        // 子オブジェクトへの継承専用であり、フォルダ自身への許可にはならない。
        let inherit_only = INHERIT_ONLY_ACE.0 as u8;
        let with_other_flags =
            inherit_only | (CONTAINER_INHERIT_ACE.0 | OBJECT_INHERIT_ACE.0) as u8;
        assert!(!ace_applies_to_object_itself(inherit_only));
        assert!(!ace_applies_to_object_itself(with_other_flags));
    }

    #[test]
    fn ace_without_inherit_only_applies_to_the_object_itself() {
        assert!(ace_applies_to_object_itself(0));
        assert!(ace_applies_to_object_itself(
            (CONTAINER_INHERIT_ACE.0 | OBJECT_INHERIT_ACE.0) as u8
        ));
    }

    #[test]
    fn undetermined_reason_mentions_missing_folder_when_path_does_not_exist() {
        let missing = std::env::temp_dir().join("hakutaku-acl-test-missing-folder-does-not-exist");
        let outcome = ensure_app_container_access(&missing);
        match outcome {
            AclOutcome::Undetermined { reason } => {
                assert!(reason.contains(&missing.display().to_string()));
            }
            other => panic!("フォルダが存在しない場合は Undetermined を返すはずです: {other:?}"),
        }
    }

    /// テスト専用の一意な作業ディレクトリ。本体コードは `SEC-009` により
    /// `std::env::temp_dir()` を参照しないが、テストコードでは実際の
    /// ファイルシステム・ACL 操作を検証するために使う（`layout.rs` のテストと
    /// 同様の方針）。`Drop` で必ず後片付けする。
    struct TestFolder {
        path: std::path::PathBuf,
    }

    impl TestFolder {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let count = COUNTER.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir()
                .join(format!("hakutaku-acl-test-{label}-{pid}-{nanos}-{count}"));
            std::fs::create_dir_all(&path).expect("テスト用フォルダを作成できません");
            Self { path }
        }
    }

    impl Drop for TestFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn ensure_app_container_access_round_trip_applies_then_reports_already_accessible() {
        // 今回の不具合の核心: 付与した直後に同じフォルダをもう一度判定すると、
        // 生の GENERIC_READ | GENERIC_EXECUTE と実際に格納される
        // FILE_GENERIC_READ | FILE_GENERIC_EXECUTE（写像後の値）の不一致により
        // 「不足」と誤判定され、Applied を繰り返してしまっていた。
        // 往復で AlreadyAccessible になることを確認する。
        let folder = TestFolder::new("round-trip");

        let first = ensure_app_container_access(&folder.path);
        assert!(
            matches!(first, AclOutcome::Applied),
            "新規フォルダでは Applied を返すはずです: {first:?}"
        );

        let second = ensure_app_container_access(&folder.path);
        assert!(
            matches!(second, AclOutcome::AlreadyAccessible),
            "付与直後の再判定は AlreadyAccessible を返すはずです: {second:?}"
        );
    }

    // 以下は Issue #45(a) の検証: DACL に拒否 ACE が含まれる場合の判定・診断文言。
    // `grant_access`（本体コード）と同じ SetEntriesInAclW / SetNamedSecurityInfoW の
    // 手順を、grfAccessMode だけ DENY_ACCESS に変えてテスト用フォルダへ拒否 ACE を
    // 設定する（管理者権限は不要。対象は自プロセスが所有するテスト用フォルダ）。

    /// テスト専用: 対象フォルダの DACL へ、App Container 用 SID への拒否 ACE
    /// （継承あり、指定したアクセスマスク）を追加する。
    fn add_deny_ace_for_app_container(path: &std::path::Path, mask: u32) {
        add_inheritable_ace(path, APP_CONTAINER_SID, mask, DENY_ACCESS);
    }

    /// テスト専用: 対象フォルダの DACL へ、指定した文字列 SID に対する ACE
    /// （`CONTAINER_INHERIT_ACE` | `OBJECT_INHERIT_ACE` 付き）を追加する。
    ///
    /// 本体の [`grant_access`] と同じ `SetEntriesInAclW` / `SetNamedSecurityInfoW`
    /// の手順を、対象 SID・アクセスマスク・許可/拒否の別だけ差し替えたもの。
    /// テスト用フォルダを組み立てる下準備と、付与後に親フォルダの権限を広げて
    /// 継承の伝播を確かめる操作の両方に使う。
    fn add_inheritable_ace(path: &std::path::Path, string_sid: &str, mask: u32, mode: ACCESS_MODE) {
        let path_wide = to_wide_null(&path.to_string_lossy());
        let target_sid_guard = convert_string_sid(string_sid, "テスト用の SID")
            .expect("テスト用の SID を作成できません");
        let target_sid = PSID(target_sid_guard.0);

        let (_descriptor_guard, dacl_ptr) =
            read_dacl(&path_wide).expect("テスト用フォルダの DACL を取得できません");
        let existing_dacl =
            dacl_ptr.expect("テスト用フォルダ（TestFolder::new が作成）には DACL があるはずです");

        let trustee = TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            // 注: 本体コードの grant_access と同じ理由（TrusteeForm が
            // TRUSTEE_IS_SID の場合、ptstrName は PSID として扱われる）で unsafe
            // は不要。
            ptstrName: PWSTR(target_sid.0 as *mut u16),
        };

        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: mask,
            grfAccessMode: mode,
            grfInheritance: CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
            Trustee: trustee,
        };

        let mut new_acl: *mut ACL = std::ptr::null_mut();
        // SAFETY: entry はこの呼び出しの間だけ生存すればよいローカル値。
        // existing_dacl は直前に取得した有効な DACL ポインタ（_descriptor_guard
        // が生存している間だけ有効）。new_acl は出力専用の変数である。
        let build_status = unsafe {
            SetEntriesInAclW(
                Some(std::slice::from_ref(&entry)),
                Some(existing_dacl),
                &mut new_acl,
            )
        };
        assert!(
            build_status.is_ok(),
            "テスト用の ACE を含む ACL を構築できません: {build_status:?}"
        );

        let new_acl_guard = LocalAllocGuard(new_acl as *mut c_void);

        // SAFETY: path_wide はこの呼び出しの間だけ生存すればよい NUL 終端
        // バッファ。new_acl は直前の SetEntriesInAclW が作成した有効な ACL。
        let apply_status = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(path_wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(new_acl as *const ACL),
                None,
            )
        };
        drop(new_acl_guard);

        assert!(
            apply_status.is_ok(),
            "テスト用の ACE を適用できません: {apply_status:?}"
        );
    }

    #[test]
    fn deny_mask_overlaps_required_detects_full_and_partial_overlap() {
        // 受け入れ条件: 必要な読み取り+実行のアクセス権と少しでも重なる拒否
        // マスクは重なりありと判定する（`Issue #45`）。
        assert!(deny_mask_overlaps_required(
            FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0
        ));
        // 単一ビット（FILE_EXECUTE 相当）だけの部分的な重なりでも検出する。
        assert!(deny_mask_overlaps_required(FILE_GENERIC_EXECUTE.0));
        assert!(deny_mask_overlaps_required(GENERIC_READ.0));

        // 注意点: FILE_GENERIC_WRITE は「書き込み」の拒否のつもりでも、
        // STANDARD_RIGHTS（READ_CONTROL・SYNCHRONIZE）を read/execute と共有して
        // いるため、必要なアクセス権と重なると判定される。これは実際の Windows
        // の AccessCheck でも同様（要求した SYNCHRONIZE 等のビットがこの拒否
        // ACE によって拒否されるため）であり、誤判定ではない。
        assert!(deny_mask_overlaps_required(FILE_GENERIC_WRITE.0));

        // STANDARD_RIGHTS を含まない、読み取り+実行と重ならない単一ビット
        // （FILE_WRITE_DATA）は重ならないと判定する。
        assert!(!deny_mask_overlaps_required(FILE_WRITE_DATA.0));
        assert!(!deny_mask_overlaps_required(0));
    }

    #[test]
    fn deny_ace_blocked_reason_mentions_sid_and_administrator_action() {
        // 受け入れ条件: 診断メッセージ（の一部）に、対象 SID と、管理者による
        // 確認が必要である旨が含まれる（`Issue #45`。パス自体は呼び出し元の
        // bootstrap::runtime 側が前置する）。
        let reason = deny_ace_blocked_reason();
        assert!(reason.contains(APP_CONTAINER_SID));
        assert!(reason.contains("拒否 ACE"));
        assert!(reason.contains("管理者"));
    }

    #[test]
    fn deny_ace_overlapping_required_access_blocks_grant_and_reports_blocked() {
        // 受け入れ条件: 対象 SID への拒否 ACE が必要なアクセス権（読み取り+
        // 実行）と重なる場合、許可 ACE を追加せず BlockedByDenyAce を返す
        // （`Issue #45`）。
        let folder = TestFolder::new("deny-overlap");
        add_deny_ace_for_app_container(&folder.path, required_access_mask());

        let outcome = ensure_app_container_access(&folder.path);
        match outcome {
            AclOutcome::BlockedByDenyAce { reason } => {
                assert!(reason.contains(APP_CONTAINER_SID));
                assert!(reason.contains("管理者"));
            }
            other => panic!(
                "拒否 ACE が必要なアクセス権と重なる場合は BlockedByDenyAce を返すはずです: {other:?}"
            ),
        }

        // 拒否 ACE が残っている限り、許可 ACE を追加していないため再判定も
        // 同じ結果になる（付与していないことの確認）。
        let second = ensure_app_container_access(&folder.path);
        assert!(
            matches!(second, AclOutcome::BlockedByDenyAce { .. }),
            "拒否 ACE が残っている限り、再判定も BlockedByDenyAce を返すはずです: {second:?}"
        );
    }

    #[test]
    fn deny_ace_not_overlapping_required_access_does_not_block_grant() {
        // 受け入れ条件: 拒否 ACE が存在していても、必要なアクセス権と重ならなけ
        // れば判定に影響せず、従来どおり付与できる（`Issue #45`。安全側に倒し
        // 過ぎて無関係な拒否 ACE まで塞き止めないことの確認）。
        let folder = TestFolder::new("deny-no-overlap");
        add_deny_ace_for_app_container(&folder.path, FILE_WRITE_DATA.0);

        let first = ensure_app_container_access(&folder.path);
        assert!(
            matches!(first, AclOutcome::Applied),
            "重ならない拒否 ACE は無視して付与するはずです: {first:?}"
        );

        let second = ensure_app_container_access(&folder.path);
        assert!(
            matches!(second, AclOutcome::AlreadyAccessible),
            "付与後の再判定は AlreadyAccessible のはずです: {second:?}"
        );
    }

    // 以下は Issue #45(b) の検証: ACL の書き戻しが、親フォルダから継承していた
    // ACE を対象フォルダの明示 ACE へ複製してしまわないこと。

    /// テスト専用: 対象の DACL を先頭から走査し、指定した文字列 SID に一致する
    /// 許可・拒否 ACE の `(AceFlags, Mask)` を並び順どおりに集める。
    ///
    /// 継承の有無は `icacls` の表示（ロケール依存）ではなく、この `AceFlags` の
    /// `INHERITED_ACE` ビットで判定する。
    fn aces_for_sid(path: &std::path::Path, string_sid: &str) -> Vec<(u8, u32)> {
        let path_wide = to_wide_null(&path.to_string_lossy());
        let sid_guard = convert_string_sid(string_sid, "テスト用の SID")
            .expect("テスト用の SID を作成できません");
        let target = PSID(sid_guard.0);

        let (_descriptor_guard, dacl_ptr) =
            read_dacl(&path_wide).expect("テスト用フォルダの DACL を取得できません");
        let dacl = dacl_ptr.expect("テスト用フォルダには DACL があるはずです");

        let mut found = Vec::new();
        // SAFETY: dacl は read_dacl が返した有効なポインタであり、
        // _descriptor_guard が生存している間だけ使う。
        let ace_count = unsafe { (*dacl).AceCount };
        for index in 0..u32::from(ace_count) {
            let mut ace_ptr: *mut c_void = std::ptr::null_mut();
            // SAFETY: dacl は有効な ACL、index は AceCount 未満、ace_ptr は出力専用。
            if unsafe { GetAce(dacl, index, &mut ace_ptr) }.is_err() {
                continue;
            }
            // SAFETY: GetAce が返したポインタは ACE_HEADER として読み取れる。
            let header = unsafe { *(ace_ptr as *const ACE_HEADER) };
            let ace_type = u32::from(header.AceType);
            let (mask, ace_sid) = if ace_type == ACCESS_ALLOWED_ACE_TYPE {
                // SAFETY: AceType を確認済みのため ACCESS_ALLOWED_ACE として読める。
                let ace = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };
                (ace.Mask, PSID(&ace.SidStart as *const u32 as *mut c_void))
            } else if ace_type == ACCESS_DENIED_ACE_TYPE {
                // SAFETY: AceType を確認済みのため ACCESS_DENIED_ACE として読める。
                let ace = unsafe { &*(ace_ptr as *const ACCESS_DENIED_ACE) };
                (ace.Mask, PSID(&ace.SidStart as *const u32 as *mut c_void))
            } else {
                continue;
            };

            // SAFETY: ace_sid・target はいずれもこの時点で有効な SID である。
            if unsafe { EqualSid(ace_sid, target) }.is_ok() {
                found.push((header.AceFlags, mask));
            }
        }
        found
    }

    /// テスト専用: ACE の `AceFlags` に `INHERITED_ACE` が立っているか。
    /// 立っていれば、その ACE は対象自身の明示 ACE ではなく親からの継承である。
    fn is_inherited(flags: u8) -> bool {
        flags & INHERITED_ACE.0 as u8 != 0
    }

    /// テスト専用: `icacls` の生出力（証拠として `--nocapture` で目視するため）。
    ///
    /// 表示文言はロケール依存であり、アサートには使わない。
    fn icacls_dump(path: &std::path::Path) -> String {
        match std::process::Command::new("icacls").arg(path).output() {
            Ok(output) => format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => format!("icacls を実行できません: {error}"),
        }
    }

    #[test]
    fn granting_access_keeps_inherited_aces_inherited() {
        // 受け入れ条件: 親から継承した ACE を持つフォルダへ許可 ACE を付与しても、
        // 継承 ACE が明示 ACE へ複製・変換されず（`INHERITED_ACE` のまま1個で
        // 残り）、付与後も親フォルダの権限変更が子へ反映され続ける
        // （`DIST-010`、`Issue #45`）。
        let parent = TestFolder::new("inherit-parent");

        // 順序依存: 継承可能 ACE を親へ付けた**後**に子を作る。子の作成時に親の
        // 継承可能 ACE が複製されるという OS の既定動作を使うため、逆順にすると
        // 前提条件（子に継承 ACE がある）が成立しない。
        add_inheritable_ace(&parent.path, BATCH_SID, FILE_GENERIC_READ.0, GRANT_ACCESS);
        let child = parent.path.join("child");
        std::fs::create_dir(&child).expect("子フォルダを作成できません");

        let before = aces_for_sid(&child, BATCH_SID);
        println!("icacls（付与前）:\n{}", icacls_dump(&child));
        println!("BATCH の (AceFlags, Mask)（付与前）: {before:02x?}");
        assert_eq!(
            before.len(),
            1,
            "前提条件: 子フォルダには BATCH の ACE がちょうど1個あるはずです: {before:02x?}"
        );
        assert!(
            is_inherited(before[0].0),
            "前提条件: 子フォルダの BATCH ACE は継承 ACE のはずです: {before:02x?}"
        );

        let outcome = ensure_app_container_access(&child);
        assert!(
            matches!(outcome, AclOutcome::Applied),
            "新規の子フォルダでは Applied を返すはずです: {outcome:?}"
        );

        let after = aces_for_sid(&child, BATCH_SID);
        println!("icacls（付与後）:\n{}", icacls_dump(&child));
        println!("BATCH の (AceFlags, Mask)（付与後）: {after:02x?}");
        assert_eq!(
            after.len(),
            1,
            "継承 ACE が明示 ACE として複製されています（継承構造が壊れます）: {after:02x?}"
        );
        assert!(
            is_inherited(after[0].0),
            "継承 ACE が明示 ACE へ変換されています（親の権限変更が反映されなくなります）: {after:02x?}"
        );

        // 継承構造が生きていることの決定的な確認: 付与の後に親の権限を広げると、
        // その変更が子へ伝播する。ACE が明示化・複製されていた場合、子は親の
        // 変更から切り離されるためこの確認が失敗する（`Issue #45` の指摘の本質）。
        add_inheritable_ace(
            &parent.path,
            BATCH_SID,
            FILE_GENERIC_READ.0 | FILE_GENERIC_EXECUTE.0,
            GRANT_ACCESS,
        );
        let propagated = aces_for_sid(&child, BATCH_SID);
        println!("icacls（親の権限変更後）:\n{}", icacls_dump(&child));
        println!("BATCH の (AceFlags, Mask)（親の権限変更後）: {propagated:02x?}");
        assert!(
            !propagated.is_empty() && propagated.iter().all(|(flags, _)| is_inherited(*flags)),
            "親の権限変更後も、子の BATCH ACE はすべて継承 ACE のはずです: {propagated:02x?}"
        );
        // SetEntriesInAclW が既存 ACE へ併合するか別 ACE を足すかは Win32 の
        // 裁量であるため、個数ではなくマスクの論理和で判定する。
        let propagated_mask = propagated
            .iter()
            .fold(0u32, |merged, (_, mask)| merged | *mask);
        assert_eq!(
            propagated_mask & FILE_GENERIC_EXECUTE.0,
            FILE_GENERIC_EXECUTE.0,
            "親フォルダの権限変更が子へ反映されていません（継承が切れています）: {propagated_mask:08x}"
        );
    }
}
