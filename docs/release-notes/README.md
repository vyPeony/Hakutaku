# リリースノート

版ごとの配布物、対応する Fixed Version WebView2 Runtime の版（`DIST-016`）、導入手順、既知の制約をまとめた記録です。ファイル名は版番号（`package.json`／`src-tauri/Tauri.toml` の `version` と一致）です。

現在は正式リリース前です。[リポジトリ運用規則の「正式リリース前の方針」](../development/repository-operations.md#正式リリース前の方針)のとおり、リリースブランチ・バージョンタグの発行や成果物の公開は行っていません。ここでの「版」は、その時点の `main` から作った配布物（P12 の段階0検証向けなど）を指します。配布 ZIP の組み立ては [`scripts/package-release.ps1`](../../scripts/package-release.ps1) が行います。

## 一覧

- [0.1.0](0.1.0.md) — P12（配布・検証フェーズ、段階0）向けの配布物。対応 Fixed Version WebView2 Runtime 版 150.0.4078.105
