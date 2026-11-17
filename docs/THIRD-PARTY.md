# Rust版の依存ライブラリ

AntiWebUI: https://github.com/bea4dev/AntiWebUI

固定コミット: f8035a5f0f7d439d2bebe3479e239c4e6254c626。MIT OR Apache-2.0。
Windows描画に必要なwgpu fork: https://github.com/bea4dev/wgpu 、コミット ff143aeeb126dce521b87f47547565c4b6a6da1e。

画像処理はimage / imageproc、プロジェクトJSONはserde / serde_json、画像UUIDはuuid、ハッシュはsha2、ZIPはzip、ファイル選択はrfdを利用しています。正確な依存バージョンはCargo.lockに固定しています。

Python、OpenCV、学習済みモデルはRust版の配布に含めません。
依存ライブラリのライセンス本文と著作権表記は、配布物のlicensesフォルダーを参照してください。
