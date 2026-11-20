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
    assert_eq!(f.project.samples[0].decision, Decision::Fix);
    assert_eq!(f.project.samples[1].decision, Decision::Fix);
    assert_eq!(f.project.samples[2].decision, Decision::Keep);
    assert!(f.project.samples[2].background);
}

#[test]
fn natural_order_handles_zero_padding_and_huge_numbers() {
    let mut names = vec![
        "100.png",
        "10.png",
        "001.png",
        "2.png",
        "1.png",
        "999999999999999999999999999999.png",
        "1000000000000000000000000000000.png",
    ];
    names.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(
        names,
        vec![
            "1.png",
            "001.png",
            "2.png",
            "10.png",
            "100.png",
            "999999999999999999999999999999.png",
            "1000000000000000000000000000000.png"
        ]
    );
    assert_eq!(natural_cmp("Α2.JPG", "α2.jpg"), std::cmp::Ordering::Equal);
}

#[test]
fn default_keep_is_not_manual_confirmation() {
    let mut f = Fixture::new();
    let a = f.image("1.png", Some("0 .5 .5 .3 .4"));
    f.import(&[a]);
    let s = &f.project.samples[0];
    assert_eq!(s.decision, Decision::Keep);
    assert!(!s.reviewed);
    assert!(!s.protected());
    let id = s.id;
    f.project.decide(&[id], Decision::Exclude);
    assert!(f.project.samples[0].protected());
    f.project.undo();
    assert!(!f.project.samples[0].protected());
    assert_eq!(f.project.samples[0].decision, Decision::Keep);
}

#[test]
fn display_mapping_roundtrip_keeps_class_ids_and_undo() {
    let mut f = Fixture::populated(1);
    let id = f.project.samples[0].id;
    f.project.decide(&[id], Decision::Exclude);
    f.project.set_display_names(vec!["η".into()]).unwrap();
    assert_eq!(f.project.display_class(0), "η (H)");
    assert_eq!(f.project.classes, vec!["H"]);
    assert_eq!(f.project.samples[0].boxes[0].class_id, 0);
    assert!(!f.project.undo.is_empty());
    f.project.save().unwrap();
    assert_eq!(
        Project::load(&f.project.file).unwrap().display_class(0),
        "η (H)"
    );
    f.project.set_display_names(vec![" ".into()]).unwrap();
    assert_eq!(f.project.display_class(0), "H");
    assert!(
        f.project
            .set_display_names(vec!["bad\nname".into()])
            .is_err()
    );
    assert!(
        f.project
            .set_display_names(vec!["1".into(), "2".into()])
            .is_err()
    );
    assert!(f.project.save_snapshot().undo.is_empty());
    assert_eq!(f.project.save_snapshot().samples, f.project.samples);
}

#[test]
fn version_one_migrates_pending_and_preserves_manual_states() {
    let mut f = Fixture::populated(4);
    f.project.samples[0].decision = Decision::Pending;
    f.project.samples[0].reviewed = false;
    f.project.samples[1].decision = Decision::Exclude;
    f.project.samples[2].decision = Decision::Hold;
    f.project.samples[3].decision = Decision::Pending;
    f.project.samples[3].reviewed = false;
    f.project.samples[3].boxes.clear();
    let mut json = serde_json::to_value(&f.project).unwrap();
    json["version"] = 1.into();
    json.as_object_mut().unwrap().remove("class_display_names");
    for s in json["samples"].as_array_mut().unwrap() {
        for field in ["background", "decision_origin", "automatic_label"] {
            s.as_object_mut().unwrap().remove(field);
        }
    }
    fs::write(&f.project.file, serde_json::to_vec(&json).unwrap()).unwrap();
    let loaded = Project::load(&f.project.file).unwrap();
    assert_eq!(loaded.version, 2);
    assert!(loaded.needs_save);
    assert_eq!(loaded.samples[0].decision, Decision::Keep);
    assert!(!loaded.samples[0].protected());
    assert_eq!(loaded.samples[1].decision, Decision::Exclude);
    assert!(loaded.samples[1].protected());
    assert_eq!(loaded.samples[2].decision, Decision::Hold);
    assert!(loaded.samples[3].background);
    assert_eq!(loaded.samples[3].decision, Decision::Keep);
}

