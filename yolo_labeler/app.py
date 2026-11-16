"""Local desktop application. Run with python -m yolo_labeler."""

import argparse
from collections import Counter
from copy import deepcopy
from dataclasses import asdict
from datetime import datetime, timezone
import json
from pathlib import Path
import queue
import threading
import tkinter as tk
from tkinter import filedialog, messagebox, simpledialog, ttk

from PIL import ImageTk

from . import __version__
from .autolabel import Config, assign_classes, propose_batch, propose_one
from .core import (Box, Project, class_errors, discover_classes, export_vtraining,
                   scan_dataset, validate_project)
from .vision import analyze_quality, import_proposals, safe_exclusion_candidates
from .widgets import BoxCanvas, preview_image

BG = "#101722"
PANEL = "#192332"
MUTED = "#9aaec5"
TEXT = "#e7edf5"
ACCENT = "#64e1ba"
STATUS = {"pending": "未確認", "keep": "採用", "exclude": "除外", "hold": "保留", "fix": "要修正"}


def button(master, text, command, accent=False, **kwargs):
    return ttk.Button(master, text=text, command=command, style="Accent.TButton" if accent else "TButton", **kwargs)


class Modal(tk.Toplevel):
    def __init__(self, master, title, size="700x500"):
        super().__init__(master)
        self.title(title)
        self.configure(bg=BG)
        self.geometry(size)
        self.transient(master)
        self.grab_set()
        self.protocol("WM_DELETE_WINDOW", self.destroy)


class Editor(Modal):
    def __init__(self, master, project: Project, index: int):
        super().__init__(master, "ラベル編集 — " + project.samples[index].path, "1120x780")
        self.result = None
        self.project, self.sample = project, project.samples[index]
        self.dirty = False
        self.proposal_resolved = not self.sample.proposal_pending and not self.sample.proposals
        self.protocol("WM_DELETE_WINDOW", self.cancel)
        top = ttk.Frame(self, padding=16)
        top.pack(fill="x")
        ttk.Label(top, text=Path(self.sample.path).name, style="Title.TLabel").pack(side="left")
        ttk.Label(top, text=f"{self.sample.width} × {self.sample.height} px", style="Muted.TLabel").pack(side="right")
        toolbar = ttk.Frame(self, padding=(16, 0, 16, 10))
        toolbar.pack(fill="x")
        self.mode = tk.StringVar(value="select")
        ttk.Radiobutton(toolbar, text="選択・移動・サイズ変更", variable=self.mode, value="select",
                        command=self.set_mode).pack(side="left")
        ttk.Radiobutton(toolbar, text="枠を描く (W)", variable=self.mode, value="draw",
                        command=self.set_mode).pack(side="left", padx=12)
        self.class_var = tk.StringVar(value=project.classes[0])
        self.classes = ttk.Combobox(toolbar, values=project.classes, textvariable=self.class_var, state="readonly", width=17)
        self.classes.pack(side="left", padx=10)
        self.classes.bind("<<ComboboxSelected>>", self.change_class)
        body = ttk.Frame(self, padding=(16, 0, 16, 0))
        body.pack(fill="both", expand=True)
        self.canvas = BoxCanvas(body, project.image_path(self.sample), self.sample.boxes, project.classes,
                                self.sample.proposals, on_change=self.changed, on_select=self.selected)
        self.canvas.pack(side="left", fill="both", expand=True)
        side = ttk.Frame(body, width=210, padding=(12, 0, 0, 0))
        side.pack(side="right", fill="y")
        ttk.Label(side, text="境界ボックス", style="Sub.TLabel").pack(anchor="w", pady=(0, 8))
        self.box_list = tk.Listbox(side, width=25, height=10, background=PANEL, foreground=TEXT,
                                   selectbackground="#305645", borderwidth=0, font=("Yu Gothic UI", 10))
        self.box_list.pack(fill="x")
        self.box_list.bind("<<ListboxSelect>>", self.list_selected)
        button(side, "選択した枠を削除 (Del)", self.canvas.delete_selected).pack(fill="x", pady=(8, 4))
        button(side, "編集を元に戻す", self.canvas.undo).pack(fill="x", pady=4)
        button(side, "画面に合わせる", self.canvas.fit).pack(fill="x", pady=4)
        ttk.Separator(side).pack(fill="x", pady=12)
        self.proposal_label = ttk.Label(side, text="", wraplength=200, style="Muted.TLabel")
        self.proposal_label.pack(anchor="w")
        button(side, "候補で枠を置き換える", self.accept_proposals).pack(fill="x", pady=(8, 4))
        button(side, "候補をすべて破棄", self.discard_proposals).pack(fill="x", pady=4)
        ttk.Label(side, text="ホイール: 拡大縮小\n右ドラッグ: 表示移動\n四隅のハンドル: サイズ変更",
                  style="Muted.TLabel", wraplength=200).pack(anchor="w", pady=15)
        error_text = "\n".join(self.sample.label_errors)
        if error_text:
            ttk.Label(side, text="元ラベルの問題:\n" + error_text[:700], foreground="#ff998c", wraplength=200).pack(anchor="w")
        footer = ttk.Frame(self, padding=16)
        footer.pack(fill="x")
        self.negative = tk.BooleanVar(value=self.sample.has_label and not self.sample.boxes and self.proposal_resolved)
        ttk.Checkbutton(footer, text="対象がない画像として確認した", variable=self.negative).pack(side="left")
        button(footer, "キャンセル", self.cancel).pack(side="right", padx=(8, 0))
        button(footer, "保存して採用", lambda: self.finish("keep"), accent=True).pack(side="right", padx=(8, 0))
        button(footer, "保存して保留", lambda: self.finish("hold")).pack(side="right")
        self.bind("<Key-w>", lambda event: self.draw_mode())
        self.bind("<Key-v>", lambda event: self.select_mode())
        self.bind("<Control-s>", lambda event: self.finish("keep"))
        self.bind("<Escape>", lambda event: self.cancel())
        self.refresh_list()

    def draw_mode(self):
        self.mode.set("draw")
        self.set_mode()

    def select_mode(self):
        self.mode.set("select")
        self.set_mode()

    def set_mode(self):
        self.canvas.mode = self.mode.get()
        self.canvas.configure(cursor="crosshair" if self.canvas.mode == "draw" else "arrow")

    def selected(self, index):
        self.box_list.selection_clear(0, "end")
        if 0 <= index < len(self.canvas.boxes):
            self.box_list.selection_set(index)
            self.class_var.set(self.project.classes[self.canvas.boxes[index].class_id])

    def list_selected(self, event):
        selection = self.box_list.curselection()
        if selection:
            self.canvas.select(selection[0])

    def change_class(self, event=None):
        class_id = self.project.classes.index(self.class_var.get())
        self.canvas.class_id = class_id
        if self.canvas.selected >= 0:
            self.canvas.checkpoint()
            self.canvas.boxes[self.canvas.selected].class_id = class_id
            self.canvas.notify()

    def changed(self):
        self.dirty = True
        self.refresh_list()

    def refresh_list(self):
        self.box_list.delete(0, "end")
        for index, box in enumerate(self.canvas.boxes):
            self.box_list.insert("end", f"{index + 1}. {self.project.classes[box.class_id]}  {box.width:.2f} × {box.height:.2f}")
        text = f"自動候補: {len(self.canvas.proposals)}枠"
        if self.sample.proposal_pending and not self.canvas.proposals and not self.proposal_resolved:
            text += "\n検出なし。内容を確認して\n候補を破棄してください。"
        if flags := self.sample.proposal_info.get("flags"):
            text += "\n" + ", ".join(flags)
        self.proposal_label.configure(text=text)

    def accept_proposals(self):
        self.canvas.checkpoint()
        if not self.canvas.proposals:
            return
        self.canvas.boxes = deepcopy(self.canvas.proposals)
        self.canvas.selected = -1
        self.canvas.proposals.clear()
        self.proposal_resolved = True
        self.canvas.notify()

    def discard_proposals(self):
        self.canvas.checkpoint()
        self.canvas.proposals.clear()
        self.proposal_resolved = True
        self.canvas.notify()

    def finish(self, decision):
        if self.canvas.proposals or not self.proposal_resolved:
            messagebox.showwarning("候補が未確認です", "自動候補を承認または破棄してください。", parent=self)
            return
        if not self.canvas.boxes and not self.negative.get():
            messagebox.showwarning("枠がありません", "対象を囲むか「対象がない画像として確認した」を選択してください。", parent=self)
            return
        problems = [error for box in self.canvas.boxes for error in box.errors(len(self.project.classes))]
        if problems:
            messagebox.showerror("枠のエラー", "\n".join(problems), parent=self)
            return
        self.result = (deepcopy(self.canvas.boxes), decision)
        self.destroy()

    def cancel(self):
        if self.dirty and not messagebox.askyesno("編集を破棄", "保存せずに編集画面を閉じますか？", parent=self):
            return
        self.destroy()


