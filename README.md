# YOLO Labeler

AntiWebUIとRustで作ったWindows x64向けの画像ラベラーです。Python・OpenCVのインストールは不要です。

画像・フォルダーの追加、10枚ずつの選別、枠の作成・移動・リサイズ、Undo、半自動候補、品質・完全重複検査、V-Training Detection ZIP出力に対応します。ライト／ダークはWindowsのアプリ設定に自動で追従します。

## 起動

配布ZIPを展開して `yolo-labeler.exe` を実行します。リポジトリでは `launch.bat`（`launch-rust.bat`も使用可）から起動できます。

保存形式は **`.ylp2`** です。同名の **`.ylp2.assets`** フォルダーと一緒に保管・移動してください。旧 `.ylproject` との互換はありません。

## 開発

Windows x64、Rust stable MSVC、Visual Studio C++ Build ToolsとWindows SDKが必要です。

```powershell
./build-rust.ps1
cargo test --workspace --locked
```

配布物は `dist/yolo-labeler-windows-x64.zip` に生成します。独自の画像・選択枠アイコンはEXEに組み込みます。再生成は `tools/create-icon.ps1` で行えます。

- [使い方・保存と復旧](docs/RUST-EDITION.md)
- [検証記録と未確認事項](docs/RUST-VALIDATION.md)
- [依存ライブラリ](docs/THIRD-PARTY.md)
- [仕様の出典](docs/SOURCES.md)
- [移行前の比較検討記録](docs/FEASIBILITY.md)

Python版のコード・依存設定・テスト・比較スクリプトは削除しました。移行時の比較結果は検証記録に残しています。画像・プロジェクト・出力ZIPはGitへ含めません。
