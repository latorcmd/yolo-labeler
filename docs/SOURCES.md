# 仕様と資料の確認記録

確認日: 2026-11-16（日本時間）。本ツールはオフラインの学習データ作成用で、大会中のロボットへ情報を送る機能はありません。大会固有の規定を判定する機能もありません。

| 資料 | 確認内容・設計への反映 |
| --- | --- |
| [M5Stack V-Training日本語公式説明](https://docs.m5stack.com/ja/guide/unitv/v_training) の「検出モード」「LabelIMGによる素材のラベル付け」「クラウドへのデータアップロード」 | YOLOラベル、同階層の画像・TXT・クラス数設定、100枚より多い素材、200MB以内のZIP。出力後に再検査する。 |
| ユーザー提供の `datasets environment fail` 通知（2026-11-16時点の会話） | Detectionの例は100枚以上、`classes.txt`・`rename.bat`・`v-training.config`を列挙。公式の「100枚より多い」を採り101枚以上に設定。`rename.bat`の必須性と空ラベルの受理条件は未確認のため、rename.batは追加せず、空枠は提出ZIPでブロック。 |
| `rcj-rescue/docs/rules/RCJRescueMaze2026-final.pdf`（最終更新表記2026-03-29）§3.6.4 p.19、§3.8 pp.21 | 被災者に似た非対象物、照明変動・予期しない照明干渉を想定している。そこからの設計上の判断として、暗さ・ぼけなどは確認候補にし、自動除外しない。クラス名は利用者のデータに合わせる。 |
| [国際Maze公式](https://junior.robocup.org/rcj-rescue-maze/) と [公式フォーラムの告知](https://junior.forum.robocup.org/t/2027-rcj-rescue-maze-final-rules-published/5600) | 2026-11-13の2027最終版公開を確認。プロジェクト指定の2026固定版とは別年度であり、自動的には切り替えない。 |
| [日本公式](https://www.robocupjunior.jp/)、リンク先の [2027尼崎運営指針・ルール案内](https://www.robocupjunior.jp/2027amagasaki_rule.html) | 2027大会の案内。対象大会未指定の今回のツールでは、クラス定義やZIP形式に大会固有の制約を追加しない。 |
| [埼玉公式](https://www.rcjj-saitama.org/)、リンク先の2026年度北埼玉ノード大会概要 | 2026年度と2027年度の案内が併存。対象大会未指定のため、参加資格や適用年度を推測しない。 |
| `C:\Users\lator\Documents\GitHub\UnitV-color\tools\yolo_labeler\label.py` と同README | 移行前の仕様参照として既存検出器、固定クラス・範囲CSV、1画像1対象を確認。現在はRustで実装し、Pythonコードの同梱は終了。元のUnitV-color側のファイルは変更していない。 |

未確認: V-Trainingサーバーでの実際の受付・学習。ZIPを自動アップロードする機能は実装しない。
