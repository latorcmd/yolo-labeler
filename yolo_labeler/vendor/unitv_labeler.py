"""Batch bounding-box proposals for dark symbols, without machine learning."""

from __future__ import annotations

import argparse
import bisect
import csv
from dataclasses import dataclass
import math
from pathlib import Path
import re
import sys

import cv2
import numpy as np


class LabelerError(Exception):
    """An input, configuration, or output error suitable for the command line."""


@dataclass(frozen=True)
class Box:
    # right and bottom are exclusive pixel coordinates.
    x: int
    y: int
    width: int
    height: int

    @property
    def right(self) -> int:
        return self.x + self.width

    @property
    def bottom(self) -> int:
        return self.y + self.height

    @property
    def area(self) -> int:
        return self.width * self.height


@dataclass(frozen=True)
class Config:
    threshold: int | None = None
    min_area_ratio: float = 0.001
    merge_component_ratio: float = 0.05
    merge_gap_ratio: float = 0.20
    padding: int = 0

    def validate(self) -> None:
        if self.threshold is not None and not 0 <= self.threshold <= 255:
            raise LabelerError("--threshold must be an integer between 0 and 255")
        for name, value in (
            ("--min-area-ratio", self.min_area_ratio),
            ("--merge-component-ratio", self.merge_component_ratio),
            ("--merge-gap-ratio", self.merge_gap_ratio),
        ):
            if not math.isfinite(value) or not 0 <= value <= 1:
                raise LabelerError(f"{name} must be a finite number between 0 and 1")
        if self.padding < 0:
            raise LabelerError("--padding must be a nonnegative integer")


@dataclass(frozen=True)
class ClassRange:
    start: int
    end: int
    class_id: int


@dataclass(frozen=True)
class ImageJob:
    path: Path
    number: int
    class_id: int


@dataclass(frozen=True)
class Detection:
    box: Box | None
    method: str
    threshold: int
    otsu_threshold: int | None
    component_count: int
    foreground_ratio: float
    initial_foreground_ratio: float
    flags: tuple[str, ...]


def load_ranges(path: Path) -> list[ClassRange]:
    ranges = []
    try:
        with path.open("r", encoding="utf-8-sig", newline="") as handle:
            reader = csv.reader(handle, strict=True)
            header = next(reader, None)
            if header is None or [s.strip() for s in header] != [
                "start", "end", "class_id"
            ]:
                raise LabelerError("CSV header must be start,end,class_id")
            for row in reader:
                if not row or all(not value.strip() for value in row):
                    continue
                if len(row) != 3 or any(
                    re.fullmatch(r"[0-9]+", value.strip()) is None for value in row
                ):
                    raise LabelerError(
                        f"CSV line {reader.line_num}: expected three nonnegative integers"
                    )
                start, end, class_id = (int(value.strip()) for value in row)
                if start > end:
                    raise LabelerError(f"CSV line {reader.line_num}: start exceeds end")
                ranges.append(ClassRange(start, end, class_id))
    except (OSError, UnicodeError, csv.Error, ValueError) as exc:
        raise LabelerError(f"Cannot read class ranges {path}: {exc}") from exc
    if not ranges:
        raise LabelerError("The class range CSV has no data rows")
    ranges.sort(key=lambda item: item.start)
    for previous, current in zip(ranges, ranges[1:]):
        if current.start <= previous.end:
            raise LabelerError(
                f"Overlapping class ranges: {previous.start}-{previous.end} "
                f"and {current.start}-{current.end}"
            )
    return ranges


def prepare_jobs(input_dir: Path, ranges: list[ClassRange]) -> list[ImageJob]:
    if not input_dir.is_dir():
        raise LabelerError(f"Input is not a directory: {input_dir}")
    paths = sorted(
        (path for path in input_dir.iterdir()
         if path.is_file() and path.suffix.lower() in {".jpg", ".jpeg", ".png"}),
        key=lambda path: (path.name.casefold(), path.name),
    )
    if not paths:
        raise LabelerError("No JPEG or PNG images found directly in the input directory")
    starts = [item.start for item in ranges]
    stems = set()
    jobs = []
    missing = []
    for path in paths:
        key = path.stem.casefold()
        if key in stems:
            raise LabelerError(f"Output name collision for image stem: {path.stem}")
        stems.add(key)
        numbers = re.findall(r"[0-9]+", path.stem)
        if len(numbers) != 1:
            raise LabelerError(
                f"Image name must contain exactly one number: {path.name}"
            )
        number = int(numbers[0])
        index = bisect.bisect_right(starts, number) - 1
        if index < 0 or number > ranges[index].end:
            missing.append(path.name)
        else:
            jobs.append(ImageJob(path, number, ranges[index].class_id))
    if missing:
        examples = ", ".join(missing[:10])
        raise LabelerError(f"No class range for {len(missing)} image(s): {examples}")
    return sorted(jobs, key=lambda job: (job.number, job.path.name.casefold()))


