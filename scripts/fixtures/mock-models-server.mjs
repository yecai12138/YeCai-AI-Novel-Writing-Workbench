// 本地 mock 的 OpenAI 兼容 /models 服务，用于阶段 5 的「获取可用模型」验收。
// 用法：node scripts/fixtures/mock-models-server.mjs [port]
// 会把每次请求的 Authorization 是否存在写进 logs/mock-models.log（不落 key 明文）。

import { createServer } from "node:http";
import { appendFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const logDir = join(here, "..", "..", "docs");
mkdirSync(logDir, { recursive: true });
const logFile = join(logDir, "mock-models.log");

const port = Number(process.argv[2] || 8791);

const MODELS = [
  { id: "mock-gpt-4o" },
  { id: "mock-gpt-4o-mini" },
  { id: "mock-qwen-plus" },
  { id: "mock-deepseek-chat" },
  { id: "mock-claude-sonnet" },
];

const server = createServer((req, res) => {
  const auth = req.headers.authorization || "";
  appendFileSync(
    logFile,
    JSON.stringify({
      at: new Date().toISOString(),
      method: req.method,
      url: req.url,
      hasAuth: auth.startsWith("Bearer "),
      authHint: auth.startsWith("Bearer ") ? `Bearer …${auth.slice(-4)}` : null,
    }) + "\n",
  );

  if (req.method === "GET" && req.url?.replace(/\/+$/, "").endsWith("/models")) {
    res.writeHead(200, { "content-type": "application/json; charset=utf-8" });
    res.end(JSON.stringify({ object: "list", data: MODELS }));
    return;
  }

  res.writeHead(404, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: { message: "not found" } }));
});

server.listen(port, "127.0.0.1", () => {
  console.log(`mock models server: http://127.0.0.1:${port}/v1/models`);
});
