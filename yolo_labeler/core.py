"""Non-destructive project storage, strict YOLO parsing and detector ZIP export."""
from __future__ import annotations

from collections import Counter
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from hashlib import sha256
from io import BytesIO
import json
import math
import os
from pathlib import Path, PurePosixPath
import shutil
import tempfile
from typing import Callable
import zipfile

from PIL import Image

IMAGE_EXTENSIONS = {".jpg", ".jpeg", ".png", ".bmp", ".webp"}
DECISIONS = {"pending", "keep", "exclude", "hold", "fix"}
MAX_ZIP_BYTES = 200_000_000
MIN_IMAGES = 101  # Docs say >100; the error email says >=100. Use the stricter bound.
EPSILON = 1e-6  # A six-decimal YOLO writer can round an edge outward by 0.75e-6.


@dataclass
class Box:
    class_id: int
    cx: float
    cy: float
    width: float
    height: float

    def errors(self, class_count: int) -> list[str]:
        errors = []
        if type(self.class_id) is not int or not 0 <= self.class_id < class_count:
            errors.append(f"クラスID {self.class_id} は 0〜{class_count - 1} の範囲外")
        values = (self.cx, self.cy, self.width, self.height)
        if not all(isinstance(v, (float, int)) and math.isfinite(v) for v in values):
            return errors + ["座標に数値でない値・NaN・無限大があります"]
        if self.width <= 0 or self.height <= 0:
            errors.append("枠の幅と高さは正の数が必要です")
        if (self.cx - self.width / 2 < -EPSILON
                or self.cy - self.height / 2 < -EPSILON
                or self.cx + self.width / 2 > 1 + EPSILON
                or self.cy + self.height / 2 > 1 + EPSILON):
            errors.append("枠が画像の範囲外にはみ出しています")
        return errors

    def line(self) -> str:
        return f"{self.class_id} {self.cx:.9f} {self.cy:.9f} {self.width:.9f} {self.height:.9f}"


def parse_yolo(text: str, class_count: int) -> tuple[list[Box], list[str]]:
    boxes, errors = [], []
    for number, line in enumerate(text.lstrip("\ufeff").splitlines(), 1):
        if not line.strip():
            continue
        parts = line.split()
        if len(parts) != 5:
            errors.append(f"{number}行目: 5項目が必要です（現在{len(parts)}項目）")
            continue
        try:
            box = Box(int(parts[0]), *(float(p) for p in parts[1:]))
        except (ValueError, OverflowError):
            errors.append(f"{number}行目: クラスIDまたは座標を読み取れません")
            continue
        problems = box.errors(class_count)
        if problems:
            errors.extend(f"{number}行目: {p}" for p in problems)
        else:
            # Normalize only roundoff accepted above. Genuine out-of-bounds
            # labels remain errors; original files are never rewritten.
            left, top = max(0.0, box.cx - box.width / 2), max(0.0, box.cy - box.height / 2)
            right, bottom = min(1.0, box.cx + box.width / 2), min(1.0, box.cy + box.height / 2)
            if (box.cx - box.width / 2 < 0 or box.cy - box.height / 2 < 0
                    or box.cx + box.width / 2 > 1 or box.cy + box.height / 2 > 1):
                box = Box(box.class_id, (left + right) / 2, (top + bottom) / 2, right - left, bottom - top)
                if problems := box.errors(class_count):
                    errors.extend(f"{number}行目: {p}" for p in problems)
                    continue
            boxes.append(box)
    return boxes, errors


def class_errors(classes: list[str]) -> list[str]:
    if not classes:
        return ["クラスを1つ以上設定してください"]
    errors = []
    if any(not isinstance(c, str) or not c.strip() or c.strip() != c
           or "\n" in c or "\r" in c for c in classes):
        errors.append("クラス名に空欄・改行・前後の空白があります")
    if len(set(classes)) != len(classes):
        errors.append("クラス名が重複しています")
    return errors


def safe_source(root: Path, relative: str) -> Path:
    posix = PurePosixPath(relative)
    if posix.is_absolute() or ".." in posix.parts or "\\" in relative or ":" in relative:
        raise ValueError(f"不正な相対パス: {relative}")
    result = root.joinpath(*posix.parts).resolve()
    if not result.is_relative_to(root.resolve()):
        raise ValueError(f"データフォルダー外のパス: {relative}")
    return result


