//! ネイティブファイル選択ダイアログ（`IFileOpenDialog`）。
//!
//! `open_log_file` コマンド（`crate::log_view`）専用です。`SEC-012`（フロント
//! エンドへ任意パスのファイルシステムアクセス権を与えない）に従い、ファイル選択
//! と絶対パスの取り扱いは Rust 側（ここ）で完結させます。フロントエンドへ渡すのは
//! `crate::log_view` が組み立てる表示用ラベル（ファイル名）だけです。
//!
//! # STA と専用スレッド
//!
//! `IFileOpenDialog` は COM の Single-Threaded Apartment（STA）でしか使えません。
//! Tauri のコマンドハンドラがどのスレッド・どの COM アパートメント状態で呼ばれるか
//! 保証できないため、ダイアログ操作のためだけの専用スレッドをそのつど立て、その
//! スレッドの中で `CoInitializeEx(COINIT_APARTMENTTHREADED)` → ダイアログ表示 →
//! 結果回収 → `CoUninitialize` を完結させます（呼び出し元スレッドは
//! `JoinHandle::join` で結果を待つだけで、自身の COM 状態には触れません）。
//! 新しい外部クレートは追加せず、既存の `windows` クレートの feature
//! （`Win32_System_Com`・`Win32_UI_Shell`）だけを使います。
//!
//! # 呼び出し元スレッドと親ウィンドウ（Issue #44）
//!
//! このモジュールの利用者（[`crate::log_view::open_log_file`]）が満たすべき
//! 前提が2つあります。どちらも Issue #44 で実測により確定した制約です。
//!
//! ## 1. 呼び出し元はイベントループスレッドであってはならない
//!
//! Tauri v2 の `#[tauri::command]` は、同期関数のままだと WebView2 の IPC
//! ハンドラから**イベントループスレッド（メインスレッド）上でインライン実行**
//! されます。そのためこの関数を同期コマンドから直接呼ぶと、[`std::thread::JoinHandle::join`]
//! で待っている間ずっとメインウィンドウのメッセージループが止まります。Issue #44
//! の実測では、ダイアログ表示中にメインウィンドウと WebView2 の子ウィンドウが
//! `IsHungAppWindow` で「応答なし」と判定され、`SendMessageTimeout(WM_NULL)` は
//! 失敗し、他の Tauri コマンドの `invoke` も届かなくなりました。
//!
//! そこで [`crate::log_view::open_log_file`] は `#[tauri::command(async)]` を
//! 付け、**同期関数のまま** Tauri のブロッキングスレッドプールで実行させます
//! （`async fn` にはしません。`crate::targets` のモジュール doc コメント
//! 「非同期化の設計」が述べるとおり、`.await` をまたいで `std::sync::MutexGuard`
//! を保持できなくなり、managed state の扱いを作り直すことになるためです）。
//!
//! ## 2. 親ウィンドウの HWND は、呼び出し元が取得して値で渡す
//!
//! [`choose_log_file`] は `owner` を引数で受け取るだけで、自分では取得しません。
//! Tauri の `WebviewWindow::hwnd()` は内部でイベントループへメッセージを送り、
//! 応答をチャネルで待ちます（呼び出し元がメインスレッドの場合だけインラインで
//! 処理されます）。つまり**ダイアログ表示用の専用スレッドの中から `hwnd()` を
//! 呼ぶとイベントループの応答待ちになり、状況によってはデッドロックします**。
//! 呼び出し元はダイアログを開く前に `hwnd()` を済ませ、得た値だけをここへ渡し
//! ます。[`HWND`] は生ポインタのラッパーで `Send` ではないため、専用スレッドへは
//! 数値（`isize`）へ落として渡し、スレッドの中で復元します。
//!
//! ## 親を渡すと何が変わるか
//!
//! `IFileOpenDialog::Show` に親を渡すと、ダイアログはオーナーモーダルになり
//! ます。ダイアログが閉じるまでメインウィンドウは無効化され（`IsWindowEnabled`
//! が `false` になり）、常にメインウィンドウの前面に置かれます。親を取得できな
//! かった場合は `HWND(null)` を渡し、所有者なしで表示します（ダイアログを
//! 開けなくするより、前面・モーダルにならない状態でも開ける方がよい、という
//! 裁定です。`LOG-020`）。

