"""Tk image previews and an editable bounding-box canvas."""

from copy import deepcopy
from pathlib import Path
import tkinter as tk

from PIL import Image, ImageDraw, ImageFont, ImageTk

from .core import Box, Sample

COLORS = ["#64e1ba", "#ffcb77", "#8ab4ff", "#ee99dd", "#a9de82", "#ff998c"]


def preview_image(path: Path, sample: Sample, classes: list[str], size=(250, 170), crop=False) -> Image.Image:
    with Image.open(path) as source:
        image = source.convert("RGB")
    width, height = image.size
    overlay = ImageDraw.Draw(image)
    for proposed, boxes in ((False, sample.boxes), (True, sample.proposals)):
        for box in boxes:
            color = "#ffcb77" if proposed else COLORS[box.class_id % len(COLORS)]
            rect = ((box.cx - box.width / 2) * width, (box.cy - box.height / 2) * height,
                    (box.cx + box.width / 2) * width, (box.cy + box.height / 2) * height)
            overlay.rectangle(rect, outline=color, width=max(2, round(width / 250)))
    boxes = sample.boxes or sample.proposals
    if crop and boxes:
        left = max(0, min(b.cx - b.width / 2 for b in boxes) - 0.04)
        top = max(0, min(b.cy - b.height / 2 for b in boxes) - 0.04)
        right = min(1, max(b.cx + b.width / 2 for b in boxes) + 0.04)
        bottom = min(1, max(b.cy + b.height / 2 for b in boxes) + 0.04)
        if right > left and bottom > top:
            image = image.crop((int(left * width), int(top * height), int(right * width), int(bottom * height)))
    image.thumbnail(size, Image.Resampling.LANCZOS)
    canvas = Image.new("RGB", size, "#0b111b")
    canvas.paste(image, ((size[0] - image.width) // 2, (size[1] - image.height) // 2))
    return canvas


class BoxCanvas(tk.Canvas):
    def __init__(self, master, path: Path, boxes: list[Box], classes: list[str], proposals: list[Box],
                 on_change=None, on_select=None, **kwargs):
        super().__init__(master, background="#0b111b", highlightthickness=0, **kwargs)
        with Image.open(path) as image:
            self.image = image.convert("RGB")
        self.boxes, self.proposals = deepcopy(boxes), deepcopy(proposals)
        self.classes, self.class_id = classes, 0
        self.selected, self.mode = -1, "select"
        self.zoom, self.pan = 1.0, (0, 0)
        self.drag, self.start, self.origin_box = None, None, None
        self.history = []
        self.on_change, self.on_select = on_change, on_select
        self.bind("<Configure>", lambda event: self.redraw())
        self.bind("<ButtonPress-1>", self.press)
        self.bind("<B1-Motion>", self.motion)
        self.bind("<ButtonRelease-1>", self.release)
        self.bind("<ButtonPress-3>", self.pan_start)
        self.bind("<B3-Motion>", self.pan_move)
        self.bind("<ButtonRelease-3>", lambda event: setattr(self, "drag", None))
        self.bind("<MouseWheel>", self.wheel)
        self.bind("<Delete>", lambda event: self.delete_selected())
        self.bind("<Control-z>", lambda event: self.undo())

    def target(self):
        width, height = max(1, self.winfo_width()), max(1, self.winfo_height())
        scale = min(width / self.image.width, height / self.image.height) * self.zoom
        x = (width - self.image.width * scale) / 2 + self.pan[0]
        y = (height - self.image.height * scale) / 2 + self.pan[1]
        return x, y, scale

    def normalized(self, x, y):
        left, top, scale = self.target()
        return (max(0, min(1, (x - left) / (self.image.width * scale))),
                max(0, min(1, (y - top) / (self.image.height * scale))))

    def checkpoint(self):
        self.history.append((deepcopy(self.boxes), deepcopy(self.proposals)))
        self.history = self.history[-40:]

    def notify(self):
        self.redraw()
        if self.on_change:
            self.on_change()

    def select(self, index):
        self.selected = index
        if self.on_select:
            self.on_select(index)
        self.redraw()

    def undo(self):
        if self.history:
            self.boxes, self.proposals = self.history.pop()
            self.select(-1)
            self.notify()

    def fit(self):
        self.zoom, self.pan = 1.0, (0, 0)
        self.redraw()

    def redraw(self):
        self.delete("all")
        left, top, scale = self.target()
        # Crop the viewport before resizing, so zooming never allocates an 8x full image.
        x1 = max(0, int(-left / scale))
        y1 = max(0, int(-top / scale))
        x2 = min(self.image.width, int((self.winfo_width() - left) / scale) + 1)
        y2 = min(self.image.height, int((self.winfo_height() - top) / scale) + 1)
        if x2 > x1 and y2 > y1:
            portion = self.image.crop((x1, y1, x2, y2))
            portion = portion.resize((max(1, round(portion.width * scale)),
                                      max(1, round(portion.height * scale))), Image.Resampling.BILINEAR)
            self.photo = ImageTk.PhotoImage(portion, master=self)
            self.create_image(left + x1 * scale, top + y1 * scale, image=self.photo, anchor="nw")
        for proposed, boxes in ((False, self.boxes), (True, self.proposals)):
            for index, box in enumerate(boxes):
                color = "#ffcb77" if proposed else COLORS[box.class_id % len(COLORS)]
                x = left + (box.cx - box.width / 2) * self.image.width * scale
                y = top + (box.cy - box.height / 2) * self.image.height * scale
                right = x + box.width * self.image.width * scale
                bottom = y + box.height * self.image.height * scale
                options = {"outline": color, "width": 2}
                if proposed:
                    options["dash"] = (6, 4)
                self.create_rectangle(x, y, right, bottom, **options)
                name = self.classes[box.class_id] if 0 <= box.class_id < len(self.classes) else f"ID {box.class_id}"
                self.create_text(x + 4, max(12, y - 13), anchor="w", fill=color,
                                 text=("候補: " if proposed else "") + name, font=("Yu Gothic UI", 11, "bold"))
                if index == self.selected and not proposed:
                    for cx, cy in ((x, y), (right, y), (x, bottom), (right, bottom)):
                        self.create_rectangle(cx - 5, cy - 5, cx + 5, cy + 5, fill=color, outline="#ffffff")

    def press(self, event):
        self.focus_set()
        left, top, scale = self.target()
        if not (left <= event.x <= left + self.image.width * scale
                and top <= event.y <= top + self.image.height * scale):
            return
        self.start = self.normalized(event.x, event.y)
        if self.mode == "draw":
            self.drag = "draw"
            return
        if 0 <= self.selected < len(self.boxes):
            box = self.boxes[self.selected]
            for name, x, y in (("tl", box.cx - box.width / 2, box.cy - box.height / 2),
                               ("tr", box.cx + box.width / 2, box.cy - box.height / 2),
                               ("bl", box.cx - box.width / 2, box.cy + box.height / 2),
                               ("br", box.cx + box.width / 2, box.cy + box.height / 2)):
                if abs(event.x - (left + x * self.image.width * scale)) < 10 and abs(event.y - (top + y * self.image.height * scale)) < 10:
                    self.checkpoint()
                    self.drag, self.origin_box = name, deepcopy(box)
                    return
        for index in range(len(self.boxes) - 1, -1, -1):
            box = self.boxes[index]
            if abs(self.start[0] - box.cx) <= box.width / 2 and abs(self.start[1] - box.cy) <= box.height / 2:
                self.select(index)
                self.checkpoint()
                self.drag, self.origin_box = "move", deepcopy(box)
                return
        self.select(-1)

    def motion(self, event):
        if not self.drag:
            return
        point = self.normalized(event.x, event.y)
        if self.drag == "draw":
            self.redraw()
            left, top, scale = self.target()
            self.create_rectangle(left + self.start[0] * self.image.width * scale,
                                  top + self.start[1] * self.image.height * scale,
                                  left + point[0] * self.image.width * scale,
                                  top + point[1] * self.image.height * scale,
                                  outline="#64e1ba", width=2, dash=(5, 3))
            return
        box = self.origin_box
        if self.drag == "move":
            self.boxes[self.selected] = Box(box.class_id,
                max(box.width / 2, min(1 - box.width / 2, box.cx + point[0] - self.start[0])),
                max(box.height / 2, min(1 - box.height / 2, box.cy + point[1] - self.start[1])), box.width, box.height)
        else:
            left, right = box.cx - box.width / 2, box.cx + box.width / 2
            top, bottom = box.cy - box.height / 2, box.cy + box.height / 2
            if "l" in self.drag:
                left = min(point[0], right - 0.001)
            else:
                right = max(point[0], left + 0.001)
            if "t" in self.drag:
                top = min(point[1], bottom - 0.001)
            else:
                bottom = max(point[1], top + 0.001)
            self.boxes[self.selected] = Box(box.class_id, (left + right) / 2, (top + bottom) / 2, right - left, bottom - top)
        self.redraw()

    def release(self, event):
        if self.drag == "draw":
            point = self.normalized(event.x, event.y)
            width, height = abs(point[0] - self.start[0]), abs(point[1] - self.start[1])
            if width * self.image.width >= 3 and height * self.image.height >= 3:
                self.checkpoint()
                self.boxes.append(Box(self.class_id, (point[0] + self.start[0]) / 2,
                                      (point[1] + self.start[1]) / 2, width, height))
                self.select(len(self.boxes) - 1)
                self.notify()
        elif self.drag:
            self.notify()
        self.drag = None
        self.redraw()

    def pan_start(self, event):
        self.pan_origin, self.pan_mouse = self.pan, (event.x, event.y)

    def pan_move(self, event):
        self.pan = (self.pan_origin[0] + event.x - self.pan_mouse[0],
                    self.pan_origin[1] + event.y - self.pan_mouse[1])
        self.redraw()

    def wheel(self, event):
        point = self.normalized(event.x, event.y)
        self.zoom = max(1, min(8, self.zoom * (1.2 if event.delta > 0 else 1 / 1.2)))
        left, top, scale = self.target()
        self.pan = (self.pan[0] + event.x - (left + point[0] * self.image.width * scale),
                    self.pan[1] + event.y - (top + point[1] * self.image.height * scale))
        self.redraw()

    def delete_selected(self):
        if 0 <= self.selected < len(self.boxes):
            self.checkpoint()
            self.boxes.pop(self.selected)
            self.select(-1)
            self.notify()