class AutoDialog(Modal):
    def __init__(self, master, project: Project, index: int):
        super().__init__(master, "半自動ラベリング — UnitV-color", "960x730")
        self.project, self.index, self.result = project, index, None
        self.preview_photo = None
        ttk.Label(self, text="黒い記号のラベル候補", style="Title.TLabel", padding=18).pack(anchor="w")
        ttk.Label(self, text="UnitV-color の既存処理を使用。1画像1対象の候補を作り、人が確認して採用します。",
                  style="Muted.TLabel", padding=(18, 0, 18, 12)).pack(anchor="w")
        body = ttk.Frame(self, padding=(18, 0, 18, 0))
        body.pack(fill="both", expand=True)
        form = ttk.Frame(body, width=350)
        form.pack(side="left", fill="y", padx=(0, 18))
        self.fixed_class = tk.StringVar(value=project.classes[0])
        ttk.Label(form, text="クラスの指定").pack(anchor="w")
        ttk.Combobox(form, values=project.classes, textvariable=self.fixed_class, state="readonly").pack(fill="x", pady=(4, 10))
        self.csv_path = tk.StringVar()
        ttk.Label(form, text="画像番号 → クラス範囲CSV（任意）").pack(anchor="w")
        row = ttk.Frame(form)
        row.pack(fill="x", pady=(4, 10))
        ttk.Entry(row, textvariable=self.csv_path, width=30).pack(side="left", fill="x", expand=True)
        button(row, "選択", self.choose_csv, width=5).pack(side="right", padx=(4, 0))
        ttk.Label(form, text="CSV指定時はCSVを優先。開始・終了番号を含みます。\nCSVなしでは、上で選んだクラスを候補に付けます。",
                  wraplength=330, style="Muted.TLabel").pack(anchor="w", pady=(0, 14))
        self.automatic = tk.BooleanVar(value=True)
        ttk.Checkbutton(form, text="自動しきい値（Otsu＋HSV補助抽出）", variable=self.automatic).pack(anchor="w")
        self.variables = {}
        for name, label, default, step in (
            ("threshold", "手動しきい値 (0〜255)", "90", 1),
            ("min_area_ratio", "最小成分面積比", "0.001", 0.001),
            ("merge_component_ratio", "追加成分の最小面積比", "0.05", 0.01),
            ("merge_gap_ratio", "成分の最大結合距離比", "0.20", 0.01),
            ("padding", "余白 (px)", "0", 1)):
            row = ttk.Frame(form)
            row.pack(fill="x", pady=6)
            ttk.Label(row, text=label).pack(side="left")
            variable = tk.StringVar(value=default)
            self.variables[name] = variable
            ttk.Spinbox(row, textvariable=variable, from_=0, to=255 if name == "threshold" else 1000 if name == "padding" else 1,
                        increment=step, width=9).pack(side="right")
        button(form, "この画像でプレビュー", self.preview, accent=True).pack(fill="x", pady=18)
        self.preview_label = ttk.Label(body, anchor="center")
        self.preview_label.pack(fill="both", expand=True)
        self.info = ttk.Label(self, text="", wraplength=900, style="Muted.TLabel", padding=18)
        self.info.pack(fill="x")
        footer = ttk.Frame(self, padding=(18, 0, 18, 18))
        footer.pack(fill="x")
        button(footer, "キャンセル", self.destroy).pack(side="right", padx=(8, 0))
        button(footer, "未承認画像の候補を一括生成", self.finish, accent=True).pack(side="right")
        self.preview()

    def choose_csv(self):
        path = filedialog.askopenfilename(parent=self, title="クラス範囲CSV", filetypes=[("CSV", "*.csv")])
        if path:
            self.csv_path.set(path)

    def settings(self):
        config = Config(None if self.automatic.get() else int(self.variables["threshold"].get()),
                        float(self.variables["min_area_ratio"].get()), float(self.variables["merge_component_ratio"].get()),
                        float(self.variables["merge_gap_ratio"].get()), int(self.variables["padding"].get()))
        config.validate()
        return config, self.project.classes.index(self.fixed_class.get()), Path(self.csv_path.get()) if self.csv_path.get() else None

    def preview(self):
        try:
            config, class_id, ranges = self.settings()
            sample = deepcopy(self.project.samples[self.index])
            class_id = assign_classes([sample], len(self.project.classes), class_id, ranges)[sample.path]
            boxes, info = propose_one(self.project.image_path(sample), class_id, config)
            sample.boxes, sample.proposals = [], boxes
            image = preview_image(self.project.image_path(sample), sample, self.project.classes, (510, 400))
            self.preview_photo = ImageTk.PhotoImage(image, master=self)
            self.preview_label.configure(image=self.preview_photo)
            self.info.configure(text=f"{sample.path}  |  {info['method']}  |  しきい値 {info['threshold']}  |  "
                                    f"{len(boxes)}枠  |  クラス {self.project.classes[class_id]}\n" + ", ".join(info["flags"]))
        except Exception as exc:
            self.info.configure(text=str(exc))

    def finish(self):
        try:
            self.result = self.settings()
            targets = [s for s in self.project.samples if not s.reviewed and s.decision not in {"keep", "exclude"}]
            assign_classes(targets, len(self.project.classes), self.result[1], self.result[2])
        except Exception as exc:
            self.result = None
            messagebox.showerror("設定のエラー", str(exc), parent=self)
            return
        self.destroy()