use std::path::PathBuf;
use std::thread;

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    SIGDN_FILESYSPATH,
};

/// ファイル選択の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSelection {
    /// 利用者がファイルを選択した（絶対パス）。
    Selected(PathBuf),
    /// 利用者がダイアログをキャンセルした。呼び出し側はこれをエラーではなく
    /// 「選択なし」の正常応答として扱う。
    Cancelled,
}

/// ダイアログ操作自体が失敗した場合の理由（日本語）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDialogError {
    pub reason: String,
}

impl std::fmt::Display for FileDialogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for FileDialogError {}

fn dialog_error(reason: impl Into<String>) -> FileDialogError {
    FileDialogError {
        reason: reason.into(),
    }
}

/// ログファイル選択用のネイティブダイアログを表示し、選択結果を返します。
///
/// 呼び出し元スレッドをブロックします（専用スレッドの完了を `join` で待つ）。
/// **イベントループスレッドから呼んではいけません**（モジュール doc コメント
/// 「1. 呼び出し元はイベントループスレッドであってはならない」、Issue #44）。
///
/// `owner` にはダイアログの親にするウィンドウの HWND を渡します。呼び出し元が
/// 自分のスレッドで取得済みの値を渡す契約です（同「2. 親ウィンドウの HWND は、
/// 呼び出し元が取得して値で渡す」）。`None` を渡した場合は所有者なしで表示し
/// ます（前面・モーダルにならない従来の挙動）。
pub fn choose_log_file(owner: Option<HWND>) -> Result<FileSelection, FileDialogError> {
    // HWND は生ポインタのラッパーで Send ではないため、そのままでは
    // thread::spawn のクロージャへ入れられない。ダイアログスレッドはこの値を
    // Show の親として渡すだけで、参照解決もメッセージ送出も行わないため、
    // 数値へ落として境界を越え、スレッドの中で復元する（Issue #44）。
    // None（親を取得できなかった場合）は 0 = HWND(null) として扱う。
    let owner_handle = owner.map_or(0_isize, |handle| handle.0 as isize);

    // モジュール doc コメントのとおり、ダイアログ操作専用のスレッドを立てる。
    let handle = thread::Builder::new()
        .name("hakutaku-file-dialog".to_string())
        .spawn(move || run_dialog_on_sta_thread(owner_handle))
        .map_err(|error| {
            dialog_error(format!(
                "ファイル選択ダイアログ用スレッドを起動できません: {error}"
            ))
        })?;

    handle.join().unwrap_or_else(|_| {
        Err(dialog_error(
            "ファイル選択ダイアログのスレッドがパニックしました",
        ))
    })
}

/// 専用スレッド上で実行される本体。COM の初期化・後始末をこの関数の中で完結させる。
///
/// `owner_handle` は [`choose_log_file`] が数値へ落とした親ウィンドウの HWND
/// （0 なら所有者なし）。この関数の中で [`HWND`] へ復元する。
fn run_dialog_on_sta_thread(owner_handle: isize) -> Result<FileSelection, FileDialogError> {
    // SAFETY: この関数はダイアログ操作専用に立てた新規スレッド上でだけ呼ばれ、
    // このスレッドは他の COM 初期化状態を持たない。対応する CoUninitialize を
    // この関数の中で必ず呼ぶ（早期 return しても抜けられるよう、CoInitializeEx
    // が成功した場合だけ実行する）。
    let init_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if init_result.is_err() {
        return Err(dialog_error(format!(
            "COM の初期化に失敗しました（{}）",
            init_result.message()
        )));
    }

    // 0 は HWND(null)（所有者なし）と同じ表現のため、分岐せずそのまま復元できる。
    let result = show_dialog(HWND(owner_handle as *mut std::ffi::c_void));

    // SAFETY: 直前の CoInitializeEx が成功した場合にだけ到達する経路であり、
    // 対応する CoUninitialize を呼ぶ。このスレッドはここで終了するため、以降
    // COM を使わない。
    unsafe { CoUninitialize() };

    result
}

