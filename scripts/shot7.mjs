// 阶段 7 截图：同一条 CDP 连接里先切 Tab（必要时展开 prompt）再截图。
// 用法：node scripts/shot7.mjs <team|chat> <输出路径>
const PORT = process.env.CDP_PORT || "9222";
const which = process.argv[2] || "team";
const out = process.argv[3] || "docs/phase7-agents.png";

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
const send = (method, params) =>
  new Promise((resolve) => {
    const id = ++nextId;
    pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });

const { writeFileSync } = await import("node:fs");

const script = `(async () => {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const dispatchClick = (el) => {
    if (!el) return false;
    el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }));
    el.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
    el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
    el.click();
    return true;
  };
  dispatchClick(document.querySelector('[data-testid="tab-${which === "team" ? "team" : "chat"}"]'));
  await sleep(320);
  ${
    which === "chat"
      ? `const summary = document.querySelector('[data-testid="ai-prompt"]')?.closest('details')?.querySelector('summary');
         if (summary) dispatchClick(summary);
         await sleep(220);`
      : ""
  }
  return {
    tab: document.querySelector('[role="tab"][data-state="active"]')?.textContent ?? null,
    cards: document.querySelectorAll('[data-agent-role]').length,
  };
})()`;

const probe = await send("Runtime.evaluate", { expression: script, awaitPromise: true, returnByValue: true });
const shot = await send("Page.captureScreenshot", { format: "png" });
if (!shot.result?.data) {
  console.error("截图失败：", JSON.stringify(shot).slice(0, 300));
  process.exit(3);
}
writeFileSync(out, Buffer.from(shot.result.data, "base64"));
ws.close();
console.log(JSON.stringify({ state: probe.result?.result?.value ?? null, out }, null, 2));
