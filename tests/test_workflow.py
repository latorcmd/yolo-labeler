"""Behavioral regression checks. All generated data stays in temporary folders."""
from copy import deepcopy
from hashlib import sha256
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import cv2
import numpy as np
from PIL import Image, ImageDraw

from yolo_labeler.core import (Box, Project, Sample, export_vtraining, inspect_zip,
                               parse_yolo, safe_source, scan_dataset, validate_project)
from yolo_labeler.autolabel import Config, assign_classes, propose_batch, propose_one
from yolo_labeler.vendor import unitv_labeler as backend
from yolo_labeler.vision import analyze_quality, safe_exclusion_candidates, import_proposals


class DatasetTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / ".qa"
        scratch.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="yolo-test-", dir=scratch)
        self.root = Path(self.temp.name) / "日本語 データ"
        self.root.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def image(self, name="001.png", label="0 0.5 0.5 0.3 0.6\n"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        image = Image.new("RGB", (160, 100), "white")
        ImageDraw.Draw(image).rectangle((55, 20, 104, 79), fill="black")
        image.save(path)
        if label is not None:
            path.with_suffix(".txt").write_text(label, encoding="utf-8")
        return path

    def accepted_project(self, count=101):
        for number in range(count):
            self.image(f"nested/{number:03}.png")
        project = scan_dataset(self.root, ["H"])
        for sample in project.samples:
            sample.decision, sample.reviewed = "keep", True
        return project

    def test_strict_yolo_parser_rejects_partial_bad_rows(self):
        for text in ("0 0.5 0.5 0.2", "0.5 0.5 0.5 0.2 0.2", "1 0.5 0.5 0.2 0.2",
                     "0 nan 0.5 0.2 0.2", "0 inf 0.5 0.2 0.2", "0 0.1 0.5 0.8 0.2",
                     "0 0.5 0.5 0 0.2", "0 0.5 0.5 -0.2 0.2"):
            with self.subTest(text=text):
                boxes, errors = parse_yolo(text, 1)
                self.assertEqual(boxes, [])
                self.assertTrue(errors)
        boxes, errors = parse_yolo("\ufeff0 0.5 0.5 1 1\n", 1)
        self.assertFalse(errors)
        self.assertEqual(len(boxes), 1)

    def test_paths_cannot_escape_source_root(self):
        for path in ("../outside.jpg", "/absolute.jpg", "C:/x.jpg", "folder\\x.jpg"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                safe_source(self.root, path)

    def test_scan_preserves_missing_invalid_and_empty_labels(self):
        self.image("good.JPG")
        self.image("missing.png", None)
        self.image("background.png", "")
        self.image("bad.png", "0 0.9 0.5 0.8 0.3")
        (self.root / "broken.jpg").write_bytes(b"bad image")
        (self.root / "orphan.txt").write_text("0 0.5 0.5 0.2 0.2")
        project = scan_dataset(self.root, ["H"])
        samples = {s.path: s for s in project.samples}
        self.assertFalse(samples["missing.png"].has_label)
        self.assertTrue(samples["background.png"].has_label)
        self.assertTrue(samples["bad.png"].label_errors)
        self.assertTrue(samples["broken.jpg"].image_error)
        self.assertTrue(any("orphan.txt" in w for w in project.import_warnings))

    def test_separate_yolo_images_and_labels_trees(self):
        path = self.image("images/train/a.png", None)
        labels = self.root / "labels/train"
        labels.mkdir(parents=True)
        (labels / "a.txt").write_text("0 0.5 0.5 0.2 0.2")
        project = scan_dataset(self.root, ["H"])
        self.assertTrue(project.samples[0].has_label)
        self.assertEqual(project.image_path(project.samples[0]), path)

    def test_six_decimal_roundoff_is_normalized_but_real_overflow_is_rejected(self):
        boxes, errors = parse_yolo("0 0.332589 0.301339 0.665179 0.602679", 1)
        self.assertFalse(errors)
        self.assertEqual(len(boxes), 1)
        box = boxes[0]
        self.assertGreaterEqual(box.cx - box.width / 2, 0)
        self.assertGreaterEqual(box.cy - box.height / 2, 0)
        self.assertTrue(parse_yolo("0 0.33 0.30 0.67 0.61", 1)[1])

    def test_atomic_project_roundtrip_and_backup(self):
        self.image()
        project = scan_dataset(self.root, ["H"])
        path = self.root.parent / "session.ylproject"
        project.save(path)
        project.samples[0].decision = "exclude"
        project.samples[0].reason = "手動選別"
        project.ui_state = {"page": 3}
        project.save()
        restored = Project.load(path)
        self.assertEqual(restored.samples[0].reason, "手動選別")
        self.assertEqual(restored.ui_state["page"], 3)
        self.assertEqual(Project.load(Path(str(path) + ".bak")).samples[0].decision, "pending")

    def test_export_exact_flat_structure_rgb_and_originals_untouched(self):
        project = self.accepted_project()
        before = {s.path: project.image_path(s).read_bytes() for s in project.samples}
        destination = self.root.parent / "detector.zip"
        report = export_vtraining(project, destination)
        self.assertEqual(report["summary"]["images"], 101)
        self.assertTrue(destination.with_suffix(".report.json").exists())
        with zipfile.ZipFile(destination) as archive:
            self.assertEqual(len(archive.namelist()), 204)
            self.assertEqual(archive.read("000001.txt").decode(), project.samples[0].boxes[0].line() + "\n")
            self.assertNotIn("rename.bat", archive.namelist())
        self.assertEqual(inspect_zip(destination)["boxes"], 101)
        self.assertTrue(all(project.image_path(s).read_bytes() == before[s.path] for s in project.samples))
        with self.assertRaises(FileExistsError):
            export_vtraining(project, destination)

    def test_export_checks_count_review_and_pending_candidates(self):
        project = self.accepted_project(100)
        issues = validate_project(project)
        self.assertTrue(any("101" in i.message for i in issues))
        project.samples[0].proposal_pending = True
        project.samples[1].reviewed = False
        issues = validate_project(project)
        self.assertTrue(any("自動候補" in i.message for i in issues))
        self.assertTrue(any("未承認" in i.message for i in issues))

    def test_export_detects_source_change_without_partial_output(self):
        project = self.accepted_project()
        self.image(project.samples[-1].path)
        project.image_path(project.samples[-1]).write_bytes(b"changed")
        destination = self.root.parent / "changed.zip"
        with self.assertRaisesRegex(ValueError, "変更"):
            export_vtraining(project, destination)
        self.assertFalse(destination.exists())
        self.assertFalse(list(destination.parent.glob(".vtraining-*")))

    def test_export_cancellation_leaves_no_output(self):
        project = self.accepted_project()
        destination = self.root.parent / "cancelled.zip"
        with self.assertRaises(InterruptedError):
            export_vtraining(project, destination, cancelled=lambda: True)
        self.assertFalse(destination.exists())

    def test_empty_labels_are_blocked_by_default(self):
        project = self.accepted_project()
        project.samples[0].boxes = []
        self.assertTrue(any("枠が0個" in i.message for i in validate_project(project)))
        self.assertFalse(any("枠が0個" in i.message for i in validate_project(project, True)))

    def test_zip_inspector_rejects_parent_folder(self):
        destination = self.root.parent / "wrong.zip"
        with zipfile.ZipFile(destination, "w") as archive:
            archive.writestr("parent/classes.txt", "H\n")
        with self.assertRaisesRegex(ValueError, "サブフォルダー"):
            inspect_zip(destination)

    def test_unitv_adapter_matches_existing_detector(self):
        path = self.image()
        config = Config(padding=3)
        with Image.open(path) as image:
            pixels = cv2.cvtColor(np.asarray(image.convert("RGB")), cv2.COLOR_RGB2BGR)
        expected = backend.detect(pixels, config)
        boxes, info = propose_one(path, 0, config)
        self.assertEqual(len(boxes), 1)
        self.assertAlmostEqual(boxes[0].width, expected.box.width / 160)
        self.assertAlmostEqual(boxes[0].cy, (expected.box.y + expected.box.height / 2) / 100)
        self.assertEqual(info["method"], expected.method)
        self.assertFalse(boxes[0].errors(1))

    def test_csv_ranges_are_inclusive_and_require_complete_mapping(self):
        samples = [Sample("001.JPG"), Sample("100.png"), Sample("101.JPG")]
        csv = self.root / "ranges.csv"
        csv.write_text("start,end,class_id\n1,100,0\n101,200,1\n")
        result = assign_classes(samples, 2, 0, csv)
        self.assertEqual(list(result.values()), [0, 0, 1])
        with self.assertRaises(ValueError):
            assign_classes([Sample("img_1_2.jpg")], 2, 0, csv)
        with self.assertRaises(ValueError):
            assign_classes([Sample("201.jpg")], 2, 0, csv)

    def test_batch_proposals_protect_human_reviewed_labels(self):
        for name in ("1.png", "2.png", "3.png", "4.png"):
            self.image(name)
        project = scan_dataset(self.root, ["H"])
        project.samples[0].reviewed = True
        project.samples[1].decision = "exclude"
        project.samples[2].decision = "keep"
        before = deepcopy(project.samples)
        result = propose_batch(project, Config(), 0)
        self.assertEqual(set(result["proposals"]), {3})
        self.assertEqual(project.samples, before)

    def test_quality_duplicate_conflicts_and_post_scan_edits_are_protected(self):
        self.image("1.png")
        shutil.copy2(self.root / "1.png", self.root / "2.png")
        shutil.copy2(self.root / "1.txt", self.root / "2.txt")
        project = scan_dataset(self.root, ["H"])
        for i, quality in analyze_quality(project).items():
            project.samples[i].quality = quality
        self.assertEqual(set(safe_exclusion_candidates(project)), {1})
        project.samples[1].boxes[0].width = 0.2
        self.assertEqual(safe_exclusion_candidates(project), {})
        project.samples[1].boxes[0].width = 0.3
        project.samples[1].proposal_pending = True
        self.assertEqual(safe_exclusion_candidates(project), {})

    def test_blur_and_lighting_are_only_review_candidates(self):
        self.image()
        project = scan_dataset(self.root, ["H"])
        project.samples[0].quality = {"flags": ["ぼけ候補", "黒つぶれ候補", "白飛び候補"]}
        self.assertEqual(safe_exclusion_candidates(project), {})

    def test_external_proposals_require_matching_class_order(self):
        self.image()
        project = scan_dataset(self.root, ["H"])
        candidates = self.root.parent / "candidates"
        candidates.mkdir()
        (candidates / "001.txt").write_text("0 0.5 0.5 0.2 0.2")
        with self.assertRaises(ValueError):
            import_proposals(project, candidates)
        (candidates / "classes.txt").write_text("S\n")
        with self.assertRaises(ValueError):
            import_proposals(project, candidates)
        (candidates / "classes.txt").write_text("H\n")
        self.assertEqual(len(import_proposals(project, candidates)["proposals"][0]), 1)


if __name__ == "__main__":
    unittest.main()
