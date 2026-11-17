use super::*;

pub fn open(
    c: Controller,
    new: Option<(PathBuf, Vec<String>)>,
    root: PathBuf,
    paths: Vec<PathBuf>,
    labels: Option<PathBuf>,
) {
    let classes = new
        .as_ref()
        .map(|(_, classes)| classes.clone())
        .unwrap_or_else(|| c.state.borrow().project.as_ref().unwrap().classes.clone());
    let names = if new.is_some() {
        classes.clone()
    } else {
        let state = c.state.borrow();
        let p = state.project.as_ref().unwrap();
        (0..classes.len()).map(|id| p.display_class(id)).collect()
    };
    open_window(
        WindowOptions {
            title: "画像の追加 — 対象とラベル".into(),
            width: 760,
            height: 520,
            modal: true,
            dialog: true,
            ..Default::default()
        },
        move || {
            theme::follow_system();
            let background = signal(false);
            let class = signal(0usize);
            let csv = signal(String::new());
            let start = c.clone();
            let new = new.clone();
            let root = root.clone();
            let paths = paths.clone();
            let labels = labels.clone();
            let classes = classes.clone();
            keyboard_surface(div()).flex().flex_col().gap(14_f32).p(24_f32).h_full().overflow_y_scroll()
            .dyn_style(|s|s.bg(theme().page).text_color(theme().text))
            .child(heading("今回追加する画像について選んでください"))
            .child(text(root.display().to_string()))
            .child(checkbox(background,"対象なし — 背景画像として追加する"))
            .child(text("チェックなしは対象あり。正常な既存ラベルを優先し、枠がない画像は下のクラスで自動ラベリングします。背景と対象ありの画像が混在する場合は分けて追加してください。"))
            .child(show(move||!background.get(),move|| {let names=names.clone();div().flex().flex_col().gap(10_f32).child(div().flex().flex_row().gap(12_f32).items_center().child(text("自動ラベルのクラス"))
                .child(select(class,names.iter().enumerate().map(|(id,name)|(id,name.clone())).collect()).on_key_down(|e|e.stop_propagation())))
            .child(div().flex().flex_row().gap(8_f32)
                .child(text_field(csv).placeholder("番号範囲CSV（任意）").w(440.).on_key_down(|e|e.stop_propagation()))
                .child(button("CSVを選ぶ").on_click(move |_|{if let Some(path)=rfd::FileDialog::new().add_filter("番号範囲",&["csv"]).pick_file(){csv.set(path.to_string_lossy().into_owned());}})))}))
            .child(text("自動検出した枠は採用します。検出できない画像や不正なラベルは要確認として残します。枠の検出には標準の二値化設定を使用します。画像からH・S・Uの種類を判定する機能ではありません。"))
            .child(button("追加する").on_click(move |_|{
                let all_paths=if paths.is_empty(){labeler_core::image_files(&root)}else{paths.clone()};
                if all_paths.is_empty(){message("対応する画像がありません");return;}
                if background.get() {
                    let project=if let Some((file,_))=&new {Project::new(file.clone(),classes.clone())}else{Ok(start.state.borrow().project.as_ref().unwrap().save_snapshot())};
                    let conflicts=project.and_then(|project|project.preflight_background_conflicts(&root,&all_paths,labels.as_deref()));
                    match conflicts {
                        Ok(conflicts) if !conflicts.is_empty()=>{
                            let examples=conflicts.iter().take(8).map(|path|path.display().to_string()).collect::<Vec<_>>().join("\n");
                            if confirm(&format!("{}枚に既存の対象枠があります。\n{examples}\n対象ありに変更しますか？\nいいえなら追加を中止します。",conflicts.len())) {background.set(false);}else{close_window();}
                            return;
                        },
                        Err(error)=>{message(error.to_string());return;}, _=>{}
                    }
                }
                let options=labeler_core::ImportOptions{background:background.get(),fixed_class:class.get(),ranges:if csv.get().trim().is_empty(){None}else{Some(PathBuf::from(csv.get()))},detector:DetectorConfig::default()};
                if !options.background {if let Some(path)=&options.ranges {if let Err(error)=labeler_core::read_ranges(path,classes.len()){message(error.to_string());return;}}}
                if let Some((file,classes))=&new {start.job(Job::New(file.clone(),classes.clone(),root.clone(),labels.clone(),options));}
                else {start.job(Job::Import(root.clone(),all_paths,labels.clone(),options));}
                close_window();
            }))
            .child(button("キャンセル (Esc)").on_click(|_|close_window()))
            .on_key_down(|e|{if e.key==Key::Named(NamedKey::Escape){close_window();}e.stop_propagation();})
        },
    );
}
