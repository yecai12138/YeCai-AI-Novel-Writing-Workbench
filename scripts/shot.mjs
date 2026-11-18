// 用 CDP 直接截取 WebView 内容（不受其它窗口遮挡），比屏幕截图可靠。
//
// 用法：node scripts/shot.mjs <输出文件路径>
// 前置同 cdp.mjs：应用需带 --remote-debugging-port=9222 启动。

import { writeFileSync } from "node:fs";

const PORT = process.env.CDP_PORT || "9222";
const out = process.argv[2];
if (!out) {
  console.error("用法：node scripts/shot.mjs <输出文件路径>");
  process.exit(1);
}

const targets = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
const page = targets.find((t) => t.type === "page");
if (!page) {
  console.error("没有找到页面目标");
  process.exit(2);
}

const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  ws.addEventListener("open", resolve, { once: true });
  ws.addEventListener("error", reject, { once: true });
});

let nextId = 0;
const pending = new Map();
ws.addEventListener("message", (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  }
});

function send(method, params) {
  const id = ++nextId;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
}

const shot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
const data = shot.result?.data;
if (!data) {
  console.error("截图失败：", JSON.stringify(shot));
  process.exit(3);
}
writeFileSync(out, Buffer.from(data, "base64"));
console.log(`saved ${out}`);
ws.close();
