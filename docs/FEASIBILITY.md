# 未実装機能とGUI移行の検討

この文書は移行前の検討記録です。現在はRust版のみを保守し、本文中で比較対象として挙げたPythonコード・テスト・起動環境は削除済みです。

**2026-11-17更新:** AntiWebUI＋Rust版を実装しました。保存は `.ylp2`、旧 `.ylproject` 互換は不要という最新の決定を反映しています。実装内容は [RUST-EDITION.md](RUST-EDITION.md)、確認結果は [RUST-VALIDATION.md](RUST-VALIDATION.md) を参照してください。以下は主に移行前の比較検討記録です。

確認日: 2026-11-16（日本時間）。引き継ぎ資料は候補一覧として扱い、現行コードと公式資料を確認した。以下の難易度と採用順は設計上の見積もり。未実装機能の試作品や認識精度の測定結果ではない。競技ルール・提出ZIP仕様は変更しない。

## 結論

GUI・画像処理・保存を含め、Pythonを使わない構成へ移すことは技術的に可能。ユーザーの補足により、既存Pythonの再利用は必須条件から外す。従来のコードは動作仕様と比較用の参照実装に使える。GUIとは独立したRustライブラリへ処理と状態管理をまとめれば、AntiWebUI / GPUI / Tauriで共用できる。

再評価後は、ユーザーの好みを含め **Rust共通処理 + AntiWebUIを最初に試す構成** とする。実用性を重視する代案は **同じRust共通処理 + TauriのWeb GUI**。どちらもPython実行環境やPythonワーカーは不要にできる。AntiWebUIの本採用は画像編集・日本語入力・Windows配布の試作が通ることを条件とする。GPU UIであることだけで速度や画像処理性能の優位を断定しない。

## 未実装機能の難易度

低: 既存処理とUI接続が中心。中: 新しい状態・アルゴリズム・複数画面の整合性が必要。高: 教師データ・評価実験、または処理/保存方式の再設計が必要。所要日数はGUI移行範囲と要求精度が未確定なので断定しない。

この表の難易度は現在のPython版を基準にした機能追加の評価。Rust版では後述の共通基盤の移植費用が別にかかる。Rustへ変えるだけで分類・学習・評価データに関する難しさが下がるわけではない。

| 機能 | 難易度 | コードの根拠・主な作業 | 完了を判断する検証 |
|---|---|---|---|
| 画像追加 | 中・今回実装 | `importing.import_images`、`Application.start_image_import`。既存状態保護、コピー公開、中止、重複パス管理 | 同名別画像、再追加、クラス順違い、コピー失敗、中止、保存再開、既存Undoを自動検証 |
| 品質しきい値GUI | 低〜中 | `vision.quality_for_image` / `analyze_quality` にぼけ引数あり。暗さ・明るさ等は固定値。設定保存、再検査、指標説明を追加 | 変更前後の候補枚数、既存判断保護、異なる解像度で比較。適切なしきい値は実データ評価が必要 |
| 半自動の複数画像比較・プリセット | 低〜中 | `AutoDialog` / `autolabel.propose_one` を再利用。画像切替、設定A/B、プリセット永続化 | 同一画像・同一条件で候補一致、切替後の古い結果混入防止 |
| 近似重複・連写グループ | 中 | `vision.quality_for_image` が64ビットdHashを保存。距離比較、グループ、代表選択UIを追加 | 同一セッション/異なる記号の誤結合、連鎖的なグループ肥大化、代表の保護。dHashが近いだけで除外しない |
| 撮影日・セッション整理 | 低〜中 | `Sample` に撮影情報なし。EXIF時刻、ファイル時刻、手動セッションを区別して保存・絞込 | EXIFなし/不正時刻/複数カメラ。同時刻を同一撮影と断定しない。画像のEXIF回転は変更しない |
| モデルとラベルの不一致検査 | 高 | `vision.import_proposals` は外部TXT入力のみ。ローカル推論アダプター、クラス対応、IoU割当、見逃し/余分な枠の比較を新設 | 未知クラス、複数枠の1対1対応、推論誤りと正解ラベル誤りの区別。既存ラベルの自動置換を避ける |
| 複数対象の自動枠生成 | 高 | `vendor.select_components` は最大成分から近傍を結合して単一Box、`Detection.box` も単一。単に戻り値を配列化しても記号内部の分割と別対象の区別は解決しない | 離れた対象、近接対象、分断された1文字、影/背景の誤検出。vendorを保持して別検出器が望ましい |
| 記号H/S/U等の自動分類 | 高 | `assign_classes` は固定/CSV指定のみ。切出し分類器か検出モデル、未知/低信頼の扱いが必要 | セッション単位の未使用データで精度・混同行列。良い枠が作れることと分類できることを分ける |
| 手動の残す/捨てるを学ぶ | 高 | `Application.changed` に判断履歴、品質特徴あり。ただし判断理由が混在し学習ラベルとして不十分 | 明示的な理由収集、固定評価、誤除外率、難例を残す率。まず並び順の提案に限定 |
| 長時間処理の途中再開 | 中〜高 | `propose_batch` / `analyze_quality` は全体の結果を返す方式。画像単位の結果保存・再開キーを新設 | 画像hash・クラス順・検出設定変更で古い結果を無効化、強制終了と部分書込から復旧 |
| 履歴閲覧 | 中 | `audit_log` はあるので閲覧・検索は可能。ただし設定変更や候補の全状態を記録してはいない | 操作と表示の対応、個人パスの扱い、履歴増大 |
| 任意時点復元・再起動後Undo/Redo | 高 | `Application.checkpoint` は起動中最大40件。完全なコマンド履歴/スナップショット方式へ変更 | クラス変更・候補承認・追加画像を含む復元、再起動後の一致。現状のログを逆適用するだけでは不足 |
| 固定評価セット・性能比較 | 高 | 現在のテストはデータ整合性とGUI動作であり、学習精度を測らない。グループ分割、学習条件固定、比較出力が必要 | 近似画像のtrain/test混入防止、同条件の学習、再現性。評価セット分割ツールだけなら中 |