@dataclass
class Sample:
    path: str
    label_path: str | None = None
    width: int = 0
    height: int = 0
    source_hash: str = ""
    boxes: list[Box] = field(default_factory=list)
    has_label: bool = False
    label_errors: list[str] = field(default_factory=list)
    image_error: str = ""
    decision: str = "pending"
    reviewed: bool = False
    reason: str = ""
    quality: dict = field(default_factory=dict)
    proposals: list[Box] = field(default_factory=list)
    proposal_pending: bool = False
    proposal_info: dict = field(default_factory=dict)


@dataclass
class Project:
    source_root: str
    classes: list[str]
    samples: list[Sample] = field(default_factory=list)
    labels_root: str | None = None
    import_warnings: list[str] = field(default_factory=list)
    path: Path | None = field(default=None, repr=False)
    schema_version: int = 1
    ui_state: dict = field(default_factory=dict)
    audit_log: list[dict] = field(default_factory=list)

    def image_path(self, sample: Sample) -> Path:
        return safe_source(Path(self.source_root), sample.path)

    def save(self, path: Path | None = None) -> None:
        destination = Path(path or self.path or "")
        if not destination.name or destination.suffix.lower() != ".ylproject":
            raise ValueError("保存先には .ylproject ファイルを指定してください")
        data = asdict(self)
        data.pop("path", None)
        destination.parent.mkdir(parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(prefix=".ylproject-", dir=destination.parent)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as stream:
                json.dump(data, stream, ensure_ascii=False, indent=2, allow_nan=False)
                stream.flush()
                os.fsync(stream.fileno())
            if destination.exists():
                shutil.copy2(destination, str(destination) + ".bak")
            os.replace(temporary, destination)
            self.path = destination.resolve()
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    @classmethod
    def load(cls, path: Path) -> Project:
        data = json.loads(Path(path).read_text(encoding="utf-8-sig"))
        if data.get("schema_version") != 1:
            raise ValueError("対応していないプロジェクト形式です")
        samples = []
        for raw in data.pop("samples"):
            sample = Sample(**{**raw, "boxes": [Box(**b) for b in raw.get("boxes", [])],
                               "proposals": [Box(**b) for b in raw.get("proposals", [])]})
            if sample.decision not in DECISIONS:
                raise ValueError("不正な選別状態です")
            safe_source(Path(data["source_root"]), sample.path)
            samples.append(sample)
        project = cls(**data, samples=samples)
        if problems := class_errors(project.classes):
            raise ValueError("\n".join(problems))
        for sample in samples:
            if any(b.errors(len(project.classes)) for b in sample.boxes + sample.proposals):
                raise ValueError(f"プロジェクト内の枠が不正です: {sample.path}")
        project.path = Path(path).resolve()
        return project


def discover_classes(root: Path, labels_root: Path | None = None) -> list[str]:
    candidates = [root / "classes.txt", root / "labels" / "classes.txt"]
    if labels_root:
        candidates.insert(0, labels_root / "classes.txt")
    for candidate in candidates:
        if candidate.is_file():
            return candidate.read_text(encoding="utf-8-sig").splitlines()
    return []


def label_for(root: Path, relative: Path, labels_root: Path | None) -> Path | None:
    candidates = []
    if labels_root:
        parts = relative.parts[1:] if relative.parts and relative.parts[0] == "images" else relative.parts
        candidates.append(labels_root.joinpath(*parts).with_suffix(".txt"))
    else:
        candidates.append(root / relative.with_suffix(".txt"))
        if "images" in relative.parts:
            parts = list(relative.parts)
            parts[parts.index("images")] = "labels"
            candidates.append(root.joinpath(*parts).with_suffix(".txt"))
    for path in candidates:
        if path.is_file() and not path.is_symlink():
            return path
    return None


def scan_dataset(root: Path, classes: list[str], labels_root: Path | None = None,
                 progress: Callable[[int, int], None] | None = None,
                 cancelled: Callable[[], bool] | None = None) -> Project:
    root = root.resolve()
    if not root.is_dir():
        raise ValueError("画像フォルダーが存在しません")
    if errors := class_errors(classes):
        raise ValueError("\n".join(errors))
    labels_root = labels_root.resolve() if labels_root else None
    skip = {".git", ".venv", "__pycache__", "exports", ".qa", "node_modules"}
    files = sorted((p for p in root.rglob("*") if p.suffix.lower() in IMAGE_EXTENSIONS
                    and p.is_file() and not p.is_symlink()
                    and not any(part in skip or part.startswith(".")
                                for part in p.relative_to(root).parts)), key=lambda p: str(p).casefold())
    project = Project(str(root), list(classes), labels_root=str(labels_root) if labels_root else None)
    used_labels = set()
    for count, path in enumerate(files, 1):
        if cancelled and cancelled():
            raise InterruptedError("読み込みを中止しました")
        relative = path.relative_to(root)
        sample = Sample(relative.as_posix())
        try:
            safe_source(root, sample.path)
            payload = path.read_bytes()
            sample.source_hash = sha256(payload).hexdigest()
            with Image.open(BytesIO(payload)) as im:
                im.load()
                sample.width, sample.height = im.size
        except (OSError, ValueError, Image.DecompressionBombError) as exc:
            sample.image_error = str(exc)
        label = label_for(root, relative, labels_root)
        if label:
            sample.has_label = True
            sample.label_path = str(label)
            used_labels.add(label.resolve())
            try:
                sample.boxes, sample.label_errors = parse_yolo(
                    label.read_text(encoding="utf-8-sig"), len(classes))
            except (OSError, UnicodeError) as exc:
                sample.label_errors = [str(exc)]
        project.samples.append(sample)
        if progress:
            progress(count, len(files))
    label_scan_root = labels_root or root
    for path in label_scan_root.rglob("*.txt"):
        if (path.name != "classes.txt" and path.resolve() not in used_labels
                and not any(p in skip or p.startswith(".")
                            for p in path.relative_to(label_scan_root).parts)):
            project.import_warnings.append(f"対応画像が見つからないTXT（確認用）: {path.relative_to(label_scan_root)}")
    return project


@dataclass
class Issue:
    severity: str
    message: str
    sample_index: int | None = None


def validate_project(project: Project, allow_negatives: bool = False) -> list[Issue]:
    issues = [Issue("error", p) for p in class_errors(project.classes)]
    accepted = [(i, s) for i, s in enumerate(project.samples) if s.decision == "keep"]
    if len(accepted) < MIN_IMAGES:
        issues.append(Issue("error", f"採用画像は{len(accepted)}枚です。出力条件は{MIN_IMAGES}枚以上です。"))
    remaining = sum(s.decision in {"pending", "hold", "fix"} for s in project.samples)
    if remaining:
        issues.append(Issue("warning", f"未確認・保留・要修正の{remaining}枚はZIPに含めません。"))
    counts = Counter()
    hashes = {}
    for index, sample in accepted:
        def error(message: str) -> None:
            issues.append(Issue("error", message, index))
        if sample.image_error:
            error(f"画像を読み込めません: {sample.image_error}")
        if not sample.reviewed:
            error("ラベルが未承認です。編集画面で保存、または一覧で確認・確定してください。")
        if not sample.has_label:
            error("ラベルがありません。背景画像も明示的な確認が必要です。")
        for problem in sample.label_errors:
            error(f"元ラベルのエラー: {problem}")
        if sample.proposals or sample.proposal_pending:
            error("未承認の自動候補があります。編集画面で承認または破棄してください。")
        if not sample.boxes and not allow_negatives:
            error("枠が0個です。背景画像の受理条件は未確認のため、標準では出力しません。")
        if not sample.source_hash:
            error("元画像のハッシュがありません。再読み込みしてください。")
        else:
            if sample.source_hash in hashes:
                issues.append(Issue("warning", "採用画像に完全重複があります。", index))
            hashes[sample.source_hash] = index
        for box in sample.boxes:
            for problem in box.errors(len(project.classes)):
                error(problem)
            counts[box.class_id] += 1
    for class_id, name in enumerate(project.classes):
        if not counts[class_id]:
            issues.append(Issue("error", f"クラス {class_id}: {name} の採用枠が0個です。クラス設定を確認してください。"))
    return issues


def inspect_zip(path: Path, allow_negatives: bool = False) -> dict:
    if path.stat().st_size > MAX_ZIP_BYTES:
        raise ValueError("ZIPが200MBを超えています")
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)) or any("/" in n or "\\" in n for n in names):
            raise ValueError("ZIP内の重複名・サブフォルダーを検出しました")
        if archive.testzip() is not None:
            raise ValueError("ZIPのCRC検査に失敗しました")
        classes = archive.read("classes.txt").decode("utf-8-sig").splitlines()
        if problems := class_errors(classes):
            raise ValueError("\n".join(problems))
        config = json.loads(archive.read("v-training.config"))
        if type(config.get("classes")) is not int or config["classes"] != len(classes):
            raise ValueError("設定のクラス数が一致しません")
        images = sorted(n for n in names if n.endswith(".jpg"))
        if len(images) < MIN_IMAGES:
            raise ValueError(f"画像が{MIN_IMAGES}枚未満です")
        allowed = {"classes.txt", "v-training.config"}
        total_boxes = 0
        for name in images:
            label = str(PurePosixPath(name).with_suffix(".txt"))
            allowed.update((name, label))
            with Image.open(BytesIO(archive.read(name))) as im:
                im.load()
                if im.format != "JPEG" or im.mode != "RGB":
                    raise ValueError(f"RGB JPEGではありません: {name}")
            boxes, errors = parse_yolo(archive.read(label).decode("utf-8-sig"), len(classes))
            if errors or (not boxes and not allow_negatives):
                raise ValueError(f"ラベルが不正です: {label}: {'; '.join(errors)}")
            total_boxes += len(boxes)
        if set(names) != allowed:
            raise ValueError("ZIPに想定外のファイル・孤立ラベルがあります")
        return {"images": len(images), "boxes": total_boxes, "classes": classes,
                "bytes": path.stat().st_size}


