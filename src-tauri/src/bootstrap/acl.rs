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
//! - 不足していれば ACE を追加する（[`AclOutcome::Applied`]）。
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
    EqualSid, GetAce, MapGenericMask, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CONTAINER_INHERIT_ACE,
    DACL_SECURITY_INFORMATION, GENERIC_MAPPING, INHERIT_ONLY_ACE, OBJECT_INHERIT_ACE,
    PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

/// App Container（ALL APPLICATION PACKAGES）を表す文字列 SID。
const APP_CONTAINER_SID: &str = "S-1-15-2-1";

/// `WebView2Runtime` フォルダの ACL 確認・設定結果。
#[derive(Clone, Debug)]
pub enum AclOutcome {
    /// 既に App Container からアクセスできる。変更していない。
    AlreadyAccessible,
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

    match dacl_already_grants_access(dacl_ptr, target_sid) {
        Ok(true) => return AclOutcome::AlreadyAccessible,
        Ok(false) => {}
        Err(reason) => return AclOutcome::Undetermined { reason },
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
    let sid_wide = to_wide_null(APP_CONTAINER_SID);
    let mut sid_out = PSID(std::ptr::null_mut());

    // SAFETY: sid_wide はこの関数のスコープ内で生存する NUL 終端バッファであり、
    // sid_out は ConvertStringSidToSidW が書き込む出力専用の変数である。
    let result = unsafe { ConvertStringSidToSidW(PCWSTR(sid_wide.as_ptr()), &mut sid_out) };

    match result {
        Ok(()) => Ok(LocalAllocGuard(sid_out.0)),
        Err(error) => Err(format!(
            "App Container 用 SID（{APP_CONTAINER_SID}）を作成できません（{}）。",
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

/// 対象の DACL に、App Container 用 SID への「読み取り + 実行、継承あり」の
/// アクセス許可が既に含まれているかを確認する。
fn dacl_already_grants_access(dacl: *const ACL, target: PSID) -> Result<bool, String> {
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
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
            // 拒否 ACE や監査 ACE など、許可 ACE 以外は対象にしない。
            continue;
        }

        if !ace_applies_to_object_itself(header.AceFlags) {
            // INHERIT_ONLY_ACE は子オブジェクトへの継承だけが目的であり、
            // フォルダ自身には権限を与えない。
            continue;
        }

        // SAFETY: AceType が ACCESS_ALLOWED_ACE_TYPE であることを確認したため、
        // このポインタは ACCESS_ALLOWED_ACE として読み取れる。
        let ace = unsafe { &*(ace_ptr as *const ACCESS_ALLOWED_ACE) };

        if !access_mask_grants_required(ace.Mask) {
            continue;
        }
        if !ace_flags_have_required_inheritance(header.AceFlags) {
            continue;
        }

        // 注: SidStart は ACCESS_ALLOWED_ACE 構造体に続く可変長 SID 領域の先頭を
        // 指すフィールドであり、そのアドレスは有効な PSID として扱える
        // （Win32 の標準的な取り扱い）。ポインタの構築自体はアドレス取得と
        // 型変換だけであり、参照外し（デリファレンス）を伴わないため unsafe は不要。
        let ace_sid = PSID(&ace.SidStart as *const u32 as *mut c_void);

        // SAFETY: ace_sid は上で得た有効な SID、target は呼び出し元が保持する
        // 有効な SID である。
        if unsafe { EqualSid(ace_sid, target) }.is_ok() {
            return Ok(true);
        }
    }

    Ok(false)
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
}
