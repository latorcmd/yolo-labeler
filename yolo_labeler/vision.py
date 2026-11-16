"""Local quality signals and reviewable annotation proposals (no model download)."""

from __future__ import annotations

from hashlib import sha256
from pathlib import Path
from typing import Callable

import cv2
import numpy as np
from PIL import Image

from .core import Box, Project, label_for, parse_yolo


def quality_for_image(path: Path, boxes: list[Box], blur_threshold: float = 45.0) -> dict:
    payload = path.read_bytes()
    with Image.open(path) as image:
        image.load()
        rgb = np.asarray(image.convert("RGB"))
    height, width = rgb.shape[:2]
    gray = cv2.cvtColor(rgb, cv2.COLOR_RGB2GRAY)
    roi_scores, minimum_side = [], min(width, height)
    # Compare at a consistent scale. When boxes exist, background edges cannot
    # hide blur of the actual target; only validated boxes should be passed here.
    for box in boxes:
        x1 = max(0, int((box.cx - box.width / 2) * width))
        y1 = max(0, int((box.cy - box.height / 2) * height))
        x2 = min(width, int(np.ceil((box.cx + box.width / 2) * width)))
        y2 = min(height, int(np.ceil((box.cy + box.height / 2) * height)))
        roi = gray[y1:y2, x1:x2]
        if roi.size and min(roi.shape) > 1:
            minimum_side = min(minimum_side, min(roi.shape))
            roi = cv2.resize(roi, (128, 128), interpolation=cv2.INTER_AREA)
            roi_scores.append(float(cv2.Laplacian(roi, cv2.CV_64F).var()))
    if not roi_scores:
        normalized = cv2.resize(gray, (256, 256), interpolation=cv2.INTER_AREA)
        roi_scores = [float(cv2.Laplacian(normalized, cv2.CV_64F).var())]
    small = cv2.resize(gray, (9, 8), interpolation=cv2.INTER_AREA)
    flags = []
    sharpness = min(roi_scores)
    dark = float(np.mean(gray < 12))
    bright = float(np.mean(gray > 245))
    if sharpness < blur_threshold:
        flags.append("ぼけ候補")
    if dark > 0.85:
        flags.append("黒つぶれ候補")
    if bright > 0.95:
        flags.append("白飛び候補")
    if minimum_side < 20:
        flags.append("対象が小さい")
    return {"sha256": sha256(payload).hexdigest(), "sharpness": round(sharpness, 2),
            "brightness": round(float(gray.mean()), 2), "dark_fraction": round(dark, 4),
            "bright_fraction": round(bright, 4), "min_target_pixels": minimum_side,
            "dhash": "".join("1" if v else "0" for v in (small[:, 1:] > small[:, :-1]).flat),
            "flags": flags, "score": len(flags) * 100 + max(0, blur_threshold - sharpness)}


def analyze_quality(project: Project, progress: Callable | None = None,
                    cancelled: Callable | None = None, blur_threshold: float = 45.0) -> dict[int, dict]:
    results, groups = {}, {}
    for i, sample in enumerate(project.samples):
        if cancelled and cancelled():
            break
        try:
            boxes = [b for b in sample.boxes if not b.errors(len(project.classes))]
            result = quality_for_image(project.image_path(sample), boxes, blur_threshold)
            if result["sha256"] != sample.source_hash:
                result["flags"].append("読み込み後に元画像が変更")
                result["changed"] = True
            groups.setdefault(result["sha256"], []).append(i)
        except (OSError, ValueError, cv2.error, Image.DecompressionBombError) as exc:
            result = {"flags": ["画像破損"], "error": str(exc), "score": 10000}
        results[i] = result
        if progress:
            progress(i + 1, len(project.samples))
    for indices in groups.values():
        if len(indices) < 2:
            continue
        # Keep reviewed/accepted representatives first, then stable input order.
        representative = min(indices, key=lambda j: (project.samples[j].decision != "keep", j))
        signatures = {tuple(sorted(b.line() for b in project.samples[j].boxes)) for j in indices}
        conflict = (len(signatures) > 1 or any(project.samples[j].label_errors
                    or project.samples[j].proposal_pending for j in indices))
        for i in indices:
            results[i]["duplicate_of"] = representative
            results[i]["duplicate_conflict"] = conflict
            results[i]["flags"].append("重複・ラベル不一致" if conflict else "完全重複")
            results[i]["score"] += 200
    return results


def safe_exclusion_candidates(project: Project) -> dict[int, str]:
    candidates = {}
    for i, sample in enumerate(project.samples):
        if sample.decision != "pending" or sample.reviewed:
            continue
        quality = sample.quality
        if quality.get("changed"):
            continue
        # Quality results can become stale after an external file edit. Never
        # automatically exclude a sample using an old content fingerprint.
        try:
            if sha256(project.image_path(sample).read_bytes()).hexdigest() != sample.source_hash:
                continue
        except OSError:
            continue
        if "画像破損" in quality.get("flags", []):
            candidates[i] = "画像破損（品質検査）"
        elif ("duplicate_of" in quality and quality["duplicate_of"] != i
              and not quality.get("duplicate_conflict") and sample.has_label
              and not sample.proposals and not sample.label_errors):
            representative = project.samples[quality["duplicate_of"]]
            if (representative.decision not in {"exclude", "hold", "fix"}
                    and representative.has_label and not representative.proposals
                    and not representative.proposal_pending and not sample.proposal_pending
                    and not representative.label_errors
                    and sorted(b.line() for b in representative.boxes) == sorted(b.line() for b in sample.boxes)):
                try:
                    if sha256(project.image_path(representative).read_bytes()).hexdigest() == sample.source_hash:
                        candidates[i] = f"完全重複（代表: {representative.path}）"
                except OSError:
                    pass
    return candidates


def import_proposals(project: Project, labels_root: Path, progress: Callable | None = None,
                     cancelled: Callable | None = None) -> dict:
    classes_file = labels_root / "classes.txt"
    if not classes_file.exists():
        raise ValueError("候補フォルダー直下に classes.txt が必要です。クラスIDの対応を確認します。")
    classes = classes_file.read_text(encoding="utf-8-sig").splitlines()
    if classes != project.classes:
        raise ValueError("候補の classes.txt とプロジェクトのクラス順が一致しません。")
    results, errors = {}, []
    for count, sample in enumerate(project.samples, 1):
        if cancelled and cancelled():
            break
        if not sample.reviewed and sample.decision not in {"exclude", "keep"}:
            label = label_for(Path(project.source_root), Path(sample.path), labels_root)
            if label:
                try:
                    boxes, problems = parse_yolo(label.read_text(encoding="utf-8-sig"), len(classes))
                    if problems:
                        errors.extend(f"{sample.path}: {p}" for p in problems)
                    else:
                        results[count - 1] = boxes
                except (OSError, UnicodeError) as exc:
                    errors.append(f"{sample.path}: {exc}")
        if progress:
            progress(count, len(project.samples))
    return {"proposals": results, "errors": errors}