class NewProjectDialog(Modal):
    def __init__(self, master):
        super().__init__(master, "新しいデータセット", "720x590")
        self.result = None
        ttk.Label(self, text="データセットを開く", style="Title.TLabel", padding=20).pack(anchor="w")
        ttk.Label(self, text="画像とラベルを読み込みます。元ファイルは変更しません。",
                  style="Muted.TLabel", padding=(20, 0, 20, 16)).pack(anchor="w")
        form = ttk.Frame(self, padding=(20, 0, 20, 0))
        form.pack(fill="both", expand=True)
        self.root_path, self.label_path = tk.StringVar(), tk.StringVar()
        for label, variable, callback in (("画像フォルダー（サブフォルダーも読み込み）", self.root_path, self.choose_root),
                                          ("ラベル専用フォルダー（別の場所にある場合だけ）", self.label_path, self.choose_labels)):
            ttk.Label(form, text=label).pack(anchor="w")
            row = ttk.Frame(form)
            row.pack(fill="x", pady=(5, 14))
            ttk.Entry(row, textvariable=variable).pack(side="left", fill="x", expand=True)
            button(row, "参照", callback, width=7).pack(side="right", padx=(8, 0))
        row = ttk.Frame(form)
        row.pack(fill="x")
        ttk.Label(row, text="クラス一覧 — 1行1クラス、先頭行がID 0").pack(side="left")
        button(row, "classes.txt読込", self.choose_classes).pack(side="right")
        self.class_text = tk.Text(form, height=8, background=PANEL, foreground=TEXT, insertbackground=TEXT,
                                  font=("Yu Gothic UI", 11), relief="flat", padx=10, pady=10)
        self.class_text.pack(fill="both", expand=True, pady=8)
        ttk.Label(form, text="YOLOの数値IDと行順を合わせてください。クラスを推測して割り当てることはありません。",
                  style="Muted.TLabel", wraplength=650).pack(anchor="w")
        footer = ttk.Frame(self, padding=20)
        footer.pack(fill="x")
        button(footer, "キャンセル", self.destroy).pack(side="right", padx=(8, 0))
        button(footer, "保存先を選んで読み込む", self.finish, accent=True).pack(side="right")

    def choose_root(self):
        path = filedialog.askdirectory(parent=self, title="画像フォルダー")
        if path:
            self.root_path.set(path)
            try:
                classes = discover_classes(Path(path))
                if classes:
                    self.set_classes(classes)
            except (OSError, UnicodeError) as exc:
                messagebox.showerror("クラス一覧", str(exc), parent=self)

    def choose_labels(self):
        path = filedialog.askdirectory(parent=self, title="ラベル専用フォルダー")
        if path:
            self.label_path.set(path)
            if (Path(path) / "classes.txt").exists():
                self.set_classes((Path(path) / "classes.txt").read_text(encoding="utf-8-sig").splitlines())

    def set_classes(self, classes):
        self.class_text.delete("1.0", "end")
        self.class_text.insert("1.0", "\n".join(classes))

    def choose_classes(self):
        path = filedialog.askopenfilename(parent=self, title="クラス一覧", filetypes=[("TXT", "*.txt")])
        if path:
            try:
                self.set_classes(Path(path).read_text(encoding="utf-8-sig").splitlines())
            except (OSError, UnicodeError) as exc:
                messagebox.showerror("読み込みエラー", str(exc), parent=self)

    def finish(self):
        classes = self.class_text.get("1.0", "end-1c").strip().splitlines()
        if errors := class_errors(classes):
            messagebox.showerror("クラス一覧", "\n".join(errors), parent=self)
            return
        root = Path(self.root_path.get())
        labels = Path(self.label_path.get()) if self.label_path.get() else None
        if not self.root_path.get() or not root.is_dir() or (labels and not labels.is_dir()):
            messagebox.showerror("フォルダー", "有効な画像・ラベルフォルダーを選択してください。", parent=self)
            return
        path = filedialog.asksaveasfilename(parent=self, title="プロジェクトの保存先", defaultextension=".ylproject",
                                          filetypes=[("YOLO Labeler project", "*.ylproject")], initialfile="dataset.ylproject")
        if path:
            self.result = (root, classes, labels, Path(path))
            self.destroy()


