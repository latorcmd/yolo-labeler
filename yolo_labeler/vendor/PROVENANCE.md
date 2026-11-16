# UnitV-color detector

`unitv_labeler.py` is an unchanged copy of the user's existing
`UnitV-color/tools/yolo_labeler/label.py`, read on 2026-11-16.

Source SHA-256:
`3a7f1e6df9b22f5287110790a391ac9b779429850cef4682b9820ce9c2fb1c8f`

The GUI uses `Config`, `load_ranges`, and `detect` directly. Detection is the same
Otsu/manual threshold, HSV fallback, and connected-component merging algorithm.
The adapter supplies raw RGB pixels via Pillow to keep EXIF coordinates consistent
with the editor and exporter. GUI proposals are stored in the project, not in the
source dataset. Classes come from the GUI or the existing range CSV; they are not
guessed from symbols. Changes in the external source are not loaded automatically.
