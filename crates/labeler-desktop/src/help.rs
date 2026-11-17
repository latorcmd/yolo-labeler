use super::*;

pub const LIST_KEYS: &str =
    "クリック / 1〜0: 採用と除外を切替   ← A / → D Enter: ページ移動   ホイール: 前後ページ";
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
            .child(text("追加ごとに対象あり／なしを指定します。対象ありは既存の正常な枠を優先し、枠がなければ指定したクラスで自動検出します。対象なしは背景画像になります。H・S・U等の種類を画像から推測する機能はありません。"))
            .child(text("自動枠はそのまま採用しますが、正しさは画像で確認してください。検出失敗や不正ラベルは要確認です。ラベル表示名でギリシャ文字などへ表示だけ変更でき、出力するID・クラス名は変わりません。"))
            .child(heading("一覧")) .child(text(LIST_KEYS))
            .child(heading("編集")) .child(text(EDIT_KEYS))
            .child(text("枠をドラッグして移動、四隅をドラッグしてリサイズします。対象なしを指定すると背景として保存します。枠を作ると対象ありに戻ります。"))
            .child(heading("共通")) .child(text(COMMON_KEYS))
            .child(heading("出力と保存"))
            .child(text(".ylp2 と同名の .ylp2.assets を一緒に保管してください。V-Training ZIPは101枚以上、200MB以下、各実クラスの枠が必要です。背景は画像＋空TXTで含めます。サービス側で背景が受理されるかは未確認です。未保存表示が出た場合はCtrl+Sで再保存してください。"))
            .child(button("閉じる (Esc)").on_click(|_|close_window()))
            .on_key_down(|e| { if e.key == Key::Named(NamedKey::Escape) { e.prevent_default(); close_window(); } e.stop_propagation(); })
}