class Application(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title("YOLO Labeler — V-Training Dataset Studio")
        self.geometry("1480x940")
        self.minsize(1200, 800)
        self.configure(bg=BG)
        self.project = None
        self.page, self.visible, self.filtered = 0, [], []
        self.selection, self.history = set(), []
        self.busy, self.unsaved = False, False
        self.photos = []
        self.status_filter = tk.StringVar(value="すべて")
        self.class_filter = tk.StringVar(value="すべてのクラス")
        self.sort_order = tk.StringVar(value="ファイル名順")
        self.search = tk.StringVar()
        self.crop = tk.BooleanVar(value=False)
        self.style_ui()
        self.build_ui()
        self.protocol("WM_DELETE_WINDOW", self.close)
        self.bind("<Control-o>", lambda e: self.open_project())
        self.bind("<Control-n>", lambda e: self.new_project())
        self.bind("<Control-s>", lambda e: self.save())
        self.bind("<Control-z>", lambda e: self.undo())
        self.bind("<Return>", self.keyboard_commit)
        self.bind("<Left>", lambda e: self.keyboard_page(-1))
        self.bind("<Right>", lambda e: self.keyboard_page(1))
        for slot, key in enumerate("1234567890"):
            self.bind(key, lambda e, n=slot: self.keyboard_toggle(n))
        self.render()

    def style_ui(self):
        style = ttk.Style(self)
        style.theme_use("clam")
        style.configure(".", font=("Yu Gothic UI", 10), background=BG, foreground=TEXT)
        style.configure("TFrame", background=BG)
        style.configure("TLabel", background=BG, foreground=TEXT)
        style.configure("Title.TLabel", font=("Yu Gothic UI", 20, "bold"))
        style.configure("Sub.TLabel", font=("Yu Gothic UI", 12, "bold"))
        style.configure("Muted.TLabel", foreground=MUTED)
        style.configure("TButton", background="#263449", foreground=TEXT, borderwidth=0, padding=(12, 9))
        style.map("TButton", background=[("active", "#354a62"), ("disabled", "#17202c")])
        style.configure("Accent.TButton", background=ACCENT, foreground="#10251f", font=("Yu Gothic UI", 10, "bold"))
        style.map("Accent.TButton", background=[("active", "#8eeaca")])
        style.configure("TEntry", fieldbackground=PANEL, foreground=TEXT, insertcolor=TEXT, padding=7)
        style.configure("TCombobox", fieldbackground=PANEL, background="#263449", foreground=TEXT, padding=6)
        style.map("TCombobox", fieldbackground=[("readonly", PANEL)], foreground=[("readonly", TEXT)])
        style.configure("TSpinbox", fieldbackground=PANEL, foreground=TEXT, padding=5)
        style.configure("TCheckbutton", background=BG, foreground=TEXT)
        style.configure("TRadiobutton", background=BG, foreground=TEXT)
        style.configure("Treeview", background=PANEL, fieldbackground=PANEL, foreground=TEXT, rowheight=28, borderwidth=0)
        style.configure("Treeview.Heading", background="#263449", foreground=TEXT)
        style.map("Treeview", background=[("selected", "#305645")])
        style.configure("Horizontal.TProgressbar", troughcolor=PANEL, background=ACCENT)
        self.option_add("*TCombobox*Listbox.background", PANEL)
        self.option_add("*TCombobox*Listbox.foreground", TEXT)
        self.option_add("*TCombobox*Listbox.selectBackground", "#305645")

    def build_ui(self):
        header = ttk.Frame(self, padding=(24, 20, 24, 12))
        header.pack(fill="x")
        branding = ttk.Frame(header)
        branding.pack(side="left")
        ttk.Label(branding, text="YOLO LABELER", style="Title.TLabel").pack(anchor="w")
        ttk.Label(branding, text="画像の選別から V-Training の学習ZIPまで", style="Muted.TLabel").pack(anchor="w", pady=(2, 0))
        button(header, "V-Training ZIPを書き出す", self.export, accent=True).pack(side="right")
        button(header, "検査", self.show_validation).pack(side="right", padx=10)
        self.dataset_label = ttk.Label(self, text="", padding=(24, 0, 24, 12), style="Muted.TLabel")
        self.dataset_label.pack(fill="x")
        shell = ttk.Frame(self, padding=(16, 0, 16, 0))
        shell.pack(fill="both", expand=True)
        sidebar = ttk.Frame(shell, width=202, padding=(6, 8, 14, 0))
        sidebar.pack(side="left", fill="y")
        sidebar.pack_propagate(False)
        ttk.Label(sidebar, text="PROJECT", style="Muted.TLabel").pack(anchor="w", pady=(0, 8))
        for text, command in (("新規データセット", self.new_project), ("プロジェクトを開く", self.open_project),
                              ("保存  Ctrl+S", self.save), ("クラス設定", self.edit_classes)):
            button(sidebar, text, command).pack(fill="x", pady=3)
        ttk.Separator(sidebar).pack(fill="x", pady=18)
        ttk.Label(sidebar, text="ASSIST", style="Muted.TLabel").pack(anchor="w", pady=(0, 8))
        button(sidebar, "半自動ラベリング", self.auto_label).pack(fill="x", pady=3)
        button(sidebar, "外部YOLO候補を読み込む", self.external_proposals).pack(fill="x", pady=3)
        button(sidebar, "品質・重複を検査", self.quality_scan).pack(fill="x", pady=3)
        button(sidebar, "破損・重複を自動除外", self.auto_exclude).pack(fill="x", pady=3)
        ttk.Separator(sidebar).pack(fill="x", pady=18)
        self.counts_label = ttk.Label(sidebar, text="", justify="left", style="Muted.TLabel")
        self.counts_label.pack(anchor="w")
        ttk.Label(sidebar, text="クリック: 除外予定を選ぶ\nダブルクリック: 枠を編集\n1〜0: 選択切替\nEnter: このページを確定\nCtrl+Z: 判断を戻す",
                  style="Muted.TLabel", justify="left").pack(side="bottom", anchor="w", pady=20)
        main = ttk.Frame(shell)
        main.pack(side="left", fill="both", expand=True)
        filters = ttk.Frame(main, padding=(4, 8, 4, 12))
        filters.pack(fill="x")
        self.status_combo = ttk.Combobox(filters, textvariable=self.status_filter,
                                        values=["すべて", *STATUS.values(), "ラベルなし", "自動候補あり", "品質の確認候補"], state="readonly", width=13)
        self.status_combo.pack(side="left", padx=(0, 8))
        self.class_combo = ttk.Combobox(filters, textvariable=self.class_filter, state="readonly", width=17)
        self.class_combo.pack(side="left", padx=(0, 8))
        self.sort_combo = ttk.Combobox(filters, textvariable=self.sort_order,
                                      values=["ファイル名順", "品質の確認候補順", "ぼけ指標の低い順", "完全重複グループ順"], state="readonly", width=18)
        self.sort_combo.pack(side="left", padx=(0, 8))
        for combo in (self.status_combo, self.class_combo, self.sort_combo):
            combo.bind("<<ComboboxSelected>>", lambda event: self.filter_changed())
        search = ttk.Entry(filters, textvariable=self.search, width=17)
        search.pack(side="left", fill="x", expand=True, padx=(0, 8))
        search.bind("<KeyRelease>", lambda event: self.filter_changed())
        ttk.Checkbutton(filters, text="対象を拡大", variable=self.crop, command=self.render).pack(side="right")
        self.grid_frame = ttk.Frame(main)
        self.grid_frame.pack(fill="both", expand=True)
        for col in range(5):
            self.grid_frame.columnconfigure(col, weight=1, uniform="card")
        for row in range(2):
            self.grid_frame.rowconfigure(row, weight=1, uniform="card")
        controls = ttk.Frame(main, padding=(4, 12, 4, 8))
        controls.pack(fill="x")
        button(controls, "前へ", lambda: self.change_page(-1), width=6).pack(side="left")
        self.page_label = ttk.Label(controls, text="", padding=10)
        self.page_label.pack(side="left")
        button(controls, "次へ", lambda: self.change_page(1), width=6).pack(side="left")
        button(controls, "このページを確定 →", self.commit_page, accent=True).pack(side="right")
        actions = ttk.Frame(main, padding=(4, 0, 4, 8))
        actions.pack(fill="x")
        self.selection_label = ttk.Label(actions, text="", style="Muted.TLabel")
        self.selection_label.pack(side="left")
        for text, state in (("選択を除外", "exclude"), ("保留", "hold"), ("要修正", "fix"), ("採用に戻す", "keep")):
            button(actions, text, lambda state=state: self.set_decision(state)).pack(side="left", padx=3)
        button(actions, "選択解除", self.clear_selection).pack(side="right")
        self.status_text = tk.StringVar(value="新規データセットを作成するか、保存したプロジェクトを開いてください。")
        ttk.Label(self, textvariable=self.status_text, style="Muted.TLabel", padding=(24, 8, 24, 12)).pack(fill="x")

    def require_project(self):
        if not self.project:
            messagebox.showinfo("データセット", "先にデータセットを読み込んでください。", parent=self)
            return False
        return not self.busy

    def keyboard_allowed(self):
        return self.grab_current() is None and not isinstance(self.focus_get(), (tk.Entry, ttk.Entry, tk.Text, ttk.Combobox, ttk.Spinbox))

    def keyboard_toggle(self, slot):
        if self.keyboard_allowed() and slot < len(self.visible):
            self.toggle(self.visible[slot])

    def keyboard_page(self, delta):
        if self.keyboard_allowed():
            self.change_page(delta)

    def keyboard_commit(self, event):
        if self.keyboard_allowed():
            self.commit_page()

    def filter_changed(self):
        self.page = 0
        self.selection.clear()
        self.render()

    def apply_filters(self):
        result = []
        reverse_status = {v: k for k, v in STATUS.items()}
        for i, sample in enumerate(self.project.samples):
            current = self.status_filter.get()
            if current in reverse_status and sample.decision != reverse_status[current]:
                continue
            if current == "ラベルなし" and sample.has_label:
                continue
            if current == "自動候補あり" and not sample.proposal_pending:
                continue
            if current == "品質の確認候補" and not sample.quality.get("flags"):
                continue
            if self.search.get().casefold() not in sample.path.casefold():
                continue
            chosen = self.class_filter.get()
            if chosen != "すべてのクラス":
                class_id = self.project.classes.index(chosen) if chosen in self.project.classes else -1
                if not any(b.class_id == class_id for b in sample.boxes + sample.proposals):
                    continue
            result.append(i)
        order = self.sort_order.get()
        if order == "品質の確認候補順":
            result.sort(key=lambda i: -self.project.samples[i].quality.get("score", -1))
        elif order == "ぼけ指標の低い順":
            result.sort(key=lambda i: self.project.samples[i].quality.get("sharpness", float("inf")))
        elif order == "完全重複グループ順":
            result.sort(key=lambda i: self.project.samples[i].quality.get("duplicate_of", len(self.project.samples) + i))
        return result

    def render(self):
        for widget in self.grid_frame.winfo_children():
            widget.destroy()
        self.photos.clear()
        if not self.project:
            self.class_combo.configure(values=["すべてのクラス"])
            welcome = ttk.Frame(self.grid_frame, padding=36)
            welcome.grid(row=0, column=0, columnspan=5, rowspan=2, sticky="nsew")
            ttk.Label(welcome, text="学習データを、見て整える。", style="Title.TLabel").pack(pady=(80, 12))
            ttk.Label(welcome, text="10枚ずつの選別 ・ 枠の編集 ・ 半自動ラベリング ・ 検査済みZIPの出力",
                      style="Muted.TLabel").pack(pady=8)
            button(welcome, "新規データセット", self.new_project, accent=True).pack(pady=24)
            ttk.Label(welcome, text="元画像と元ラベルを保持しながら作業できます。\n保存したプロジェクトから、いつでも続きを再開できます。",
                      style="Muted.TLabel", justify="center").pack(pady=8)
            return
        self.filtered = self.apply_filters()
        pages = max(1, (len(self.filtered) + 9) // 10)
        self.page = max(0, min(self.page, pages - 1))
        self.visible = self.filtered[self.page * 10:self.page * 10 + 10]
        self.class_combo.configure(values=["すべてのクラス", *self.project.classes])
        counts = Counter(s.decision for s in self.project.samples)
        self.counts_label.configure(text="全画像   " + str(len(self.project.samples)) + " 枚\n\n" + "\n".join(f"{label}   {counts[state]}" for state, label in STATUS.items()))
        self.dataset_label.configure(text=f"{self.project.path.name if self.project.path else '未保存'}  /  {self.project.source_root}" + ("  [未保存の変更]" if self.unsaved else ""))
        self.page_label.configure(text=f"{self.page + 1} / {pages}   ·   {len(self.filtered)}枚")
        self.selection_label.configure(text=f"除外予定 {len(self.selection)}枚  ")
        for slot, index in enumerate(self.visible):
            self.make_card(slot, index)
        if not self.visible:
            ttk.Label(self.grid_frame, text="この条件に一致する画像はありません。", anchor="center",
                      style="Sub.TLabel").grid(row=0, column=0, columnspan=5, rowspan=2, sticky="nsew")

    def make_card(self, slot, index):
        sample = self.project.samples[index]
        chosen = index in self.selection
        border = "#f07c82" if chosen else "#385449" if sample.decision == "keep" else "#2b3a4e"
        frame = tk.Frame(self.grid_frame, bg=border, padx=2, pady=2)
        frame.grid(row=slot // 5, column=slot % 5, padx=5, pady=5, sticky="nsew")
        inside = tk.Frame(frame, bg=PANEL)
        inside.pack(fill="both", expand=True)
        head = tk.Frame(inside, bg=PANEL)
        head.pack(fill="x", padx=8, pady=6)
        tk.Label(head, text=f"{(slot + 1) % 10}  {Path(sample.path).name[:18]}", bg=PANEL, fg=TEXT,
                 font=("Yu Gothic UI", 10, "bold"), anchor="w").pack(side="left", fill="x", expand=True)
        tk.Label(head, text="除外予定" if chosen else STATUS[sample.decision], bg=PANEL,
                 fg="#ff998c" if chosen else ACCENT if sample.decision == "keep" else MUTED,
                 font=("Yu Gothic UI", 9)).pack(side="right")
        try:
            self.update_idletasks()
            width = max(165, min(320, (max(self.grid_frame.winfo_width(), 900) // 5) - 26))
            image = preview_image(self.project.image_path(sample), sample, self.project.classes, (width, 165), self.crop.get())
            photo = ImageTk.PhotoImage(image, master=self)
            self.photos.append(photo)
            preview = tk.Label(inside, image=photo, bg="#0b111b", cursor="hand2")
        except Exception:
            preview = tk.Label(inside, text="画像を読み込めません", height=9, bg="#0b111b", fg="#ff998c")
        preview.pack(fill="both", expand=True)
        preview.bind("<Button-1>", lambda event, i=index: self.toggle(i))
        preview.bind("<Double-Button-1>", lambda event, i=index: self.edit(i))
        names = sorted({self.project.classes[b.class_id] for b in sample.boxes if 0 <= b.class_id < len(self.project.classes)})
        label_text = " / ".join(names) if names else "ラベルなし" if not sample.has_label else "背景（枠なし）"
        if sample.proposal_pending:
            label_text += f"  · 候補 {len(sample.proposals)}"
        tk.Label(inside, text=label_text, bg=PANEL, fg=TEXT, anchor="w", font=("Yu Gothic UI", 10)).pack(fill="x", padx=8, pady=(7, 2))
        flags = sample.quality.get("flags", [])
        detail = "ラベルのエラーあり" if sample.label_errors else " / ".join(flags[:2]) or f"{sample.width} × {sample.height} px"
        tk.Label(inside, text=detail[:27], bg=PANEL, fg="#ffcb77" if sample.label_errors or flags else MUTED,
                 anchor="w", font=("Yu Gothic UI", 9)).pack(fill="x", padx=8)
        button(inside, "枠・ラベルを編集", lambda i=index: self.edit(i)).pack(fill="x", padx=8, pady=8)

    def toggle(self, index):
        if self.busy:
            return
        if index in self.selection:
            self.selection.remove(index)
        else:
            self.selection.add(index)
        self.render()

    def clear_selection(self):
        self.selection.clear()
        self.render()

    def change_page(self, delta):
        if not self.project or self.busy:
            return
        self.page += delta
        self.selection.clear()
        self.render()
        self.remember_view()

    def remember_view(self):
        if self.project:
            self.project.ui_state = {"page": self.page, "status": self.status_filter.get(),
                                     "class": self.class_filter.get(), "sort": self.sort_order.get(),
                                     "search": self.search.get(), "crop": self.crop.get()}

    def save(self):
        if not self.project or self.busy:
            return False
        self.remember_view()
        try:
            self.project.save()
        except (OSError, ValueError, TypeError) as exc:
            self.unsaved = True
            messagebox.showerror("保存できません", str(exc), parent=self)
            return False
        self.unsaved = False
        self.status_text.set("保存しました: " + str(self.project.path))
        return True

    def checkpoint(self, indices):
        snapshot = {i: deepcopy(self.project.samples[i]) for i in indices}
        self.history.append(snapshot)
        self.history = self.history[-40:]

    def changed(self):
        self.unsaved = True
        if self.history:
            changes = []
            for index, before in self.history[-1].items():
                after = self.project.samples[index]
                if before != after:
                    changes.append({"path": after.path, "before": {
                        "decision": before.decision, "reviewed": before.reviewed,
                        "boxes": [asdict(b) for b in before.boxes]}, "after": {
                        "decision": after.decision, "reviewed": after.reviewed,
                        "boxes": [asdict(b) for b in after.boxes]}, "reason": after.reason})
            if changes:
                self.project.audit_log.append({"at": datetime.now(timezone.utc).isoformat(), "changes": changes})
        self.save()
        self.render()

    def undo(self):
        if self.busy or self.grab_current() is not None or not self.project or not self.history:
            return
        snapshot = self.history.pop()
        changes = []
        for index, sample in snapshot.items():
            changes.append({"path": sample.path, "decision": sample.decision})
            self.project.samples[index] = sample
        self.selection.clear()
        self.project.audit_log.append({"at": datetime.now(timezone.utc).isoformat(), "undo": changes})
        self.unsaved = True
        self.save()
        self.render()
        self.status_text.set("直前の判断・編集を取り消しました。")

    @staticmethod
    def approvable(sample):
        return (sample.has_label and bool(sample.boxes) and not sample.image_error
                and not sample.label_errors and not sample.proposal_pending and not sample.proposals)

    def commit_page(self):
        if not self.require_project() or self.grab_current() is not None:
            return
        indices = list(self.visible)
        self.checkpoint(indices)
        skipped = 0
        for index in indices:
            sample = self.project.samples[index]
            if index in self.selection:
                sample.decision, sample.reason = "exclude", "手動選別"
            elif sample.decision in {"pending", "keep"}:
                if self.approvable(sample):
                    sample.decision, sample.reviewed = "keep", True
                    sample.reason = "10枚一覧で確認"
                else:
                    sample.decision = "fix"
                    skipped += 1
        old_filtered = list(self.filtered)
        self.selection.clear()
        # A pending-only filter shrinks after approval. Keep its current page to
        # avoid skipping the next ten samples; unfiltered pages advance normally.
        if self.apply_filters() == old_filtered:
            self.page += 1
        self.changed()
        self.status_text.set(f"このページを確定しました。枠・ラベルの確認が必要: {skipped}枚。")

    def set_decision(self, decision):
        if not self.require_project() or not self.selection:
            return
        indices = sorted(self.selection)
        self.checkpoint(indices)
        skipped = 0
        for index in indices:
            sample = self.project.samples[index]
            if decision == "keep" and not (self.approvable(sample) or (
                    sample.reviewed and sample.has_label and not sample.boxes
                    and not sample.label_errors and not sample.image_error
                    and not sample.proposal_pending and not sample.proposals)):
                skipped += 1
                continue
            sample.decision = decision
            sample.reason = "手動選別"
            if decision == "keep":
                sample.reviewed = True
        self.selection.clear()
        self.changed()
        if skipped:
            self.status_text.set(f"{skipped}枚は未承認候補やラベルの問題があるため採用できません。枠を編集してください。")

    def edit(self, index):
        if not self.require_project():
            return
        sample = self.project.samples[index]
        if sample.image_error:
            messagebox.showerror("画像の問題", sample.image_error, parent=self)
            return
        self.selection.discard(index)
        try:
            dialog = Editor(self, self.project, index)
            self.wait_window(dialog)
        except (OSError, ValueError) as exc:
            messagebox.showerror("編集画面", str(exc), parent=self)
            return
        if dialog.result is not None:
            self.checkpoint([index])
            sample.boxes, sample.decision = dialog.result
            sample.has_label, sample.reviewed = True, True
            sample.label_errors = []
            sample.proposals, sample.proposal_pending = [], False
            sample.reason = "編集画面で確認"
            self.changed()
        else:
            self.render()

    def may_switch(self):
        if self.busy or self.grab_current() is not None:
            return False
        if self.project:
            return self.save()
        return True

    def attach_project(self, project):
        self.project = project
        self.history, self.selection = [], set()
        self.unsaved = False
        state = project.ui_state
        self.page = state.get("page", 0)
        self.status_filter.set(state.get("status", "すべて"))
        self.class_filter.set(state.get("class", "すべてのクラス"))
        self.sort_order.set(state.get("sort", "ファイル名順"))
        self.search.set(state.get("search", ""))
        self.crop.set(state.get("crop", False))
        self.render()

    def new_project(self):
        if not self.may_switch():
            return
        dialog = NewProjectDialog(self)
        self.wait_window(dialog)
        if not dialog.result:
            return
        root, classes, labels, path = dialog.result
        def done(project):
            project.save(path)
            self.attach_project(project)
            self.status_text.set(f"{len(project.samples)}枚を読み込みました。元ファイルはそのままです。")
        self.run_job("データセットを読み込み中", lambda progress, cancelled:
                     scan_dataset(root, classes, labels, progress, cancelled), done)

    def open_project(self):
        if not self.may_switch():
            return
        path = filedialog.askopenfilename(parent=self, title="プロジェクトを開く",
                                          filetypes=[("YOLO Labeler project", "*.ylproject")])
        if path:
            self.load_project(Path(path))

    def load_project(self, path):
        try:
            project = Project.load(path)
            if not Path(project.source_root).is_dir():
                messagebox.showwarning("画像フォルダーがありません",
                    "移動後の画像フォルダーを指定してください。相対的な画像の配置は維持してください。", parent=self)
                root = filedialog.askdirectory(parent=self, title="移動後の画像フォルダー")
                if not root:
                    return
                project.source_root = str(Path(root).resolve())
                project.save()
            self.attach_project(project)
            self.status_text.set("保存したプロジェクトを開きました。")
        except (OSError, ValueError, TypeError, KeyError) as exc:
            messagebox.showerror("プロジェクトを開けません", str(exc), parent=self)

    def edit_classes(self):
        if not self.require_project():
            return
        dialog = Modal(self, "クラス設定", "530x480")
        ttk.Label(dialog, text="行順がYOLOのID順です。既存IDは維持します。", padding=18).pack()
        text = tk.Text(dialog, height=12, bg=PANEL, fg=TEXT, insertbackground=TEXT)
        text.pack(fill="both", expand=True, padx=18)
        text.insert("1.0", "\n".join(self.project.classes))
        def apply():
            classes = text.get("1.0", "end-1c").strip().splitlines()
            errors = class_errors(classes)
            if len(classes) < len(self.project.classes):
                errors.append("既存クラスの削除はできません。末尾への追加または名称変更ができます。")
            if sorted(classes) == sorted(self.project.classes) and classes != self.project.classes:
                errors.append("既存クラスの並び替えはできません。数値IDの対応を維持してください。")
            if errors:
                messagebox.showerror("クラス設定", "\n".join(errors), parent=dialog)
                return
            self.project.classes = classes
            self.class_filter.set("すべてのクラス")
            self.changed()
            dialog.destroy()
        button(dialog, "ID順を維持して保存", apply, accent=True).pack(pady=18)
        self.wait_window(dialog)

    def run_job(self, title, worker, done, discard_on_cancel=True):
        if self.busy:
            return
        self.busy = True
        events, cancelled = queue.Queue(), threading.Event()
        dialog = Modal(self, title, "540x200")
        ttk.Label(dialog, text=title, style="Sub.TLabel", padding=18).pack(anchor="w")
        status = tk.StringVar(value="準備中…")
        ttk.Label(dialog, textvariable=status, padding=(18, 0)).pack(anchor="w")
        bar = ttk.Progressbar(dialog, mode="indeterminate")
        bar.pack(fill="x", padx=18, pady=12)
        bar.start()
        def cancel():
            cancelled.set()
            status.set("処理の区切りで中止します…")
            cancel_button.state(["disabled"])
        cancel_button = button(dialog, "中止", cancel)
        cancel_button.pack(anchor="e", padx=18)
        dialog.protocol("WM_DELETE_WINDOW", cancel)
        def progress(current, total):
            # Coalesce progress reports: a large dataset must not flood Tk's queue.
            if current == total or current % max(1, total // 100) == 0:
                events.put(("progress", (current, total)))
        def work():
            try:
                events.put(("done", worker(progress, cancelled.is_set)))
            except BaseException as exc:
                events.put(("error", exc))
        def poll():
            try:
                while True:
                    kind, value = events.get_nowait()
                    if kind == "progress":
                        current, total = value
                        bar.stop()
                        bar.configure(mode="determinate", maximum=max(1, total), value=current)
                        status.set(f"{current:,} / {total:,}")
                        continue
                    bar.stop()
                    dialog.destroy()
                    self.busy = False
                    if kind == "error":
                        if isinstance(value, InterruptedError):
                            self.status_text.set(str(value))
                        else:
                            messagebox.showerror(title, str(value), parent=self)
                    elif cancelled.is_set() and discard_on_cancel:
                        # No partial proposal/quality state is applied on cancellation.
                        self.status_text.set("処理を中止しました。")
                    else:
                        try:
                            done(value)
                        except Exception as exc:
                            messagebox.showerror(title, str(exc), parent=self)
                    return
            except queue.Empty:
                self.after(60, poll)
        threading.Thread(target=work, daemon=True).start()
        self.after(60, poll)

    def auto_label(self):
        if not self.require_project() or not self.project.samples:
            return
        index = self.visible[0] if self.visible else 0
        dialog = AutoDialog(self, self.project, index)
        self.wait_window(dialog)
        if dialog.result:
            config, class_id, ranges = dialog.result
            self.run_job("半自動ラベリング", lambda progress, cancelled:
                         propose_batch(self.project, config, class_id, ranges, progress, cancelled),
                         self.apply_proposals)

    def apply_proposals(self, result):
        self.checkpoint(result["proposals"])
        for index, value in result["proposals"].items():
            sample = self.project.samples[index]
            if isinstance(value, tuple):
                sample.proposals, sample.proposal_info = value
            else:
                sample.proposals, sample.proposal_info = value, {"backend": "YOLO import"}
            sample.proposal_pending = True
        self.changed()
        self.status_text.set(f"{len(result['proposals'])}枚の候補を生成しました。枠の編集画面で確認してください。")
        if result["errors"]:
            messagebox.showwarning("処理できなかった画像", "\n".join(result["errors"][:20]), parent=self)

    def external_proposals(self):
        if not self.require_project():
            return
        path = filedialog.askdirectory(parent=self, title="外部候補フォルダー（classes.txtが必要）")
        if path:
            self.run_job("外部候補を読み込み中", lambda progress, cancelled:
                         import_proposals(self.project, Path(path), progress, cancelled), self.apply_proposals)

    def quality_scan(self):
        if not self.require_project():
            return
        def done(results):
            self.checkpoint(results)
            for index, quality in results.items():
                self.project.samples[index].quality = quality
            self.changed()
            self.status_text.set("品質と完全重複を検査しました。「品質の確認候補順」で確認できます。")
        self.run_job("品質・重複を検査中", lambda progress, cancelled:
                     analyze_quality(self.project, progress, cancelled), done)

    def auto_exclude(self):
        if not self.require_project():
            return
        candidates = safe_exclusion_candidates(self.project)
        if not candidates:
            messagebox.showinfo("自動除外", "対象がありません。先に品質・重複を検査してください。", parent=self)
            return
        if not messagebox.askyesno("除外する画像の確認",
                f"未確認の破損・完全重複 {len(candidates)}枚を除外状態にします。\n"
                "元ファイルは保持し、Ctrl+Zで取り消せます。\n\n"
                "\n".join(self.project.samples[i].path for i in list(candidates)[:6]), parent=self):
            return
        self.checkpoint(candidates)
        for index, reason in candidates.items():
            sample = self.project.samples[index]
            sample.decision, sample.reason = "exclude", reason
        self.changed()

    def show_validation(self):
        if not self.require_project():
            return
        dialog = Modal(self, "V-Training出力前検査", "1050x650")
        issues = validate_project(self.project)
        ttk.Label(dialog, text=f"エラー {sum(i.severity == 'error' for i in issues)}件 / "
                  f"注意 {sum(i.severity == 'warning' for i in issues)}件", style="Title.TLabel", padding=18).pack(anchor="w")
        tree = ttk.Treeview(dialog, columns=("severity", "file", "message"), show="headings")
        for key, label, width in (("severity", "種類", 70), ("file", "画像", 220), ("message", "内容", 670)):
            tree.heading(key, text=label)
            tree.column(key, width=width)
        scroll = ttk.Scrollbar(dialog, orient="vertical", command=tree.yview)
        tree.configure(yscrollcommand=scroll.set)
        scroll.pack(side="right", fill="y")
        tree.pack(fill="both", expand=True, padx=18, pady=(0, 18))
        for index, issue in enumerate(issues):
            name = self.project.samples[issue.sample_index].path if issue.sample_index is not None else "全体"
            tree.insert("", "end", iid=str(index), values=("エラー" if issue.severity == "error" else "注意", name, issue.message))
        for warning in self.project.import_warnings:
            tree.insert("", "end", values=("読み込み", "", warning))
        def jump(event=None):
            selection = tree.selection()
            if selection and selection[0].isdigit() and int(selection[0]) < len(issues):
                sample_index = issues[int(selection[0])].sample_index
                if sample_index is not None:
                    dialog.destroy()
                    self.edit(sample_index)
        tree.bind("<Double-Button-1>", jump)
        self.wait_window(dialog)

    def export(self):
        if not self.require_project():
            return
        issues = validate_project(self.project)
        if any(i.severity == "error" for i in issues):
            self.show_validation()
            return
        if not self.save():
            return
        path = filedialog.asksaveasfilename(parent=self, title="V-Training用ZIPの保存先",
                                          defaultextension=".zip", initialfile="vtraining-detector.zip",
                                          filetypes=[("ZIP archive", "*.zip")])
        if not path:
            return
        def done(report):
            summary = report["summary"]
            messagebox.showinfo("ZIPを書き出しました", f"画像 {summary['images']}枚 / 枠 {summary['boxes']}個\n"
                    f"ZIPの展開・CRC・ラベルを再検査しました。\n\n{path}", parent=self)
            self.status_text.set("V-Training用ZIPを作成しました: " + path)
        self.run_job("ZIPを作成・再検査中", lambda progress, cancelled:
                     export_vtraining(self.project, Path(path), progress=progress, cancelled=cancelled),
                     done, discard_on_cancel=False)

    def close(self):
        if self.busy or self.grab_current() is not None:
            return
        if self.project and not self.save():
            if not messagebox.askyesno("未保存の変更があります", "保存せず終了しますか？", parent=self):
                return
        self.destroy()


def main(argv=None):
    parser = argparse.ArgumentParser(description="YOLOデータの選別・枠編集・V-Training ZIP出力")
    parser.add_argument("project", nargs="?", type=Path, help="開く.ylprojectファイル")
    parser.add_argument("--version", action="version", version=__version__)
    args = parser.parse_args(argv)
    app = Application()
    if args.project:
        app.after(100, lambda: app.load_project(args.project))
    app.mainloop()
    return 0
