use super::*;

pub const LIST_KEYS: &str = "クリック / 1〜0: 採用と除外を切替   ← A / → D Enter: ページ移動（左右・A/D・前後ボタン長押しで早送り）   ホイール: 前後ページ";
pub const EDIT_KEYS: &str = "V: 選択   W: 枠を描く   P: パン   Delete: 枠を削除   ホイール: ズーム\nCtrl+Z: 編集Undo   Ctrl+Enter: 保存して採用   Esc: キャンセル";
pub const COMMON_KEYS: &str =
    "Ctrl+N: 新規   Ctrl+O: 開く   Ctrl+S: 保存   Ctrl+Z: Undo   F1: ヘルプ";

pub fn open() {
    open_window(
        WindowOptions {
            title: "使い方・キーボード操作".into(),
            width: 850,
            height: 680,
            modal: true,
            dialog: true,
            ..Default::default()
        },
        || {
            theme::follow_system();
            content()
        },
    );
}

pub fn content() -> Div {
    keyboard_surface(div()).flex().flex_col().gap(14_f32).p(24_f32).h_full().overflow_y_scroll()
            .dyn_style(|s| s.bg(theme().page).text_color(theme().text))
            .child(heading("画像を追加 → 不要な画像を除外 → ZIP出力"))
            .child(text("正常なラベルを持つ画像は初めから採用です。不要な画像だけクリックすると除外します。もう一度クリックすると採用へ戻せます。元画像は消しません。変更は自動保存します。"))
            .child(heading("画像の追加と背景"))
            .child(text("追加ごとに対象あり／なしを指定します。対象ありでは、既存TXTを使うか、全画像を大津法で自動検出するかを選べます。既存TXTを使うと、枠がない画像だけ自動検出します。対象なしは背景画像になります。H・S・U等の種類を画像から推測する機能はありません。"))
            .child(text("自動枠はそのまま採用しますが、正しさは画像で確認してください。検出失敗や不正ラベルは要確認です。ラベル表示名でギリシャ文字などへ表示だけ変更でき、出力するID・クラス名は変わりません。"))
            .child(heading("一覧")) .child(text(LIST_KEYS))
            .child(text("カードの「選択」または「このページを全選択」で対象を選びます。ページをまたいで選択でき、背景設定・クラス変更・枠削除と大津法での再検出・プロジェクトからの画像削除を一括で実行できます。Ctrl+Zでまとめて戻せます。"))
            .child(heading("編集")) .child(text(EDIT_KEYS))
            .child(text("枠をドラッグして移動、四隅をドラッグしてリサイズします。対象なしを指定すると背景として保存します。枠を作ると対象ありに戻ります。"))
            .child(heading("共通")) .child(text(COMMON_KEYS))
            .child(heading("出力と保存"))
            .child(text("ZIP出力の「加工画像を追加」で、学習用の加工画像を加えられます。回転は±45度以内、射影変換は強さ0.1〜0.2、明るさとコントラストは各±20%です。精度は同じ独立した未加工の検証画像で比較してください。"))
            .child(text(".ylp2 と同名の .ylp2.assets を一緒に保管してください。V-Training ZIPは101枚以上、200MB以下、各実クラスの枠が必要です。背景は画像＋空TXTで含めます。背景入りZIPはV-Trainingで受理された実績があります。未保存表示が出た場合はCtrl+Sで再保存してください。"))
            .child(button("閉じる (Esc)").on_click(|_|close_window()))
            .on_key_down(|e| { if e.key == Key::Named(NamedKey::Escape) { e.prevent_default(); close_window(); } e.stop_propagation(); })
}
