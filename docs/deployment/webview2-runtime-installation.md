# WebView2 Runtime 導入手順書

- 状態: 採用済み（Accepted）
- 管理責任者: リポジトリメンテナー
- 最終更新日: 2026-08-18

## 目的

この文書は、Hakutaku を対象端末へ導入する担当者（以下「導入担当者」）向けに、WebView2 Runtime を事前確認・配置する手順をまとめたものです。Hakutaku は Microsoft Edge WebView2 Runtime がなければ起動できません。特に、Evergreen WebView2 Runtime（OS へインストールする通常の Runtime）が導入されていない端末では、Runtime 追加パッケージをあらかじめ配置しておくことが起動の前提条件になります（`DIST-012`）。Hakutaku はこの Runtime をネットワークから自動取得しません。起動できなかった場合に表示されるダイアログはその場での案内にしかならないため、導入作業の前にこの手順書で確認してください。

この文書は導入担当者向けであり、Hakutaku のソースコードやビルド環境を前提にしません。WebView2 Runtime の解決の仕組み、設定キー、開発時の配置手順など、開発者向けの内容は[WebView2 Runtime の解決経路と開発環境での扱い](../development/webview2-runtime.md)を正本とし、本書では扱いません。

## 全体の流れ

1. 対象端末に Evergreen WebView2 Runtime が導入済みかどうかを確認する
2. 導入済みなら本体 ZIP だけを展開する（形態A）
3. 未導入なら本体 ZIP に加えて Runtime 追加パッケージを取得・配置する（形態B）
4. `Hakutaku.exe` を実行し、起動を確認する

配布物の構成と導入手順の要約は[リリースノート 0.1.0](../release-notes/0.1.0.md)の「導入手順」節にもあります。本書はこれと矛盾しない範囲で、事前確認の具体的な操作、フォルダの取り違え防止、責任範囲をより詳しく扱います。

## 手順1: Evergreen WebView2 Runtime の導入状況を確認する

対象端末によって Evergreen WebView2 Runtime の導入状況は異なります。次のいずれかの方法で、導入済みかどうかを確認してください。

### 平易な方法（推奨）

- **Windows 11:** 「設定」アプリを開き、「アプリ」→「インストールされているアプリ」の一覧を「webview2」で検索します。「Microsoft Edge WebView2 Runtime」が表示されれば導入済みです。
- **Windows 10:** 「設定」アプリを開き、「アプリ」→「アプリと機能」の一覧を「webview2」で検索します。表示されない場合は、コントロールパネルの「プログラムと機能」でも同じ一覧を確認できます。

一覧に表示された項目のバージョン欄で、導入済みの版も確認できます。

### より正確な方法（バージョンをスクリプト等で確認したい場合）