#[test]
fn import_background_conflict_has_no_publication() {
    let mut f = Fixture::new();
    let a = f.image("a.png", Some("0 .5 .5 .3 .4"));
    let options = ImportOptions {
        background: true,
        ..Default::default()
    };
    assert_eq!(
        f.project
            .preflight_background_conflicts(&f.source, &[a.clone()], None)
            .unwrap(),
        vec![PathBuf::from("a.png")]
    );
    assert!(
        f.project
            .import_with_options(&f.source, &[a], None, &options, &flag(), &mut |_, _| {})
            .is_err()
    );
    assert!(f.project.samples.is_empty());
    assert!(!f.project.assets().join("batches").exists());
}

#[test]
fn import_background_missing_txt_and_undo_roundtrip() {
    let mut f = Fixture::new();
    let a = f.image("a.png", None);
    let options = ImportOptions {
        background: true,
        ..Default::default()
    };
    f.project
        .import_with_options(
            &f.source,
            &[a.clone()],
            None,
            &options,
            &flag(),
            &mut |_, _| {},
        )
        .unwrap();
    let s = &f.project.samples[0];
    assert!(s.background && s.has_label && s.approvable());
    assert_eq!(s.decision, Decision::Keep);
    assert!(!a.with_extension("txt").exists());
    assert_eq!(
        fs::read(f.project.image_path(s).unwrap().with_extension("txt")).unwrap(),
        b""
    );
    let id = s.id;
    f.project
        .edit(
            id,
            vec![NormalizedBox {
                class_id: 0,
                cx: 0.5,
                cy: 0.5,
                width: 0.3,
                height: 0.4,
            }],
            Decision::Keep,
            true,
            false,
        )
        .unwrap();
    assert!(!f.project.samples[0].background);
    f.project.undo();
    assert!(f.project.samples[0].background);
    f.project.save().unwrap();
    assert!(Project::load(&f.project.file).unwrap().samples[0].background);
}

