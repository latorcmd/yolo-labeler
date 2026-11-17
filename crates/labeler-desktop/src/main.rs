#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app_icon;
mod editor;
mod help;
mod intake;
#[cfg(test)]
mod interaction_tests;
mod persistence;
mod qa;
mod theme;
mod thumbnails;
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
    save_generation: u64,
    thumbnail_slots: HashMap<Uuid, (u64, Signal<Option<Arc<Bitmap>>>)>,
    next_slot: u64,
    last_wheel: Option<std::time::Instant>,
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
    viewport: Signal<(f32, f32)>,
    keyboard_focus: NodeRef,
    edit: Signal<Option<editor::Edit>>,
    editing: Signal<bool>,
    preview: Signal<Option<Preview>>,
    cancel: Arc<AtomicBool>,
    events: UiSender<Event>,
    thumbnails: thumbnails::Loader,
    saver: persistence::Saver,
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
    Intake(
        Option<(PathBuf, Vec<String>)>,
        PathBuf,
        Vec<PathBuf>,
        Option<PathBuf>,
    ),
    Saved(Uuid, u64, std::result::Result<(), String>),
    Preview(u64, Result<Preview>),
    Progress(u64, usize, usize),
    Done(u64, Result<(Project, String)>, bool),
    Thumbnail(Uuid, Result<image::RgbaImage>),
    EditImage(Uuid, Result<image::RgbaImage>),
}
#[derive(Clone)]
enum Job {
    New(
        PathBuf,
        Vec<String>,
        PathBuf,
        Option<PathBuf>,
        labeler_core::ImportOptions,
    ),
    Load(PathBuf),
    Import(
        PathBuf,
        Vec<PathBuf>,
        Option<PathBuf>,
        labeler_core::ImportOptions,
    ),
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
fn sample_order(a: &Sample, b: &Sample) -> std::cmp::Ordering {
    labeler_core::natural_cmp(
        &a.origin_relative
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        &b.origin_relative
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
    )
    .then_with(|| {
        labeler_core::natural_cmp(
            &a.origin_root.join(&a.origin_relative).to_string_lossy(),
            &b.origin_root.join(&b.origin_relative).to_string_lossy(),
        )
    })
    .then(a.id.cmp(&b.id))
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
    fn save_request(&self) -> Option<(Project, u64)> {
        let mut state = self.state.borrow_mut();
        let p = state.project.as_mut()?;
        p.view.search = self.search.get_untracked();
        p.view.status = self.filter.get_untracked();
        p.view.class = self.class.get_untracked();
        p.view.sort = self.sort.get_untracked();
        p.view.crop = self.crop.get_untracked();
        let snapshot = p.save_snapshot();
        state.save_generation += 1;
        state.dirty = true;
        Some((snapshot, state.save_generation))
    }
    fn save(&self) -> bool {
        let Some((project, generation)) = self.save_request() else {
            return true;
        };
        self.status.set("保存中…".into());
        match self.saver.submit(project, generation) {
            Ok(()) => true,
            Err(error) => {
                self.status.set(format!("未保存: {error} / Ctrl+Sで再試行"));
                false
            }
        }
    }
    fn flush(&self) -> bool {
        {
            let state = self.state.borrow();
            if !state.dirty && state.project.as_ref().is_none_or(|p| !p.needs_save) {
                return true;
            }
        }
        let Some((project, generation)) = self.save_request() else {
            return true;
        };
        match self.saver.flush(project, generation) {
            Ok(()) => {
                let mut state = self.state.borrow_mut();
                state.dirty = false;
                if let Some(p) = &mut state.project {
                    p.needs_save = false;
                }
                self.status.set("保存済み".into());
                true
            }
            Err(error) => {
                self.status.set(format!("未保存: {error} / Ctrl+Sで再試行"));
                message(format!("保存できません。再保存してください: {error}"));
                false
            }
        }
    }
    fn job(&self, job: Job) {
        if !self.allowed() {
            return;
        }
        if !self.flush() {
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
                    Job::New(file, classes, root, labels, options) => {
                        let mut p = Project::new(file, classes)?;
                        p.save()?;
                        let paths = labeler_core::image_files(&root);
                        let n = p.import_with_options(
                            &root,
                            &paths,
                            labels.as_deref(),
                            &options,
                            &stop,
                            &mut progress,
                        )?;
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
                            Job::Import(root, paths, labels, options) => {
                                let first = p.samples.len();
                                let paths = if paths.is_empty() {
                                    labeler_core::image_files(&root)
                                } else {
                                    paths
                                };
                                let n = p.import_with_options(
                                    &root,
                                    &paths,
                                    labels.as_deref(),
                                    &options,
                                    &stop,
                                    &mut progress,
                                )?;
                                if n > 0 {
                                    let added = p.samples[first].id;
                                    let page = {
                                        let mut order = p.samples.iter().collect::<Vec<_>>();
                                        order.sort_by(|a, b| sample_order(a, b));
                                        order.iter().position(|s| s.id == added).unwrap_or(0) / 10
                                    };
                                    p.view = labeler_core::ViewState {
                                        page,
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
        intake::open(self.clone(), Some((file, classes)), root, vec![], labels);
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
                intake::open(self.clone(), None, root, vec![], labels);
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
            intake::open(self.clone(), None, root, paths, labels);
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
                        || (class == usize::MAX && s.background)
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
            _ => v.sort_by(|&a, &b| sample_order(&p.samples[a], &p.samples[b])),
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
            let page = p.view.page.saturating_add_signed(delta).min(n - 1);
            if page == p.view.page {
                return;
            }
            p.view.page = page;
            s.selection.clear();
        }
        self.save();
        self.bump();
        self.keyboard_focus.focus();
    }
    fn toggle(&self, id: Uuid) {
        if !self.project_ready() {
            return;
        }
        let decision = self
            .state
            .borrow()
            .project
            .as_ref()
            .and_then(|p| p.samples.iter().find(|s| s.id == id))
            .map(|s| s.decision);
        if let Some(decision) = decision {
            self.mutate(|p, _| {
                p.decide(
                    &[id],
                    if decision == Decision::Exclude {
                        Decision::Keep
                    } else {
                        Decision::Exclude
                    },
                )
            });
            let mut state = self.state.borrow_mut();
            state.selection.clear();
            state.selection.insert(id);
            drop(state);
            self.keyboard_focus.focus();
        }
    }
    fn wheel(&self, delta: f64) {
        if delta.abs() < 0.01 || !self.project_ready() {
            return;
        }
        let now = std::time::Instant::now();
        {
            let mut state = self.state.borrow_mut();
            let previous = state.last_wheel.replace(now);
            if previous.is_some_and(|last| now.duration_since(last).as_millis() < 180) {
                return;
            }
        }
        self.page(if delta > 0. { 1 } else { -1 });
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
        let edit = editor::Edit::new(
            s.clone(),
            (0..p.classes.len()).map(|id| p.display_class(id)).collect(),
        );
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
    fn preload(&self) {
        let visible = self.visible();
        let state = self.state.borrow();
        let Some(p) = &state.project else { return };
        let page = p.view.page;
        let mut requests = vec![];
        for current in [Some(page), page.checked_add(1), page.checked_sub(1)]
            .into_iter()
            .flatten()
        {
            for &index in visible.iter().skip(current * 10).take(10) {
                let sample = &p.samples[index];
                if !state.cache.contains_key(&sample.id) {
                    if let Ok(path) = p.image_path(sample) {
                        requests.push((sample.id, path, self.events.work()));
                    }
                }
            }
        }
        drop(state);
        self.thumbnails.replace(requests);
    }
    fn external(&self) {
        if !self.project_ready() {
            return;
        }
        let mut origins = vec![];
        for sample in &self.state.borrow().project.as_ref().unwrap().samples {
            if !origins.iter().any(|(id, _)| *id == sample.batch) {
                origins.push((sample.batch, sample.origin_root.display().to_string()));
            }
        }
        if origins.is_empty() {
            message("先に画像を追加してください");
            return;
        }
        let c = self.clone();
        open_window(
            WindowOptions {
                title: "外部候補の追加元".into(),
                width: 720,
                height: 280,
                modal: true,
                dialog: true,
                ..Default::default()
            },
            move || {
                theme::follow_system();
                let index = signal(0usize);
                let origins = origins.clone();
                let read = c.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap(14_f32)
                    .p(22_f32)
                    .dyn_style(|s| s.bg(theme().page).text_color(theme().text))
                    .child(text("候補ラベルを対応させる追加元を選んでください。"))
                    .child(
                        select(
                            index,
                            origins
                                .iter()
                                .enumerate()
                                .map(|(i, (id, path))| {
                                    (i, format!("{path} ({})", &id.to_string()[..8]))
                                })
                                .collect(),
                        )
                        .on_key_down(|e| e.stop_propagation()),
                    )
                    .child(button("候補フォルダーを選ぶ").on_click(move |_| {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("元の相対パスに対応する候補フォルダー")
                            .pick_folder()
                        {
                            read.job(Job::External(origins[index.get()].0, path));
                            close_window();
                        }
                    }))
                    .child(button("キャンセル").on_click(|_| close_window()))
            },
        );
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
    button(label.to_owned())
        .on_click(move |_| f(c.clone()))
        .on_key_down(|event| {
            if !event.modifiers.ctrl
                && matches!(event.key, Key::Named(NamedKey::Enter | NamedKey::Space))
            {
                event.stop_propagation();
            }
        })
}
fn thumbnail_card(c: Controller, s: Sample, number: usize) -> Div {
    let id = s.id;
    let mut state = c.state.borrow_mut();
    let bmp = state.cache.get(&id).cloned();
    if bmp.is_some() {
        state.lru.retain(|old| *old != id);
        state.lru.push_back(id);
    }
    let classes = state.project.as_ref().unwrap();
    let mut counts = std::collections::BTreeMap::new();
    for b in &s.boxes {
        *counts.entry(b.class_id).or_insert(0) += 1;
    }
    let labels = if s.background {
        "背景（対象なし）".into()
    } else if counts.is_empty() {
        "ラベルなし".into()
    } else {
        counts
            .iter()
            .map(|(&id, count)| format!("{} ×{count}", classes.display_class(id)))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    state.next_slot += 1;
    let token = state.next_slot;
    let slot = signal(bmp);
    state.thumbnail_slots.insert(id, (token, slot));
    drop(state);
    let cleanup = c.state.clone();
    on_cleanup(move || {
        let mut state = cleanup.borrow_mut();
        if state
            .thumbnail_slots
            .get(&id)
            .is_some_and(|(current, _)| *current == token)
        {
            state.thumbnail_slots.remove(&id);
        }
    });
    let click = c.clone();
    let edit = c.clone();
    let excluded = s.decision == Decision::Exclude;
    let crop = c.crop.get_untracked();
    let boxes = s.boxes.clone();
    let proposals = s.proposals.clone();
    let mut issues = s.label_errors.clone();
    if s.image_error.is_some() {
        issues.push("画像を読み込めません".into());
    }
    if s.proposal_pending {
        issues.push("候補の承認が必要".into());
    }
    if s.decision == Decision::Fix && issues.is_empty() {
        issues.push("枠を編集して確認してください".into());
    }
    if let Some(quality) = &s.quality {
        if !quality.flags.is_empty() {
            issues.push(format!("参考: {}", quality.flags.join(" / ")));
        }
    }
    let image_height = (c.viewport.get().1 / 2. - 130. - if issues.is_empty() { 0. } else { 24. })
        .clamp(24., 160.);
    let automatic = if s.automatic_label { " · 自動" } else { "" };
    div()
        .flex()
        .flex_col()
        .w(pct(19.2))
        .min_w(130.)
        .gap(2_f32)
        .p(5_f32)
        .rounded(8_f32)
        .border(if excluded { 3_f32 } else { 1_f32 })
        .dyn_style(move |style| {
            style.bg(theme().surface).border_color(if excluded {
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
            .text_size(13_f32)
            .max_h(20.)
            .overflow_hidden(),
        )
        .child(dyn_view(move || {
            editor::preview_sized(
                slot.get(),
                boxes.clone(),
                s.width,
                s.height,
                proposals.clone(),
                crop,
                image_height,
            )
        }))
        .child(text(labels).text_size(13_f32).max_h(32.).overflow_hidden())
        .child(
            text(format!(
                "{} · {}枠{automatic}",
                s.decision.label(),
                s.boxes.len()
            ))
            .text_size(12_f32),
        )
        .child(if issues.is_empty() {
            div().into_view()
        } else {
            text(issues.join(" / "))
                .text_size(11_f32)
                .max_h(16.)
                .overflow_hidden()
                .into_view()
        })
        .child(button("枠を編集").on_click(move |e| {
            e.stop_propagation();
            edit.edit_sample(id);
        }))
        .on_click(move |_| click.toggle(id))
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
    let filename = p
        .file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    drop(state);
    c.preload();
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
    let class_state = c.state.clone();
    let class_version = c.version;
    let classes = memo(move || {
        class_version.get();
        class_state
            .borrow()
            .project
            .as_ref()
            .map(|p| {
                (0..p.classes.len())
                    .map(|id| p.display_class(id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
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
                    .child(dyn_view(move || {
                        select(
                            class,
                            classes
                                .get()
                                .iter()
                                .enumerate()
                                .map(|(i, name)| (i, name.clone()))
                                .collect(),
                        )
                    }))
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
    let needs_save = project.as_ref().is_some_and(|p| p.needs_save);
    let initial_status = if project.is_some() {
        "読み込み済み — 不要な画像だけクリックして除外"
    } else {
        "画像フォルダーから新規作成するか、.ylp2を開いてください"
    };
    let initial_view = project.as_ref().map(|p| p.view.clone()).unwrap_or_default();
    let initial_edit = if qa_edit {
        project.as_ref().and_then(|p| {
            p.samples.first().map(|s| {
                let mut e = editor::Edit::new(
                    s.clone(),
                    (0..p.classes.len()).map(|id| p.display_class(id)).collect(),
                );
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
        dirty: needs_save,
        ..Default::default()
    }));
    let version = signal(0u64);
    let status = signal(initial_status.to_owned());
    let busy = signal(false);
    let editing = signal(initial_edit.is_some());
    let edit: Signal<Option<editor::Edit>> = signal(initial_edit);
    let preview = signal(None);
    let search = signal(initial_view.search);
    let filter = signal(initial_view.status);
    let class = signal(initial_view.class);
    let sort = signal(initial_view.sort);
    let crop = signal(initial_view.crop);
    let viewport = signal((1480_f32, 720_f32));
    let keyboard_root = node_ref();
    let event_state = state.clone();
    let controller_slot = Rc::new(RefCell::new(None::<Controller>));
    let event_controller = controller_slot.clone();
    let events = ui_channel(move |event| match event {
        Event::Intake(new, root, paths, labels) => {
            if let Some(controller) = event_controller.borrow().clone() {
                intake::open(controller, new, root, paths, labels);
            }
        }
        Event::Saved(id, generation, result) => {
            let mut state = event_state.borrow_mut();
            if state.project.as_ref().is_none_or(|p| p.id != id)
                || state.save_generation != generation
            {
                return;
            }
            match result {
                Ok(()) => {
                    state.dirty = false;
                    if let Some(p) = &mut state.project {
                        p.needs_save = false;
                    }
                    status.set("保存済み".into());
                }
                Err(error) => {
                    state.dirty = true;
                    status.set(format!("未保存: {error} / Ctrl+Sで再試行"));
                }
            }
        }
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
                    s.save_generation += 1;
                    s.thumbnail_slots.clear();
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
            if !s
                .project
                .as_ref()
                .is_some_and(|p| p.samples.iter().any(|x| x.id == id))
            {
                return;
            }
            let slot = s.thumbnail_slots.get(&id).map(|(_, slot)| *slot);
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
                let bitmap = bitmap(rgba);
                s.cache_bytes += bytes;
                s.lru.push_back(id);
                s.cache.insert(id, bitmap.clone());
                drop(s);
                if let Some(slot) = slot {
                    slot.set(Some(bitmap));
                }
            }
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
    let thumbnails = thumbnails::Loader::new(events.clone());
    let save_events = events.clone();
    let saver = persistence::Saver::new(move |id, generation, result| {
        save_events.send(Event::Saved(id, generation, result))
    });
    let close_saver = saver.clone();
    on_cleanup(move || close_saver.close());
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
        viewport,
        keyboard_focus: keyboard_root,
        edit,
        editing,
        preview,
        cancel: Arc::new(AtomicBool::new(false)),
        events,
        thumbnails,
        saver,
    };
    *controller_slot.borrow_mut() = Some(c.clone());
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
    if needs_save && qa_theme.is_none() {
        c.save();
    }
    let filters = c.clone();
    effect(move || {
        let (q, f, k, o, r) = (
            search.get(),
            filter.get(),
            class.get(),
            sort.get(),
            crop.get(),
        );
        let mut changed = false;
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
                changed = true;
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
        if changed && filters.allowed() {
            filters.save();
        }
    });
    let close = c.clone();
    on_close_request(move |request| {
        if close.busy.get() || close.editing.get() {
            request.prevent();
            message("処理または編集を終えてから閉じてください");
        } else if !close.flush() {
            request.prevent();
        }
    });
    let class_state = c.state.clone();
    let class_list = memo(move || {
        version.get();
        class_state
            .borrow()
            .project
            .as_ref()
            .map(|p| {
                (0..p.classes.len())
                    .map(|id| p.display_class(id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    let body = c.clone();
    let editor_c = c.clone();
    let keyboard = c.clone();
    let pager = c.clone();
    let wheel = c.clone();
    let footer = c.clone();
    let cancel_job = c.clone();
    let initial_focus = std::cell::Cell::new(false);
    div()
        .relative()
        .focusable()
        .child(
            div()
                .absolute()
                .left(0.)
                .top(0.)
                .w(1.)
                .h(1.)
                .focusable()
                .node_ref(keyboard_root),
        )
        .on_resized(move |_, _| {
            if !initial_focus.replace(true) {
                keyboard_root.focus();
            }
        })
        .flex()
        .flex_col()
        .gap(8_f32)
        .p(14_f32)
        .w_full()
        .h_full()
        .min_h(0.)
        .overflow_hidden()
        .dyn_style(|s| s.bg(theme().page).text_color(theme().text))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(6_f32)
                .items_center()
                .child(heading("YOLO LABELER"))
                .children(vec![
                    action(&c, "新規", |c| c.new_dataset()),
                    action(&c, "開く", |c| c.open()),
                    action(&c, "画像を追加", |c| c.add(false)),
                    action(&c, "フォルダー追加", |c| c.add(true)),
                    action(&c, "Undo", |c| {
                        if c.editing.get() {
                            editor::undo_current(&c);
                        } else {
                            c.mutate(|p, _| {
                                p.undo();
                            });
                        }
                    }),
                    action(&c, "ZIP出力", |c| c.export()),
                    action(&c, "詳細・設定", advanced),
                    button("ヘルプ (F1)").on_click(|_| help::open()),
                ]),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(8_f32)
                .child(
                    text_field(search)
                        .on_key_down(guard_input)
                        .placeholder("ファイル名で検索")
                        .w(220.),
                )
                .child(
                    select(
                        filter,
                        [
                            "すべて",
                            "未判断",
                            "採用",
                            "除外",
                            "保留",
                            "要確認",
                            "ラベルなし",
                            "未承認候補",
                            "品質候補",
                        ]
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i, (*s).into()))
                        .collect(),
                    )
                    .on_key_down(guard_input),
                )
                .child(dyn_view(move || {
                    let names = class_list.get();
                    let mut options = vec![(0, "全ラベル".into())];
                    options.extend(names.into_iter().enumerate().map(|(i, s)| (i + 1, s)));
                    options.push((usize::MAX, "背景（対象なし）".into()));
                    select(class, options).on_key_down(guard_input)
                }))
                .child(
                    select(
                        sort,
                        [
                            "ファイル名順（数字順）",
                            "品質候補順",
                            "ぼけ指標順",
                            "完全重複順",
                        ]
                        .iter()
                        .enumerate()
                        .map(|(i, s)| (i, (*s).into()))
                        .collect(),
                    )
                    .on_key_down(guard_input),
                )
                .child(checkbox(crop, "対象を拡大")),
        )
        .child(
            div()
                .flex_1()
                .min_h(0.)
                .on_resized(move |w, h| {
                    viewport.set_if_changed((w, h));
                })
                .overflow_y_scroll()
                .child(dyn_view(move || {
                    if editing.get() {
                        editor::view(editor_c.clone()).into_view()
                    } else {
                        let body = body.clone();
                        dyn_view(move || grid(body.clone()))
                    }
                }))
                .on_scroll(move |e| {
                    if wheel.allowed() && !e.modifiers.ctrl && !e.modifiers.alt {
                        e.stop_propagation();
                        wheel.wheel(e.delta_y as f64);
                    }
                }),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(10_f32)
                .items_center()
                .child(action(&c, "← 前へ (A)", |c| c.page(-1)))
                .child(dyn_view(move || {
                    pager.version.get();
                    let state = pager.state.borrow();
                    let page = state.project.as_ref().map(|p| p.view.page + 1).unwrap_or(0);
                    drop(state);
                    text(format!(
                        "{page} / {} ページ",
                        pager.visible().len().div_ceil(10).max(1)
                    ))
                }))
                .child(action(&c, "次へ (D) →", |c| c.page(1)))
                .child(action(&c, "保存 (Ctrl+S)", |c| {
                    if c.editing.get() {
                        editor::save_current(c);
                    } else if c.allowed() {
                        c.save();
                    }
                }))
                .child(
                    text(move || status.get())
                        .text_size(12_f32)
                        .flex_1()
                        .min_w(0.)
                        .max_h(36.)
                        .overflow_hidden(),
                )
                .child(show(
                    move || busy.get(),
                    move || {
                        let c = cancel_job.clone();
                        button("処理を中止")
                            .on_click(move |_| c.cancel.store(true, Ordering::Relaxed))
                    },
                )),
        )
        .child(
            text(move || {
                if footer.editing.get() {
                    help::EDIT_KEYS
                } else {
                    help::LIST_KEYS
                }
            })
            .text_size(12_f32),
        )
        .on_key_down(move |e| {
            if e.key == Key::Named(NamedKey::F(1)) {
                e.prevent_default();
                help::open();
                return;
            }
            if keyboard.editing.get() {
                editor::shortcut(&keyboard, e);
                return;
            }
            if !keyboard.allowed() || e.is_default_prevented() {
                return;
            }
            if e.modifiers.ctrl && !e.modifiers.alt && !e.modifiers.logo {
                if e.key.is_char('s') {
                    keyboard.save();
                } else if e.key.is_char('z') {
                    keyboard.mutate(|p, _| {
                        p.undo();
                    });
                } else if e.key.is_char('n') {
                    keyboard.new_dataset();
                } else if e.key.is_char('o') {
                    keyboard.open();
                } else {
                    return;
                }
                e.prevent_default();
                return;
            }
            if e.modifiers.ctrl || e.modifiers.alt || e.modifiers.logo || e.repeat {
                return;
            }
            match &e.key {
                Key::Named(NamedKey::ArrowLeft) => keyboard.page(-1),
                Key::Named(NamedKey::ArrowRight | NamedKey::Enter) => keyboard.page(1),
                key if key.is_char('a') => keyboard.page(-1),
                key if key.is_char('d') => keyboard.page(1),
                Key::Character(s) if s.len() == 1 && "1234567890".contains(s.as_str()) => {
                    if let Some(id) = keyboard
                        .page_ids()
                        .get("1234567890".find(s.as_str()).unwrap())
                    {
                        keyboard.toggle(*id);
                    }
                }
                _ => return,
            }
            e.prevent_default();
        })
}
fn keyboard_surface(view: Div) -> Div {
    let target = node_ref();
    let focused = std::cell::Cell::new(false);
    view.focusable().node_ref(target).on_resized(move |_, _| {
        if !focused.replace(true) {
            target.focus();
        }
    })
}
fn guard_input(e: &anti_webui::core::event::KeyboardEvent) {
    if !(e.modifiers.ctrl && e.key.is_char('s')) && e.key != Key::Named(NamedKey::F(1)) {
        e.stop_propagation();
    }
}
fn advanced(c: Controller) {
    if !c.project_ready() {
        return;
    }
    open_window(
        WindowOptions {
            title: "詳細・設定".into(),
            width: 900,
            height: 740,
            modal: true,
            dialog: true,
            ..Default::default()
        },
        move || {
            theme::follow_system();
            let quality = c.clone();
            let exclude = c.clone();
            let external = c.clone();
            let check = c.clone();
            keyboard_surface(div())
                .flex()
                .flex_col()
                .gap(12_f32)
                .p(20_f32)
                .h_full()
                .overflow_y_scroll()
                .dyn_style(|s| s.bg(theme().page).text_color(theme().text))
                .child(heading("ラベルと検査"))
                .child(action(
                    &c,
                    "ラベル表示名の対応表",
                    editor::display_names_dialog,
                ))
                .child(action(&c, "保存用クラスの設定", editor::class_dialog))
                .child(button("品質・完全重複を検査").on_click(move |_| quality.job(Job::Quality)))
                .child(button("検査済みの破損・完全重複を除外").on_click(move |_| {
                    let ids = exclude
                        .state
                        .borrow()
                        .project
                        .as_ref()
                        .map(|p| p.exclusion_candidates())
                        .unwrap_or_default();
                    if confirm(&format!("{}枚を除外する？元画像は保持します。", ids.len()))
                    {
                        exclude.mutate(|p, _| p.decide(&ids, Decision::Exclude));
                    }
                }))
                .child(button("外部YOLO候補を読み込む").on_click(move |_| external.external()))
                .child(button("出力前の検査").on_click(move |_| {
                    if let Some(p) = check.state.borrow().project.as_ref() {
                        let errors = p.validate_export();
                        message(if errors.is_empty() {
                            "出力できます".into()
                        } else {
                            errors.join("\n")
                        });
                    }
                }))
                .child(configuration(c.clone()))
                .child(button("閉じる (Esc)").on_click(|_| close_window()))
                .on_key_down(|e| {
                    if e.key == Key::Named(NamedKey::Escape) {
                        close_window();
                    }
                    e.stop_propagation();
                })
        },
    );
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
            .title("YOLO Labeler")
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
