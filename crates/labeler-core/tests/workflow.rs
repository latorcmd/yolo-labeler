use labeler_core::*;
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tempfile::TempDir;

fn flag() -> AtomicBool {
    AtomicBool::new(false)
}
struct Fixture {
    _temp: TempDir,
    source: PathBuf,
    project: Project,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("日本語 画像");
        fs::create_dir(&source).unwrap();
        let project = Project::new(temp.path().join("test.ylp2"), vec!["H".into()]).unwrap();
        Self {
            _temp: temp,
            source,
            project,
        }
    }
    fn image(&self, name: &str, label: Option<&str>) -> PathBuf {
        let p = self.source.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        image::RgbImage::from_pixel(32, 24, image::Rgb([255, 255, 255]))
            .save(&p)
            .unwrap();
        if let Some(t) = label {
            fs::write(p.with_extension("txt"), t).unwrap();
        }
        p
    }
    fn import(&mut self, paths: &[PathBuf]) {
        self.project
            .import(&self.source, paths, None, &flag(), &mut |_, _| {})
            .unwrap();
    }
    fn populated(n: usize) -> Self {
        let mut f = Self::new();
        let paths = (0..n)
            .map(|i| f.image(&format!("{i:03}.png"), Some("0 .5 .5 .3 .4")))
            .collect::<Vec<_>>();
        f.import(&paths);
        for s in &mut f.project.samples {
            s.decision = Decision::Keep;
            s.reviewed = true;
        }
        f
    }
}
#[test]
fn parser_rejects_bad_rows() {
    for t in [
        "0 .5 .5 .2",
        "0.5 .5 .5 .2 .2",
        "1 .5 .5 .2 .2",
        "0 NaN .5 .2 .2",
        "0 inf .5 .2 .2",
        "0 .1 .5 .8 .2",
        "0 .5 .5 0 .2",
    ] {
        let (b, e) = parse_yolo(t, 1);
        assert!(b.is_empty(), "{t}");
        assert!(!e.is_empty());
    }
}
#[test]
fn parser_roundoff() {
    let (b, e) = parse_yolo("0 0.166667 .5 .333334 1", 1);
    assert!(e.is_empty());
    assert!(b[0].cx - b[0].width / 2. >= 0.);
    assert!(!parse_yolo("0 .1 .5 .3 .3", 1).1.is_empty());
}
#[test]
fn parser_bom_and_empty() {
    assert_eq!(parse_yolo("\u{feff}0 .5 .5 1 1\n", 1).0.len(), 1);
    assert!(parse_yolo(" \n", 1).1.is_empty());
}
#[test]
fn classes_validate() {
    for c in [
        vec![],
        vec!["".into()],
        vec!["H".into(), "H".into()],
        vec![" H".into()],
    ] {
        assert!(validate_classes(&c).is_err());
    }
}
#[test]
fn paths_stay_inside() {
    let f = Fixture::new();
    for r in ["../escape", "/outside", "a/../x"] {
        assert!(safe_join(&f.source, std::path::Path::new(r)).is_err());
    }
}
#[test]
fn old_projects_rejected() {
    let f = Fixture::new();
    let p = f.source.join("old.ylproject");
    fs::write(&p, "{}").unwrap();
    assert!(Project::load(&p).is_err());
}
#[test]
fn save_roundtrip_and_backup() {
    let mut f = Fixture::populated(1);
    f.project.save().unwrap();
    f.project.samples[0].reason = "二回目".into();
    f.project.save().unwrap();
    let loaded = Project::load(&f.project.file).unwrap();
    assert_eq!(loaded.samples, f.project.samples);
    assert!(f.project.file.with_extension("ylp2.bak").exists());
}
#[test]
fn import_preserves_originals() {
    let mut f = Fixture::new();
    let a = f.image("a.png", Some("0 .5 .5 .2 .2"));
    let bytes = fs::read(&a).unwrap();
    f.import(&[a.clone()]);
    assert_eq!(fs::read(&a).unwrap(), bytes);
    assert!(
        f.project
            .image_path(&f.project.samples[0])
            .unwrap()
            .exists()
    );
}
#[test]
fn duplicate_paths_skipped() {
    let mut f = Fixture::new();
    let a = f.image("a.png", None);
    f.import(&[a.clone(), a.clone()]);
    f.import(&[a]);
    assert_eq!(f.project.samples.len(), 1);
}
#[test]
fn same_basename_kept() {
    let mut f = Fixture::new();
    let a = f.image("one/a.png", None);
    let b = f.image("two/a.png", None);
    f.import(&[a, b]);
    assert_ne!(f.project.samples[0].path, f.project.samples[1].path);
}
#[test]
fn imports_missing_invalid_empty() {
    let mut f = Fixture::new();
    let a = f.image("missing.png", None);
    let b = f.image("invalid.png", Some("bad"));
    let c = f.image("empty.png", Some(""));
    f.import(&[a, b, c]);
    assert!(!f.project.samples[0].has_label);
    assert!(!f.project.samples[1].label_errors.is_empty());
    assert!(f.project.samples[2].has_label);
    assert!(f.project.samples.iter().all(|s| !s.reviewed));
}
#[test]
fn imports_corrupt_image() {
    let mut f = Fixture::new();
    let p = f.source.join("bad.png");
    fs::write(&p, "not image").unwrap();
    f.import(&[p]);
    assert!(f.project.samples[0].image_error.is_some());
}
#[test]
fn imports_separate_labels() {
    let mut f = Fixture::new();
    let a = f.image("images/day/a.png", None);
    let label = f.source.join("labels/day/a.txt");
    fs::create_dir_all(label.parent().unwrap()).unwrap();
    fs::write(label, "0 .5 .5 .2 .2").unwrap();
    f.import(&[a]);
    assert!(f.project.samples[0].has_label);
}
#[test]
fn imports_custom_labels() {
    let mut f = Fixture::new();
    let a = f.image("images/day/a.png", None);
    let labels = f.source.join("separate");
    fs::create_dir_all(labels.join("day")).unwrap();
    fs::write(labels.join("day/a.txt"), "0 .5 .5 .2 .2").unwrap();
    f.project
        .import(&f.source, &[a], Some(&labels), &flag(), &mut |_, _| {})
        .unwrap();
    assert!(f.project.samples[0].has_label);
}
#[test]
fn class_mismatch_no_additions() {
    let mut f = Fixture::new();
    let a = f.image("a.png", None);
    fs::write(f.source.join("classes.txt"), "S").unwrap();
    assert!(
        f.project
            .import(&f.source, &[a], None, &flag(), &mut |_, _| {})
            .is_err()
    );
    assert!(f.project.samples.is_empty());
}
#[test]
fn import_cancel_cleans_staging() {
    let mut f = Fixture::new();
    let a = f.image("a.png", None);
    let stop = flag();
    assert!(
        f.project
            .import(&f.source, &[a], None, &stop, &mut |_, _| stop
                .store(true, Ordering::Relaxed))
            .is_err()
    );
    assert!(f.project.samples.is_empty());
    assert_eq!(
        fs::read_dir(f.project.assets().join("batches"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn import_failed_source_no_partial_state() {
    let mut f = Fixture::new();
    let a = f.image("a.png", None);
    assert!(
        f.project
            .import(
                &f.source,
                &[a, f.source.join("missing.png")],
                None,
                &flag(),
                &mut |_, _| {}
            )
            .is_err()
    );
    assert!(f.project.samples.is_empty());
}
#[test]
fn commit_and_undo() {
    let mut f = Fixture::populated(2);
    let ids = f.project.samples.iter().map(|s| s.id).collect::<Vec<_>>();
    f.project.commit_page(&ids, &HashSet::from([ids[1]]));
    assert_eq!(f.project.samples[1].decision, Decision::Exclude);
    assert!(f.project.undo());
    assert_eq!(f.project.samples[1].decision, Decision::Keep);
}
#[test]
fn candidates_require_resolution() {
    let mut f = Fixture::populated(1);
    let id = f.project.samples[0].id;
    f.project.samples[0].proposal_pending = true;
    f.project.decide(&[id], Decision::Keep);
    assert_eq!(f.project.samples[0].decision, Decision::Fix);
    assert!(
        f.project
            .edit(id, vec![], Decision::Keep, false, true)
            .is_err()
    );
}
#[test]
fn negative_requires_confirmation() {
    let mut f = Fixture::populated(1);
    let id = f.project.samples[0].id;
    assert!(
        f.project
            .edit(id, vec![], Decision::Keep, true, false)
            .is_err()
    );
    f.project
        .edit(id, vec![], Decision::Keep, true, true)
        .unwrap();
    assert!(!f.project.validate_export().is_empty());
}
#[test]
fn edit_undo_restores_proposals() {
    let mut f = Fixture::populated(1);
    let id = f.project.samples[0].id;
    f.project.samples[0].proposal_pending = true;
    let before = f.project.samples[0].clone();
    f.project
        .edit(id, before.boxes.clone(), Decision::Hold, true, false)
        .unwrap();
    assert!(!f.project.samples[0].proposal_pending);
    f.project.undo();
    assert_eq!(f.project.samples[0], before);
}
#[test]
fn undo_retains_added_images() {
    let mut f = Fixture::populated(1);
    let id = f.project.samples[0].id;
    f.project.decide(&[id], Decision::Exclude);
    let b = f.image("more.png", None);
    f.import(&[b]);
    f.project.undo();
    assert_eq!(f.project.samples.len(), 2);
    assert_eq!(f.project.samples[0].decision, Decision::Keep);
}
#[test]
fn csv_inclusive_and_overlap() {
    let f = Fixture::new();
    let p = f.source.join("range.csv");
    fs::write(&p, "start,end,class_id\n1,10,0\n11,20,0\n").unwrap();
    let ranges = read_ranges(&p, 1).unwrap();
    assert_eq!(
        class_from_ranges(std::path::Path::new("010.png"), &ranges).unwrap(),
        0
    );
    assert!(class_from_ranges(std::path::Path::new("1_2.png"), &ranges).is_err());
    fs::write(&p, "start,end,class_id\n1,10,0\n10,20,0\n").unwrap();
    assert!(read_ranges(&p, 1).is_err());
}
#[test]
fn class_changes_keep_ids() {
    let mut f = Fixture::populated(1);
    assert!(f.project.set_classes(vec![]).is_err());
    f.project
        .set_classes(vec!["renamed".into(), "S".into()])
        .unwrap();
    assert_eq!(f.project.samples[0].boxes[0].class_id, 0);
}
#[test]
fn external_candidates_use_original_relative() {
    let mut f = Fixture::populated(1);
    f.project.samples[0].reviewed = false;
    f.project.samples[0].decision = Decision::Pending;
    let labels = f.source.join("predictions");
    fs::create_dir(&labels).unwrap();
    fs::write(labels.join("classes.txt"), "H\n").unwrap();
    fs::write(labels.join("000.txt"), "0 .5 .5 .1 .1").unwrap();
    let batch = f.project.samples[0].batch;
    assert_eq!(
        f.project
            .import_proposals(batch, &labels, &flag(), &mut |_, _| {})
            .unwrap(),
        1
    );
    assert_eq!(f.project.samples[0].proposals[0].width, 0.1);
}
#[test]
fn reviewed_samples_protected_from_candidates() {
    let mut f = Fixture::populated(1);
    f.project
        .propose(&DetectorConfig::default(), 0, None, &flag(), &mut |_, _| {})
        .unwrap();
    assert!(!f.project.samples[0].proposal_pending);
}
#[test]
fn export_requires_101() {
    let f = Fixture::populated(100);
    assert!(!f.project.validate_export().is_empty());
}
#[test]
fn export_flat_rgb_structure() {
    let f = Fixture::populated(101);
    let p = f.source.join("out.zip");
    f.project.export(&p, &flag(), &mut |_, _| {}).unwrap();
    let summary = inspect_zip(&p).unwrap();
    assert_eq!(summary["images"], 101);
    assert!(p.with_extension("report.json").exists());
    assert!(f.project.export(&p, &flag(), &mut |_, _| {}).is_err());
}
#[test]
fn export_detects_modified_image() {
    let f = Fixture::populated(101);
    fs::write(
        f.project.image_path(&f.project.samples[0]).unwrap(),
        "changed",
    )
    .unwrap();
    let out = f.source.join("out.zip");
    assert!(f.project.export(&out, &flag(), &mut |_, _| {}).is_err());
    assert!(!out.exists());
}
#[test]
fn export_cancel_no_output() {
    let f = Fixture::populated(101);
    let out = f.source.join("out.zip");
    let stop = flag();
    assert!(
        f.project
            .export(&out, &stop, &mut |_, _| stop.store(true, Ordering::Relaxed))
            .is_err()
    );
    assert!(!out.exists());
    assert!(!out.with_extension("report.json").exists());
}
#[test]
fn duplicates_do_not_exclude_reviewed() {
    let mut f = Fixture::populated(2);
    f.project.analyze(&flag(), &mut |_, _| {}).unwrap();
    assert!(f.project.exclusion_candidates().is_empty());
    f.project.samples[1].reviewed = false;
    f.project.samples[1].decision = Decision::Pending;
    assert_eq!(
        f.project.exclusion_candidates(),
        vec![f.project.samples[1].id]
    );
}
#[test]
fn conflicting_duplicate_labels_protected() {
    let mut f = Fixture::populated(2);
    f.project.samples[1].boxes[0].width = 0.2;
    for s in &mut f.project.samples {
        s.reviewed = false;
        s.decision = Decision::Pending;
    }
    f.project.analyze(&flag(), &mut |_, _| {}).unwrap();
    assert!(f.project.exclusion_candidates().is_empty());
}

#[test]
fn duplicate_representative_edit_invalidates_exclusion() {
    let mut f = Fixture::populated(2);
    f.project.samples[1].reviewed = false;
    f.project.samples[1].decision = Decision::Pending;
    f.project.analyze(&flag(), &mut |_, _| {}).unwrap();
    assert_eq!(f.project.exclusion_candidates().len(), 1);
    let id = f.project.samples[0].id;
    let mut boxes = f.project.samples[0].boxes.clone();
    boxes[0].width = 0.1;
    f.project
        .edit(id, boxes, Decision::Keep, true, false)
        .unwrap();
    assert!(f.project.exclusion_candidates().is_empty());
}
#[test]
fn stale_quality_version_and_settings_block_exclusion() {
    let mut f = Fixture::populated(2);
    f.project.samples[1].reviewed = false;
    f.project.samples[1].decision = Decision::Pending;
    f.project.analyze(&flag(), &mut |_, _| {}).unwrap();
    f.project.samples[1]
        .quality
        .as_mut()
        .unwrap()
        .algorithm_version = "old".into();
    assert!(f.project.exclusion_candidates().is_empty());
    f.project.samples[1]
        .quality
        .as_mut()
        .unwrap()
        .algorithm_version = labeler_vision::ALGORITHM_VERSION.into();
    f.project.samples[1]
        .quality
        .as_mut()
        .unwrap()
        .parameter_fingerprint = "old settings".into();
    assert!(f.project.exclusion_candidates().is_empty());
}
#[test]
fn individual_nested_images_folder_finds_labels() {
    let mut f = Fixture::new();
    let a = f.image("images/train/a.png", None);
    fs::create_dir_all(f.source.join("labels/train")).unwrap();
    fs::write(f.source.join("labels/train/a.txt"), "0 .5 .5 .2 .2").unwrap();
    f.project
        .import(
            a.parent().unwrap(),
            &[a.clone()],
            None,
            &flag(),
            &mut |_, _| {},
        )
        .unwrap();
    assert!(f.project.samples[0].has_label);
    assert_eq!(
        f.project.samples[0].origin_relative,
        PathBuf::from("images/train/a.png")
    );
}
#[test]
fn import_recovers_after_manifest_save_failure() {
    let mut f = Fixture::new();
    f.project.save().unwrap();
    let manifest = fs::read(&f.project.file).unwrap();
    fs::remove_file(&f.project.file).unwrap();
    fs::create_dir(&f.project.file).unwrap();
    let a = f.image("a.png", Some("0 .5 .5 .3 .3"));
    f.import(&[a]);
    assert!(f.project.needs_save);
    fs::remove_dir(&f.project.file).unwrap();
    fs::write(&f.project.file, manifest).unwrap();
    let recovered = Project::load(&f.project.file).unwrap();
    assert_eq!(recovered.samples.len(), 1);
    assert!(recovered.needs_save);
    recovered.save().unwrap();
    assert_eq!(Project::load(&f.project.file).unwrap().samples.len(), 1);
}

#[test]
fn proposal_cancellation_after_last_image_keeps_existing_state() {
    let mut f = Fixture::populated(1);
    let s = &mut f.project.samples[0];
    s.reviewed = false;
    s.decision = Decision::Pending;
    let before = s.clone();
    let cancel = flag();
    assert!(
        f.project
            .propose(&DetectorConfig::default(), 0, None, &cancel, &mut |_, _| {
                cancel.store(true, Ordering::Relaxed)
            })
            .is_err()
    );
    assert_eq!(f.project.samples[0], before);
}
#[test]
fn corrupt_images_do_not_abort_other_proposals() {
    let mut f = Fixture::new();
    let a = f.image("good.png", Some("0 .5 .5 .3 .3"));
    let bad = f.source.join("bad.png");
    fs::write(&bad, "not an image").unwrap();
    f.import(&[a, bad]);
    assert_eq!(
        f.project
            .propose(&DetectorConfig::default(), 0, None, &flag(), &mut |_, _| {})
            .unwrap(),
        1
    );
    assert!(!f.project.warnings.is_empty());
}