追加候補: 通常YOLO出力は中（相対パス/クラス/既存ファイル保護）、隔離コピーは低〜中、完全削除は復元・対象確認を含め中〜高、EXE/インストーラーは中（Python/OpenCV同梱とクリーンWindows検証）、クラス統合/削除は高（boxes/proposals/Undo/履歴/CSVのID整合性）。いずれも今回の実装対象外。

推奨順は画像追加 → 複数画像プレビュー/しきい値調整 → 近似重複グループ → 評価セット基盤 → 学習型機能。GUI移行を先に行うなら、機能をTk画面へ大量に追加する前に処理APIを分離する。

## フレームワーク比較

公式資料上の仕様と、このアプリへの適性の推論を分ける。速度・メモリ・配布サイズは未測定。

| 候補 | 公式資料で確認できた構造 | このアプリへの判断 | 移行難易度 |
|---|---|---|---|
| Tauri 2 | OSのWebViewを利用、HTML/JS/CSSの画面、Rust側 | Rust側へ画像処理・保存・検出を実装。10枚一覧・フィルターはWeb UI、枠編集はCanvas/SVG。UIとRustの境界は残る。Python不要 | 中〜高・実用性を重視する代案 |
| Electron | Chromiumの複数プロセス構造、Nodeのmain、Webのrenderer、preload/IPC | TypeScript + sharpで画像入出力・縮小が可能。独自検出はworker等で実装、またはRust native addon/実行ファイルを利用。Python不要。Chromiumとnative依存の配布を考慮 | 中〜高・Web開発を優先する場合 |
| GPUI | RustのGPU UI。公式READMEはpre-1.0で破壊的変更あり。WindowsはWin32/DirectWrite | Rust共通処理を直接呼べる。GUI・画像処理の言語を統一できる。独自編集UIの実装とAPI変更への追従は必要。今回GPUIがAntiWebUIより適するという実測根拠はない | 高・Rustネイティブの比較候補 |
| AntiWebUI | Rust + 独自wgpu描画、WebViewなし。signalによる更新、Windows Win32/IMM32、widgets群、0.1.0 workspace | Rust共通処理を直接呼び、Web用通信やPython管理を省ける。好みに合うため最初の試作候補。独自編集・IME・配布を実証して本採用を判断 | 高・今回の優先試作候補 |

どの候補でも画像処理の言語は選べる。AntiWebUI/GPUIを選ぶために高度な画像処理まで自作する必要はない。Rustの既存ライブラリ、必要ならOpenCVのRustバインディングを使える。ただしRust版の検証はどのGUIでも必要な共通作業であり、その負担をAntiWebUIだけの欠点として数えない。

