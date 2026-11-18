// 快照 Popover 截图：用 CDP 的真实鼠标事件点开面板（合成 click 会让 Radix 认为
// 焦点不在页面里，随后的 captureScreenshot 会把 Popover 关掉），点完立刻截图。
//
// 用法：node scripts/shot-popover.mjs <输出路径> [等待毫秒]

import { writeFileSync } from "node:fs";

const PORT = process.env.CDP_PORT || "9222";
const out = process.argv[2] ?? "docs/phase6-snapshots.png";
const wait = Number(process.argv[3] ?? 400);

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
const send = (method, params) => {
  const id = ++nextId;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  return r.result?.result?.value ?? null;
};

// 1. 关掉可能开着的设置弹窗，确保打开了一个有快照的文件
await evaluate(`(async () => {
  const all = (s, r = document) => [...r.querySelectorAll(s)];
  const text = (el) => (el ? (el.textContent || "").trim() : null);
  for (let i = 0; i < 3 && all('[role="dialog"]').length; i += 1) {
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await new Promise((r) => setTimeout(r, 250));
  }
  const node = all("button").find((b) => (text(b) || "").includes("全书梗概"));
  if (node) node.click();
  await new Promise((r) => setTimeout(r, 800));
  return Boolean(document.querySelector(".cm-content"));
})()`);

// 2. 取「快照」按钮的中心坐标
const rect = await evaluate(`(() => {
  const b = [...document.querySelectorAll("button")].find((x) => ((x.textContent || "").trim().startsWith("快照")));
  if (!b) return null;
  const r = b.getBoundingClientRect();
  return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2), label: (b.textContent || "").trim() };
})()`);
console.log("button:", JSON.stringify(rect));
if (!rect) {
  console.error("没找到「快照」按钮");
  process.exit(3);
}

// 3. 真实鼠标点击（trusted），Radix 才会认为焦点在页面内
const base = { x: rect.x, y: rect.y, button: "left", clickCount: 1, buttons: 1 };
await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: rect.x, y: rect.y, buttons: 0 });
await send("Input.dispatchMouseEvent", { type: "mousePressed", ...base });
await send("Input.dispatchMouseEvent", { type: "mouseReleased", ...base, buttons: 0 });
await sleep(wait);

const state = await evaluate(`(() => {
  const panel = [...document.querySelectorAll('[data-radix-popper-content-wrapper]')]
    .map((w) => (w.textContent || "").trim())
    .find((t) => t.includes("每次覆盖保存前的旧版本"));
  return panel ? panel.slice(0, 200) : null;
})()`);
console.log("panel:", JSON.stringify(state));

// 4. 截图：用 screencast 拿一帧，而不是 Page.captureScreenshot——
//    WebView2 上 captureScreenshot 会让弹层先被关掉（截出来只有背景）。
let frame = null;
ws.addEventListener("message", (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.method === "Page.screencastFrame" && msg.params?.data) {
    frame = msg.params.data;
    ws.send(JSON.stringify({ id: 900000 + Math.floor(Math.random() * 1000), method: "Page.screencastFrameAck", params: { sessionId: msg.params.sessionId } }));
  }
});

await send("Page.startScreencast", { format: "jpeg", quality: 92, maxWidth: 1600, maxHeight: 1200 });
// 注意：这里**不要**再点一次按钮——Radix 的 trigger 是 toggle，再点会把面板关掉。
await sleep(600);
await send("Page.stopScreencast");

if (!frame) {
  // 兜底：退回普通截图
  const shot = await send("Page.captureScreenshot", { format: "png" });
  frame = shot.result?.data;
}
if (!frame) {
  console.error("截图失败");
  process.exit(5);
}
writeFileSync(out, Buffer.from(frame, "base64"));
console.log(`saved ${out}`);
ws.close();