/// ダイアログの作成・表示・結果取得を行う。COM は呼び出し元
/// （[`run_dialog_on_sta_thread`]）が初期化済みである前提。
///
/// `owner` はダイアログの親にするウィンドウ。`HWND(null)` なら所有者なし。
fn show_dialog(owner: HWND) -> Result<FileSelection, FileDialogError> {
    // SAFETY: CLSCTX_INPROC_SERVER での CoCreateInstance は標準的な使い方であり、
    // 返るインターフェースポインタは windows クレートの COM ラッパー型が Drop 時に
    // 自動的に Release する（参照カウントの管理はクレート側に委譲される）。
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }.map_err(
            |error| {
                dialog_error(format!(
                    "ファイル選択ダイアログを作成できません（{}）",
                    error.message()
                ))
            },
        )?;

    // SAFETY: dialog は直前に作成した有効な COM インターフェースである。
    // GetOptions は out 引数を持たず、既定オプションのビットフラグを返すだけの
    // 呼び出しである。
    let options = unsafe { dialog.GetOptions() }.map_err(|error| {
        dialog_error(format!(
            "ダイアログの既定オプションを取得できません（{}）",
            error.message()
        ))
    })?;

    // FOS_FORCEFILESYSTEM: 仮想アイテムを除外し、実在するファイルシステムの
    // パスだけを対象にする。FOS_FILEMUSTEXIST: 存在しないパスの入力を許可しない。
    // 拡張子によるフィルターは設定しない（LOG-020: 任意のローカルログファイルを
    // アドホックに開けることを優先し、拡張子を限定しない）。
    //
    // SAFETY: dialog は有効な COM インターフェースであり、options は直前に
    // 取得したビットフラグ値である。
    unsafe { dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST) }.map_err(
        |error| {
            dialog_error(format!(
                "ダイアログのオプションを設定できません（{}）",
                error.message()
            ))
        },
    )?;

    // 親を渡すとオーナーモーダルになり、閉じるまでメインウィンドウは無効化され、
    // 常にその前面に置かれる（Issue #44。詳細はモジュール doc コメント）。
    //
    // SAFETY: dialog は有効な COM インターフェースである。owner は呼び出し元の
    // スレッドで取得済みのメインウィンドウの HWND か、取得できなかった場合の
    // HWND(null)（所有者なし）のいずれかである。
    let show_result = unsafe { dialog.Show(Some(owner)) };

    if let Err(error) = show_result {
        // 利用者がキャンセルした場合、Show は ERROR_CANCELLED 相当の HRESULT で
        // 失敗する。これは異常系ではなく「選択なし」の正常応答として扱う。
        if error.code() == ERROR_CANCELLED.to_hresult() {
            return Ok(FileSelection::Cancelled);
        }
        return Err(dialog_error(format!(
            "ファイル選択ダイアログの表示に失敗しました（{}）",
            error.message()
        )));
    }

    // SAFETY: dialog は有効な COM インターフェースであり、直前の Show が成功
    // しているため、選択結果を取得できる状態にある。
    let item: IShellItem = unsafe { dialog.GetResult() }.map_err(|error| {
        dialog_error(format!("選択結果を取得できません（{}）", error.message()))
    })?;

    // SAFETY: item は直前に取得した有効な COM インターフェースである。
    // SIGDN_FILESYSPATH は実ファイルシステムパスを要求する。
    let path_ptr = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.map_err(|error| {
        dialog_error(format!(
            "選択したファイルのパスを取得できません（{}）",
            error.message()
        ))
    })?;

    // SAFETY: path_ptr は直前の呼び出しが成功した場合にのみ有効な、
    // CoTaskMemAlloc 済みの NUL 終端ワイド文字列を指す。使用後に必ず
    // CoTaskMemFree で解放する（このブロックの直後）。
    let path_string_result = unsafe { path_ptr.to_string() };

    // SAFETY: path_ptr は上記のとおり CoTaskMemAlloc 済みのメモリを指しており、
    // このプロセス内でこの1箇所だけが解放を担う。
    unsafe { CoTaskMemFree(Some(path_ptr.0.cast())) };

    let path_string = path_string_result.map_err(|error| {
        dialog_error(format!(
            "選択したファイルのパスを UTF-16 として解釈できません（{error}）"
        ))
    })?;

    Ok(FileSelection::Selected(PathBuf::from(path_string)))
}
