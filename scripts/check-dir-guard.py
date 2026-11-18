# 手动验证 guard-agent-dir.py 的判定：把环境变量和 stdin 都在 Python 内部准备好，
# 免得 PowerShell → python 之间因代码页转换把中文路径弄成乱码（那是测试脚手架的问题，不是守卫的问题）。
import json
import os
import runpy
import sys

# Windows 控制台默认 cp936，打印 "⛔" 会抛 UnicodeEncodeError；先把 stdout 钉成 UTF-8。
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

BOOK = r"D:\桌面\大制作\小说\验收示例\长篇\我的小说"
HOOK = os.path.join(BOOK, ".claude", "hooks", "guard-agent-dir.py")

cases = [
    ("允许：大纲下", os.path.join(BOOK, "大纲", "细纲_第001章.md"), 0),
    ("拦截：同书另一目录（正文）", os.path.join(BOOK, "正文", "第001章_雨夜来客.md"), 2),
    ("拦截：设定下", os.path.join(BOOK, "设定", "人物.md"), 2),
    ("拦截：逃离书根", os.path.join(BOOK, "..", "..", "..", "evil.md"), 2),
]

for label, target, expect in cases:
    os.environ["STORYDESK_AGENT_DIR"] = os.path.join(BOOK, "大纲")

    # 用 subprocess 起一个干净的子进程，stdin 写 hook 载荷，再读 stderr 与退出码
    import subprocess

    payload = json.dumps({"tool_input": {"file_path": target}}, ensure_ascii=False)
    proc = subprocess.run(
        [sys.executable, HOOK],
        input=payload.encode("utf-8"),
        capture_output=True,
        env={**os.environ, "STORYDESK_AGENT_DIR": os.path.join(BOOK, "大纲")},
    )
    stderr = proc.stderr.decode("utf-8", "replace").strip()
    ok = "OK" if proc.returncode == expect else "FAIL"
    print(f"[{ok}] {label}: exit={proc.returncode}（期望 {expect}）stderr={stderr!r}")