レジストリで Evergreen WebView2 Runtime の版を直接確認する方法もあります。手順と参照するキーは[WebView2 Runtime の解決経路と開発環境での扱いの「段階0で確認できないこと」](../development/webview2-runtime.md#段階0で確認できないこと)に実例があります。この方法は、複数端末へ同じ手順を展開する場合や、一覧表示だけでは版を特定しづらい場合に使ってください。

いずれの方法でも確認できない、または一覧に該当項目がない場合は、Evergreen WebView2 Runtime は未導入として扱い、次の「手順2」の形態Bへ進んでください。

## 手順2: 配布物を展開する

配布物は次の2種類の ZIP です。どちらも配布担当者から入手してください（現在は正式リリース前のため、公開の配布ページはありません）。

| ファイル | 内容 | 必要な場合 |
| --- | --- | --- |
| `Hakutaku-<版>.zip` | 本体一式（`Hakutaku.exe`、`hakutaku.yaml.sample`、リリースノート） | 常に必要 |
| `Hakutaku-WebView2Runtime-<Runtime版>.zip` | Fixed Version WebView2 Runtime 一式（任意追加パッケージ） | Evergreen WebView2 Runtime が未導入の端末だけ必要 |

### 形態A: Evergreen WebView2 Runtime が導入済みの端末

1. `Hakutaku-<版>.zip` を、書き込み権限のあるローカルフォルダへ展開します。
2. できた `Hakutaku` フォルダをそのまま実行可能な場所へ配置します。
3. `Hakutaku.exe` を実行します。導入済みの Evergreen WebView2 Runtime が自動的に使用されます。

Runtime 追加パッケージは不要です。

### 形態B: Evergreen WebView2 Runtime が未導入の端末

1. `Hakutaku-<版>.zip` を、書き込み権限のあるローカルフォルダへ展開します。
2. `Hakutaku-WebView2Runtime-<Runtime版>.zip` を、別の場所（デスクトップや一時フォルダなど）へいったん展開します。
3. 展開してできた `WebView2Runtime` フォルダを、1. で展開した `Hakutaku` フォルダの直下へ、フォルダごと移動またはコピーします。結果として次の配置になるようにしてください。

   ```
   Hakutaku\
   ├─ Hakutaku.exe
   ├─ hakutaku.yaml.sample
   ├─ release-notes.md
   └─ WebView2Runtime\
      ├─ msedgewebview2.exe
      └─ （その他の Runtime 構成ファイル）
   ```

4. `WebView2Runtime` フォルダを開き、`msedgewebview2.exe` がそのフォルダの直下にあることを確認します。`WebView2Runtime\WebView2Runtime\msedgewebview2.exe` のように、もう一段フォルダが入れ子になっている場合は配置が誤っています。内側の `WebView2Runtime` フォルダの中身を一段外へ出し、`msedgewebview2.exe` が `WebView2Runtime` の直下に来るよう配置し直してください（この配置をここでは「平坦化」と呼びます）。
5. `Hakutaku.exe` を実行します。Evergreen WebView2 Runtime が見つからない場合、この `WebView2Runtime`（Fixed Version Runtime）が自動的に使用されます。

エクスプローラーの「すべて展開」は、既定で ZIP 名と同じ名前のフォルダを作ってその中に展開するため、そのまま `Hakutaku` フォルダの直下へ展開すると余分な階層ができます。手順2〜4のとおり、いったん別の場所へ展開してから `WebView2Runtime` フォルダだけを移動・確認する進め方を推奨します。

## `WebView2` と `WebView2Runtime` の違い

名前が似ているため取り違えやすい、2つのフォルダです。`Hakutaku.exe` と同じフォルダの直下に、どちらも作られる・置かれることがあります。

| フォルダ名 | 役割 | 用意する人 |
| --- | --- | --- |
| `WebView2` | WebView2 の**ユーザーデータ**（閲覧・実行状態、キャッシュなど）の保存先。Hakutaku が起動時に自動生成します。 | Hakutaku 自身（導入担当者が用意する必要はありません） |
| `WebView2Runtime` | **Fixed Version Runtime 本体**（`msedgewebview2.exe` など）の配置先。Evergreen WebView2 Runtime が未導入の端末でだけ必要です。 | 導入担当者（本書の「形態B」の手順で配置します） |

`WebView2Runtime` フォルダを削除したり、`WebView2` フォルダへ Runtime 追加パッケージの中身を展開したりしないよう注意してください。起動できなかった場合に表示されるダイアログにも、この2つの違いが明記されます（詳細は「手順3」を参照）。

## Runtime の版について

- 現在の配布物が対象とする Fixed Version WebView2 Runtime の版は **150.0.4078.105（x64）** です（`DIST-016`）。この版は Hakutaku のビルドごとに固定されており、動作確認もこの版で行っています。
- 任意の別の版へ差し替えた場合の動作は保証されません。`WebView2Runtime` フォルダの中身を、配布された `Hakutaku-WebView2Runtime-<Runtime版>.zip` 以外のものへ差し替えないでください。
- Fixed Version Runtime は自動更新されません。Runtime 側の修正（セキュリティ更新を含む）を取り込みたい場合は、新しい版の `Hakutaku-WebView2Runtime-<新しい版>.zip` を配布担当者から再度入手し、`WebView2Runtime` フォルダ一式を丸ごと置き換える必要があります（`DIST-011`）。差分だけを上書きする更新はできません。

対応する Fixed Version WebView2 Runtime の版は、導入する Hakutaku の版ごとに[リリースノート](../release-notes/README.md)で確認してください。

## 手順3: 起動を確認する

`Hakutaku.exe` を実行して画面が表示されれば、WebView2 Runtime の解決は成功しています。

起動できなかった場合は、ネイティブのダイアログ（Windows 標準のメッセージボックス）が表示され、見つからなかった Runtime の種類と、Fixed Version Runtime を配置すべきフォルダの絶対パスが案内されます。このダイアログは、`WebView2Runtime`（Runtime の配置先）と `WebView2`（ユーザーデータの保存先）を取り違えないよう、両者の役割の違いも明記します。ダイアログの案内に従い、本書の「手順2」の形態Bを再確認したうえで `Hakutaku.exe` を再起動してください。

Hakutaku は WebView2 Runtime をネットワークから自動取得しません。ダイアログが表示された場合は、配布担当者から Runtime 追加パッケージを入手し、手動で配置する必要があります。

## 導入したデータの取り扱いと後片付け

- Hakutaku が実行時に作成・書き込みするフォルダは、`Hakutaku.exe` と同じフォルダ直下の `logs`、`temp`、`WebView2` に限られます。`%LOCALAPPDATA%` などユーザープロファイル配下へは書き込みません。**導入フォルダ（`Hakutaku` フォルダ）ごと退避・削除すれば、Hakutaku が残したデータをすべて処分できます。**
- **`logs`、`temp`、`WebView2`、`WebView2Runtime` の各フォルダを、別の場所を指すリンク（シンボリックリンクやジャンクション）へ置き換える運用はできません。** リンクにすると書き込みがリンク先へ抜け、導入フォルダごと削除してもデータが残るため、Hakutaku は起動時にこれを検出し、対象フォルダの絶対パスと対処をダイアログで案内して起動を中止します（`SEC-009`）。データの保存先を別のドライブへ置きたい場合は、フォルダ単位のリンクではなく、導入フォルダ全体をそのドライブへ移動してください。
- 診断ログ（`logs` フォルダ）には、個人情報等の機密データ、認証情報、参照元のフルパスなどの実値が、マスキングされずに平文で記録され得ます。この方針の採否と経緯は[ADR-0010: 診断ログは実値をマスキングせず出力し、`logs` フォルダの管理は利用者・導入組織の責任とする](../architecture/decisions/0010-plaintext-diagnostic-logs.md)を参照してください。Hakutaku 自身はマスキング、暗号化、アクセス制御を行いません。`logs` フォルダの閲覧制限、持ち出し、保管、削除は、利用者または導入組織の責任範囲です。導入時点で、`logs` フォルダへのアクセス権限を対象端末の運用方針に合わせて設定することを推奨します。
- 同様に、`WebView2` フォルダ（ユーザーデータ）にも WebView2 が保持する描画・実行状態が残り得ます。内容は個人情報等の機密データを含み得るものとして扱い、`logs` と同じく閲覧制限・持ち出し・保管・削除は利用者または導入組織の責任範囲です。
- `WebView2Runtime` フォルダは導入担当者が配置した Runtime 本体であり、Hakutaku の利用者データではありません。導入フォルダを削除する際は、`WebView2Runtime` を含めフォルダごと削除して構いません。

## 参照

- [リリースノート 0.1.0](../release-notes/0.1.0.md) — 配布物の構成と導入手順の要約
- [エラーコード一覧（導入担当者向け）](error-codes.md) — 起動できなかった場合に診断ログの `code=` 欄へ記録されるエラーコードの意味と対処
- [WebView2 Runtime の解決経路と開発環境での扱い](../development/webview2-runtime.md) — Runtime の3つの解決経路、開発者向けの詳細（本書の対象外）
- [Windows ビルド互換性と依存追加時の確認](../development/windows-build-compatibility.md) — 対応 OS の下限と、固定している Runtime 版の入手元
- [データ取り扱いとセキュリティ](../security/data-handling.md) — 診断ログ・WebView2 データの責任範囲（`SEC-005`、`SEC-009`、`SEC-010`）の正本
- [ADR-0010: 診断ログは実値をマスキングせず出力し、`logs` フォルダの管理は利用者・導入組織の責任とする](../architecture/decisions/0010-plaintext-diagnostic-logs.md)
