"""Tk interaction regression checks using only generated test images."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

from PIL import Image, ImageDraw

from yolo_labeler.app import Application, AutoDialog, Editor
from yolo_labeler.core import Box, Project, scan_dataset


class GuiTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parents[1] / ".qa"
        scratch.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="yolo-gui-", dir=scratch)
        self.root = Path(self.temp.name)
        for index in range(23):
            image = Image.new("RGB", (160, 120), "white")
            ImageDraw.Draw(image).rectangle((50, 20, 100, 90), fill="black")
            image.save(self.root / f"{index:03}.png")
            (self.root / f"{index:03}.txt").write_text("0 0.5 0.5 0.3 0.6\n")
        self.project = scan_dataset(self.root, ["H"])
        self.project.save(self.root / "test.ylproject")
        self.app = Application()
        self.app.withdraw()
        self.app.attach_project(self.project)
        self.app.update()

    def tearDown(self):
        self.app.destroy()
        self.temp.cleanup()

    def test_ten_cards_commit_undo_and_save(self):
        self.assertEqual(len(self.app.visible), 10)
        self.app.toggle(1)
        self.app.commit_page()
        self.assertEqual(self.project.samples[1].decision, "exclude")
        self.assertEqual(self.project.samples[0].decision, "keep")
        self.assertEqual(self.app.visible[0], 10)
        self.assertEqual(Project.load(self.project.path).samples[1].decision, "exclude")
        self.app.undo()
        self.assertTrue(all(s.decision == "pending" for s in self.project.samples))

    def test_pending_filter_does_not_skip_next_page(self):
        self.app.status_filter.set("未確認")
        self.app.filter_changed()
        self.app.commit_page()
        self.assertEqual(self.app.visible[0], 10)
        self.app.commit_page()
        self.assertEqual(self.app.visible, [20, 21, 22])

    def test_unapproved_proposals_cannot_be_kept_by_grid(self):
        self.project.samples[0].proposal_pending = True
        self.app.commit_page()
        self.assertEqual(self.project.samples[0].decision, "fix")
        self.assertFalse(self.project.samples[0].reviewed)

    def test_editor_replaces_proposals_and_can_undo(self):
        sample = self.project.samples[0]
        sample.proposal_pending = True
        sample.proposals = [Box(0, 0.5, 0.5, 0.1, 0.2)]
        editor = Editor(self.app, self.project, 0)
        editor.update()
        editor.accept_proposals()
        self.assertEqual(len(editor.canvas.boxes), 1)
        self.assertEqual(editor.canvas.boxes[0].width, 0.1)
        editor.canvas.undo()
        self.assertEqual(len(editor.canvas.proposals), 1)
        editor.discard_proposals()
        editor.finish("keep")
        self.assertIsNotNone(editor.result)

    def test_canvas_draw_move_resize_delete_and_undo(self):
        self.app.deiconify()
        self.app.update()
        editor = Editor(self.app, self.project, 0)
        editor.update()
        canvas = editor.canvas
        canvas.mode = "draw"
        left, top, scale = canvas.target()
        event = lambda x, y: SimpleNamespace(x=left + x * 160 * scale, y=top + y * 120 * scale)
        canvas.press(event(0.1, 0.1))
        canvas.release(event(0.3, 0.3))
        self.assertEqual(len(canvas.boxes), 2)
        canvas.mode = "select"
        canvas.press(event(0.2, 0.2))
        canvas.motion(event(0.3, 0.3))
        canvas.release(event(0.3, 0.3))
        self.assertAlmostEqual(canvas.boxes[1].cx, 0.3)
        canvas.press(event(0.2, 0.2))
        canvas.motion(event(0.15, 0.1))
        canvas.release(event(0.15, 0.1))
        self.assertAlmostEqual(canvas.boxes[1].width, 0.25)
        self.assertFalse(canvas.boxes[1].errors(1))
        canvas.delete_selected()
        self.assertEqual(len(canvas.boxes), 1)
        canvas.undo()
        self.assertEqual(len(canvas.boxes), 2)
        editor.destroy()

    def test_auto_dialog_has_valid_unitv_preview(self):
        dialog = AutoDialog(self.app, self.project, 0)
        dialog.update()
        self.assertIsNotNone(dialog.preview_photo)
        self.assertIn("otsu", dialog.info.cget("text"))
        dialog.destroy()


if __name__ == "__main__":
    unittest.main()
