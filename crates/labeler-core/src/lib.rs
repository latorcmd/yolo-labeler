//! GUI-independent, non-destructive project operations.
use anyhow::{Context, Result, ensure};
pub use labeler_vision::augment::AugmentOptions;
pub use labeler_vision::{DetectorConfig, NormalizedBox, Quality};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const MIN_IMAGES: usize = 101;
pub const MAX_ZIP_BYTES: u64 = 200_000_000;
const EPS: f64 = 1e-6;
const REMOVE_SAMPLES_ACTION: &str = "一括削除";
pub type Progress<'a> = &'a mut dyn FnMut(usize, usize);

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOrigin {
    #[default]
    AutoDefault,
    Manual,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportLabelSource {
    #[default]
    Existing,
    Otsu,
}

#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    pub background: bool,
    pub label_source: ImportLabelSource,
    pub fixed_class: usize,
    pub ranges: Option<PathBuf>,
    pub detector: DetectorConfig,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    #[default]
    Pending,
    Keep,
    Exclude,
    Hold,
    Fix,
}
impl Decision {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "未確認",
            Self::Keep => "採用",
            Self::Exclude => "除外",
            Self::Hold => "保留",
            Self::Fix => "要確認",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    pub id: Uuid,
    pub batch: Uuid,
    pub path: PathBuf,
    pub origin_root: PathBuf,
    pub origin_relative: PathBuf,
    pub source_hash: String,
    pub width: u32,
    pub height: u32,
    pub boxes: Vec<NormalizedBox>,
    pub has_label: bool,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub decision_origin: DecisionOrigin,
    #[serde(default)]
    pub automatic_label: bool,
    pub label_errors: Vec<String>,
    pub image_error: Option<String>,
    pub decision: Decision,
    pub reviewed: bool,
    pub reason: String,
    pub proposals: Vec<NormalizedBox>,
    pub proposal_pending: bool,
    pub proposal_info: serde_json::Value,
    pub quality: Option<Quality>,
    pub duplicate_of: Option<Uuid>,
    pub duplicate_conflict: bool,
}
impl Sample {
    pub fn approvable(&self) -> bool {
        self.has_label
            && ((!self.boxes.is_empty() && !self.background)
                || (self.boxes.is_empty() && self.background))
            && self.label_errors.is_empty()
            && self.image_error.is_none()
            && !self.proposal_pending
            && self.proposals.is_empty()
    }
    pub fn protected(&self) -> bool {
        self.reviewed || self.decision_origin == DecisionOrigin::Manual
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ViewState {
    pub page: usize,
    pub search: String,
    pub status: usize,
    pub class: usize,
    pub sort: usize,
    pub crop: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Audit {
    pub at: u64,
    pub action: String,
    pub samples: Vec<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub format: String,
    pub version: u32,
    pub id: Uuid,
    pub classes: Vec<String>,
    #[serde(default)]
    pub class_display_names: Vec<String>,
    pub samples: Vec<Sample>,
    pub view: ViewState,
    pub audit: Vec<Audit>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub file: PathBuf,
    #[serde(skip)]
    pub undo: VecDeque<Vec<(usize, Sample)>>,
    #[serde(skip)]
    pub revision: u64,
    #[serde(skip)]
    pub needs_save: bool,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn cancelled(flag: &AtomicBool) -> Result<()> {
    ensure!(!flag.load(Ordering::Relaxed), "処理を中止しました");
    Ok(())
}
pub fn validate_classes(classes: &[String]) -> Result<()> {
    ensure!(!classes.is_empty(), "クラスを1つ以上設定してください");
    let mut seen = HashSet::new();
    for c in classes {
        ensure!(
            !c.is_empty() && c.trim() == c && !c.contains(['\r', '\n']),
            "クラス名が不正です"
        );
        ensure!(seen.insert(c), "クラス名が重複しています");
    }
    Ok(())
}
pub fn validate_box(b: &NormalizedBox, n: usize) -> Result<()> {
    ensure!(b.class_id < n, "クラスIDが範囲外です");
    ensure!(
        [b.cx, b.cy, b.width, b.height]
            .iter()
            .all(|v| v.is_finite()),
        "座標にNaNまたは無限大があります"
    );
    ensure!(b.width > 0. && b.height > 0., "幅と高さは正の数が必要です");
    ensure!(
        b.cx - b.width / 2. >= -EPS
            && b.cy - b.height / 2. >= -EPS
            && b.cx + b.width / 2. <= 1. + EPS
            && b.cy + b.height / 2. <= 1. + EPS,
        "枠が画像の範囲外です"
    );
    Ok(())
}
pub fn parse_yolo(text: &str, n: usize) -> (Vec<NormalizedBox>, Vec<String>) {
    let mut boxes = vec![];
    let mut errors = vec![];
    for (i, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let result = (|| -> Result<NormalizedBox> {
            let p: Vec<_> = line.split_whitespace().collect();
            ensure!(p.len() == 5, "5項目が必要です");
            let mut b = NormalizedBox {
                class_id: p[0].parse()?,
                cx: p[1].parse()?,
                cy: p[2].parse()?,
                width: p[3].parse()?,
                height: p[4].parse()?,
            };
            validate_box(&b, n)?;
            let (l, t, r, d) = (
                (b.cx - b.width / 2.).max(0.),
                (b.cy - b.height / 2.).max(0.),
                (b.cx + b.width / 2.).min(1.),
                (b.cy + b.height / 2.).min(1.),
            );
            if b.cx - b.width / 2. < 0.
                || b.cy - b.height / 2. < 0.
                || b.cx + b.width / 2. > 1.
                || b.cy + b.height / 2. > 1.
            {
                b.cx = (l + r) / 2.;
                b.cy = (t + d) / 2.;
                b.width = r - l;
                b.height = d - t;
            }
            Ok(b)
        })();
        match result {
            Ok(b) => boxes.push(b),
            Err(e) => errors.push(format!("{}行目: {e}", i + 1)),
        }
    }
    (boxes, errors)
}
pub fn yolo_text(boxes: &[NormalizedBox]) -> String {
    boxes
        .iter()
        .map(|b| {
            format!(
                "{} {:.9} {:.9} {:.9} {:.9}\n",
                b.class_id, b.cx, b.cy, b.width, b.height
            )
        })
        .collect()
}
pub fn safe_join(root: &Path, rel: &Path) -> Result<PathBuf> {
    ensure!(
        !rel.as_os_str().is_empty() && rel.components().all(|p| matches!(p, Component::Normal(_))),
        "不正な相対パスです"
    );
    let joined = root.join(rel);
    // Check existing ancestors too: never follow a junction/symlink out of assets.
    let canonical_root = fs::canonicalize(root)?;
    let mut ancestor = joined.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().context("パスが不正です")?;
    }
    ensure!(
        fs::canonicalize(ancestor)?.starts_with(canonical_root),
        "画像フォルダー外のパスです"
    );
    Ok(joined)
}
pub fn image_files(root: &Path) -> Vec<PathBuf> {
    let mut files = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let s = e.file_name().to_string_lossy();
            e.depth() == 0
                || (!s.starts_with('.')
                    && !matches!(
                        s.as_ref(),
                        "target" | "node_modules" | "exports" | "__pycache__"
                    )
                    && !s.ends_with(".ylp2.assets"))
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_image(e.path()))
        .map(|e| e.into_path())
        .collect::<Vec<_>>();
    files.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    files
}
/// Case-insensitive natural order without parsing numeric runs into a fixed-size integer.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let lower_a = a.to_lowercase();
    let lower_b = b.to_lowercase();
    let (a, b) = (lower_a.as_bytes(), lower_b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (start_i, start_j) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let mut significant_i = start_i;
            let mut significant_j = start_j;
            while significant_i + 1 < i && a[significant_i] == b'0' {
                significant_i += 1;
            }
            while significant_j + 1 < j && b[significant_j] == b'0' {
                significant_j += 1;
            }
            let order = (i - significant_i)
                .cmp(&(j - significant_j))
                .then_with(|| a[significant_i..i].cmp(&b[significant_j..j]))
                .then_with(|| (i - start_i).cmp(&(j - start_j)));
            if order != Ordering::Equal {
                return order;
            }
        } else {
            let order = a[i].cmp(&b[j]);
            if order != Ordering::Equal {
                return order;
            }
            i += 1;
            j += 1;
        }
    }
    a.len().cmp(&b.len())
}
pub fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|x| x.to_str())
        .map(|x| {
            matches!(
                x.to_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "bmp" | "webp"
            )
        })
        .unwrap_or(false)
}
pub fn discover_classes(root: &Path, labels: Option<&Path>) -> Result<Vec<String>> {
    let mut candidates = vec![root.join("classes.txt"), root.join("labels/classes.txt")];
    if let Some(l) = labels {
        candidates.insert(0, l.join("classes.txt"));
    }
    for p in candidates {
        if p.is_file() {
            let text = fs::read_to_string(p)?;
            let classes = text
                .trim_start_matches('\u{feff}')
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            validate_classes(&classes)?;
            return Ok(classes);
        }
    }
    Ok(vec![])
}
fn label_path(root: &Path, rel: &Path, labels: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = vec![];
    if let Some(l) = labels {
        candidates.push(
            l.join(rel.strip_prefix("images").unwrap_or(rel))
                .with_extension("txt"),
        );
    } else {
        candidates.push(root.join(rel).with_extension("txt"));
        let parts = rel
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect::<Vec<_>>();
        if let Some(i) = parts.iter().position(|s| s == "images") {
            let mut replaced = PathBuf::new();
            for (j, p) in parts.iter().enumerate() {
                replaced.push(if j == i {
                    std::ffi::OsStr::new("labels")
                } else {
                    p
                });
            }
            candidates.push(root.join(replaced).with_extension("txt"));
        }
    }
    candidates
        .into_iter()
        .find(|p| p.is_file() && !p.is_symlink())
}
impl Project {
    pub fn new(file: PathBuf, classes: Vec<String>) -> Result<Self> {
        validate_classes(&classes)?;
        ensure!(
            file.extension().is_some_and(|s| s == "ylp2"),
            "保存形式は .ylp2 です"
        );
        ensure!(!file.exists(), "同名のプロジェクトがあります");
        let file = std::path::absolute(file)?;
        Ok(Self {
            format: "yolo-labeler-rust".into(),
            version: 2,
            id: Uuid::new_v4(),
            classes,
            class_display_names: vec![],
            samples: vec![],
            view: ViewState::default(),
            audit: vec![],
            warnings: vec![],
            file,
            undo: VecDeque::new(),
            revision: 0,
            needs_save: false,
        })
    }
    pub fn assets(&self) -> PathBuf {
        self.file.with_extension("ylp2.assets")
    }
    /// Clone persistent state for the writer without copying the 40-operation Undo history.
    pub fn save_snapshot(&self) -> Self {
        Self {
            format: self.format.clone(),
            version: self.version,
            id: self.id,
            classes: self.classes.clone(),
            class_display_names: self.class_display_names.clone(),
            samples: self.samples.clone(),
            view: self.view.clone(),
            audit: self.audit.clone(),
            warnings: self.warnings.clone(),
            file: self.file.clone(),
            undo: VecDeque::new(),
            revision: self.revision,
            needs_save: self.needs_save,
        }
    }
    pub fn preflight_background_conflicts(
        &self,
        root: &Path,
        paths: &[PathBuf],
        labels: Option<&Path>,
    ) -> Result<Vec<PathBuf>> {
        let root = fs::canonicalize(root)?;
        let root = root
            .ancestors()
            .find(|p| p.file_name().is_some_and(|s| s == "images"))
            .and_then(Path::parent)
            .unwrap_or(&root);
        let mut conflicts = vec![];
        for path in paths {
            ensure!(
                !path.is_symlink() && is_image(path),
                "対応する画像ではありません: {}",
                path.display()
            );
            let original = fs::canonicalize(path)?;
            ensure!(original.starts_with(root), "追加元フォルダー外の画像です");
            if self
                .samples
                .iter()
                .any(|s| s.origin_root.join(&s.origin_relative) == original)
            {
                continue;
            }
            let rel = original.strip_prefix(root)?;
            if let Some(label) = label_path(root, rel, labels) {
                if let Ok(text) = fs::read_to_string(label) {
                    let (boxes, errors) = parse_yolo(&text, self.classes.len());
                    if !boxes.is_empty() && errors.is_empty() {
                        conflicts.push(rel.to_path_buf());
                    }
                }
            }
        }
        Ok(conflicts)
    }
    pub fn image_path(&self, s: &Sample) -> Result<PathBuf> {
        safe_join(&self.assets(), &s.path)
    }
    pub fn load(file: &Path) -> Result<Self> {
        ensure!(
            file.extension().is_some_and(|s| s == "ylp2"),
            "旧 .ylproject は非対応です。.ylp2 を選んでください"
        );
        let mut p: Self = serde_json::from_slice(&fs::read(file)?)?;
        ensure!(
            p.format == "yolo-labeler-rust" && matches!(p.version, 1 | 2),
            "非対応のプロジェクト形式です"
        );
        validate_classes(&p.classes)?;
        p.file = fs::canonicalize(file)?;
        fs::create_dir_all(p.assets())?;
        let mut ids = HashSet::new();
        for s in &p.samples {
            ensure!(ids.insert(s.id), "画像UUIDが重複しています");
            safe_join(&p.assets(), &s.path)?;
            for b in s.boxes.iter().chain(s.proposals.iter()) {
                validate_box(b, p.classes.len())?;
            }
        }
        p.recover_imports()?;
        if p.version == 1 {
            for s in &mut p.samples {
                s.background = s.has_label && s.boxes.is_empty() && s.label_errors.is_empty();
                if s.decision == Decision::Pending && !s.reviewed {
                    s.decision = if s.approvable() {
                        Decision::Keep
                    } else {
                        Decision::Fix
                    };
                    s.decision_origin = DecisionOrigin::AutoDefault;
                } else {
                    s.decision_origin = DecisionOrigin::Manual;
                }
            }
            p.version = 2;
            p.needs_save = true;
        }
        Ok(p)
    }
    pub fn display_class(&self, id: usize) -> String {
        let Some(original) = self.classes.get(id) else {
            return format!("#{id}");
        };
        match self
            .class_display_names
            .get(id)
            .map(String::as_str)
            .filter(|s| !s.is_empty() && *s != original)
        {
            Some(display) => format!("{display} ({original})"),
            None => original.clone(),
        }
    }
    pub fn set_display_names(&mut self, names: Vec<String>) -> Result<()> {
        ensure!(
            names.len() <= self.classes.len(),
            "表示名の数がクラス数を超えています"
        );
        ensure!(
            names.iter().all(|n| !n.contains(['\r', '\n'])),
            "表示名は1行で指定してください"
        );
        self.class_display_names = names.into_iter().map(|n| n.trim().to_owned()).collect();
        self.log("ラベル表示名変更", vec![]);
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        validate_classes(&self.classes)?;
        let parent = self.file.parent().context("保存先が不正です")?;
        fs::create_dir_all(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut tmp, self)?;
        tmp.as_file().sync_all()?;
        if self.file.exists() {
            fs::copy(&self.file, self.file.with_extension("ylp2.bak"))?;
        }
        tmp.persist(&self.file).map_err(|e| e.error)?;
        let journals = self.assets().join("transactions");
        if journals.exists() {
            let registered = self.registered_sample_ids();
            for e in fs::read_dir(journals)? {
                let path = e?.path();
                if path.extension().is_some_and(|s| s == "json") {
                    if let Ok(samples) = fs::read(&path).and_then(|b| {
                        serde_json::from_slice::<Vec<Sample>>(&b).map_err(std::io::Error::other)
                    }) {
                        if samples.iter().all(|s| registered.contains(&s.id)) {
                            fs::remove_file(path)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn registered_sample_ids(&self) -> HashSet<Uuid> {
        self.samples
            .iter()
            .map(|s| s.id)
            .chain(
                self.audit
                    .iter()
                    .filter(|a| a.action == REMOVE_SAMPLES_ACTION)
                    .flat_map(|a| a.samples.iter().copied()),
            )
            .collect()
    }
    fn recover_imports(&mut self) -> Result<()> {
        let transactions = self.assets().join("transactions");
        if !transactions.exists() {
            return Ok(());
        }
        let mut registered = self.registered_sample_ids();
        for e in fs::read_dir(transactions)? {
            let path = e?.path();
            if path.extension().is_none_or(|s| s != "json") {
                continue;
            }
            let samples: Vec<Sample> = serde_json::from_slice(&fs::read(&path)?)?;
            for s in samples {
                if !registered.contains(&s.id) && self.image_path(&s)?.is_file() {
                    registered.insert(s.id);
                    self.samples.push(s);
                    self.needs_save = true;
                    self.warnings
                        .push("中断された画像追加を復旧しました。保存してください。".into());
                }
            }
        }
        Ok(())
    }
    pub fn import(
        &mut self,
        root: &Path,
        paths: &[PathBuf],
        labels: Option<&Path>,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<usize> {
        self.import_internal(root, paths, labels, None, cancel, progress)
    }
    pub fn import_with_options(
        &mut self,
        root: &Path,
        paths: &[PathBuf],
        labels: Option<&Path>,
        options: &ImportOptions,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<usize> {
        self.import_internal(root, paths, labels, Some(options), cancel, progress)
    }
    fn import_internal(
        &mut self,
        root: &Path,
        paths: &[PathBuf],
        labels: Option<&Path>,
        options: Option<&ImportOptions>,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<usize> {
        let root = fs::canonicalize(root)?;
        let root = root
            .ancestors()
            .find(|p| p.file_name().is_some_and(|s| s == "images"))
            .and_then(Path::parent)
            .unwrap_or(&root)
            .to_path_buf();
        let use_existing_labels =
            !options.is_some_and(|o| o.label_source == ImportLabelSource::Otsu);
        if use_existing_labels {
            let classes = discover_classes(&root, labels)?;
            ensure!(
                classes.is_empty() || classes == self.classes,
                "追加元の classes.txt の順序・名称が一致しません"
            );
        }
        let ranges = if let Some(options) = options.filter(|o| !o.background) {
            ensure!(
                options.fixed_class < self.classes.len(),
                "クラスIDが範囲外です"
            );
            options
                .ranges
                .as_ref()
                .map(|p| read_ranges(p, self.classes.len()))
                .transpose()?
        } else {
            None
        };
        if use_existing_labels && options.is_some_and(|o| o.background) {
            let mut conflicts = vec![];
            for path in paths {
                cancelled(cancel)?;
                ensure!(
                    !path.is_symlink() && is_image(path),
                    "対応する画像ではありません: {}",
                    path.display()
                );
                let original = fs::canonicalize(path)?;
                ensure!(original.starts_with(&root), "追加元フォルダー外の画像です");
                if self
                    .samples
                    .iter()
                    .any(|s| s.origin_root.join(&s.origin_relative) == original)
                {
                    continue;
                }
                let rel = original.strip_prefix(&root)?;
                if let Some(label) = label_path(&root, rel, labels) {
                    if let Ok(text) = fs::read_to_string(label) {
                        let (boxes, errors) = parse_yolo(&text, self.classes.len());
                        if !boxes.is_empty() && errors.is_empty() {
                            conflicts.push(rel.display().to_string());
                        }
                    }
                }
            }
            ensure!(
                conflicts.is_empty(),
                "対象なしの指定と既存の対象枠が矛盾しています。対象ありへ変更するか、追加を中止してください:\n{}",
                conflicts.join("\n")
            );
        }
        if !self.file.exists() {
            self.save()?;
        }
        fs::create_dir_all(self.assets().join("batches"))?;
        fs::create_dir_all(self.assets().join("transactions"))?;
        let batch = Uuid::new_v4();
        let stage = tempfile::Builder::new()
            .prefix(".import-")
            .tempdir_in(self.assets())?;
        let mut known = self
            .samples
            .iter()
            .map(|s| s.origin_root.join(&s.origin_relative))
            .collect::<HashSet<_>>();
        let mut used_stems = self
            .samples
            .iter()
            .filter_map(|s| s.path.file_stem())
            .map(|stem| stem.to_string_lossy().to_lowercase())
            .collect::<HashSet<_>>();
        let mut added = vec![];
        for (i, path) in paths.iter().enumerate() {
            cancelled(cancel)?;
            ensure!(
                !path.is_symlink() && is_image(path),
                "対応する画像ではありません: {}",
                path.display()
            );
            let original = fs::canonicalize(path)?;
            ensure!(original.starts_with(&root), "追加元フォルダー外の画像です");
            if !known.insert(original.clone()) {
                progress(i + 1, paths.len());
                continue;
            }
            let rel = original.strip_prefix(&root)?.to_path_buf();
            let id = Uuid::new_v4();
            let stem = original
                .file_stem()
                .context("画像名不正")?
                .to_string_lossy();
            let mut unique_stem = stem.to_string();
            let mut suffix = 2;
            while !used_stems.insert(unique_stem.to_lowercase()) {
                unique_stem = format!("{stem}_{suffix}");
                suffix += 1;
            }
            let name = if unique_stem == stem {
                original.file_name().context("画像名不正")?.to_os_string()
            } else {
                let extension = original.extension().context("画像形式不正")?;
                let mut name = std::ffi::OsString::from(unique_stem);
                name.push(".");
                name.push(extension);
                name
            };
            let local = PathBuf::from(id.to_string()).join(name);
            let target = stage.path().join(&local);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::copy(&original, &target)?;
            let payload = fs::read(&target)?;
            let image = image::load_from_memory(&payload);
            let (width, height, image_error) = match &image {
                Ok(im) => (im.width(), im.height(), None),
                Err(e) => (0, 0, Some(e.to_string())),
            };
            let label = if use_existing_labels {
                label_path(&root, &rel, labels)
            } else {
                None
            };
            let mut has_label = label.is_some();
            let (mut boxes, mut label_errors) = if let Some(l) = label {
                fs::copy(&l, target.with_extension("txt"))?;
                match fs::read_to_string(l) {
                    Ok(t) => parse_yolo(&t, self.classes.len()),
                    Err(e) => (vec![], vec![e.to_string()]),
                }
            } else {
                (vec![], vec![])
            };
            let mut background = has_label && boxes.is_empty() && label_errors.is_empty();
            let mut automatic_label = false;
            if let Some(options) = options {
                if let Ok(image) = &image {
                    if options.background {
                        boxes.clear();
                        label_errors.clear();
                        has_label = true;
                        background = true;
                        fs::write(target.with_extension("txt"), "")?;
                    } else if boxes.is_empty() && label_errors.is_empty() {
                        background = false;
                        let class = if let Some(ranges) = &ranges {
                            class_from_ranges(&rel, ranges)?
                        } else {
                            options.fixed_class
                        };
                        let detected = match options.label_source {
                            ImportLabelSource::Existing => {
                                labeler_vision::detect(image, &options.detector)
                            }
                            ImportLabelSource::Otsu => {
                                labeler_vision::detect_otsu(image, &options.detector)
                            }
                        };
                        cancelled(cancel)?;
                        match detected {
                            Ok(mut detection) if !detection.boxes.is_empty() => {
                                for b in &mut detection.boxes {
                                    b.class_id = class;
                                }
                                boxes = detection.boxes;
                                has_label = true;
                                automatic_label = true;
                                fs::write(target.with_extension("txt"), yolo_text(&boxes))?;
                            }
                            Ok(_) => label_errors.push(
                                "対象の枠を検出できませんでした。枠を編集してください".into(),
                            ),
                            Err(error) => label_errors.push(format!("自動ラベリング失敗: {error}")),
                        }
                    }
                }
            }
            let decision = if has_label
                && (!boxes.is_empty() || background)
                && label_errors.is_empty()
                && image_error.is_none()
            {
                Decision::Keep
            } else {
                Decision::Fix
            };
            added.push(Sample {
                id,
                batch,
                path: PathBuf::from("batches").join(batch.to_string()).join(local),
                origin_root: root.clone(),
                origin_relative: rel,
                source_hash: hash(&payload),
                width,
                height,
                boxes,
                has_label,
                background,
                decision_origin: DecisionOrigin::AutoDefault,
                automatic_label,
                label_errors,
                image_error,
                decision,
                reviewed: false,
                reason: String::new(),
                proposals: vec![],
                proposal_pending: false,
                proposal_info: serde_json::Value::Null,
                quality: None,
                duplicate_of: None,
                duplicate_conflict: false,
            });
            progress(i + 1, paths.len());
        }
        cancelled(cancel)?;
        if added.is_empty() {
            return Ok(0);
        }
        let journal = self
            .assets()
            .join("transactions")
            .join(format!("{batch}.json"));
        let mut jf = fs::File::create(&journal)?;
        serde_json::to_writer(&mut jf, &added)?;
        jf.sync_all()?;
        drop(jf);
        fs::rename(
            stage.path(),
            self.assets().join("batches").join(batch.to_string()),
        )?;
        let n = added.len();
        self.log("画像追加", added.iter().map(|s| s.id).collect());
        self.samples.extend(added);
        // Once published, apply even when cancellation arrives after rename.
        // save failure leaves the journal for recovery, and returns the live project.
        if let Err(e) = self.save() {
            self.needs_save = true;
            self.warnings.push(format!(
                "追加済みですが保存に失敗しました。再保存してください: {e}"
            ));
        }
        Ok(n)
    }
    fn log(&mut self, action: &str, ids: Vec<Uuid>) {
        self.revision += 1;
        self.audit.push(Audit {
            at: now(),
            action: action.into(),
            samples: ids,
        });
    }
    pub fn checkpoint(&mut self, ids: &[Uuid]) {
        let selected = ids.iter().copied().collect::<HashSet<_>>();
        let snapshot = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, sample)| selected.contains(&sample.id))
            .map(|(index, sample)| (index, sample.clone()))
            .collect();
        self.undo.push_back(snapshot);
        while self.undo.len() > 40 {
            self.undo.pop_front();
        }
    }
    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop_back() else {
            return false;
        };
        let ids = snapshot.iter().map(|(_, sample)| sample.id).collect();
        // Edits keep sample order, and imports append. Restore deleted rows
        // in ascending index order so each row returns to its previous slot.
        for (index, before) in snapshot {
            if let Some(sample) = self.samples.get_mut(index).filter(|s| s.id == before.id) {
                *sample = before;
            } else {
                self.samples.insert(index, before);
            }
        }
        self.log("Undo", ids);
        true
    }
    pub fn decide(&mut self, ids: &[Uuid], decision: Decision) {
        self.checkpoint(ids);
        for s in &mut self.samples {
            if ids.contains(&s.id) {
                s.decision_origin = DecisionOrigin::Manual;
                if decision == Decision::Keep && !s.approvable() {
                    s.decision = Decision::Fix;
                } else {
                    s.decision = decision;
                    if decision == Decision::Keep {
                        s.reviewed = true;
                    }
                }
                s.reason = "手動選別".into();
            }
        }
        self.log("判断", ids.to_vec());
    }
    pub fn commit_page(&mut self, ids: &[Uuid], excluded: &HashSet<Uuid>) {
        self.checkpoint(ids);
        for s in &mut self.samples {
            if !ids.contains(&s.id) {
                continue;
            }
            s.decision_origin = DecisionOrigin::Manual;
            if excluded.contains(&s.id) {
                s.decision = Decision::Exclude;
            } else if matches!(s.decision, Decision::Pending | Decision::Keep) {
                if s.approvable() {
                    s.decision = Decision::Keep;
                    s.reviewed = true;
                } else {
                    s.decision = Decision::Fix;
                }
            }
            s.reason = "10枚一覧で確認".into();
        }
        self.log("ページ確定", ids.to_vec());
    }
    pub fn edit(
        &mut self,
        id: Uuid,
        boxes: Vec<NormalizedBox>,
        decision: Decision,
        resolved: bool,
        negative: bool,
    ) -> Result<()> {
        for b in &boxes {
            validate_box(b, self.classes.len())?;
        }
        ensure!(resolved, "自動候補を承認または破棄してください");
        ensure!(!boxes.is_empty() || negative, "対象なしの確認が必要です");
        self.checkpoint(&[id]);
        let s = self
            .samples
            .iter_mut()
            .find(|s| s.id == id)
            .context("画像がありません")?;
        Self::apply_label_edit(s, boxes, decision, negative, "編集画面で確認");
        self.log("枠編集", vec![id]);
        Ok(())
    }
    fn apply_label_edit(
        s: &mut Sample,
        boxes: Vec<NormalizedBox>,
        decision: Decision,
        negative: bool,
        reason: &str,
    ) {
        s.boxes = boxes;
        s.has_label = true;
        s.background = s.boxes.is_empty() && negative;
        s.automatic_label = false;
        s.decision_origin = DecisionOrigin::Manual;
        s.label_errors.clear();
        s.proposals.clear();
        s.proposal_pending = false;
        s.proposal_info = serde_json::Value::Null;
        s.reviewed = true;
        s.decision = decision;
        s.reason = reason.into();
        s.quality = None;
    }

    pub fn mark_background(&mut self, ids: &[Uuid]) {
        if ids.is_empty() {
            return;
        }
        self.checkpoint(ids);
        for s in &mut self.samples {
            if ids.contains(&s.id) {
                let decision = match s.decision {
                    Decision::Exclude | Decision::Hold => s.decision,
                    _ if s.image_error.is_some() => Decision::Fix,
                    _ => Decision::Keep,
                };
                Self::apply_label_edit(s, Vec::new(), decision, true, "一括で背景に設定");
            }
        }
        self.log("一括で背景に設定", ids.to_vec());
    }

    pub fn change_box_class(&mut self, ids: &[Uuid], class_id: usize) -> Result<usize> {
        ensure!(class_id < self.classes.len(), "クラスIDが範囲外です");
        let selected = ids.iter().copied().collect::<HashSet<_>>();
        let changed = self
            .samples
            .iter()
            .filter(|sample| {
                selected.contains(&sample.id)
                    && sample.boxes.iter().any(|bbox| bbox.class_id != class_id)
            })
            .map(|sample| sample.id)
            .collect::<Vec<_>>();
        if changed.is_empty() {
            return Ok(0);
        }
        self.checkpoint(&changed);
        for sample in self.samples.iter_mut().filter(|s| selected.contains(&s.id)) {
            for bbox in &mut sample.boxes {
                bbox.class_id = class_id;
            }
        }
        let count = changed.len();
        self.log("枠のクラスを一括変更", changed);
        Ok(count)
    }

    pub fn remove_samples(&mut self, ids: &[Uuid]) -> usize {
        let selected = ids.iter().copied().collect::<HashSet<_>>();
        let removed = self
            .samples
            .iter()
            .filter(|s| selected.contains(&s.id))
            .map(|s| s.id)
            .collect::<Vec<_>>();
        if removed.is_empty() {
            return 0;
        }
        self.checkpoint(&removed);
        self.samples.retain(|s| !selected.contains(&s.id));
        let count = removed.len();
        self.log(REMOVE_SAMPLES_ACTION, removed);
        count
    }

    pub fn replace_boxes_with_otsu(
        &mut self,
        ids: &[Uuid],
        class_id: usize,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<(usize, usize)> {
        ensure!(class_id < self.classes.len(), "クラスIDが範囲外です");
        let selected = ids.iter().copied().collect::<HashSet<_>>();
        let samples = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| selected.contains(&s.id))
            .collect::<Vec<_>>();
        let config = DetectorConfig::default();
        let mut results = Vec::with_capacity(samples.len());
        for (i, (index, sample)) in samples.iter().enumerate() {
            cancelled(cancel)?;
            let result = (|| -> Result<_> {
                let image = image::open(self.image_path(sample)?)?;
                let mut detection = labeler_vision::detect_otsu(&image, &config)?;
                ensure!(
                    !detection.boxes.is_empty(),
                    "対象の枠を検出できませんでした。枠を編集してください"
                );
                for bbox in &mut detection.boxes {
                    bbox.class_id = class_id;
                }
                Ok(detection.boxes)
            })();
            results.push((*index, sample.id, result));
            progress(i + 1, samples.len());
        }
        cancelled(cancel)?;
        if results.is_empty() {
            return Ok((0, 0));
        }
        let changed = results.iter().map(|(_, id, _)| *id).collect::<Vec<_>>();
        self.checkpoint(&changed);
        let mut detected = 0;
        let mut failed = 0;
        for (index, _, result) in results {
            let sample = &mut self.samples[index];
            let (boxes, error) = match result {
                Ok(boxes) => {
                    detected += 1;
                    (boxes, None)
                }
                Err(error) => {
                    failed += 1;
                    (Vec::new(), Some(error.to_string()))
                }
            };
            let preserved = matches!(sample.decision, Decision::Exclude | Decision::Hold);
            let origin = sample.decision_origin;
            let decision = if preserved {
                sample.decision
            } else if error.is_some() || sample.image_error.is_some() {
                Decision::Fix
            } else {
                Decision::Keep
            };
            Self::apply_label_edit(sample, boxes, decision, false, "大津法で枠を一括再検出");
            sample.automatic_label = error.is_none();
            sample.has_label = error.is_none();
            sample.reviewed = false;
            sample.decision_origin = if preserved {
                origin
            } else {
                DecisionOrigin::AutoDefault
            };
            sample.label_errors.extend(error);
        }
        self.log("大津法で枠を一括再検出", changed);
        Ok((detected, failed))
    }

    pub fn set_classes(&mut self, classes: Vec<String>) -> Result<()> {
        validate_classes(&classes)?;
        ensure!(
            classes.len() >= self.classes.len(),
            "クラスの削除はできません"
        );
        let old: HashSet<_> = self.classes.iter().collect();
        let new: HashSet<_> = classes.iter().collect();
        ensure!(
            old != new || self.classes == classes,
            "クラスの並べ替えはできません"
        );
        self.classes = classes;
        self.undo.clear();
        self.log("クラス設定", vec![]);
        Ok(())
    }
    pub fn import_proposals(
        &mut self,
        batch: Uuid,
        labels: &Path,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<usize> {
        ensure!(
            discover_classes(labels, None)? == self.classes,
            "候補フォルダーのclasses.txtが一致しません"
        );
        let mut results = vec![];
        for (i, s) in self.samples.iter().enumerate() {
            cancelled(cancel)?;
            if s.batch == batch && !s.protected() {
                let rel = s
                    .origin_relative
                    .strip_prefix("images")
                    .unwrap_or(&s.origin_relative);
                let p = safe_join(labels, &rel.with_extension("txt"))?;
                if p.is_file() {
                    let (b, e) = parse_yolo(&fs::read_to_string(p)?, self.classes.len());
                    ensure!(e.is_empty(), "候補ラベル不正: {}", e.join("; "));
                    results.push((s.id, b));
                }
            }
            progress(i + 1, self.samples.len());
        }
        cancelled(cancel)?;
        let ids = results.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        self.checkpoint(&ids);
        for (id, b) in results {
            let s = self.samples.iter_mut().find(|s| s.id == id).unwrap();
            s.proposals = b;
            s.proposal_pending = true;
            s.decision = Decision::Fix;
            s.proposal_info = serde_json::json!({"backend":"external-yolo"});
        }
        self.log("外部候補", ids.clone());
        Ok(ids.len())
    }
    pub fn propose(
        &mut self,
        config: &DetectorConfig,
        fixed: usize,
        ranges: Option<&Path>,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<usize> {
        ensure!(fixed < self.classes.len(), "クラスIDが範囲外です");
        let ranges = if let Some(p) = ranges {
            Some(read_ranges(p, self.classes.len())?)
        } else {
            None
        };
        let mut results = vec![];
        let mut failures = vec![];
        for (i, s) in self.samples.iter().enumerate() {
            cancelled(cancel)?;
            if !s.protected() {
                let class = if let Some(r) = &ranges {
                    class_from_ranges(&s.origin_relative, r)?
                } else {
                    fixed
                };
                let detected = (|| -> Result<_> {
                    let image = image::open(self.image_path(s)?)?;
                    labeler_vision::detect(&image, config)
                })();
                let mut d = match detected {
                    Ok(d) => d,
                    Err(e) => {
                        failures.push(format!("{}: {e}", s.origin_relative.display()));
                        progress(i + 1, self.samples.len());
                        continue;
                    }
                };
                for b in &mut d.boxes {
                    b.class_id = class;
                }
                results.push((s.id, d));
            }
            progress(i + 1, self.samples.len());
        }
        cancelled(cancel)?;
        self.warnings.extend(failures);
        let ids = results.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        self.checkpoint(&ids);
        for (id, d) in results {
            let s = self.samples.iter_mut().find(|s| s.id == id).unwrap();
            s.proposals = d.boxes;
            s.proposal_pending = true;
            s.decision = Decision::Fix;
            s.proposal_info = serde_json::json!({"backend":"rust-imageproc-v1","method":d.method,"threshold":d.threshold,"component_count":d.component_count,"flags":d.flags,"config":config});
        }
        self.log("候補生成", ids.clone());
        Ok(ids.len())
    }
    pub fn analyze(&mut self, cancel: &AtomicBool, progress: Progress<'_>) -> Result<()> {
        let mut results = vec![];
        for (i, s) in self.samples.iter().enumerate() {
            cancelled(cancel)?;
            let q = labeler_vision::quality(&self.image_path(s)?, &s.boxes, 45.0);
            results.push((s.id, q));
            progress(i + 1, self.samples.len());
        }
        cancelled(cancel)?;
        for (id, result) in results {
            let s = self.samples.iter_mut().find(|s| s.id == id).unwrap();
            match result {
                Ok(q) => s.quality = Some(q),
                Err(e) => {
                    s.quality = Some(Quality {
                        sha256: s.source_hash.clone(),
                        flags: vec!["画像破損".into()],
                        algorithm_version: labeler_vision::ALGORITHM_VERSION.into(),
                        parameter_fingerprint: labeler_vision::quality_fingerprint(45.),
                        score: 10000.,
                        ..Default::default()
                    });
                    s.image_error = Some(e.to_string());
                }
            }
            s.duplicate_of = None;
            s.duplicate_conflict = false;
        }
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, s) in self.samples.iter().enumerate() {
            if let Some(q) = &s.quality {
                if q.sha256 == s.source_hash {
                    groups.entry(q.sha256.clone()).or_default().push(i);
                }
            }
        }
        for indices in groups.values().filter(|g| g.len() > 1) {
            let rep = *indices
                .iter()
                .min_by_key(|&&i| (self.samples[i].decision != Decision::Keep, i))
                .unwrap();
            let signature = |s: &Sample| {
                let mut v = s
                    .boxes
                    .iter()
                    .map(|b| yolo_text(std::slice::from_ref(b)))
                    .collect::<Vec<_>>();
                v.sort();
                (s.background, v)
            };
            let base = signature(&self.samples[rep]);
            let conflict = indices.iter().any(|&i| {
                signature(&self.samples[i]) != base
                    || !self.samples[i].label_errors.is_empty()
                    || self.samples[i].proposal_pending
            });
            let rep_id = self.samples[rep].id;
            for &i in indices {
                self.samples[i].duplicate_of = Some(rep_id);
                self.samples[i].duplicate_conflict = conflict;
            }
        }
        self.log("品質検査", vec![]);
        Ok(())
    }
    pub fn exclusion_candidates(&self) -> Vec<Uuid> {
        self.samples
            .iter()
            .filter(|s| {
                if s.decision != Decision::Pending
                    || s.reviewed
                    || s.proposal_pending
                    || !s.label_errors.is_empty()
                {
                    return false;
                }
                let Ok(path) = self.image_path(s) else {
                    return false;
                };
                let Ok(bytes) = fs::read(path) else {
                    return false;
                };
                if hash(&bytes) != s.source_hash {
                    return false;
                }
                let Some(q) = &s.quality else { return false };
                if q.sha256 != s.source_hash
                    || q.algorithm_version != labeler_vision::ALGORITHM_VERSION
                    || q.parameter_fingerprint != labeler_vision::quality_fingerprint(45.)
                {
                    return false;
                }
                if s.image_error.is_some() {
                    return image::load_from_memory(&bytes).is_err();
                }
                let Some(rep) = s
                    .duplicate_of
                    .filter(|id| *id != s.id)
                    .and_then(|id| self.samples.iter().find(|x| x.id == id))
                else {
                    return false;
                };
                !s.duplicate_conflict
                    && s.background == rep.background
                    && s.has_label
                    && s.proposals.is_empty()
                    && rep.has_label
                    && rep.proposals.is_empty()
                    && rep.label_errors.is_empty()
                    && !rep.proposal_pending
                    && rep.quality.as_ref().is_some_and(|q| {
                        q.sha256 == rep.source_hash
                            && q.algorithm_version == labeler_vision::ALGORITHM_VERSION
                            && q.parameter_fingerprint == labeler_vision::quality_fingerprint(45.)
                    })
                    && {
                        let mut a = s
                            .boxes
                            .iter()
                            .map(|b| yolo_text(std::slice::from_ref(b)))
                            .collect::<Vec<_>>();
                        let mut b = rep
                            .boxes
                            .iter()
                            .map(|b| yolo_text(std::slice::from_ref(b)))
                            .collect::<Vec<_>>();
                        a.sort();
                        b.sort();
                        a == b
                    }
                    && !matches!(
                        rep.decision,
                        Decision::Exclude | Decision::Hold | Decision::Fix
                    )
                    && self
                        .image_path(rep)
                        .and_then(|p| Ok(hash(&fs::read(p)?)))
                        .is_ok_and(|h| h == s.source_hash)
            })
            .map(|s| s.id)
            .collect()
    }
    pub fn validate_export(&self) -> Vec<String> {
        let mut errors = vec![];
        for sample in &self.samples {
            if matches!(sample.decision, Decision::Pending | Decision::Fix) {
                errors.push(format!(
                    "要確認の画像が残っています: {}",
                    sample.origin_relative.display()
                ));
            }
        }
        if let Err(e) = validate_classes(&self.classes) {
            errors.push(e.to_string());
        }
        let accepted = self
            .samples
            .iter()
            .filter(|s| s.decision == Decision::Keep)
            .collect::<Vec<_>>();
        if accepted.len() < MIN_IMAGES {
            errors.push(format!(
                "採用画像は{}枚です。101枚以上必要です。",
                accepted.len()
            ));
        }
        let mut counts = vec![0; self.classes.len()];
        for s in accepted {
            if !s.approvable() {
                errors.push(format!(
                    "未承認・ラベル不正・背景未指定: {}",
                    s.origin_relative.display()
                ));
            }
            for b in &s.boxes {
                if let Err(e) = validate_box(b, self.classes.len()) {
                    errors.push(e.to_string());
                } else {
                    counts[b.class_id] += 1;
                }
            }
        }
        for (i, n) in counts.iter().enumerate() {
            if *n == 0 {
                errors.push(format!("クラス {} の採用枠が0個です", self.classes[i]));
            }
        }
        errors
    }
    pub fn export(
        &self,
        destination: &Path,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<serde_json::Value> {
        self.export_with_augmentation(destination, None, cancel, progress)
    }

    pub fn export_with_augmentation(
        &self,
        destination: &Path,
        augmentation: Option<(labeler_vision::augment::AugmentOptions, u64)>,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<serde_json::Value> {
        self.export_with_limit(destination, augmentation, MAX_ZIP_BYTES, cancel, progress)
    }

    fn export_with_limit(
        &self,
        destination: &Path,
        augmentation: Option<(labeler_vision::augment::AugmentOptions, u64)>,
        limit: u64,
        cancel: &AtomicBool,
        progress: Progress<'_>,
    ) -> Result<serde_json::Value> {
        use rand::{RngExt, SeedableRng, rngs::StdRng, seq::SliceRandom};
        if let Some((options, _)) = augmentation {
            ensure!(
                (1..=3).contains(&options.copies) && options.any_effect(),
                "加工枚数は1〜3枚にし、加工を1種類以上選んでください"
            );
        }
        let errors = self.validate_export();
        ensure!(errors.is_empty(), "{}", errors.join("\n"));
        ensure!(
            destination.extension().is_some_and(|s| s == "zip"),
            "出力形式はZIPです"
        );
        let report_path = destination.with_extension("report.json");
        ensure!(
            !destination.exists() && !report_path.exists(),
            "同名ZIPまたはレポートがあります"
        );
        let parent = destination.parent().context("出力先不正")?;
        fs::create_dir_all(parent)?;
        let tmp = tempfile::NamedTempFile::new_in(parent)?;
        let mut zip = ZipWriter::new(tmp.reopen()?);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("classes.txt", options)?;
        zip.write_all((self.classes.join("\n") + "\n").as_bytes())?;
        zip.start_file("v-training.config", options)?;
        zip.write_all(
            serde_json::to_string(&serde_json::json!({"classes":self.classes.len()}))?.as_bytes(),
        )?;
        let accepted = self
            .samples
            .iter()
            .filter(|s| s.decision == Decision::Keep)
            .collect::<Vec<_>>();
        let mut mapping = vec![];
        let planned = augmentation.map_or(0, |(o, _)| accepted.len() * usize::from(o.copies));
        let total = accepted.len() + planned;
        for (i, s) in accepted.iter().enumerate() {
            cancelled(cancel)?;
            let image = self.read_export_image(s)?;
            let mut jpeg = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
                .encode_image(&image)?;
            let name = format!("{:06}", i + 1);
            zip.start_file(format!("{name}.jpg"), options)?;
            zip.write_all(&jpeg)?;
            zip.start_file(format!("{name}.txt"), options)?;
            zip.write_all(yolo_text(&s.boxes).as_bytes())?;
            mapping.push(serde_json::json!({"output":name,"id":s.id,"origin_root":s.origin_root,"origin_relative":s.origin_relative,"sha256":s.source_hash}));
            progress(i + 1, total);
        }
        zip.finish()?.sync_all()?;
        cancelled(cancel)?;
        let mut size = fs::metadata(tmp.path())?.len();
        ensure!(size <= limit, "元画像だけでZIPの容量上限を超えています");
        let mut augmentation_report = None;
        let mut merged = None;
        if let Some((settings, seed)) = augmentation {
            let mut rng = StdRng::seed_from_u64(seed);
            let mut order: Vec<_> = (0..accepted.len()).collect();
            let mut skipped = Vec::new();
            let mut by_capacity = 0;
            let mut unavailable = 0;
            let mut done = accepted.len();
            let mut append = None;
            // Stored entries have fixed local/central headers and no compression overhead.
            // Leave room for the ZIP64 end records if the entry count crosses the ZIP32 limit.
            let stored =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            let mut entries = 2 + accepted.len() * 2;
            for round in 1..=settings.copies {
                order.shuffle(&mut rng);
                for &index in &order {
                    cancelled(cancel)?;
                    let s = accepted[index];
                    let candidate_seed: u64 = rng.random();
                    let mut reason = "容量上限";
                    let mut applied = None;
                    let name = format!("{:06}", mapping.len() + 1);
                    let overhead = 2 * (30 + 46 + 2 * (name.len() as u64 + 4));
                    let zip64 = if entries < 65535 && entries + 2 >= 65535 {
                        128
                    } else {
                        0
                    };
                    if size + overhead + zip64 < limit {
                        let source = self.read_export_image(s)?;
                        let (candidate, effects) = labeler_vision::augment::generate(
                            &source,
                            &s.boxes,
                            settings,
                            candidate_seed,
                        );
                        applied = Some(effects);
                        if let Some((image, boxes)) = candidate {
                            for b in &boxes {
                                validate_box(b, self.classes.len())?;
                            }
                            let mut jpeg = Vec::new();
                            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
                                .encode_image(&image)?;
                            let label = yolo_text(&boxes);
                            let cost = overhead + zip64 + jpeg.len() as u64 + label.len() as u64;
                            if size + cost <= limit {
                                cancelled(cancel)?;
                                if append.is_none() {
                                    // Copy the existing local entries without their old central directory.
                                    let output = tempfile::NamedTempFile::new_in(parent)?;
                                    let mut writer = ZipWriter::new(output.reopen()?);
                                    writer.merge_archive(ZipArchive::new(tmp.reopen()?)?)?;
                                    merged = Some(output);
                                    append = Some(writer);
                                }
                                let zip = append.as_mut().unwrap();
                                zip.start_file(format!("{name}.jpg"), stored)?;
                                zip.write_all(&jpeg)?;
                                zip.start_file(format!("{name}.txt"), stored)?;
                                zip.write_all(label.as_bytes())?;
                                size += cost;
                                entries += 2;
                                mapping.push(serde_json::json!({
                                    "output":name,"id":s.id,"origin_root":s.origin_root,
                                    "origin_relative":s.origin_relative,"sha256":s.source_hash,
                                    "augmentation":{"copy":round,"seed":candidate_seed,"applied":effects}
                                }));
                                done += 1;
                                progress(done, total);
                                continue;
                            }
                        } else {
                            reason = "安全に適用できる加工なし";
                        }
                    }
                    if reason == "容量上限" {
                        by_capacity += 1;
                    } else {
                        unavailable += 1;
                    }
                    skipped.push(
                        serde_json::json!({"id":s.id,"copy":round,"seed":candidate_seed,
                        "reason":reason,"applied":applied}),
                    );
                    done += 1;
                    progress(done, total);
                }
            }
            if let Some(zip) = append {
                zip.finish()?.sync_all()?;
            }
            augmentation_report = Some(serde_json::json!({
                "seed":seed,"settings":settings,"requested":planned,"added":mapping.len()-accepted.len(),
                "skipped_capacity":by_capacity,"skipped_unavailable":unavailable,"skipped":skipped
            }));
        }
        let tmp = merged.unwrap_or(tmp);
        ensure!(
            fs::metadata(tmp.path())?.len() <= limit,
            "完成ZIPが容量上限を超えています"
        );
        cancelled(cancel)?;
        let summary = inspect_zip(tmp.path())?;
        cancelled(cancel)?;
        let mut report = serde_json::json!({"format":"V-Training Detection","created_utc_seconds":now(),"summary":summary,"mapping":mapping,"warnings":self.warnings});
        if let Some(metadata) = augmentation_report {
            report["augmentation"] = metadata;
        }
        let mut rt = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut rt, &report)?;
        rt.as_file().sync_all()?;
        let output = tmp.persist_noclobber(destination).map_err(|e| e.error)?;
        drop(output);
        if let Err(e) = rt.persist_noclobber(&report_path) {
            fs::remove_file(destination)?;
            return Err(e.error.into());
        }
        Ok(report)
    }
    fn read_export_image(&self, sample: &Sample) -> Result<image::RgbImage> {
        let bytes = fs::read(self.image_path(sample)?)?;
        ensure!(
            hash(&bytes) == sample.source_hash,
            "元画像が変更されています: {}",
            sample.origin_relative.display()
        );
        let image = image::load_from_memory(&bytes)?.to_rgb8();
        ensure!(
            image.dimensions() == (sample.width, sample.height),
            "画像寸法が変わっています"
        );
        Ok(image)
    }
}
pub fn inspect_zip(path: &Path) -> Result<serde_json::Value> {
    ensure!(
        fs::metadata(path)?.len() <= MAX_ZIP_BYTES,
        "ZIPが200MBを超えています"
    );
    let mut zip = ZipArchive::new(fs::File::open(path)?)?;
    let mut names = HashSet::new();
    let mut inflated = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        ensure!(
            !f.name().contains(['/', '\\']) && names.insert(f.name().to_owned()),
            "ZIPの構造が不正です"
        );
        inflated = inflated.checked_add(f.size()).context("ZIPサイズ不正")?;
        ensure!(inflated <= 2_000_000_000, "ZIP展開サイズが大きすぎます");
    }
    let mut classes = String::new();
    zip.by_name("classes.txt")?.read_to_string(&mut classes)?;
    let classes = classes.lines().map(str::to_owned).collect::<Vec<_>>();
    validate_classes(&classes)?;
    let config: serde_json::Value = serde_json::from_reader(zip.by_name("v-training.config")?)?;
    ensure!(
        config["classes"].as_u64() == Some(classes.len() as u64),
        "クラス数が一致しません"
    );
    let images = names
        .iter()
        .filter(|s| s.ends_with(".jpg"))
        .cloned()
        .collect::<Vec<_>>();
    ensure!(images.len() >= MIN_IMAGES, "画像は101枚以上必要です");
    let mut allowed = HashSet::from(["classes.txt".to_owned(), "v-training.config".to_owned()]);
    let mut counts = vec![0usize; classes.len()];
    let mut backgrounds = 0usize;
    for name in &images {
        let mut bytes = vec![];
        zip.by_name(name)?.read_to_end(&mut bytes)?;
        ensure!(
            image::guess_format(&bytes)? == image::ImageFormat::Jpeg,
            "JPEGではありません"
        );
        let im = image::load_from_memory(&bytes)?;
        ensure!(
            im.color() == image::ColorType::Rgb8,
            "RGB JPEGではありません"
        );
        let label = Path::new(name)
            .with_extension("txt")
            .to_string_lossy()
            .into_owned();
        let mut t = String::new();
        zip.by_name(&label)?.read_to_string(&mut t)?;
        let (boxes, e) = parse_yolo(&t, classes.len());
        ensure!(e.is_empty(), "ラベルが不正です: {label}");
        if boxes.is_empty() {
            backgrounds += 1;
        }
        for b in boxes {
            counts[b.class_id] += 1;
        }
        allowed.insert(name.clone());
        allowed.insert(label);
    }
    ensure!(names == allowed, "ZIPに想定外のファイルがあります");
    ensure!(
        counts.iter().all(|n| *n > 0),
        "採用枠が0個のクラスがあります"
    );
    Ok(
        serde_json::json!({"images":images.len(),"backgrounds":backgrounds,"classes":classes,"bytes":fs::metadata(path)?.len()}),
    )
}
#[derive(Clone, Debug, Deserialize)]
pub struct ClassRange {
    pub start: u64,
    pub end: u64,
    pub class_id: usize,
}
pub fn read_ranges(path: &Path, n: usize) -> Result<Vec<ClassRange>> {
    let mut r = csv::Reader::from_path(path)?;
    let mut ranges = r
        .deserialize()
        .collect::<std::result::Result<Vec<ClassRange>, _>>()?;
    ranges.sort_by_key(|r| r.start);
    ensure!(!ranges.is_empty(), "範囲CSVが空です");
    for (i, r) in ranges.iter().enumerate() {
        ensure!(r.start <= r.end && r.class_id < n, "範囲CSVが不正です");
        if i > 0 {
            ensure!(ranges[i - 1].end < r.start, "CSVの範囲が重複しています");
        }
    }
    Ok(ranges)
}
pub fn class_from_ranges(path: &Path, ranges: &[ClassRange]) -> Result<usize> {
    let stem = path.file_stem().context("画像名不正")?.to_string_lossy();
    let groups = stem
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    ensure!(groups.len() == 1, "画像名に数字のまとまりが1つ必要です");
    let n: u64 = groups[0].parse()?;
    ranges
        .iter()
        .find(|r| r.start <= n && n <= r.end)
        .map(|r| r.class_id)
        .context("CSVに対応する範囲がありません")
}

#[cfg(test)]
mod augmentation_export_tests {
    use super::*;

    #[test]
    fn export_keeps_originals_reproduces_copies_and_respects_budget() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("images");
        fs::create_dir(&source)?;
        let mut inputs = Vec::new();
        for i in 0..MIN_IMAGES {
            let file = source.join(format!("{i}.png"));
            image::RgbImage::from_fn(32, 24, |x, y| image::Rgb([x as u8, y as u8, 128]))
                .save(&file)?;
            fs::write(
                file.with_extension("txt"),
                if i == 0 { "" } else { "0 .5 .5 .2 .4" },
            )?;
            inputs.push(file);
        }
        let mut p = Project::new(dir.path().join("project.ylp2"), vec!["H".into()])?;
        let cancel = AtomicBool::new(false);
        p.import(&source, &inputs, None, &cancel, &mut |_, _| {})?;
        let original = dir.path().join("original.zip");
        p.export(&original, &cancel, &mut |_, _| {})?;
        let base = fs::metadata(original)?.len();
        let settings = Some((AugmentOptions::default(), 234));
        let full = dir.path().join("full.zip");
        let mut report = p.export_with_augmentation(&full, settings, &cancel, &mut |_, _| {})?;
        assert_eq!(report["summary"]["images"], 202);
        assert_eq!(report["summary"]["backgrounds"], 2);
        let limited = dir.path().join("limited.zip");
        let mut small =
            p.export_with_limit(&limited, settings, base + 1800, &cancel, &mut |_, _| {})?;
        let added = small["augmentation"]["added"].as_u64().unwrap();
        assert!(added > 0 && added < 101);
        assert_eq!(small["summary"]["images"], 101 + added);
        assert!(fs::metadata(limited)?.len() <= base + 1800);
        let small_again = dir.path().join("again.zip");
        let mut again =
            p.export_with_limit(&small_again, settings, base + 1800, &cancel, &mut |_, _| {})?;
        small.as_object_mut().unwrap().remove("created_utc_seconds");
        again.as_object_mut().unwrap().remove("created_utc_seconds");
        assert_eq!(small, again);
        for entry in report["mapping"].as_array_mut().unwrap().iter().take(101) {
            assert!(entry.get("augmentation").is_none());
        }
        let rejected = dir.path().join("too-small.zip");
        assert!(
            p.export_with_limit(&rejected, settings, base - 1, &cancel, &mut |_, _| {})
                .is_err()
        );
        assert!(!rejected.exists() && !rejected.with_extension("report.json").exists());
        let interrupted = dir.path().join("cancelled.zip");
        assert!(
            p.export_with_augmentation(&interrupted, settings, &cancel, &mut |done, _| {
                if done > MIN_IMAGES {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .is_err()
        );
        assert!(!interrupted.exists() && !interrupted.with_extension("report.json").exists());
        Ok(())
    }
}
