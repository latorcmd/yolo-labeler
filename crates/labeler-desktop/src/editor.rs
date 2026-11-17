use super::*;
use labeler_core::NormalizedBox;
const W: f64 = 900.;
const H: f64 = 520.;
#[derive(Clone)]
struct Snapshot {
    boxes: Vec<NormalizedBox>,
    proposals: Vec<NormalizedBox>,
    resolved: bool,
    selected: Option<usize>,
}
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Select,
    Draw,
    Pan,
}
#[derive(Clone)]
struct Drag {
    start: (f64, f64),
    before: Snapshot,
    pan: (f64, f64),
    corner: Option<usize>,
    recorded: bool,
}
#[derive(Clone)]
pub struct Edit {
    pub sample: Sample,
    pub bitmap: Option<Arc<Bitmap>>,
    classes: Vec<String>,
    boxes: Vec<NormalizedBox>,
    proposals: Vec<NormalizedBox>,
    resolved: bool,
    negative: bool,
    selected: Option<usize>,
    class: usize,
    mode: Mode,
    zoom: f64,
    pan: (f64, f64),
    history: VecDeque<Snapshot>,
    drag: Option<Drag>,
    dirty: bool,
}
impl Edit {
    pub fn new(s: Sample, classes: Vec<String>) -> Self {
        Self {
            boxes: s.boxes.clone(),
            proposals: s.proposals.clone(),
            resolved: !s.proposal_pending && s.proposals.is_empty(),
            negative: s.has_label && s.boxes.is_empty(),
            sample: s,
            classes,
            bitmap: None,
            selected: None,
            class: 0,
            mode: Mode::Select,
            zoom: 1.,
            pan: (0., 0.),
            history: VecDeque::new(),
            drag: None,
            dirty: false,
        }
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            boxes: self.boxes.clone(),
            proposals: self.proposals.clone(),
            resolved: self.resolved,
            selected: self.selected,
        }
    }
    fn restore(&mut self, s: Snapshot) {
        self.boxes = s.boxes;
        self.proposals = s.proposals;
        self.resolved = s.resolved;
        self.selected = s.selected;
    }
    fn checkpoint(&mut self) {
        self.history.push_back(self.snapshot());
        while self.history.len() > 40 {
            self.history.pop_front();
        }
        self.dirty = true;
    }
    fn undo(&mut self) {
        if let Some(s) = self.history.pop_back() {
            self.restore(s);
            self.dirty = true;
        }
    }
    fn transform(&self) -> (f64, f64, f64, f64) {
        let scale = (W / self.sample.width.max(1) as f64).min(H / self.sample.height.max(1) as f64)
            * self.zoom;
        let w = self.sample.width as f64 * scale;
        let h = self.sample.height as f64 * scale;
        ((W - w) / 2. + self.pan.0, (H - h) / 2. + self.pan.1, w, h)
    }
    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        let (l, t, w, h) = self.transform();
        (((x - l) / w).clamp(0., 1.), ((y - t) / h).clamp(0., 1.))
    }
    fn begin(&mut self, x: f64, y: f64) {
        let p = self.point(x, y);
        let before = self.snapshot();
        let mut corner = None;
        if self.mode == Mode::Select {
            self.selected = None;
            let (_, _, w, h) = self.transform();
            for (i, b) in self.boxes.iter().enumerate().rev() {
                let (l, t, r, d) = (
                    b.cx - b.width / 2.,
                    b.cy - b.height / 2.,
                    b.cx + b.width / 2.,
                    b.cy + b.height / 2.,
                );
                let corners = [(l, t), (r, t), (l, d), (r, d)];
                if let Some(k) = corners
                    .iter()
                    .position(|&(a, b)| (p.0 - a).abs() * w < 9. && (p.1 - b).abs() * h < 9.)
                {
                    self.selected = Some(i);
                    corner = Some(k);
                    break;
                }
                if p.0 >= l && p.0 <= r && p.1 >= t && p.1 <= d {
                    self.selected = Some(i);
                    self.class = b.class_id;
                    break;
                }
            }
        }
        let recorded =
            self.mode == Mode::Draw || (self.mode == Mode::Select && self.selected.is_some());
        if recorded {
            self.checkpoint();
        }
        self.drag = Some(Drag {
            start: p,
            before,
            pan: self.pan,
            corner,
            recorded,
        });
    }
    fn motion(&mut self, x: f64, y: f64, total: (f64, f64)) {
        let Some(d) = self.drag.clone() else { return };
        if self.mode == Mode::Pan {
            self.pan = (d.pan.0 + total.0, d.pan.1 + total.1);
            return;
        }
        let p = self.point(x, y);
        if self.mode == Mode::Draw {
            let (l, t, r, b) = (
                d.start.0.min(p.0),
                d.start.1.min(p.1),
                d.start.0.max(p.0),
                d.start.1.max(p.1),
            );
            self.boxes = d.before.boxes.clone();
            self.boxes.push(NormalizedBox {
                class_id: self.class,
                cx: (l + r) / 2.,
                cy: (t + b) / 2.,
                width: r - l,
                height: b - t,
            });
            self.selected = Some(self.boxes.len() - 1);
        } else if let Some(i) = self.selected {
            if let Some(base) = d.before.boxes.get(i) {
                let mut b = base.clone();
                if let Some(k) = d.corner {
                    let (mut l, mut t, mut r, mut bottom) = (
                        b.cx - b.width / 2.,
                        b.cy - b.height / 2.,
                        b.cx + b.width / 2.,
                        b.cy + b.height / 2.,
                    );
                    if k % 2 == 0 {
                        l = p.0.min(r - 0.0001)
                    } else {
                        r = p.0.max(l + 0.0001)
                    }
                    if k < 2 {
                        t = p.1.min(bottom - 0.0001)
                    } else {
                        bottom = p.1.max(t + 0.0001)
                    }
                    b.cx = (l + r) / 2.;
                    b.cy = (t + bottom) / 2.;
                    b.width = r - l;
                    b.height = bottom - t;
                } else {
                    b.cx = (base.cx + p.0 - d.start.0).clamp(b.width / 2., 1. - b.width / 2.);
                    b.cy = (base.cy + p.1 - d.start.1).clamp(b.height / 2., 1. - b.height / 2.);
                }
                self.boxes[i] = b;
            }
        }
    }
    fn end(&mut self, cancel: bool) {
        if let Some(d) = self.drag.take() {
            if cancel {
                self.restore(d.before);
                self.pan = d.pan;
                if d.recorded {
                    self.history.pop_back();
                }
            } else {
                self.boxes.retain(|b| b.width > 0.0001 && b.height > 0.0001);
                if self.selected.is_some_and(|i| i >= self.boxes.len()) {
                    self.selected = None;
                }
            }
        }
    }
}
fn outline(b: &NormalizedBox, l: f64, t: f64, w: f64, h: f64, color: Color, selected: bool) -> Div {
    let x = l + (b.cx - b.width / 2.) * w;
    let y = t + (b.cy - b.height / 2.) * h;
    let bw = b.width * w;
    let bh = b.height * h;
    let mut node = div()
        .absolute()
        .left(x as f32)
        .top(y as f32)
        .w(bw as f32)
        .h(bh as f32)
        .border(3_f32)
        .border_color(Color::BLACK)
        .pointer_events_none()
        .child(
            div()
                .absolute()
                .left(1.)
                .top(1.)
                .w((bw - 2.).max(0.) as f32)
                .h((bh - 2.).max(0.) as f32)
                .border(1_f32)
                .border_color(color),
        )
        .child(
            text(format!("{}", b.class_id))
                .bg(Color::BLACK)
                .text_color(Color::WHITE)
                .text_size(12_f32),
        );
    if selected {
        for (x, y) in [(0., 0.), (bw - 8., 0.), (0., bh - 8.), (bw - 8., bh - 8.)] {
            node = node.child(
                div()
                    .absolute()
                    .left(x as f32)
                    .top(y as f32)
                    .w(8.)
                    .h(8.)
                    .bg(Color::WHITE)
                    .border(1_f32)
                    .border_color(Color::BLACK),
            );
        }
    }
    node
}
pub fn preview(
    bmp: Option<Arc<Bitmap>>,
    boxes: Vec<NormalizedBox>,
    width: u32,
    height: u32,
    proposals: Vec<NormalizedBox>,
    crop: bool,
) -> Div {
    // Thumbnail pixels are raw, unrotated RGB(A); framework file decoders can apply EXIF.
    let zoom_box = if crop { boxes.first().cloned() } else { None };
    let (l, t, w, h) = if let Some(b) = zoom_box {
        let scale = (0.9 / b.width).min(0.9 / b.height).clamp(1., 5.);
        (0.5 - b.cx * scale, 0.5 - b.cy * scale, scale, scale)
    } else {
        (0., 0., 1., 1.)
    };
    let ratio = width.max(1) as f32 / height.max(1) as f32;
    let picture = div()
        .relative()
        .w((160_f32 * ratio).min(240.))
        .max_w(pct(100.))
        .aspect_ratio(ratio)
        .overflow_clip()
        .bg(rgb(0x111722))
        .pointer_events_none()
        .child(
            bitmap_view(move || bmp.clone())
                .absolute()
                .left(pct((l * 100.) as f32))
                .top(pct((t * 100.) as f32))
                .w(pct((w * 100.) as f32))
                .h(pct((h * 100.) as f32)),
        )
        .children(
            boxes
                .into_iter()
                .map(|b| preview_outline(b, l, t, w, h, rgb(0x64e1ba))),
        )
        .children(
            proposals
                .into_iter()
                .map(|b| preview_outline(b, l, t, w, h, rgb(0xffc857))),
        );
    div()
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .h(160.)
        .bg(rgb(0x111722))
        .pointer_events_none()
        .child(picture)
}
fn preview_outline(b: NormalizedBox, l: f64, t: f64, w: f64, h: f64, color: Color) -> Div {
    div()
        .absolute()
        .left(pct(((l + (b.cx - b.width / 2.) * w) * 100.) as f32))
        .top(pct(((t + (b.cy - b.height / 2.) * h) * 100.) as f32))
        .w(pct((b.width * w * 100.) as f32))
        .h(pct((b.height * h * 100.) as f32))
        .border(2_f32)
        .border_color(Color::BLACK)
        .pointer_events_none()
        .child(div().w_full().h_full().border(1_f32).border_color(color))
}
fn update(c: &Controller, f: impl FnOnce(&mut Edit)) {
    c.edit.update(|edit| {
        if let Some(e) = edit {
            f(e)
        }
    });
}
fn finish(c: Controller, decision: Decision) {
    let Some(e) = c.edit.get() else { return };
    let result = {
        let mut s = c.state.borrow_mut();
        let Some(p) = &mut s.project else { return };
        p.edit(e.sample.id, e.boxes, decision, e.resolved, e.negative)
    };
    match result {
        Ok(()) => {
            batch(|| {
                c.edit.set(None);
                c.editing.set(false);
            });
            c.state.borrow_mut().dirty = true;
            c.save();
            c.bump();
        }
        Err(e) => message(e.to_string()),
    }
}
pub fn view(c: Controller) -> Div {
    let e = c.edit.get_untracked().unwrap();
    let title = e.sample.origin_relative.display().to_string();
    let classes = e.classes.clone();
    let class = signal(e.class);
    let negative = signal(e.negative);
    let ec = c.clone();
    effect(move || {
        let v = class.get();
        update(&ec, |e| {
            if e.class != v {
                if let Some(i) = e.selected {
                    e.checkpoint();
                    e.boxes[i].class_id = v;
                }
                e.class = v;
            }
        })
    });
    let nc = c.clone();
    effect(move || {
        let v = negative.get();
        update(&nc, |e| e.negative = v)
    });
    let bmc = c.clone();
    let style_c = c.clone();
    let overlays = c.clone();
    let drag_c = c.clone();
    let scroll_c = c.clone();
    let keys = c.clone();
    let c_info = c.clone();
    let cancelled = c.clone();
    let canvas = div()
        .relative()
        .w(W as f32)
        .h(H as f32)
        .overflow_hidden()
        .bg(rgb(0x111722))
        .focusable()
        .select_none()
        .child(
            bitmap_view(move || bmc.edit.with(|e| e.as_ref().and_then(|e| e.bitmap.clone())))
                .absolute()
                .pointer_events_none()
                .dyn_style(move |s| {
                    style_c.edit.with(|e| {
                        if let Some(e) = e {
                            let (l, t, w, h) = e.transform();
                            s.left(l as f32).top(t as f32).w(w as f32).h(h as f32)
                        } else {
                            s
                        }
                    })
                }),
        )
        .child(dyn_view(move || {
            let Some(e) = overlays.edit.get() else {
                return div();
            };
            let (l, t, w, h) = e.transform();
            div()
                .absolute()
                .left(0.)
                .top(0.)
                .w_full()
                .h_full()
                .pointer_events_none()
                .children(
                    e.boxes
                        .iter()
                        .enumerate()
                        .map(|(i, b)| outline(b, l, t, w, h, rgb(0x64e1ba), e.selected == Some(i))),
                )
                .children(
                    e.proposals
                        .iter()
                        .map(|b| outline(b, l, t, w, h, rgb(0xffc857), false)),
                )
        }))
        .grab_threshold(0.)
        .on_grab(move |event| {
            let scale = window_scale().as_f64();
            let (x, y) = (event.local.x as f64 / scale, event.local.y as f64 / scale);
            update(&drag_c, |e| match event.phase {
                GrabPhase::Start => e.begin(x, y),
                GrabPhase::Move => e.motion(x, y, (event.total.x as f64, event.total.y as f64)),
                GrabPhase::End => e.end(false),
                GrabPhase::Cancel => e.end(true),
            });
        })
        .on_scroll(move |event| {
            event.stop_propagation();
            update(&scroll_c, |e| {
                e.zoom =
                    (e.zoom * if event.delta_y < 0. { 1.12 } else { 1. / 1.12 }).clamp(0.25, 16.)
            });
        })
        .on_key_down(move |event| {
            event.stop_propagation();
            if event.key.is_char('w') {
                update(&keys, |e| e.mode = Mode::Draw)
            } else if event.key.is_char('v') {
                update(&keys, |e| e.mode = Mode::Select)
            } else if event.key.is_char('p') {
                update(&keys, |e| e.mode = Mode::Pan)
            } else if event.modifiers.ctrl && event.key.is_char('z') {
                update(&keys, Edit::undo)
            } else if event.key == Key::Named(NamedKey::Delete) {
                update(&keys, |e| {
                    if let Some(i) = e.selected {
                        e.checkpoint();
                        e.boxes.remove(i);
                        e.selected = None;
                    }
                })
            }
        });
    let list_c = c.clone();
    let buttons = vec![
        action(&c, "選択 (V)", |c| update(&c, |e| e.mode = Mode::Select)),
        action(&c, "描画 (W)", |c| update(&c, |e| e.mode = Mode::Draw)),
        action(&c, "パン (P)", |c| update(&c, |e| e.mode = Mode::Pan)),
        action(&c, "画面に合わせる", |c| {
            update(&c, |e| {
                e.zoom = 1.;
                e.pan = (0., 0.);
            })
        }),
        action(&c, "編集Undo", |c| update(&c, Edit::undo)),
        action(&c, "枠を削除", |c| {
            update(&c, |e| {
                if let Some(i) = e.selected {
                    e.checkpoint();
                    e.boxes.remove(i);
                    e.selected = None;
                }
            })
        }),
    ];
    div().flex().flex_col().gap(10_f32).p(12_f32).border(1_f32).dyn_style(|s|s.bg(theme().surface).border_color(theme().border))
  .child(heading(title)).child(div().flex().flex_row().gap(6_f32).children(buttons).child(select(class,classes.iter().enumerate().map(|(i,n)|(i,n.clone())).collect())))
  .child(div().flex().flex_row().gap(12_f32).child(canvas).child(div().flex().flex_col().gap(8_f32).w(220.)
   .child(text(move||c_info.edit.with(|e|e.as_ref().map(|e|format!("{}枠 / 候補{}枠\n{}\n{}",e.boxes.len(),e.proposals.len(),if e.resolved{"候補解決済み"}else{"候補の承認・破棄が必要"},e.sample.label_errors.join("\n"))).unwrap_or_default())))
   .child(action(&c,"候補で枠を置換",|c|update(&c,|e|{e.checkpoint();e.boxes=e.proposals.clone();e.proposals.clear();e.resolved=true;e.selected=None;})))
   .child(action(&c,"候補をすべて破棄",|c|update(&c,|e|{e.checkpoint();e.proposals.clear();e.resolved=true;})))
   .child(dyn_view(move||{let Some(e)=list_c.edit.get()else{return div()};let mut list=div().flex().flex_col().gap(4_f32);for(i,b)in e.boxes.iter().enumerate(){let lc=list_c.clone();let value=b.clone();list=list.child(button(format!("{}. {} {:.3}×{:.3}",i+1,e.classes[b.class_id],b.width,b.height)).on_click(move |_|{update(&lc,|e|{e.selected=Some(i);e.class=value.class_id;});class.set(value.class_id);}));}list}))
   .child(text("ホイール: ズーム\nP: パンモード\n四隅をドラッグ: リサイズ\n枠をドラッグ: 移動"))))
  .child(div().flex().flex_row().gap(8_f32).child(checkbox(negative,"対象がない画像として確認した"))
   .child(action(&c,"保存して採用",|c|finish(c,Decision::Keep))).child(action(&c,"保存して保留",|c|finish(c,Decision::Hold)))
   .child(button("キャンセル").on_click(move |_|{if !cancelled.edit.with(|e|e.as_ref().is_some_and(|e|e.dirty))||confirm("編集内容を破棄して閉じる？"){batch(||{cancelled.edit.set(None);cancelled.editing.set(false);});}})))
}
pub fn new_dataset_dialog(c: Controller, file: PathBuf, root: PathBuf, labels: Option<PathBuf>) {
    open_window(
        WindowOptions {
            title: "新規データセットのクラス".into(),
            width: 540,
            height: 480,
            modal: true,
            dialog: true,
            ..Default::default()
        },
        move || {
            theme::follow_system();
            let value = signal(String::new());
            let start = c.clone();
            div()
                .flex()
                .flex_col()
                .gap(12_f32)
                .p(20_f32)
                .dyn_style(|s| s.bg(theme().page).text_color(theme().text))
                .child(text(
                    "クラス名を1行ずつ入力してください。先頭行がID 0です。",
                ))
                .child(text_area_field(value).rows(12))
                .child(button("classes.txtを読み込む").on_click(move |_| {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("クラス一覧", &["txt"])
                        .pick_file()
                    {
                        match std::fs::read_to_string(p) {
                            Ok(t) => value.set(t.trim_start_matches('\u{feff}').to_owned()),
                            Err(e) => message(e.to_string()),
                        }
                    }
                }))
                .child(button("作成").on_click(move |_| {
                    let classes = value.get().lines().map(str::to_owned).collect::<Vec<_>>();
                    if let Err(e) = labeler_core::validate_classes(&classes) {
                        message(e.to_string());
                        return;
                    }
                    start.job(Job::New(
                        file.clone(),
                        classes,
                        root.clone(),
                        labels.clone(),
                    ));
                    close_window();
                }))
        },
    );
}
pub fn class_dialog(c: Controller) {
    if !c.project_ready() {
        return;
    }
    let classes = c
        .state
        .borrow()
        .project
        .as_ref()
        .unwrap()
        .classes
        .join("\n");
    open_window(
        WindowOptions {
            title: "クラス設定（行順がID）".into(),
            width: 540,
            height: 480,
            modal: true,
            dialog: true,
            ..Default::default()
        },
        move || {
            let value = signal(classes);
            let save = c.clone();
            div()
                .flex()
                .flex_col()
                .gap(12_f32)
                .p(20_f32)
                .dyn_style(|s| s.bg(theme().page).text_color(theme().text))
                .child(text("既存IDを維持し、名称変更と末尾追加ができます。"))
                .child(text_area_field(value).rows(12))
                .child(button("保存").on_click(move |_| {
                    let classes = value.get().lines().map(str::to_owned).collect();
                    let result = save
                        .state
                        .borrow_mut()
                        .project
                        .as_mut()
                        .unwrap()
                        .set_classes(classes);
                    match result {
                        Ok(()) => {
                            save.save();
                            save.bump();
                            close_window();
                        }
                        Err(e) => message(e.to_string()),
                    }
                }))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edit() -> Edit {
        let json = serde_json::json!({"id":Uuid::nil(),"batch":Uuid::nil(),"path":"x.png","origin_root":".","origin_relative":"x.png","source_hash":"x","width":160,"height":100,"boxes":[{"class_id":0,"cx":0.5,"cy":0.5,"width":0.2,"height":0.2}],"has_label":true,"label_errors":[],"image_error":null,"decision":"pending","reviewed":false,"reason":"","proposals":[],"proposal_pending":false,"proposal_info":null,"quality":null,"duplicate_of":null,"duplicate_conflict":false});
        Edit::new(serde_json::from_value(json).unwrap(), vec!["H".into()])
    }
    #[test]
    fn coordinates_survive_zoom_and_pan() {
        let mut e = edit();
        e.zoom = 2.5;
        e.pan = (22., -11.);
        let (l, t, w, h) = e.transform();
        let p = e.point(l + w * 0.3, t + h * 0.7);
        assert!((p.0 - 0.3).abs() < 1e-9);
        assert!((p.1 - 0.7).abs() < 1e-9);
    }
    #[test]
    fn drawing_undo() {
        let mut e = edit();
        e.mode = Mode::Draw;
        let (l, t, w, h) = e.transform();
        e.begin(l + w * 0.1, t + h * 0.1);
        e.motion(l + w * 0.3, t + h * 0.3, (0., 0.));
        e.end(false);
        assert_eq!(e.boxes.len(), 2);
        e.undo();
        assert_eq!(e.boxes.len(), 1);
    }
    #[test]
    fn candidate_undo_restores_resolution() {
        let mut e = edit();
        e.resolved = false;
        e.proposals = e.boxes.clone();
        e.checkpoint();
        e.proposals.clear();
        e.resolved = true;
        e.undo();
        assert!(!e.resolved);
        assert_eq!(e.proposals.len(), 1);
    }
    #[test]
    fn cancelled_drag_restores() {
        let mut e = edit();
        e.mode = Mode::Draw;
        let before = e.boxes.clone();
        e.begin(30., 30.);
        e.motion(200., 200., (170., 170.));
        e.end(true);
        assert_eq!(e.boxes, before);
    }
}
