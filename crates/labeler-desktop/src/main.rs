#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app_icon;
mod editor;
mod qa;
mod theme;
use anti_webui::{core::images::Bitmap, prelude::*};
use anyhow::{Context, Result};
use labeler_core::{Decision, DetectorConfig, Project, Sample};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use uuid::Uuid;

#[derive(Default)]
struct State {
    project: Option<Project>,
    selection: HashSet<Uuid>,
    cache: HashMap<Uuid, Arc<Bitmap>>,
    lru: VecDeque<Uuid>,
    cache_bytes: usize,
    loading: HashSet<Uuid>,
    request: u64,
    dirty: bool,
}
#[derive(Clone)]
struct Controller {
    state: Rc<RefCell<State>>,
    version: Signal<u64>,
    status: Signal<String>,
    busy: Signal<bool>,
    search: Signal<String>,
    filter: Signal<usize>,
    class: Signal<usize>,
    sort: Signal<usize>,
    crop: Signal<bool>,
    edit: Signal<Option<editor::Edit>>,
    editing: Signal<bool>,
    preview: Signal<Option<Preview>>,
    cancel: Arc<AtomicBool>,
    events: UiSender<Event>,
    thumbnails: std::sync::mpsc::Sender<(Uuid, PathBuf, UiWork)>,
}
thread_local! {static QA_CONTROLLER:RefCell<Option<Controller>>=const{RefCell::new(None)};}
#[derive(Clone)]
struct Preview {
    bitmap: Arc<Bitmap>,
    boxes: Vec<labeler_core::NormalizedBox>,
    width: u32,
    height: u32,
    text: String,
}
enum Event {
    Preview(u64, Result<Preview>),
    Progress(u64, usize, usize),
    Done(u64, Result<(Project, String)>, bool),
    Thumbnail(Uuid, Result<image::RgbaImage>),
    EditImage(Uuid, Result<image::RgbaImage>),
}
#[derive(Clone)]
enum Job {
    New(PathBuf, Vec<String>, PathBuf, Option<PathBuf>),
    Load(PathBuf),
    Import(PathBuf, Vec<PathBuf>, Option<PathBuf>),
    Quality,
    Propose(DetectorConfig, usize, Option<PathBuf>),
    External(Uuid, PathBuf),
    Export(PathBuf),
}
fn message(text: impl AsRef<str>) {
    rfd::MessageDialog::new()
        .set_title("YOLO Labeler")
        .set_description(text.as_ref())
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}
fn confirm(text: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title("YOLO Labeler")
        .set_description(text)
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes
}
fn bitmap(mut rgba: image::RgbaImage) -> Arc<Bitmap> {
    for p in rgba.pixels_mut() {
        for c in 0..3 {
            p[c] = ((p[c] as u16 * p[3] as u16 + 127) / 255) as u8;
        }
    }
    Arc::new(Bitmap::from_premultiplied(
        rgba.width(),
        rgba.height(),
        rgba.into_raw(),
        false,
    ))
}
impl Controller {
    fn bump(&self) {
        self.version.update(|v| *v += 1);
    }
    fn allowed(&self) -> bool {
        !self.busy.get() && !self.editing.get()
    }
    fn project_ready(&self) -> bool {
        self.allowed() && self.state.borrow().project.is_some()
    }
    fn save(&self) -> bool {
        let result = {
            let mut state = self.state.borrow_mut();
            if let Some(p) = &mut state.project {
                p.view.search = self.search.get();
                p.view.status = self.filter.get();
                p.view.class = self.class.get();
                p.view.sort = self.sort.get();
                p.view.crop = self.crop.get();
                p.save()
            } else {
                return true;
            }
        };
        match result {
            Ok(()) => {
                let mut state = self.state.borrow_mut();
                state.dirty = false;
                if let Some(p) = &mut state.project {
                    p.needs_save = false;
                }
                self.status.set("保存しました".into());
                true
            }
            Err(e) => {
                self.state.borrow_mut().dirty = true;
                message(format!("保存できません。再保存してください: {e:#}"));
                false
            }
        }
    }
    fn job(&self, job: Job) {
        if !self.allowed() {
            return;
        }
        if !self.save() {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.request += 1;
        let request = state.request;
        let mut project = state.project.clone();
        drop(state);
        self.cancel.store(false, Ordering::Relaxed);
        self.busy.set(true);
        let sender = self.events.clone();
        let stop = self.cancel.clone();
        let keep_view = !matches!(&job, Job::New(..) | Job::Load(..) | Job::Import(..));
        self.status.set("処理中…".into());
        std::thread::spawn(move || {
            let mut progress = |a, b| sender.send(Event::Progress(request, a, b));
            let result = (|| -> Result<(Project, String)> {
                match job {
                    Job::New(file, classes, root, labels) => {
                        let mut p = Project::new(file, classes)?;
                        p.save()?;
                        let paths = labeler_core::image_files(&root);
                        let n = p.import(&root, &paths, labels.as_deref(), &stop, &mut progress)?;
                        Ok((p, format!("{n}枚を読み込みました")))
                    }
                    Job::Load(path) => {
                        Ok((Project::load(&path)?, "プロジェクトを開きました".into()))
                    }
                    other => {
                        let p = project
                            .as_mut()
                            .context("先にデータセットを作成してください")?;
                        let text = match other {
                            Job::Import(root, paths, labels) => {
                                let first = p.samples.len();
                                let paths = if paths.is_empty() {
                                    labeler_core::image_files(&root)
                                } else {
                                    paths
                                };
                                let n = p.import(
                                    &root,
                                    &paths,
                                    labels.as_deref(),
                                    &stop,
                                    &mut progress,
                                )?;
                                if n > 0 {
                                    p.view = labeler_core::ViewState {
                                        page: first / 10,
                                        ..Default::default()
                                    };
                                }
                                format!("{n}枚追加しました（登録済みパスはスキップ）")
                            }
                            Job::Quality => {
                                p.analyze(&stop, &mut progress)?;
                                "品質・完全重複を検査しました".into()
                            }
                            Job::Propose(config, class, ranges) => {
                                let n = p.propose(
                                    &config,
                                    class,
                                    ranges.as_deref(),
                                    &stop,
                                    &mut progress,
                                )?;
                                format!("{n}枚の候補を生成しました。承認が必要です")
                            }
                            Job::External(batch, labels) => {
                                let n = p.import_proposals(batch, &labels, &stop, &mut progress)?;
                                format!("外部候補を{n}枚に読み込みました")
                            }
                            Job::Export(path) => {
                                p.export(&path, &stop, &mut progress)?;
                                format!("検査済みZIPを書き出しました: {}", path.display())
                            }
                            _ => unreachable!(),
                        };
                        // Do not turn an already published import into a discarded result.
                        if let Err(e) = p.save() {
                            p.needs_save = true;
                            p.warnings.push(format!("未保存: {e}"));
                        } else {
                            p.needs_save = false;
                        }
                        Ok((project.take().unwrap(), text))
                    }
                }
            })();
            sender.send(Event::Done(request, result, keep_view));
        });
    }
    fn new_dataset(&self) {
        if !self.allowed() {
            return;
        }
        let Some(root) = rfd::FileDialog::new()
            .set_title("画像フォルダー")
            .pick_folder()
        else {
            return;
        };
        let labels = if confirm("画像と別のラベルフォルダーを指定する？") {
            rfd::FileDialog::new()
                .set_title("ラベルフォルダー")
                .pick_folder()
        } else {
            None
        };
        let classes = match labeler_core::discover_classes(&root, labels.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                message(e.to_string());
                return;
            }
        };
        let Some(file) = rfd::FileDialog::new()
            .set_title("新規プロジェクトの保存先")
            .add_filter("YOLO Labeler Rust", &["ylp2"])
            .set_file_name("dataset.ylp2")
            .save_file()
        else {
            return;
        };
        if classes.is_empty() {
            editor::new_dataset_dialog(self.clone(), file, root, labels);
            return;
        }
        self.job(Job::New(file, classes, root, labels));
    }
    fn open(&self) {
        if self.allowed() {
            if let Some(file) = rfd::FileDialog::new()
                .add_filter("YOLO Labeler Rust", &["ylp2"])
                .pick_file()
            {
                self.job(Job::Load(file));
            }
        }
    }
    fn add(&self, folder: bool) {
        if !self.project_ready() {
            return;
        }
        let labels = if confirm("追加元と別のラベルフォルダーを指定する？") {
            rfd::FileDialog::new()
                .set_title("追加元のラベルフォルダー")
                .pick_folder()
        } else {
            None
        };
        if folder {
            if let Some(root) = rfd::FileDialog::new()
                .set_title("追加元フォルダー（子フォルダーも対象）")
                .pick_folder()
            {
                self.job(Job::Import(root, vec![], labels));
            }
        } else if let Some(paths) = rfd::FileDialog::new()
            .add_filter("画像", &["jpg", "jpeg", "png", "bmp", "webp"])
            .pick_files()
        {
            if paths.is_empty() {
                return;
            }
            let mut root = paths[0].parent().unwrap().to_path_buf();
            while !paths.iter().all(|p| p.starts_with(&root)) {
                if !root.pop() {
                    message("同じドライブの画像を選択してください");
                    return;
                }
            }
            if root.file_name().is_some_and(|s| s == "images") {
                root.pop();
            }
            self.job(Job::Import(root, paths, labels));
        }
    }
    fn mutate(&self, f: impl FnOnce(&mut Project, &HashSet<Uuid>)) {
        if !self.project_ready() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            let selection = state.selection.clone();
            if let Some(p) = &mut state.project {
                f(p, &selection);
            }
            state.selection.clear();
            state.dirty = true;
        }
        self.save();
        self.bump();
    }
    fn visible(&self) -> Vec<usize> {
        let state = self.state.borrow();
        let Some(p) = &state.project else {
            return vec![];
        };
        let search = self.search.get().to_lowercase();
        let filter = self.filter.get();
        let class = self.class.get();
        let mut v = p
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                (match filter {
                    0 => true,
                    1 => s.decision == Decision::Pending,
                    2 => s.decision == Decision::Keep,
                    3 => s.decision == Decision::Exclude,
                    4 => s.decision == Decision::Hold,
                    5 => s.decision == Decision::Fix,
                    6 => !s.has_label,
                    7 => s.proposal_pending,
                    8 => {
                        s.quality.as_ref().is_some_and(|q| !q.flags.is_empty())
                            || s.duplicate_of.is_some()
                    }
                    _ => true,
                }) && s
                    .origin_relative
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&search)
                    && (class == 0
                        || s.boxes
                            .iter()
                            .chain(s.proposals.iter())
                            .any(|b| b.class_id + 1 == class))
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        match self.sort.get() {
            1 => v.sort_by(|&a, &b| {
                p.samples[b]
                    .quality
                    .as_ref()
                    .map(|q| q.score)
                    .unwrap_or(-1.)
                    .total_cmp(
                        &p.samples[a]
                            .quality
                            .as_ref()
                            .map(|q| q.score)
                            .unwrap_or(-1.),
                    )
            }),
            2 => v.sort_by(|&a, &b| {
                p.samples[a]
                    .quality
                    .as_ref()
                    .map(|q| q.sharpness)
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(
                        &p.samples[b]
                            .quality
                            .as_ref()
                            .map(|q| q.sharpness)
                            .unwrap_or(f64::INFINITY),
                    )
            }),
            3 => v.sort_by_key(|&i| p.samples[i].duplicate_of.unwrap_or(p.samples[i].id)),
            _ => {}
        }
        v
    }
    fn page_ids(&self) -> Vec<Uuid> {
        let indices = self.visible();
        let state = self.state.borrow();
        let Some(p) = &state.project else {
            return vec![];
        };
        indices
            .into_iter()
            .skip(p.view.page * 10)
            .take(10)
            .map(|i| p.samples[i].id)
            .collect()
    }
    fn page(&self, delta: isize) {
        if !self.project_ready() {
            return;
        }
        let n = self.visible().len().div_ceil(10).max(1);
        {
            let mut s = self.state.borrow_mut();
            let p = s.project.as_mut().unwrap();
            p.view.page = p.view.page.saturating_add_signed(delta).min(n - 1);
            s.selection.clear();
        }
        self.save();
        self.bump();
    }
    fn commit(&self) {
        let before = self.visible();
        let ids = self.page_ids();
        self.mutate(|p, selection| p.commit_page(&ids, selection));
        if self.visible() == before {
            self.page(1)
        } else {
            self.bump()
        }
    }
    fn toggle(&self, id: Uuid) {
        if self.allowed() {
            let mut s = self.state.borrow_mut();
            if !s.selection.insert(id) {
                s.selection.remove(&id);
            }
            drop(s);
            self.bump();
        }
    }
    fn edit_sample(&self, id: Uuid) {
        if !self.project_ready() {
            return;
        }
        let state = self.state.borrow();
        let p = state.project.as_ref().unwrap();
        let Some(s) = p.samples.iter().find(|s| s.id == id) else {
            return;
        };
        let path = match p.image_path(s) {
            Ok(p) => p,
            Err(e) => {
                message(e.to_string());
                return;
            }
        };
        let edit = editor::Edit::new(s.clone(), p.classes.clone());
        drop(state);
        batch(|| {
            self.edit.set(Some(edit));
            self.editing.set(true);
        });
        let sender = self.events.clone();
        std::thread::spawn(move || {
            sender.send(Event::EditImage(
                id,
                image::open(path).map(|i| i.to_rgba8()).map_err(Into::into),
            ))
        });
    }
    fn preview_detect(&self, config: DetectorConfig, class: usize, ranges: Option<PathBuf>) {
        if !self.project_ready() {
            return;
        }
        let result = (|| -> Result<_> {
            let ids = self.page_ids();
            let state = self.state.borrow();
            let p = state.project.as_ref().unwrap();
            let s = p
                .samples
                .iter()
                .find(|s| ids.first() == Some(&s.id))
                .context("画像がありません")?;
            let class = if let Some(path) = ranges {
                labeler_core::class_from_ranges(
                    &s.origin_relative,
                    &labeler_core::read_ranges(&path, p.classes.len())?,
                )?
            } else {
                class
            };
            Ok((p.image_path(s)?, class))
        })();
        let (path, class) = match result {
            Ok(v) => v,
            Err(e) => {
                message(e.to_string());
                return;
            }
        };
        self.busy.set(true);
        self.cancel.store(false, Ordering::Relaxed);
        self.state.borrow_mut().request += 1;
        let request = self.state.borrow().request;
        let sender = self.events.clone();
        let cancel = self.cancel.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<Preview> {
                let im = image::open(path)?;
                let mut d = labeler_vision::detect(&im, &config)?;
                labeler_core::cancelled(&cancel)?;
                for b in &mut d.boxes {
                    b.class_id = class;
                }
                let text = format!(
                    "{} / 閾値{} / {}枠 / {}",
                    d.method,
                    d.threshold,
                    d.boxes.len(),
                    d.flags.join(", ")
                );
                Ok(Preview {
                    width: im.width(),
                    height: im.height(),
                    bitmap: bitmap(im.to_rgba8()),
                    boxes: d.boxes,
                    text,
                })
            })();
            sender.send(Event::Preview(request, result));
        });
    }
    fn thumbnail(&self, id: Uuid, path: PathBuf) {
        let mut state = self.state.borrow_mut();
        if state.cache.contains_key(&id) || !state.loading.insert(id) {
            return;
        }
        let _ = self.thumbnails.send((id, path, self.events.work()));
    }
    fn external(&self) {
        if !self.project_ready() {
            return;
        }
        let state = self.state.borrow();
        let p = state.project.as_ref().unwrap();
        let mut batches = vec![];
        for s in &p.samples {
            if !batches.contains(&s.batch) {
                batches.push(s.batch);
            }
        }
        drop(state);
        let batch = if batches.len() == 1 {
            batches[0]
        } else {
            let state = self.state.borrow();
            let chosen = state
                .selection
                .iter()
                .next()
                .and_then(|id| {
                    state
                        .project
                        .as_ref()
                        .unwrap()
                        .samples
                        .iter()
                        .find(|s| s.id == *id)
                })
                .map(|s| s.batch);
            let Some(batch) = chosen else {
                message("複数の追加元があります。読み込む追加元の画像を1枚選択してください");
                return;
            };
            batch
        };
        if let Some(labels) = rfd::FileDialog::new()
            .set_title("選択した追加元に対応する外部候補フォルダー")
            .pick_folder()
        {
            self.job(Job::External(batch, labels));
        }
    }
    fn export(&self) {
        if !self.project_ready() {
            return;
        }
        let errors = self
            .state
            .borrow()
            .project
            .as_ref()
            .unwrap()
            .validate_export();
        if !errors.is_empty() {
            message(errors.join("\n"));
            return;
        }
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("V-Training ZIP", &["zip"])
            .set_file_name("dataset.zip")
            .save_file()
        {
            self.job(Job::Export(p));
        }
    }
}
fn action(c: &Controller, label: &str, f: impl Fn(Controller) + 'static) -> Div {
    let c = c.clone();
    button(label.to_owned()).on_click(move |_| f(c.clone()))
}
fn thumbnail_card(c: Controller, s: Sample, number: usize) -> Div {
    let id = s.id;
    let state = c.state.borrow();
    let bmp = state.cache.get(&id).cloned();
    let selected = state.selection.contains(&id);
    let path = state.project.as_ref().unwrap().image_path(&s).ok();
    drop(state);
    if bmp.is_none() {
        if let Some(p) = path {
            c.thumbnail(id, p);
        }
    }
    let click = c.clone();
    let edit = c.clone();
    let crop = c.crop.get();
    let boxes = s.boxes.clone();
    div()
        .flex()
        .flex_col()
        .w(pct(19.2))
        .min_w(140.)
        .gap(5_f32)
        .p(7_f32)
        .rounded(8_f32)
        .border(if selected { 3_f32 } else { 1_f32 })
        .dyn_style(move |style| {
            style.bg(theme().surface).border_color(if selected {
                theme().danger
            } else {
                theme().border
            })
        })
        .child(
            text(format!(
                "{}  {}",
                (number + 1) % 10,
                s.origin_relative
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ))
            .text_size(13_f32),
        )
        .child(editor::preview(
            bmp,
            boxes,
            s.width,
            s.height,
            s.proposals.clone(),
            crop,
        ))
        .child(
            text(format!(
                "{}{} · {}枠",
                if selected { "除外予定 / " } else { "" },
                s.decision.label(),
                s.boxes.len()
            ))
            .text_size(12_f32),
        )
        .child(
            text(
                s.quality
                    .as_ref()
                    .map(|q| q.flags.join(" / "))
                    .unwrap_or_else(|| {
                        if s.image_error.is_some() {
                            "画像破損".into()
                        } else if !s.label_errors.is_empty() {
                            "ラベル不正".into()
                        } else if s.proposal_pending {
                            "未承認候補".into()
                        } else {
                            String::new()
                        }
                    }),
            )
            .text_size(11_f32),
        )
        .child(action(&edit, "枠を編集", move |c| c.edit_sample(id)))
        .on_click(move |e| {
            if e.click_count >= 2 {
                click.edit_sample(id)
            } else {
                click.toggle(id)
            }
        })
}
fn grid(c: Controller) -> Div {
    c.version.get();
    let indices = c.visible();
    let mut state = c.state.borrow_mut();
    let Some(p) = &mut state.project else {
        return div()
            .p(36_f32)
            .child(heading("画像を見て、学習データを整える。"))
            .child(text(
                "新規データセットを作成するか、.ylp2 を開いてください。",
            ));
    };
    let pages = indices.len().div_ceil(10).max(1);
    p.view.page = p.view.page.min(pages - 1);
    let start = p.view.page * 10;
    let page = p.view.page;
    let total = p.samples.len();
    let kept = p
        .samples
        .iter()
        .filter(|s| s.decision == Decision::Keep)
        .count();
    let samples = indices
        .into_iter()
        .skip(start)
        .take(10)
        .map(|i| p.samples[i].clone())
        .collect::<Vec<_>>();
    let filename = p.file.display().to_string();
    drop(state);
    let mut body = div().flex().flex_col().gap(10_f32).child(text(format!(
        "{filename}  · 全{total}枚 / 採用{kept}枚  · {} / {pages}ページ",
        page + 1
    )));
    for row in 0..2 {
        body = body.child(
            div().flex().flex_row().gap(8_f32).children(
                samples
                    .iter()
                    .skip(row * 5)
                    .take(5)
                    .cloned()
                    .enumerate()
                    .map(|(i, s)| thumbnail_card(c.clone(), s, row * 5 + i)),
            ),
        );
    }
    body
}
fn configuration(c: Controller) -> Div {
    let classes = c
        .state
        .borrow()
        .project
        .as_ref()
        .map(|p| p.classes.clone())
        .unwrap_or_default();
    let class = signal(0usize);
    let threshold = signal(String::new());
    let area = signal("0.001".to_owned());
    let gap = signal("0.20".to_owned());
    let ratio = signal("0.05".to_owned());
    let padding = signal("0".to_owned());
    let ranges = signal(String::new());
    let opened = signal(false);
    let preview_data = c.preview;
    let read = move || -> Result<DetectorConfig> {
        Ok(DetectorConfig {
            threshold: if threshold.get().trim().is_empty() {
                None
            } else {
                Some(threshold.get().parse()?)
            },
            min_area_ratio: area.get().parse()?,
            merge_gap_ratio: gap.get().parse()?,
            merge_component_ratio: ratio.get().parse()?,
            padding: padding.get().parse()?,
        })
    };
    let run = c.clone();
    let pc = c.clone();
    div()
        .child(button("半自動の設定").on_click(move |_| opened.update(|v| *v = !*v)))
        .child(show(
            move || opened.get(),
            move || {
                let run = run.clone();
                let pc = pc.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(6_f32)
                    .p(12_f32)
                    .border(1_f32)
                    .dyn_style(|s| s.bg(theme().surface).border_color(theme().border))
                    .child(select(
                        class,
                        classes
                            .iter()
                            .enumerate()
                            .map(|(i, n)| (i, n.clone()))
                            .collect(),
                    ))
                    .child(text(
                        "しきい値（空欄=Otsu） / 最小面積 / 結合距離 / 成分比 / 余白px",
                    ))
                    .child(
                        div().flex().flex_row().gap(8_f32).children(
                            [threshold, area, gap, ratio, padding].map(|v| {
                                text_field(v).on_key_down(|e| e.stop_propagation()).w(130.)
                            }),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(8_f32)
                            .child(
                                text_field(ranges)
                                    .on_key_down(|e| e.stop_propagation())
                                    .placeholder("範囲CSV（任意）")
                                    .w(380.),
                            )
                            .child(button("CSVを選択").on_click(move |_| {
                                if let Some(p) = rfd::FileDialog::new()
                                    .add_filter("CSV", &["csv"])
                                    .pick_file()
                                {
                                    ranges.set(p.to_string_lossy().into_owned());
                                }
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(8_f32)
                            .child(
                                button("先頭画像のプレビュー").on_click(move |_| match read() {
                                    Ok(config) => pc.preview_detect(
                                        config,
                                        class.get(),
                                        if ranges.get().is_empty() {
                                            None
                                        } else {
                                            Some(PathBuf::from(ranges.get()))
                                        },
                                    ),
                                    Err(e) => message(e.to_string()),
                                }),
                            )
                            .child(button("一括候補生成").on_click(move |_| match read() {
                                Ok(config) => run.job(Job::Propose(
                                    config,
                                    class.get(),
                                    if ranges.get().is_empty() {
                                        None
                                    } else {
                                        Some(PathBuf::from(ranges.get()))
                                    },
                                )),
                                Err(e) => message(e.to_string()),
                            })),
                    )
                    .child(dyn_view(move || {
                        if let Some(p) = preview_data.get() {
                            div().w(480.).child(text(p.text)).child(editor::preview(
                                Some(p.bitmap),
                                p.boxes,
                                p.width,
                                p.height,
                                vec![],
                                false,
                            ))
                        } else {
                            div()
                        }
                    }))
            },
        ))
}
fn app_with(project: Option<Project>, qa_theme: Option<bool>, qa_edit: bool) -> Div {
    let initial_view = project.as_ref().map(|p| p.view.clone()).unwrap_or_default();
    let initial_edit = if qa_edit {
        project.as_ref().and_then(|p| {
            p.samples.first().map(|s| {
                let mut e = editor::Edit::new(s.clone(), p.classes.clone());
                if let Ok(path) = p.image_path(s) {
                    if let Ok(im) = image::open(path) {
                        e.bitmap = Some(bitmap(im.to_rgba8()));
                    }
                }
                e
            })
        })
    } else {
        None
    };
    let state = Rc::new(RefCell::new(State {
        project,
        ..Default::default()
    }));
    let version = signal(0u64);
    let status = signal("新規データセットを作成するか、.ylp2 を開いてください".to_owned());
    let busy = signal(false);
    let editing = signal(initial_edit.is_some());
    let edit: Signal<Option<editor::Edit>> = signal(initial_edit);
    let preview = signal(None);
    let search = signal(initial_view.search);
    let filter = signal(initial_view.status);
    let class = signal(initial_view.class);
    let sort = signal(initial_view.sort);
    let crop = signal(initial_view.crop);
    let event_state = state.clone();
    let events = ui_channel(move |event| match event {
        Event::Preview(req, result) => {
            if req != event_state.borrow().request {
                return;
            }
            busy.set(false);
            match result {
                Ok(p) => {
                    status.set(p.text.clone());
                    preview.set(Some(p));
                }
                Err(e) => status.set(format!("プレビューできません: {e}")),
            }
        }
        Event::Progress(req, a, b) => {
            if req == event_state.borrow().request {
                status.set(format!("処理中  {a} / {b}"));
            }
        }
        Event::Done(req, result, keep_view) => {
            if req != event_state.borrow().request {
                return;
            }
            busy.set(false);
            match result {
                Ok((mut p, text)) => {
                    if keep_view {
                        if let Some(current) = &event_state.borrow().project {
                            if current.id == p.id {
                                p.view = current.view.clone();
                            }
                        }
                    }
                    let view = p.view.clone();
                    let unsaved = p.needs_save;
                    let text = if unsaved {
                        format!("{text} / 未保存です。保存をやり直してください。")
                    } else {
                        text
                    };
                    let mut s = event_state.borrow_mut();
                    s.project = Some(p);
                    s.selection.clear();
                    s.cache.clear();
                    s.lru.clear();
                    s.cache_bytes = 0;
                    s.loading.clear();
                    s.dirty = unsaved;
                    drop(s);
                    batch(|| {
                        search.set(view.search);
                        filter.set(view.status);
                        class.set(view.class);
                        sort.set(view.sort);
                        crop.set(view.crop);
                        status.set(text);
                        version.update(|v| *v += 1);
                    });
                }
                Err(e) => status.set(format!("処理を完了できませんでした: {e:#}")),
            }
        }
        Event::Thumbnail(id, result) => {
            let mut s = event_state.borrow_mut();
            if result.is_ok() {
                s.loading.remove(&id);
            }
            if !s
                .project
                .as_ref()
                .is_some_and(|p| p.samples.iter().any(|x| x.id == id))
            {
                return;
            }
            if let Ok(rgba) = result {
                let bytes = rgba.len();
                if let Some(old) = s.cache.remove(&id) {
                    s.cache_bytes = s
                        .cache_bytes
                        .saturating_sub((old.width() * old.height() * 4) as usize);
                }
                s.lru.retain(|old| *old != id);
                while s.cache_bytes + bytes > 128 << 20 {
                    let Some(old) = s.lru.pop_front() else { break };
                    if let Some(b) = s.cache.remove(&old) {
                        s.cache_bytes = s
                            .cache_bytes
                            .saturating_sub((b.width() * b.height() * 4) as usize);
                    }
                }
                s.cache_bytes += bytes;
                s.lru.push_back(id);
                s.cache.insert(id, bitmap(rgba));
            }
            drop(s);
            version.update(|v| *v += 1);
        }
        Event::EditImage(id, result) => match result {
            Ok(rgba) => edit.update(|value| {
                if let Some(e) = value {
                    if e.sample.id == id {
                        e.bitmap = Some(bitmap(rgba));
                    }
                }
            }),
            Err(e) => message(e.to_string()),
        },
    });
    let (thumbnails, requests) = std::sync::mpsc::channel::<(Uuid, PathBuf, UiWork)>();
    let thumbnail_events = events.clone();
    std::thread::spawn(move || {
        while let Ok((id, path, _work)) = requests.recv() {
            let im = image::open(path)
                .map(|im| im.thumbnail(360, 220).to_rgba8())
                .map_err(Into::into);
            thumbnail_events.send(Event::Thumbnail(id, im));
        }
    });
    let c = Controller {
        state,
        version,
        status,
        busy,
        search,
        filter,
        class,
        sort,
        crop,
        edit,
        editing,
        preview,
        cancel: Arc::new(AtomicBool::new(false)),
        events,
        thumbnails,
    };
    let tc = c.clone();
    if qa_theme.is_some() {
        QA_CONTROLLER.with(|handle| *handle.borrow_mut() = Some(c.clone()));
        on_cleanup(|| QA_CONTROLLER.with(|handle| *handle.borrow_mut() = None));
    }

    if let Some(dark) = qa_theme {
        set_theme(if dark { Theme::dark() } else { Theme::light() });
    } else {
        theme::follow_system();
    }
    update_settings(|s| s.images.cache_bytes = 128 << 20);
    let filters = c.clone();
    effect(move || {
        let (q, f, k, o, r) = (
            search.get(),
            filter.get(),
            class.get(),
            sort.get(),
            crop.get(),
        );
        let mut s = filters.state.borrow_mut();
        s.selection.clear();
        if let Some(p) = &mut s.project {
            if (
                p.view.search.clone(),
                p.view.status,
                p.view.class,
                p.view.sort,
                p.view.crop,
            ) != (q.clone(), f, k, o, r)
            {
                p.view.page = 0;
                p.view.search = q;
                p.view.status = f;
                p.view.class = k;
                p.view.sort = o;
                p.view.crop = r;
            }
        }
        drop(s);
        version.update(|v| *v += 1);
    });
    let close = c.clone();
    on_close_request(move |request| {
        if close.busy.get() || close.editing.get() {
            request.prevent();
            message("処理または編集を終えてから閉じてください");
        } else if !close.save() {
            request.prevent();
        }
    });
    let config_state = c.state.clone();
    let project_id = memo(move || {
        version.get();
        config_state.borrow().project.as_ref().map(|p| p.id)
    });
    let class_state = c.state.clone();
    let class_list = memo(move || {
        version.get();
        class_state
            .borrow()
            .project
            .as_ref()
            .map(|p| p.classes.clone())
            .unwrap_or_default()
    });
    let body = c.clone();
    let editor_c = c.clone();
    let keyboard = c.clone();
    let config = c.clone();
    let count = c.clone();
    div()
        .flex()
        .flex_col()
        .gap(12_f32)
        .p(18_f32)
        .w_full()
        .h_full()
        .overflow_y_scroll()
        .dyn_style(|s| s.bg(theme().page).text_color(theme().text))
        .child(
            div()
                .flex()
                .flex_row()
                .gap(10_f32)
                .items_center()
                .child(heading("YOLO LABELER")),
        )
        .child(div().flex().flex_row().gap(6_f32).children(vec![
            action(&c, "新規", |c| c.new_dataset()),
            action(&c, "開く", |c| c.open()),
            action(&c, "保存", |c| {
                if c.allowed() {
                    c.save();
                }
            }),
            action(&c, "画像を追加", |c| c.add(false)),
            action(&c, "フォルダー追加", |c| c.add(true)),
            action(&c, "クラス設定", |c| editor::class_dialog(c)),
            action(&c, "Undo", |c| {
                c.mutate(|p, _| {
                    p.undo();
                })
            }),
            action(&c, "ZIP出力", |c| c.export()),
        ]))
        .child(div().flex().flex_row().gap(6_f32).children(vec![
            action(&c, "品質・重複検査", |c| {
                if c.project_ready() {
                    c.job(Job::Quality)
                }
            }),
            action(&c, "破損・重複を除外", |c| {
                if !c.project_ready() {
                    return;
                }
                let ids = c
                    .state
                    .borrow()
                    .project
                    .as_ref()
                    .unwrap()
                    .exclusion_candidates();
                if confirm(&format!(
                    "未確認の破損・完全重複 {}枚を除外状態にする？元画像は保持します。",
                    ids.len()
                )) {
                    c.mutate(move |p, _| p.decide(&ids, Decision::Exclude));
                }
            }),
            action(&c, "外部YOLO候補", |c| c.external()),
            action(&c, "検査", |c| {
                if let Some(p) = &c.state.borrow().project {
                    let mut e = p.validate_export();
                    e.extend(p.warnings.clone());
                    message(if e.is_empty() {
                        "出力前検査に問題はありません".into()
                    } else {
                        e.join("\n")
                    });
                }
            }),
        ]))
        .child(dyn_view(move || {
            project_id.get();
            class_list.get();
            if config.state.borrow().project.is_some() {
                configuration(config.clone()).into_view()
            } else {
                div().into_view()
            }
        }))
        .child(
            div()
                .flex()
                .flex_row()
                .gap(10_f32)
                .child(
                    text_field(search)
                        .on_key_down(|e| e.stop_propagation())
                        .placeholder("ファイル名検索")
                        .w(230.),
                )
                .child(select(
                    filter,
                    [
                        "すべて",
                        "未確認",
                        "採用",
                        "除外",
                        "保留",
                        "要修正",
                        "ラベルなし",
                        "自動候補あり",
                        "品質候補",
                    ]
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (i, (*s).into()))
                    .collect(),
                ))
                .child(dyn_view(move || {
                    let classes = class_list.get();
                    let mut options = vec![(0, "すべてのクラス".into())];
                    options.extend(classes.iter().enumerate().map(|(i, s)| (i + 1, s.clone())));
                    select(class, options)
                }))
                .child(select(
                    sort,
                    ["ファイル名順", "品質候補順", "ぼけ指標順", "完全重複順"]
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i, (*s).into()))
                        .collect(),
                ))
                .child(checkbox(crop, "対象付近を拡大")),
        )
        .child(dyn_view(move || {
            if editing.get() {
                editor::view(editor_c.clone()).into_view()
            } else {
                let body = body.clone();
                dyn_view(move || grid(body.clone()))
            }
        }))
        .child(div().flex().flex_row().gap(8_f32).children(vec![
            action(&c, "前へ", |c| c.page(-1)),
            action(&c, "次へ", |c| c.page(1)),
            action(&c, "ページ確定 →", |c| c.commit()),
            action(&c, "選択を除外", |c| {
                c.mutate(|p, s| p.decide(&s.iter().copied().collect::<Vec<_>>(), Decision::Exclude))
            }),
            action(&c, "保留", |c| {
                c.mutate(|p, s| p.decide(&s.iter().copied().collect::<Vec<_>>(), Decision::Hold))
            }),
            action(&c, "要修正", |c| {
                c.mutate(|p, s| p.decide(&s.iter().copied().collect::<Vec<_>>(), Decision::Fix))
            }),
            action(&c, "採用に戻す", |c| {
                c.mutate(|p, s| p.decide(&s.iter().copied().collect::<Vec<_>>(), Decision::Keep))
            }),
        ]))
        .child(
            text(move || {
                count.version.get();
                format!(
                    "除外予定 {}枚  · クリック/1〜0で選択、ダブルクリックで編集、Enterで確定",
                    count.state.borrow().selection.len()
                )
            })
            .text_size(12_f32),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(12_f32)
                .child(text(move || status.get()))
                .child(show(
                    move || busy.get(),
                    move || {
                        let c = tc.clone();
                        button("中止").on_click(move |_| c.cancel.store(true, Ordering::Relaxed))
                    },
                )),
        )
        .on_key_down(move |e| {
            if !keyboard.allowed() || e.is_default_prevented() {
                return;
            }
            if e.modifiers.ctrl && e.key.is_char('s') {
                keyboard.save();
                e.prevent_default();
            } else if e.modifiers.ctrl && e.key.is_char('z') {
                keyboard.mutate(|p, _| {
                    p.undo();
                });
                e.prevent_default();
            } else {
                match &e.key {
                    Key::Named(NamedKey::ArrowLeft) => keyboard.page(-1),
                    Key::Named(NamedKey::ArrowRight) => keyboard.page(1),
                    Key::Named(NamedKey::Enter) => keyboard.commit(),
                    Key::Character(s) if s.len() == 1 && "1234567890".contains(s.as_str()) => {
                        let slot = "1234567890".find(s.as_str()).unwrap();
                        if let Some(id) = keyboard.page_ids().get(slot) {
                            keyboard.toggle(*id)
                        }
                    }
                    _ => {}
                }
            }
        })
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let project = args
        .iter()
        .position(|s| s == "--project")
        .and_then(|i| args.get(i + 1))
        .map(|p| Project::load(std::path::Path::new(p)))
        .transpose()?;
    if let Some(i) = args.iter().position(|s| s == "--qa-screen") {
        let output = args.get(i + 1).context("PNGの保存先が必要です")?;
        let dark = args.iter().any(|s| s == "--dark");
        let edit = args.iter().any(|s| s == "--edit");
        let scale = args
            .iter()
            .position(|s| s == "--scale")
            .and_then(|i| args.get(i + 1))
            .map(|s| s.parse::<f64>())
            .transpose()?
            .unwrap_or(1.);
        qa::screen(
            std::path::Path::new(output),
            project,
            dark,
            edit,
            scale,
            args.iter().any(|s| s == "--qa-stress"),
            args.iter().any(|s| s == "--qa-roundtrip"),
        )?;
    } else {
        app_icon::install();
        anti_webui::App::new()
            .title("YOLO Labeler — AntiWebUI")
            .size(1480, 940)
            .run(move || app_with(project, None, false))
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    Ok(())
}
fn main() {
    let qa = std::env::args().any(|s| s == "--qa-screen");
    let result = if qa {
        std::thread::Builder::new()
            .name("headless-qa".into())
            .stack_size(16 << 20)
            .spawn(run)
            .expect("QA thread")
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("headless QA panicked")))
    } else {
        run()
    };
    if let Err(e) = result {
        if std::env::args().any(|s| s == "--qa-screen") {
            eprintln!("{e:#}");
            std::process::exit(1);
        } else {
            message(format!("起動できません: {e:#}"));
        }
    }
}
