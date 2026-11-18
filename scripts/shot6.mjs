// 阶段 6 截图：在**同一个 CDP 连接**里先操作再截图。
// 分两次调用会有问题——快照 Popover 在两次连接之间会因为失焦/外部交互被关掉，
// 设置弹窗的长内容也需要先滚到目标位置才拍得到。
//
// 用法：node scripts/shot6.mjs <providers|snapshots> <输出路径>

import { writeFileSync } from "node:fs";

const PORT = process.env.CDP_PORT || "9222";
const mode = process.argv[2] ?? "providers";
const out = process.argv[3];
if (!out) {
  console.error("用法：node scripts/shot6.mjs <providers|snapshots> <输出路径>");
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
const send = (method, params) => {
  const id = ++nextId;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const PRELUDE = `
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const all = (sel, root = document) => [...root.querySelectorAll(sel)];
const textOf = (el) => (el ? (el.textContent || "").trim() : null);
const byText = (text, sel = "button", root = document) =>
  all(sel, root).find((el) => (el.textContent || "").trim() === text) || null;
const dispatchClick = (el) => {
  if (!el) return false;
  el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }));
  el.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
  el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
  el.click();
  return true;
};
const dialogs = () => all('[role="dialog"]');
const dialog = () => dialogs()[0] || null;
`;

const MODES = {
  /** 设置 → AI 引擎 → 滚到「全部提供商」表格。 */
  providers: `
    await (async () => {
      if (!dialog()) dispatchClick(byText("设置"));
      for (let i = 0; i < 40 && !dialog(); i += 1) await sleep(80);
      const side = byText("AI 引擎", "button", dialog() || document);
      if (side) dispatchClick(side);
      await sleep(600);
      const presetTab = all('[role="tab"]').find((t) => textOf(t) === "第三方模型提供商");
      if (presetTab) dispatchClick(presetTab);
      await sleep(500);
    })();
    // 弹窗内部滚动容器滚到底，让表格完整可见
    const scroller = [...(dialog()?.querySelectorAll("div") || [])]
      .filter((d) => d.scrollHeight > d.clientHeight + 40)
      .sort((a, b) => b.scrollHeight - a.scrollHeight)[0];
    if (scroller) {
      scroller.scrollTop = scroller.scrollHeight;
      await sleep(400);
    }
    return { scrolled: Boolean(scroller), rows: all('[role="dialog"] tbody tr').length };
  `,

  /** 打开某文件后点「快照」，Popover 留在打开状态再截图。 */
  snapshots: `
    await (async () => {
      // 先关掉设置弹窗
      for (let i = 0; i < 3 && dialog(); i += 1) {
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
        await sleep(250);
      }
      // 打开一个**有快照**的文件（当前工作区里快照都在 设定/全书梗概.md 上；
      // 快照列表在 openFile / save 之后才加载，所以必须先点文件）
      const fileNode =
        all("button").find((b) => (textOf(b) || "").includes("全书梗概")) ||
        all("button").find((b) => (textOf(b) || "").includes("第001章"));
      if (fileNode) {
        dispatchClick(fileNode);
        await sleep(900);
      }
      if (!document.querySelector(".cm-content")) {
        for (let i = 0; i < 40 && !document.querySelector(".cm-content"); i += 1) await sleep(100);
      }
      // 快照列表是「保存后刷新」的（store 只在 save/openFile 里 loadSnapshots），
      // 所以先点一次保存，保证面板里有内容
      const saveBtn = all("button").find((b) => (textOf(b) || "") === "保存");
      if (saveBtn && !saveBtn.disabled) {
        dispatchClick(saveBtn);
        await sleep(900);
      }
      const btn = all("button").find((b) => (textOf(b) || "").startsWith("快照"));
      if (btn) dispatchClick(btn);
      await sleep(800);
    })();
    const panel = all('[data-radix-popper-content-wrapper]').map((w) => textOf(w))
      .find((t) => t && t.includes("每次覆盖保存前的旧版本")) || null;
    return { panelOpen: Boolean(panel), panelText: (panel || "").slice(0, 120) };
  `,
};

const body = MODES[mode];
if (!body) {
  console.error(`未知模式：${mode}`);
  process.exit(4);
}

const evaled = await send("Runtime.evaluate", {
  expression: `(async () => { ${PRELUDE}\n${body}\n})()`,
  awaitPromise: true,
  returnByValue: true,
});
if (evaled.result?.exceptionDetails) {
  console.error("JS 异常：", evaled.result.exceptionDetails.text);
  process.exit(3);
}
console.log(JSON.stringify(evaled.result?.result?.value ?? null));

// Popover 在保存触发的重渲染里会被关掉，所以截图前确认它还在；不在就再点一次，
// 并且**不要**在开面板后 sleep 太久（超过 ~500ms 就容易被后续重渲染关掉）。
if (mode === "snapshots") {
  const probe = `(() => {
    const has = Boolean([...document.querySelectorAll('[data-radix-popper-content-wrapper]')]
      .map((w) => (w.textContent || ""))
      .find((t) => t.includes("每次覆盖保存前的旧版本")));
    if (!has) {
      const btns = [...document.querySelectorAll("button")];
      const b = btns.find((x) => ((x.textContent || "").trim().startsWith("快照")));
      if (b) b.click();
    }
    return has;
  })()`;
  for (let i = 0; i < 6; i += 1) {
    const r = await send("Runtime.evaluate", { expression: probe, returnByValue: true });
    if (r.result?.result?.value === true) break;
    await sleep(300);
  }
}

const shot = await send("Page.captureScreenshot", { format: "png" });
const data = shot.result?.data;
if (!data) {
  console.error("截图失败", JSON.stringify(shot).slice(0, 300));
  process.exit(5);
}
writeFileSync(out, Buffer.from(data, "base64"));
console.log(`saved ${out}`);
ws.close();