#[test]
fn import_present_keeps_existing_and_detects_missing_boxes() {
    let mut f = Fixture::new();
    let existing = f.image("1.png", Some("0 .5 .5 .2 .2"));
    let automatic = f.image("2.png", None);
    let empty = f.image("3.png", Some(""));
    let invalid = f.image("4.png", Some("bad"));
    for path in [&automatic, &empty] {
        let mut im = image::RgbImage::from_pixel(160, 120, image::Rgb([255, 255, 255]));
        for y in 20..90 {
            for x in 50..100 {
                im.put_pixel(x, y, image::Rgb([0, 0, 0]));
            }
        }
        im.save(path).unwrap();
    }
    f.project
        .import_with_options(
            &f.source,
            &[existing, automatic, empty, invalid],
            None,
            &ImportOptions::default(),
            &flag(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(f.project.samples[0].boxes[0].width, 0.2);
    assert!(!f.project.samples[0].automatic_label);
    for s in &f.project.samples[1..3] {
        assert!(s.automatic_label && s.approvable() && !s.background);
        assert_eq!(s.decision, Decision::Keep);
    }
    assert_eq!(f.project.samples[3].decision, Decision::Fix);
    assert!(!f.project.samples[3].label_errors.is_empty());
}

#[test]
fn otsu_import_ignores_valid_invalid_and_empty_source_labels() {
    let mut f = Fixture::new();
    let paths = [
        f.image("valid.png", Some("0 .5 .5 .2 .2")),
        f.image("invalid.png", Some("bad")),
        f.image("empty.png", Some("")),
        f.image("no-target.png", Some("0 .5 .5 .2 .2")),
    ];
    fs::write(f.source.join("classes.txt"), "Unrelated\n").unwrap();
    let image = image::RgbImage::from_fn(32, 24, |x, _| {
        image::Rgb([if x < 24 { 100 } else { 255 }; 3])
    });
    for path in &paths[..3] {
        image.save(path).unwrap();
    }
    let options = ImportOptions {
        label_source: ImportLabelSource::Otsu,
        detector: DetectorConfig {
            threshold: Some(255),
            ..Default::default()
        },
        ..Default::default()
    };
    f.project
        .import_with_options(&f.source, &paths, None, &options, &flag(), &mut |_, _| {})
        .unwrap();

    for sample in &f.project.samples[..3] {
        assert!(sample.automatic_label && !sample.background);
        assert_eq!(sample.decision, Decision::Keep);
        assert!(sample.label_errors.is_empty());
        assert_eq!(sample.boxes[0].width, 0.75);
        let imported_txt = f.project.image_path(sample).unwrap().with_extension("txt");
        let (boxes, errors) = parse_yolo(&fs::read_to_string(imported_txt).unwrap(), 1);
        assert_eq!(boxes, sample.boxes);
        assert!(errors.is_empty());
    }
    let missed = &f.project.samples[3];
    assert_eq!(missed.decision, Decision::Fix);
    assert!(!missed.has_label && !missed.background && !missed.automatic_label);
    assert!(
        !f.project
            .image_path(missed)
            .unwrap()
            .with_extension("txt")
            .exists()
    );
    for (path, label) in paths
        .iter()
        .zip(["0 .5 .5 .2 .2", "bad", "", "0 .5 .5 .2 .2"])
    {
        assert_eq!(
            fs::read_to_string(path.with_extension("txt")).unwrap(),
            label
        );
    }
}

#[test]
fn ignoring_existing_labels_allows_explicit_background() {
    let mut f = Fixture::new();
    let path = f.image("background.png", Some("0 .5 .5 .2 .2"));
    let options = ImportOptions {
        background: true,
        label_source: ImportLabelSource::Otsu,
        ..Default::default()
    };
    f.project
        .import_with_options(&f.source, &[path], None, &options, &flag(), &mut |_, _| {})
        .unwrap();
    let sample = &f.project.samples[0];
    assert!(sample.background && sample.approvable() && !sample.automatic_label);
    assert!(
        fs::read(f.project.image_path(sample).unwrap().with_extension("txt"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failed_detection_stays_fix_and_cancel_does_not_publish() {
    let mut f = Fixture::new();
    let a = f.image("1.png", None);
    f.project
        .import_with_options(
            &f.source,
            &[a],
            None,
            &ImportOptions::default(),
            &flag(),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(f.project.samples[0].decision, Decision::Fix);
    assert!(!f.project.samples[0].background);
    let b = f.image("2.png", None);
    let stop = flag();
    let options = ImportOptions {
        background: true,
        ..Default::default()
    };
    assert!(
        f.project
            .import_with_options(&f.source, &[b], None, &options, &stop, &mut |_, _| stop
                .store(true, Ordering::Relaxed))
            .is_err()
    );
    assert_eq!(f.project.samples.len(), 1);
}

#[test]
fn background_zip_has_empty_txt_and_original_class_names() {
    use std::io::Read;
    let mut f = Fixture::populated(101);
    f.project.samples[1].boxes.clear();
    f.project.samples[1].background = true;
    f.project.samples[1].reviewed = false;
    f.project.samples[0].reviewed = false;
    f.project.set_display_names(vec!["η".into()]).unwrap();
    let out = f.source.join("background.zip");
    f.project.export(&out, &flag(), &mut |_, _| {}).unwrap();
    assert_eq!(inspect_zip(&out).unwrap()["images"], 101);
    assert_eq!(inspect_zip(&out).unwrap()["backgrounds"], 1);
    let mut zip = zip::ZipArchive::new(fs::File::open(out).unwrap()).unwrap();
    let mut label = String::new();
    zip.by_name("000002.txt")
        .unwrap()
        .read_to_string(&mut label)
        .unwrap();
    assert!(label.is_empty());
    let mut classes = String::new();
    zip.by_name("classes.txt")
        .unwrap()
        .read_to_string(&mut classes)
        .unwrap();
    assert_eq!(classes, "H\n");
    for s in &mut f.project.samples {
        s.boxes.clear();
        s.background = true;
    }
    assert!(
        f.project
            .validate_export()
            .iter()
            .any(|s| s.contains("採用枠が0個"))
    );
}

#[test]
fn unresolved_images_block_export_until_held_or_excluded() {
    let mut f = Fixture::populated(101);
    let missing = f.image("missing.png", None);
    f.import(&[missing]);
    let index = f.project.samples.len() - 1;
    let id = f.project.samples[index].id;
    assert!(
        f.project
            .validate_export()
            .iter()
            .any(|s| s.contains("要確認"))
    );
    f.project.samples[index].decision = Decision::Pending;
    assert!(
        f.project
            .validate_export()
            .iter()
            .any(|s| s.contains("要確認"))
    );
    f.project.decide(&[id], Decision::Hold);
    assert!(f.project.validate_export().is_empty());
    f.project.decide(&[id], Decision::Exclude);
    assert!(f.project.validate_export().is_empty());
}

#[test]
fn import_csv_assigns_detected_classes_and_manual_edit_clears_auto() {
    let mut f = Fixture::new();
    f.project.set_classes(vec!["H".into(), "S".into()]).unwrap();
    let path = f.image("2.png", None);
    let mut im = image::RgbImage::from_pixel(160, 120, image::Rgb([255, 255, 255]));
    for y in 20..90 {
        for x in 50..100 {
            im.put_pixel(x, y, image::Rgb([0, 0, 0]));
        }
    }
    im.save(&path).unwrap();
    let csv = f.source.join("ranges.csv");
    fs::write(&csv, "start,end,class_id\n1,3,1\n").unwrap();
    let options = ImportOptions {
        ranges: Some(csv),
        ..Default::default()
    };
    f.project
        .import_with_options(&f.source, &[path], None, &options, &flag(), &mut |_, _| {})
        .unwrap();
    let s = &f.project.samples[0];
    assert_eq!(s.boxes[0].class_id, 1);
    assert!(s.automatic_label);
    let (id, boxes) = (s.id, s.boxes.clone());
    f.project
        .edit(id, boxes, Decision::Keep, true, false)
        .unwrap();
    let s = &f.project.samples[0];
    assert!(!s.automatic_label && !s.background);
    assert_eq!(s.decision_origin, DecisionOrigin::Manual);
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
fn background_selection_undoes_as_a_group() {
    let mut f = Fixture::populated(3);
    f.project.samples[0].proposal_pending = true;
    f.project.samples[1].decision = Decision::Exclude;
    let before = f.project.samples.clone();
    f.project.mark_background(&[before[0].id, before[1].id]);
    for s in &f.project.samples[..2] {
        assert!(s.background && s.has_label && s.reviewed);
        assert!(s.boxes.is_empty() && !s.proposal_pending);
    }
    assert_eq!(f.project.samples[0].decision, Decision::Keep);
    assert_eq!(f.project.samples[1].decision, Decision::Exclude);
    assert_eq!(f.project.samples[2], before[2]);
    assert!(f.project.undo());
    assert_eq!(f.project.samples, before);
}

#[test]
fn matching_image_stems_get_unique_copy_names() {
    let mut f = Fixture::new();
    let label = "0 .5 .5 .3 .4";
    let first = f.image("H/000123.png", Some(label));
    let second = f.image("S/000123.bmp", Some(label));
    let third = f.image("U/000123.PNG", Some(label));
    f.import(&[first, second.clone()]);
    f.import(&[third]);
    let names = f
        .project
        .samples
        .iter()
        .map(|s| s.path.file_name().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["000123.png", "000123_2.bmp", "000123_3.PNG"]);
    assert_eq!(f.project.samples[1].boxes.len(), 1);
    assert!(second.is_file());
    assert_eq!(
        fs::read_to_string(second.with_extension("txt")).unwrap(),
        label
    );
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
    assert_eq!(f.project.samples[0].decision, Decision::Fix);
    assert!(f.project.undo());
    assert_eq!(f.project.samples[0].decision, Decision::Keep);
}

#[test]
fn batch_class_change_only_updates_existing_boxes_and_undoes_together() {
    let mut f = Fixture::populated(3);
    f.project.set_classes(vec!["H".into(), "S".into()]).unwrap();
    let [a, b, _] = f.project.samples.as_mut_slice() else {
        unreachable!()
    };
    b.boxes.clear();
    b.background = true;
    let mut second_box = a.boxes[0].clone();
    second_box.cx = 0.25;
    a.boxes.push(second_box);
    a.proposals = vec![a.boxes[0].clone()];
    a.proposal_pending = true;
    let selected = [a.id, b.id];
    let original = serde_json::to_value(&f.project.samples).unwrap();
    assert_eq!(f.project.change_box_class(&selected, 1).unwrap(), 1);
    let mut expected = original.clone();
    for bbox in expected[0]["boxes"].as_array_mut().unwrap() {
        bbox["class_id"] = 1.into();
    }
    assert_eq!(serde_json::to_value(&f.project.samples).unwrap(), expected);
    assert_eq!(f.project.change_box_class(&selected, 1).unwrap(), 0);
    assert!(f.project.undo());
    assert_eq!(serde_json::to_value(&f.project.samples).unwrap(), original);
}

#[test]
fn batch_removal_persists_without_recovery_resurrecting_images_and_undo_restores_order() {
    let mut f = Fixture::populated(5);
    let original = f.project.samples.clone();
    let removed = [original[1].id, original[3].id, original[4].id];
    assert_eq!(f.project.remove_samples(&removed), 3);
    f.project.save().unwrap();
    let journal = f
        .project
        .assets()
        .join("transactions")
        .join("leftover.json");
    fs::write(&journal, serde_json::to_vec(&original).unwrap()).unwrap();
    let reopened = Project::load(&f.project.file).unwrap();
    assert_eq!(
        reopened.samples.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![original[0].id, original[2].id]
    );
    let new_path = f.image("new.png", Some("0 .5 .5 .3 .4"));
    f.import(&[new_path]);
    let added = f.project.samples.last().unwrap().id;
    assert!(!journal.exists());
    assert!(f.project.undo());
    let expected = original
        .iter()
        .map(|s| s.id)
        .chain([added])
        .collect::<Vec<_>>();
    assert_eq!(
        f.project.samples.iter().map(|s| s.id).collect::<Vec<_>>(),
        expected
    );
    for sample in &original {
        assert!(sample.origin_root.join(&sample.origin_relative).is_file());
        assert!(f.project.image_path(sample).unwrap().is_file());
    }
}

#[test]
fn batch_otsu_replaces_selected_boxes_and_leaves_no_partial_changes_on_cancel() {
    let mut f = Fixture::new();
    let paths = [
        f.image("1.png", Some("0 .5 .5 .2 .2")),
        f.image("2.png", Some("0 .5 .5 .2 .2")),
        f.image("3.png", Some("0 .5 .5 .2 .2")),
    ];
    let image = image::RgbImage::from_fn(32, 24, |x, y| {
        image::Rgb(
            [if (8..24).contains(&x) && (6..18).contains(&y) {
                0
            } else {
                255
            }; 3],
        )
    });
    image.save(&paths[0]).unwrap();
    f.import(&paths);
    f.project.set_classes(vec!["H".into(), "S".into()]).unwrap();
    let [a, b, _] = f.project.samples.as_mut_slice() else {
        unreachable!()
    };
    a.decision = Decision::Hold;
    a.decision_origin = DecisionOrigin::Manual;
    a.proposals = a.boxes.clone();
    a.proposal_pending = true;
    a.proposal_info = serde_json::json!({"method":"old"});
    let selected = [a.id, b.id];
    let original = serde_json::to_value(&f.project.samples).unwrap();

    let stop = flag();
    assert!(
        f.project
            .replace_boxes_with_otsu(&selected, 1, &stop, &mut |done, total| {
                if done == total {
                    stop.store(true, Ordering::Relaxed);
                }
            })
            .is_err()
    );
    assert_eq!(serde_json::to_value(&f.project.samples).unwrap(), original);
    assert!(f.project.undo.is_empty());

    assert_eq!(
        f.project
            .replace_boxes_with_otsu(&selected, 1, &flag(), &mut |_, _| {})
            .unwrap(),
        (1, 1)
    );
    let [a, b, c] = f.project.samples.as_slice() else {
        unreachable!()
    };
    assert_eq!(a.decision, Decision::Hold);
    assert_eq!(a.decision_origin, DecisionOrigin::Manual);
    assert!(a.automatic_label && a.approvable() && a.proposal_info.is_null());
    assert_eq!(a.boxes[0].class_id, 1);
    assert_eq!(a.boxes[0].width, 0.5);
    assert_eq!(b.decision, Decision::Fix);
    assert!(b.boxes.is_empty() && !b.has_label && !b.label_errors.is_empty());
    assert_eq!(serde_json::to_value(c).unwrap(), original[2]);
    assert!(f.project.undo());
    assert_eq!(serde_json::to_value(&f.project.samples).unwrap(), original);
}