def box_distance(first: Box, second: Box) -> float:
    dx = max(first.x - second.right, second.x - first.right, 0)
    dy = max(first.y - second.bottom, second.y - first.bottom, 0)
    return math.hypot(dx, dy)


def select_components(mask: np.ndarray, config: Config) -> tuple[Box | None, int]:
    height, width = mask.shape
    _, _, stats, _ = cv2.connectedComponentsWithStats(mask, connectivity=8)
    min_area = max(1, math.ceil(width * height * config.min_area_ratio))
    components = [
        (Box(int(x), int(y), int(w), int(h)), int(area))
        for x, y, w, h, area in stats[1:]
        if area >= min_area
    ]
    if not components:
        return None, 0
    components.sort(key=lambda item: item[1], reverse=True)
    selected = [components[0][0]]
    main_area = components[0][1]
    pending = [box for box, area in components[1:]
               if area >= main_area * config.merge_component_ratio]
    max_distance = min(width, height) * config.merge_gap_ratio
    # Grow a component graph, rather than the union rectangle: empty space inside
    # a wide union must not itself attract an unrelated dark component.
    while pending:
        additions = [box for box in pending if any(
            box_distance(box, previous) <= max_distance for previous in selected
        )]
        if not additions:
            break
        selected.extend(additions)
        pending = [box for box in pending if box not in additions]
    x = min(box.x for box in selected)
    y = min(box.y for box in selected)
    right = max(box.right for box in selected)
    bottom = max(box.bottom for box in selected)
    return Box(x, y, right - x, bottom - y), len(selected)


def detect(image: np.ndarray, config: Config) -> Detection:
    config.validate()
    height, width = image.shape[:2]
    gray = cv2.cvtColor(image, cv2.COLOR_BGR2GRAY)
    if config.threshold is None:
        threshold, mask = cv2.threshold(
            gray, 0, 255, cv2.THRESH_BINARY_INV | cv2.THRESH_OTSU
        )
        method = "otsu"
        otsu_threshold = int(threshold)
    else:
        threshold, mask = cv2.threshold(
            gray, config.threshold, 255, cv2.THRESH_BINARY_INV
        )
        method = "manual"
        otsu_threshold = None
    box, count = select_components(mask, config)
    initial_ratio = float(np.count_nonzero(mask) / mask.size)
    flags = []
    if config.threshold is None and (
        initial_ratio > 0.5 or (box is not None and box.area / (width * height) > 0.9)
    ):
        value = cv2.cvtColor(image, cv2.COLOR_BGR2HSV)[:, :, 2]
        mask = np.where(value < 60, 255, 0).astype(np.uint8)
        box, count = select_components(mask, config)
        threshold = 60
        method = "hsv_v60"
        flags.append("fallback_used")
    foreground_ratio = float(np.count_nonzero(mask) / mask.size)
    if foreground_ratio > 0.5:
        flags.append("large_foreground")
    if box is not None:
        if box.area / (width * height) > 0.9:
            flags.append("large_box")
        if count > 1:
            flags.append("merged_components")
        x = max(0, box.x - config.padding)
        y = max(0, box.y - config.padding)
        right = min(width, box.right + config.padding)
        bottom = min(height, box.bottom + config.padding)
        box = Box(x, y, right - x, bottom - y)
        if x == 0 or y == 0 or right == width or bottom == height:
            flags.append("touches_border")
    return Detection(box, method, int(threshold), otsu_threshold, count,
                     foreground_ratio, initial_ratio, tuple(flags))


def yolo_line(class_id: int, box: Box | None, width: int, height: int) -> str:
    if box is None:
        return ""
    values = ((box.x + box.width / 2) / width,
              (box.y + box.height / 2) / height,
              box.width / width, box.height / height)
    return f"{class_id} " + " ".join(f"{value:.6f}" for value in values) + "\n"


def read_image(path: Path) -> np.ndarray:
    # Avoid cv2.imread's Unicode path limitations on Windows.
    data = np.frombuffer(path.read_bytes(), dtype=np.uint8)
    if not data.size:
        raise LabelerError("Empty image file")
    image = cv2.imdecode(data, cv2.IMREAD_COLOR)
    if image is None:
        raise LabelerError("Cannot decode image")
    return image


def preview_bytes(image: np.ndarray, class_id: int, detection: Detection) -> bytes:
    preview = image.copy()
    box = detection.box
    text = "NO DETECTION" if box is None else f"class {class_id}"
    color = (0, 200, 255) if "fallback_used" in detection.flags else (0, 0, 255)
    if box is not None:
        cv2.rectangle(preview, (box.x, box.y), (box.right - 1, box.bottom - 1), color, 1)
    text_size = cv2.getTextSize(text, cv2.FONT_HERSHEY_SIMPLEX, 0.4, 1)[0]
    cv2.rectangle(preview, (0, 0), (text_size[0] + 6, text_size[1] + 8), (255, 255, 255), -1)
    cv2.putText(preview, text, (3, text_size[1] + 3),
                cv2.FONT_HERSHEY_SIMPLEX, 0.4, color, 1, cv2.LINE_AA)
    success, encoded = cv2.imencode(".png", preview)
    if not success:
        raise LabelerError("Cannot encode preview PNG")
    return encoded.tobytes()