## Pythonを使わない画像処理の実現性

| 現在の処理 | Rustでの候補・実装 | 移植の難しさと確認点 |
|---|---|---|
| Pillowの読込・RGB化・縮小・JPEG出力 | `image` に入出力・画像操作・JPEG encoderあり | 中。JPEG復号差、透過画像のRGB化、EXIF回転、色空間、補間、サブサンプリングを確認。JPEGバイト列の一致ではなく出力仕様を検証 |
| 黒つぶれ/白飛び/平均輝度/小対象 | 画素配列の集計とBoxの寸法計算 | 低〜中。しきい値の境界、グレースケール変換係数と丸めを固定 |
| ぼけ指標・dHash | `image` + フィルター/配列処理、必要なら`imageproc` | 中。現在のINTER_AREA縮小、Laplacianの境界処理・型・分散の定義に合わせるか、アルゴリズム版を変えて再校正 |
| 大津二値化 | `imageproc::contrast::otsu_level` + 二値化、または互換実装 | 中。同点時のしきい値選択、反転二値化でしきい値と等しい画素をどちらに入れるかを比較 |
| 連結成分と近接領域結合 | `imageproc` の8近傍連結成分 + 面積/外接矩形集計 + 現行の結合手順 | 中。面積同点の順序、近接判定、最小面積の切上げ、paddingと画像端の制限を比較 |
| HSV補助抽出 | 現在はV成分のみなので、8bit RGB各画素の最大値と60を比較する処理へ置換可能 | 低〜中。RGB復号/変換条件とfallback発動条件は比較が必要。HSV全機能を移植する必要はない |
| YOLO解析・クラス対応・選別・候補保護 | Rustの型とコマンド層へ移植 | 中。NaN/無限大、丸め許容、未承認状態、CSV境界、旧プロジェクト読込を維持 |
| ファイル追加・保存・hash・ZIP | RustのファイルAPIとJSON/hash/ZIPライブラリを選定 | 中。保存の一時ファイル/backup、Windowsの置換失敗、キャンセル、元データ保護、ZIP再検査が重要。cratesの具体版は実装時に選定 |
| 将来の学習済みモデル推論 | `ort` 等でONNX Runtimeへ接続する候補 | 中〜高。学習済みモデルのONNX書出し可否、前処理/後処理、対象演算、実行backendを確認。Pythonを使わなくてもデータ不足や精度検証は解消しない |

現状は学習モデルの学習・推論を実装しておらず、NumPyの使用も配列変換と基本的な集計が中心。Python特有の巨大な学習基盤を移植する仕事ではない。画像処理移植の基礎部分は中程度と評価する。GUI/履歴/配布を含むアプリ全体の作り直しは依然中〜高で、短期間で全部移せるとまでは言わない。

画像処理の選択は2案ある。**長期の配布を単純にしたいならimage/imageproc中心**、**現行OpenCVとの意味的一致を優先するならRust + OpenCV**。後者もPythonは不要だが、OpenCV/Clang等のビルド環境・native libraryの配布は必要。OpenCVを使えば完全一致するわけではなく、同じバージョン・入力画素・設定で比較する必要がある。現段階ではimage/imageproc中心に試作し、互換性差が大きい処理だけOpenCV採用を再検討する。

現在の品質キャッシュはOpenCV由来なので、別実装で再計算する場合は `quality_algorithm_version` のような識別子を保存して新旧の値を混ぜない。候補の方式/設定も保存する。新実装で完全一致を目指す部分と、精度を比較して変更を許容する部分を明示する。

## 移行時の共通構成案

`labeler-core`（仮称のRust crate）にプロジェクト状態・YOLO・追加・保存・履歴・ZIP、`labeler-vision`に画像処理/検出を置く。AntiWebUI/GPUIはこれらを直接利用し、Tauriはcommand経由で利用。Electron採用時はTypeScript処理を中心にするかRustへの接続が別途必要。GUI描画のGPU利用と画像処理のGPU利用は別で、初期版はCPUワーカーで画像処理を走らせる設計が妥当。

