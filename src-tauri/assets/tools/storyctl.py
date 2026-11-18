#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""StoryDesk 内置章节体检脚本（oh-story storyctl 契约的最小实现）。

为什么有这个文件：oh-story 的原版 storyctl.py 需要用户在宿主里装 skill 才有；
StoryDesk 把它打进 exe，首次使用时释放到 `<工作区>/.storydesk/tools/`，
没有外部安装也能做「写前/写后体检」。

契约（与 src/services/gate.rs 对齐，两边都不许单方面改）：
- 调用：storyctl.py chapter check --project <书根> --chapter N [--plain] [--min-chars M]
- `--plain`：输出白话中文；缺省输出 JSON（{"ok":...} / {"error_code":...,"message":...}）
- 通过 exit 0；不通过 exit 1；脚本自身崩溃才走 exit != 0 且带 Traceback。
- stdout 一律 UTF-8 字节流（Windows 控制台默认 cp936，必须用 sys.stdout.buffer 写）。

检查口径（与本地兜底检查同口径）：
1. 细纲恰好一份：`大纲/细纲_*第NNN章*.md`（兼容 `第1章` / `第001章`）；
2. 细纲非空：不计 `#` 与空白 ≥ 300 字；
3. 正文恰好一份：`正文/第NNN章_*.md`；
4. 正文非空：不计空白与 Markdown 标记 ≥ min-chars（默认 800）字。
"""

import argparse
import json
import sys
from pathlib import Path

OUTLINE_MIN_CHARS = 300
PROSE_MIN_CHARS = 800

# 不计入正文字数的 Markdown 标记符号（与 gate.rs 的 count_prose_chars 一致）
_MARKUP = set("#>*`-_【】")


def _count_chars(text: str) -> int:
    """不计空白与 Markdown 标记的字数（gate.rs 同口径）。"""
    return sum(1 for ch in text if not ch.isspace() and ch not in _MARKUP)


def _outline_chars(text: str) -> int:
    """细纲字数：不计 `#` 与空白（hook 同口径）。"""
    return sum(1 for ch in text if not ch.isspace() and ch != "#")


def _find_chapter_files(directory: Path, prefixes, suffix=".md"):
    """列出文件名命中任一前缀的文件。"""
    if not directory.is_dir():
        return []
    hits = []
    for path in sorted(directory.iterdir()):
        if not path.is_file() or path.suffix != suffix:
            continue
        name = path.name
        if any(name.startswith(p) for p in prefixes):
            hits.append(path)
    return hits


def chapter_paths(book: Path, chapter: int):
    numbered = f"第{chapter:03d}章"
    loose = f"第{chapter}章"
    outlines = [
        p for p in _find_chapter_files(book / "大纲", ["细纲"])
        if numbered in p.name or loose in p.name
    ]
    prose = _find_chapter_files(book / "正文", [numbered, loose])
    return outlines, prose


def check_chapter(book: Path, chapter: int, min_chars: int):
    """返回 (ok: bool, message: str, stats: dict)。检查顺序与 oh-story 的语义一致。"""
    outlines, prose = chapter_paths(book, chapter)

    # 1) 细纲：写正文的硬前置。多条/没有都算没查成。
    if len(outlines) != 1:
        return False, f"这次没查成：第 {chapter} 章没有细纲，或者细纲不止一份。", {}
    outline_chars = _outline_chars(outlines[0].read_text(encoding="utf-8", errors="replace"))
    if outline_chars < OUTLINE_MIN_CHARS:
        return False, (
            f"这次没查成：第 {chapter} 章的细纲是空的"
            f"（不计 # 号和空白不到 {OUTLINE_MIN_CHARS} 字）。"
        ), {"outlineChars": outline_chars}

    # 2) 正文：写完之后的体检主体。
    if len(prose) == 0:
        return False, f"第 {chapter} 章还没有正文（正文/第{chapter:03d}章_*.md）。", {
            "outlineChars": outline_chars,
        }
    if len(prose) > 1:
        return False, f"第 {chapter} 章有 {len(prose)} 份正文文件，要求恰好一份。", {
            "outlineChars": outline_chars,
        }
    prose_chars = _count_chars(prose[0].read_text(encoding="utf-8", errors="replace"))
    if prose_chars < min_chars:
        return False, f"第 {chapter} 章正文只有 {prose_chars} 字（要求 ≥ {min_chars} 字）。", {
            "outlineChars": outline_chars,
            "chars": prose_chars,
        }

    return True, f"第 {chapter} 章体检通过：正文 {prose_chars} 字，细纲 {outline_chars} 字。", {
        "outlineChars": outline_chars,
        "chars": prose_chars,
        "file": prose[0].name,
    }


def _emit(ok: bool, message: str, stats: dict, plain: bool):
    """stdout 按字节写 UTF-8（Windows 控制台默认 cp936，read_to_string 会炸）。"""
    if plain:
        payload = message + "\n"
    elif ok:
        payload = json.dumps({"ok": True, "message": message, **stats}, ensure_ascii=False) + "\n"
    else:
        payload = json.dumps({"error_code": "CHECK_FAILED", "message": message}, ensure_ascii=False) + "\n"
    sys.stdout.buffer.write(payload.encode("utf-8"))
    sys.stdout.buffer.flush()


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="storyctl.py", description="StoryDesk 章节体检（oh-story 契约）")
    sub = parser.add_subparsers(dest="cmd", required=True)

    check = sub.add_parser("chapter", help="章节体检")
    check_sub = check.add_subparsers(dest="action", required=True)
    cc = check_sub.add_parser("check")
    cc.add_argument("--project", required=True, help="书根目录")
    cc.add_argument("--chapter", type=int, required=True)
    cc.add_argument("--plain", action="store_true", help="白话输出（缺省 JSON）")
    cc.add_argument("--min-chars", type=int, default=PROSE_MIN_CHARS, help="正文最少字数")

    args = parser.parse_args(argv)
    if not (args.cmd == "chapter" and args.action == "check"):
        parser.error("只支持 chapter check")

    book = Path(args.project)
    if not book.is_dir():
        _emit(False, f"书目录不存在：{book}", {}, plain=True)
        return 1

    ok, message, stats = check_chapter(book, args.chapter, args.min_chars)
    _emit(ok, message, stats, plain=args.plain)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