REPORT_FIELDS = [
    "filename", "image_number", "class_id", "status", "image_width", "image_height",
    "x", "y", "width", "height", "method", "threshold", "otsu_threshold",
    "component_count", "foreground_ratio", "initial_foreground_ratio", "flags", "error",
]


def validate_output(output_dir: Path) -> None:
    if output_dir.is_symlink():
        raise LabelerError("Output directory must not be a symbolic link")
    if output_dir.exists():
        if not output_dir.is_dir() or any(output_dir.iterdir()):
            raise LabelerError(f"Output must be a new or empty directory: {output_dir}")


def run_batch(input_dir: Path, ranges_path: Path, output_dir: Path, config: Config) -> dict[str, int]:
    config.validate()
    ranges = load_ranges(ranges_path)
    jobs = prepare_jobs(input_dir, ranges)
    validate_output(output_dir)
    # Everything above is read-only: reject bad mappings before creating output.
    output_dir.mkdir(parents=True, exist_ok=True)
    labels_dir = output_dir / "labels"
    previews_dir = output_dir / "previews"
    labels_dir.mkdir()
    previews_dir.mkdir()
    totals = {"ok": 0, "no_detection": 0, "read_error": 0, "flagged": 0}
    with (output_dir / "report.csv").open("x", encoding="utf-8-sig", newline="") as report:
        writer = csv.DictWriter(report, fieldnames=REPORT_FIELDS)
        writer.writeheader()
        report.flush()
        for job in jobs:
            row = {"filename": job.path.name, "image_number": job.number,
                   "class_id": job.class_id}
            try:
                image = read_image(job.path)
            except (OSError, LabelerError, cv2.error) as exc:
                row.update(status="read_error", error=str(exc))
                totals["read_error"] += 1
            else:
                detection = detect(image, config)
                height, width = image.shape[:2]
                status = "ok" if detection.box is not None else "no_detection"
                # Encode before writing either file; a PNG failure is never success.
                preview = preview_bytes(image, job.class_id, detection)
                label = yolo_line(job.class_id, detection.box, width, height)
                with (labels_dir / f"{job.path.stem}.txt").open("x", encoding="utf-8", newline="\n") as handle:
                    handle.write(label)
                with (previews_dir / f"{job.path.stem}.png").open("xb") as handle:
                    handle.write(preview)
                row.update(status=status, image_width=width, image_height=height,
                           method=detection.method, threshold=detection.threshold,
                           otsu_threshold=detection.otsu_threshold,
                           component_count=detection.component_count,
                           foreground_ratio=f"{detection.foreground_ratio:.6f}",
                           initial_foreground_ratio=f"{detection.initial_foreground_ratio:.6f}",
                           flags=";".join(detection.flags))
                if detection.box is not None:
                    box = detection.box
                    row.update(x=box.x, y=box.y, width=box.width, height=box.height)
                totals[status] += 1
                if detection.flags:
                    totals["flagged"] += 1
            writer.writerow(row)
            report.flush()
    return totals


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True, help="Directory of numbered JPEG/PNG images")
    parser.add_argument("--class-ranges", type=Path, required=True, help="CSV with start,end,class_id")
    parser.add_argument("--output", type=Path, required=True, help="New or empty output directory")
    parser.add_argument("--threshold", type=int, help="Manual grayscale threshold, 0-255; disables Otsu/fallback")
    parser.add_argument("--min-area-ratio", type=float, default=0.001, help="Minimum component area / image area (default: 0.001)")
    parser.add_argument("--merge-component-ratio", type=float, default=0.05, help="Minimum added component area / main area (default: 0.05)")
    parser.add_argument("--merge-gap-ratio", type=float, default=0.20, help="Maximum component gap / image short side (default: 0.20)")
    parser.add_argument("--padding", type=int, default=0, help="Bounding-box padding in pixels (default: 0)")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    config = Config(args.threshold, args.min_area_ratio, args.merge_component_ratio,
                    args.merge_gap_ratio, args.padding)
    try:
        totals = run_batch(args.input, args.class_ranges, args.output, config)
    except (LabelerError, OSError, cv2.error) as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2
    print(f"Done: {totals['ok']} detected, {totals['no_detection']} empty, "
          f"{totals['read_error']} read errors, {totals['flagged']} flagged")
    print(f"Output: {args.output.resolve()}")
    return 1 if totals["read_error"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