Rust coreをプロジェクト状態の所有者とし、`load_project` / `import_images` / `edit_boxes` / `set_decision` / `propose` / `quality` / `export` をコマンド化。クラス検証、Undo、候補保護、保存は画面から分離する。ワーカーは入力snapshotと要求ID/プロジェクト版を受け、進捗・結果をchannelで返す。古い結果の適用とUIスレッドでの重い処理を防ぐ。外部サービスやPythonとのIPCは必須ではない。Tauriでも画像全体をBase64で往復させず、サムネイル/管理された画像経路を使う。

段階移行は、(1) 小さな互換fixtureと既存実装の比較結果を保存、(2) Rust coreと画像処理を実装、(3) AntiWebUIの編集画面で結合試験、(4) 新形式.ylp2で保存・再開を確認（旧.ylprojectの互換読込は行わない）、(5) Windows配布確認の順。移行期間中だけPython版を比較用に使っても、新アプリの配布依存にはしない。既存32テストが通った事実はRust版の保証にはならず、テストケースをRustへ移し直す。

移植時の注意: 今回の画像追加では同名ファイル保護のため相対パスに取込IDを加える。範囲CSVは元のファイル名の数字を使えるが、外部YOLO候補読込は現在の相対パスに対応する必要がある。GUI移行時に永続的な画像IDと元パス対応を整理したい。

## AntiWebUIを選ぶための具体的な試作条件

1. 10枚一覧、スクロール/ページ送り、1,818枚のメタデータを保持しつつ画像を遅延読込。使用メモリ・初回表示・切替時間を測る。
2. 1枚の編集画面で枠の作成/移動/四隅リサイズ/削除/Undoを実装。ズーム/パン後も元画像の正規化座標が一致することを検証。
3. Windows 100/125/150/200% DPI、日本語パス、IME、ダイアログ、ドラッグがウィンドウ外へ出たときの挙動を実機確認。
4. Rust画像処理ワーカーへ1枚要求し、進捗/中止/終了/エラーを確認。既存Python版との枠・品質指標の比較と、UIを待たせないことを検証。
5. Rust/Python未導入の別Windows環境へ配布して起動・読込・保存を確認。依存コミットを固定する。

これが通ればAntiWebUIを本採用する根拠になる。未達なら同じRust coreを使ってTauriへ移れるため、画像処理を再度作り直さずに済む。ただしGUI部分の書き換えは必要。検討時点ではcargoが未導入だったが、2026-11-17にRust MSVCを導入しAntiWebUI版のWindowsビルド・起動を確認した。検証範囲と未確認事項はRUST-VALIDATION.mdに記録する。

## 検証と資料

今回、既存24件に画像追加8件を加えた32件のunittestが成功。生成画像を使ったTkテストを含む。追加コピー・ラベル対応・保存再開・中止/失敗回収・既存Undoを検証した。人による画面目視、実データの大量追加、各候補GUIのビルド/性能、認識精度、V-Trainingサーバーでの学習成功は未確認。

- [Tauri概要](https://v2.tauri.app/start/)
- [Tauri sidecar](https://v2.tauri.app/develop/sidecar/)
- [Electron process model](https://www.electronjs.org/docs/latest/tutorial/process-model)
- [GPUI公式](https://gpui.rs/)、[GPUI README](https://github.com/zed-industries/zed/tree/main/crates/gpui)
- [AntiWebUI](https://github.com/bea4dev/AntiWebUI)、[README](https://raw.githubusercontent.com/bea4dev/AntiWebUI/main/README.md)、[Cargo.toml](https://raw.githubusercontent.com/bea4dev/AntiWebUI/main/Cargo.toml)
- [Tauri architecture](https://v2.tauri.app/concept/architecture/)
- [image](https://docs.rs/image/latest/image/)、[imageproc](https://docs.rs/imageproc/latest/imageproc/)、[Otsu等](https://docs.rs/imageproc/latest/imageproc/contrast/index.html)、[連結成分](https://docs.rs/imageproc/latest/imageproc/region_labelling/fn.connected_components.html)
- [opencv-rust](https://github.com/twistedfall/opencv-rust)、[インストール条件](https://github.com/twistedfall/opencv-rust/blob/master/INSTALL.md)
- [sharp](https://sharp.pixelplumbing.com/)、[ort](https://docs.rs/ort/latest/ort/)

Web取得結果にはキャッシュの時刻差があり得る。移行開始時に依存バージョン/コミットとWindows向けAPIを再確認する。
