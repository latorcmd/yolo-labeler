"""GUI adapter around the existing UnitV-color/tools/yolo_labeler algorithm."""

import bisect
from dataclasses import asdict
from pathlib import Path
import re

import cv2
import numpy as np
from PIL import Image

from .core import Box, Project, Sample
from .vendor import unitv_labeler as backend

Config = backend.Config


def assign_classes(samples: list[Sample], class_count: int, fixed_class: int,
                   ranges_path: Path | None = None) -> dict[str, int]:
    if not 0 <= fixed_class < class_count:
        raise ValueError("クラスIDが範囲外です")
    if ranges_path is None:
        return {s.path: fixed_class for s in samples}
    ranges = backend.load_ranges(ranges_path)
    if any(r.class_id >= class_count for r in ranges):
        raise ValueError("範囲CSVのクラスIDがクラス一覧の範囲外です")
    starts = [r.start for r in ranges]
    result = {}
    for sample in samples:
        numbers = re.findall(r"[0-9]+", Path(sample.path).stem)
        if len(numbers) != 1:
            raise ValueError(f"画像名に数字のまとまりが1つ必要です: {sample.path}")
        number = int(numbers[0])
        index = bisect.bisect_right(starts, number) - 1
        if index < 0 or number > ranges[index].end:
            raise ValueError(f"範囲CSVに対応がありません: {sample.path}")
        result[sample.path] = ranges[index].class_id
    return result


def propose_one(path: Path, class_id: int, config: Config) -> tuple[list[Box], dict]:
    config.validate()
    # Raw pixels are also used by the editor/exporter. Ignore EXIF orientation.
    with Image.open(path) as source:
        image = cv2.cvtColor(np.asarray(source.convert("RGB")), cv2.COLOR_RGB2BGR)
    detection = backend.detect(image, config)
    height, width = image.shape[:2]
    box = detection.box
    boxes = [] if box is None else [Box(class_id, (box.x + box.width / 2) / width,
                                      (box.y + box.height / 2) / height,
                                      box.width / width, box.height / height)]
    info = {"backend": "UnitV-color/tools/yolo_labeler", "method": detection.method,
            "threshold": detection.threshold, "otsu_threshold": detection.otsu_threshold,
            "component_count": detection.component_count, "flags": list(detection.flags),
            "foreground_ratio": detection.foreground_ratio, "class_id": class_id,
            "config": asdict(config), "status": "ok" if box else "no_detection"}
    return boxes, info


def propose_batch(project: Project, config: Config, fixed_class: int, ranges_path: Path | None = None,
                  progress=None, cancelled=None) -> dict:
    config.validate()
    targets = [(i, s) for i, s in enumerate(project.samples)
               if not s.reviewed and s.decision not in {"keep", "exclude"}]
    assignments = assign_classes([s for _, s in targets], len(project.classes), fixed_class, ranges_path)
    proposals, errors = {}, []
    for count, (index, sample) in enumerate(targets, 1):
        if cancelled and cancelled():
            break
        try:
            proposals[index] = propose_one(project.image_path(sample), assignments[sample.path], config)
        except (OSError, ValueError, cv2.error, backend.LabelerError) as exc:
            errors.append(f"{sample.path}: {exc}")
        if progress:
            progress(count, len(targets))
    return {"proposals": proposals, "errors": errors}