def export_vtraining(project: Project, destination: Path, allow_negatives: bool = False,
                     progress: Callable[[int, int], None] | None = None,
                     cancelled: Callable[[], bool] | None = None) -> dict:
    errors = [i for i in validate_project(project, allow_negatives) if i.severity == "error"]
    if errors:
        raise ValueError("\n".join(i.message for i in errors[:12]))
    destination = destination.resolve()
    if destination.suffix.lower() != ".zip":
        raise ValueError("出力先には .zip を指定してください")
    report_path = destination.with_suffix(".report.json")
    if destination.exists() or report_path.exists():
        raise FileExistsError("同名のZIPまたはレポートがあります。別の名前で出力してください。")
    destination.parent.mkdir(parents=True, exist_ok=True)
    selected = [s for s in project.samples if s.decision == "keep"]
    descriptor, temporary = tempfile.mkstemp(prefix=".vtraining-", suffix=".zip", dir=destination.parent)
    os.close(descriptor)
    mapping = []
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr("classes.txt", "\n".join(project.classes) + "\n")
            archive.writestr("v-training.config", json.dumps({"classes": len(project.classes)}) + "\n")
            for number, sample in enumerate(selected, 1):
                if cancelled and cancelled():
                    raise InterruptedError("ZIP出力を中止しました")
                payload = project.image_path(sample).read_bytes()
                if sha256(payload).hexdigest() != sample.source_hash:
                    raise ValueError(f"読み込み後に元画像が変更されています: {sample.path}。再読み込みが必要です。")
                with Image.open(BytesIO(payload)) as im:
                    im.load()
                    if im.size != (sample.width, sample.height):
                        raise ValueError(f"画像寸法が変更されています: {sample.path}")
                    jpeg = BytesIO()
                    # No EXIF transpose: annotations use the raw pixel coordinate system.
                    im.convert("RGB").save(jpeg, format="JPEG", quality=95, subsampling=0)
                name = f"{number:06d}"
                archive.writestr(name + ".jpg", jpeg.getvalue())
                archive.writestr(name + ".txt", "".join(b.line() + "\n" for b in sample.boxes))
                mapping.append({"output": name + ".jpg", "source": sample.path,
                                "source_sha256": sample.source_hash, "boxes": len(sample.boxes)})
                if progress:
                    progress(number, len(selected))
        summary = inspect_zip(Path(temporary), allow_negatives)
        if cancelled and cancelled():
            raise InterruptedError("ZIP出力を中止しました")
        report = {"created_utc": datetime.now(timezone.utc).isoformat(), "format": "V-Training Detection",
                  "spec_checked": "2026-11-16", "source_root": project.source_root,
                  "documentation": "https://docs.m5stack.com/ja/guide/unitv/v_training",
                  "negative_images_opt_in": allow_negatives, "summary": summary,
                  "warnings": [asdict(i) for i in validate_project(project, allow_negatives)
                               if i.severity == "warning"], "mapping": mapping}
        # Exclusive creates prevent overwriting an existing user file, even in a race.
        with destination.open("xb") as output, open(temporary, "rb") as source:
            try:
                shutil.copyfileobj(source, output)
            except BaseException:
                output.close()
                destination.unlink(missing_ok=True)
                raise
        report_created = False
        try:
            with report_path.open("x", encoding="utf-8") as output:
                report_created = True
                json.dump(report, output, ensure_ascii=False, indent=2)
        except BaseException:
            destination.unlink(missing_ok=True)
            if report_created:
                report_path.unlink(missing_ok=True)
            raise
        return report
    finally:
        Path(temporary).unlink(missing_ok=True)
