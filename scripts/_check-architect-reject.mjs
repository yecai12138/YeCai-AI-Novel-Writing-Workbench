// 阶段 11 验证：大纲架构师拿到「一句话需求」而不是「细纲任务」时必须拒绝写入。
// 直接以假 CLI 的 stdin 契约喂一段伪 prompt，避免为了验证去改真实规划器的输出。
import { spawnSync } from "node:child_process";
import { statSync } from "node:fs";

const BOOK = "D:/桌面/大制作/小说/验收示例/长篇/我的小说";
const SHIM = "C:/storydesk-shim/fake-claude.mjs";

const prompt = [
  "你现在是「野菜工作台」的【大纲架构师】。",
  "你的核心职责是处理 大纲/ 目录下的内容。",
  "角色约束：只负责 大纲/，禁止写 正文/ 与 设定/；禁止把整句需求原样当细纲。",
  "",
  `项目目录：${BOOK}`,
  "补充要求：把设定里的几个文件内容给我写了",
].join("\n");

const before = statSync(`${BOOK}/大纲/细纲_第001章.md`).mtimeMs;

const run = spawnSync(process.execPath, [SHIM, "-p", prompt, "--output-format", "stream-json"], {
  encoding: "utf8",
});

const lines = run.stdout.split("\n").filter((l) => l.trim());
const texts = [];
let result = null;
for (const line of lines) {
  try {
    const evt = JSON.parse(line);
    if (evt.type === "result") result = evt.result;
    const text = evt.message?.content?.find((c) => c.type === "text")?.text;
    if (text) texts.push(text);
    const tool = evt.message?.content?.find((c) => c.type === "tool_use");
    if (tool?.name === "Write") texts.push(`WRITE: ${tool.input.file_path}`);
  } catch {
    /* 非 JSON 行（进度提示之类）忽略 */
  }
}

const after = statSync(`${BOOK}/大纲/细纲_第001章.md`).mtimeMs;
console.log(
  JSON.stringify(
    {
      exit: run.status,
      result,
      wroteAnything: !texts.every((t) => !t.startsWith("WRITE")),
      outlineUntouched: before === after,
      texts,
    },
    null,
    2,
  ),
);
