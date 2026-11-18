#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""StoryDesk 内置任务包生成脚本（oh-story build_architect_brief 契约的最小实现）。

契约（与 src/services/gate.rs 对齐）：
- 调用：build_architect_brief.py --project <书根> --task world|volume|outline
        [--volume N] [--chapters a-b]
- 把任务包 markdown 写到 `<书根>/.storydesk/prompts/<task>-<NNN>.md`
  并打印一行「任务包：<绝对路径>」（gate.rs 从输出里认 .md 路径，取最后出现的那个）；
- stdout 一律 UTF-8 字节流（sys.stdout.buffer 写，别被控制台代码页转码）。

任务包是给执行智能体读的第一手说明：目标、必读材料、交付要求与回执格式。
应用层 prompt 的四行契约（「先完整读取任务包…」）指向的就是这里的产物。
"""

import argparse
import sys
from pathlib import Path

TASKS = ("world", "volume", "outline")

# 任务包开头要求的回执项（四行契约的最后一行说「交付后只回任务包开头要求的几项」）。
RECEIPT = "完成后只回以下几项（不要把正文贴回来）：\n1. 写入的文件路径；\n2. 一句话概括交付物；\n3. 字数（或要点数）。"


def _number(args) -> int:
    if args.chapters:
        try:
            return int(args.chapters.split("-", 1)[0])
        except ValueError:
            pass
    if args.volume:
        return args.volume
    return 1


def _brief_body(book: Path, task: str, args, number: int) -> str:
    common = (
        f"项目目录：{book}\n"
        "先读 设定/ 下的世界观、人物、势力与文风文件，再读 大纲/ 下已有的大纲与细纲；"
        "已有文件里与本次题材冲突的旧内容，一律按当前题材重写，不要沿用。\n"
    )
    if task == "outline":
        n = f"{number:03d}"
        return (
            f"# 任务包：生成第 {number} 章细纲\n\n{common}\n"
            "## 目标\n"
            f"在 大纲/ 下生成 细纲_第{n}章.md（恰好一份，不要另存副本）。\n\n"
            "## 交付要求\n"
            "细纲必须写清：本章目标、出场人物、主要冲突、情节转折、结尾钩子；"
            "并把上一章的伏笔/状态衔接进来；不计 # 号和空白不少于 300 字。\n\n"
            "## 回执\n" + RECEIPT
        )
    if task == "volume":
        return (
            f"# 任务包：生成第 {number} 卷卷纲\n\n{common}\n"
            "## 目标\n"
            f"在 大纲/ 下生成本卷的卷纲（卷级目标、主线走向、各章要点清单）。\n\n"
            "## 交付要求\n"
            "卷纲要能支撑逐章细纲的展开：每章一行要点（章号 + 一句话目标 + 悬念）；"
            "写明本卷结束时主线与人物应到达的状态。\n\n"
            "## 回执\n" + RECEIPT
        )
    # world：全书世界观设定
    return (
        f"# 任务包：补全世界观设定\n\n{common}\n"
        "## 目标\n"
        "补全 设定/ 下空缺的世界观、势力、人物与文风文件（先看已有的，缺什么补什么）。\n\n"
        "## 交付要求\n"
        "每个文件一个主题，标题清晰；人物要有动机与约束，势力要有立场与冲突点；"
        "文风文件写清叙事视角、节奏与用词偏好。\n\n"
        "## 回执\n" + RECEIPT
    )


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="build_architect_brief.py", description="StoryDesk 任务包生成")
    parser.add_argument("--project", required=True, help="书根目录")
    parser.add_argument("--task", required=True, choices=TASKS)
    parser.add_argument("--volume", type=int)
    parser.add_argument("--chapters", help="章区间，如 1-3")
    args = parser.parse_args(argv)

    book = Path(args.project)
    if not book.is_dir():
        print(f"书目录不存在：{book}", file=sys.stderr)
        return 1

    number = _number(args)
    out_dir = book / ".storydesk" / "prompts"
    out_dir.mkdir(parents=True, exist_ok=True)
    out = out_dir / f"{args.task}-{number:03d}.md"
    out.write_text(_brief_body(book, args.task, args, number), encoding="utf-8")

    # 最后一行必须是路径（gate.rs 的 extract_md_path 取最后出现的 .md token）。
    sys.stdout.buffer.write(f"任务包已生成。\n任务包：{out}\n".encode("utf-8"))
    sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
