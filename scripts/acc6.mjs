// 阶段 6 验收驱动：提供商删除 + 快照删除（都走真实 UI 点击 + 二次确认）。
// 用法：node scripts/acc6.mjs <step>
// 前置：应用以 dev 模式启动并带 --remote-debugging-port=9222。

const PORT = process.env.CDP_PORT || "9222";
const step = process.argv[2] || "providers";

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

// 页面内辅助函数。模块四避坑指南逐条落实在这段里。
const PRELUDE = `
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const all = (sel, root = document) => [...root.querySelectorAll(sel)];
const textOf = (el) => (el ? (el.textContent || "").trim() : null);
const byText = (text, sel = "button", root = document) =>
  all(sel, root).find((el) => (el.textContent || "").trim() === text) || null;
const hasText = (text, sel = "button", root = document) =>
  all(sel, root).some((el) => (el.textContent || "").trim() === text);
const waitFor = async (fn, ms = 8000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = fn();
    if (v) return v;
    await sleep(80);
  }
  return null;
};

/** 坑 1：Radix 的 Tabs/Select 不响应 .click()，必须四连派发。 */
const dispatchClick = (el) => {
  if (!el) return false;
  el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }));
  el.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
  el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
  el.click();
  return true;
};

const setInput = (el, value) => {
  if (!el) throw new Error("setInput: 元素不存在");
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value").set.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
};

const dialogs = () => all('[role="dialog"]');
const dialog = () => dialogs()[0] || null;
/** 坑 2：设置弹窗与模型选择弹窗嵌套，取「文本最短且含关键字」的那个。 */
const shortestDialog = (kw) =>
  dialogs().filter((d) => (d.textContent || "").includes(kw))
    .sort((a, b) => (a.textContent || "").length - (b.textContent || "").length)[0] || null;
const pickerDialog = () => shortestDialog("确认添加");
/** 二次确认框（删除类）：文案里必然带「不可恢复」或「永久清除」。
 *  注意 ConfirmDialog 渲染在设置弹窗内部（嵌套 dialog），必须取文本**最短**的那个，
 *  否则会选中外层设置弹窗，拿到的是表格行里的「删除」按钮。 */
const confirmDialog = () =>
  dialogs()
    .filter((d) => {
      const t = d.textContent || "";
      return t.includes("不可恢复") || t.includes("永久清除");
    })
    .sort((a, b) => (a.textContent || "").length - (b.textContent || "").length)[0] || null;
const confirmButton = () => {
  const d = confirmDialog();
  if (!d) return null;
  const btns = [...d.querySelectorAll("button")];
  return btns.find((b) => textOf(b) === "删除") || btns.find((b) => textOf(b) === "确定") || null;
};

/** 「全部提供商」表格行。 */
const providerRows = () =>
  all('[role="dialog"] tbody tr').map((tr) => {
    const cells = [...tr.querySelectorAll("td")].map((td) => textOf(td));
    return {
      el: tr,
      name: cells[0] || null,
      id: cells[1] || null,
      key: cells[2] || null,
      protocol: cells[3] || null,
      models: cells[4] || null,
      actions: [...tr.querySelectorAll("button")].map((b) => textOf(b)).filter(Boolean),
      text: textOf(tr),
    };
  });
const rowById = (id) => providerRows().find((r) => (r.id || "") === id) || null;
const rowButton = (row, label) =>
  row ? [...row.el.querySelectorAll("button")].find((b) => textOf(b) === label) || null : null;

const openAiTab = async () => {
  if (!dialog()) {
    const btn = byText("设置");
    if (btn) dispatchClick(btn);
    await waitFor(() => dialog());
  }
  // 设置面板左侧的「环境自检 / AI 引擎 / 关于」是普通按钮，不是 role=tab
  const sideBtn = byText("AI 引擎", "button", dialog() || document);
  if (sideBtn) dispatchClick(sideBtn);
  await sleep(600);
};
const closeDialogs = async () => {
  for (let i = 0; i < 3; i += 1) {
    const d = dialog();
    if (!d) break;
    const esc = new KeyboardEvent("keydown", { key: "Escape", bubbles: true });
    document.dispatchEvent(esc);
    await sleep(250);
  }
};
/** 快照面板挂在编辑器工具栏的「快照」按钮上。 */
const snapButton = () => all("button").find((b) => (textOf(b) || "").startsWith("快照")) || null;
const snapPanel = () =>
  all('[data-radix-popper-content-wrapper]').map((w) => textOf(w)).find((t) => t && t.includes("每次覆盖保存前的旧版本")) || null;
const snapRows = () => {
  const wrapper = all('[data-radix-popper-content-wrapper]').find((w) =>
    (w.textContent || "").includes("每次覆盖保存前的旧版本"));
  if (!wrapper) return [];
  return [...wrapper.querySelectorAll('button[aria-label="删除此版本快照"]')].map((btn) => ({
    btn,
    row: btn.parentElement,
    text: textOf(btn.parentElement),
  }));
};
`;

const STEPS = {
  /** 提供商表格结构：字段、Badge、操作按钮。 */
  providers: `
    await openAiTab();
    const rows = providerRows();
    return {
      rowCount: rows.length,
      rows: rows.map((r) => ({
        name: r.name, id: r.id, key: r.key, protocol: r.protocol, models: r.models, actions: r.actions,
      })),
      headers: all('[role="dialog"] thead th').map((th) => textOf(th)),
    };
  `,

  /** 验收 1 前半：建一个带 Key 的测试提供商，并设为当前（一口气做完）。 */
  "add-provider": `
    await openAiTab();
    const customTab = all('[role="tab"]').find((t) => textOf(t) === "自定义模型 API");
    dispatchClick(customTab);
    await sleep(400);
    if (!document.getElementById("custom-id")) return { error: "自定义 Tab 没挂载" };

    setInput(document.getElementById("custom-id"), "acc6-gateway");
    setInput(document.getElementById("custom-name"), "验收网关");
    setInput(document.getElementById("custom-base"), "http://127.0.0.1:8791/v1");
    setInput(document.getElementById("custom-key"), "sk-acc6-delete-me");
    await sleep(250);
    byText("手动输入模型 ID，回车添加") || null;
    const manual = [...document.querySelectorAll("input")].find((i) => (i.placeholder || "").includes("手动输入模型"));
    if (manual) {
      setInput(manual, "mock-gpt-4o");
      await sleep(120);
      byText("添加")?.click();
      await sleep(250);
    }
    byText("创建提供商")?.click();
    await sleep(2000);
    const hint = all('[role="dialog"] div').map((d) => textOf(d)).find((t) => t && t.startsWith("已创建提供商")) || null;

    // 设为当前
    const presetTab = all('[role="tab"]').find((t) => textOf(t) === "第三方模型提供商");
    dispatchClick(presetTab);
    await sleep(500);
    const row = rowById("acc6-gateway");
    const setBtn = rowButton(row, "设为当前");
    if (setBtn) dispatchClick(setBtn);
    await sleep(1200);

    const after = providerRows();
    return {
      hint,
      created: Boolean(row),
      setActive: Boolean(setBtn),
      rows: after.map((r) => ({ name: r.name, id: r.id, key: r.key, models: r.models, actions: r.actions })),
      active: after.filter((r) => (r.text || "").includes("当前")).map((r) => r.id),
    };
  `,

  /** 验收 1 后半 + 验收 4：删除当前提供商，必须出现二次确认并真的删掉。 */
  "delete-provider": `
    await openAiTab();
    const before = providerRows();
    const row = rowById("acc6-gateway");
    if (!row) return { error: "没找到 acc6-gateway 行", rows: before.map((r) => r.id) };
    const delBtn = rowButton(row, "删除");
    if (!delBtn) return { error: "行内没有「删除」按钮", actions: row.actions };
    dispatchClick(delBtn);
    const confirm = await waitFor(() => confirmDialog(), 5000);
    const confirmText = confirm ? textOf(confirm).replace(/\\s+/g, " ").slice(0, 200) : null;
    const yes = confirmButton();
    if (!yes) return { error: "二次确认框里没有确认按钮", confirmText };
    dispatchClick(yes);
    await sleep(2500);
    const after = providerRows();
    return {
      confirmText,
      askedConfirm: Boolean(confirm),
      remaining: after.map((r) => r.id),
      stillThere: after.some((r) => r.id === "acc6-gateway"),
      activeIds: after.filter((r) => (r.text || "").includes("当前")).map((r) => r.id),
    };
  `,

  /** 验收 3 前置：先在磁盘上连写三次，制造快照，然后在 UI 里打开该文件。 */
  "snapshot-seed": `
    await closeDialogs();
    const ws = localStorage.getItem("storydesk.lastWorkspace") || null;
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (!ws || !invoke) return { error: "拿不到工作区或 invoke", ws };
    const bookRoot = ws + "/长篇/我的小说";
    const file = bookRoot + "/设定/全书梗概.md";
    const base = (await invoke("read_file", { workspaceRoot: ws, path: file })).content;
    const w1 = await invoke("write_file", { workspaceRoot: ws, path: file, content: base + "\\n<!-- acc6 seed 1 -->\\n" });
    const w2 = await invoke("write_file", { workspaceRoot: ws, path: file, content: w1.file.content + "\\n<!-- acc6 seed 2 -->\\n" });
    const w3 = await invoke("write_file", { workspaceRoot: ws, path: file, content: w2.file.content + "\\n<!-- acc6 seed 3 -->\\n" });
    const list = await invoke("list_snapshots", { workspaceRoot: ws, path: file });
    // 在 UI 里打开这个文件，快照按钮才会出现
    const node = all("button").find((b) => (textOf(b) || "").includes("全书梗概"));
    if (node) dispatchClick(node);
    const ready = await waitFor(() => document.querySelector(".cm-content"), 8000);
    return {
      workspace: ws,
      snapshots: list.map((s) => ({ id: s.id, bytes: s.bytes, path: s.path })),
      opened: Boolean(ready),
      snapButton: textOf(snapButton()),
    };
  `,

  /** 验收 3 中段：打开快照面板，读空状态/提示与列表项。 */
  snapshots: `
    const btn = snapButton();
    if (!btn) return { error: "找不到「快照」按钮（编辑器没打开文件？）" };
    dispatchClick(btn);
    await sleep(700);
    const panel = snapPanel();
    const rows = snapRows();
    return {
      panelText: panel ? panel.slice(0, 300) : null,
      hint: panel && panel.includes("每次覆盖保存前的旧版本（最多留 20 份）"),
      emptyHint: panel && panel.includes("还没有快照。保存一次后就会出现。"),
      count: rows.length,
      rows: rows.map((r) => (r.text || "").replace(/\\s+/g, " ")),
    };
  `,

  /** 验收 3 + 4：点垃圾桶 → 必须弹二次确认 → 确认后该行消失。 */
  "delete-snapshot": `
    let rows = snapRows();
    if (rows.length === 0) {
      const btn = snapButton();
      if (btn) dispatchClick(btn);
      await sleep(700);
      rows = snapRows();
    }
    if (rows.length === 0) return { error: "面板里没有快照可删", panel: snapPanel() };
    const target = rows[0];
    dispatchClick(target.btn);
    const confirm = await waitFor(() => confirmDialog(), 5000);
    const confirmText = confirm ? textOf(confirm).replace(/\\s+/g, " ").slice(0, 200) : null;
    const yes = confirmButton();
    if (!yes) return { error: "没有二次确认按钮", confirmText };
    dispatchClick(yes);
    await sleep(1800);
    const after = snapRows();
    return {
      deletedRow: (target.text || "").replace(/\\s+/g, " "),
      confirmText,
      askedConfirm: Boolean(confirm),
      before: rows.length,
      after: after.length,
      remaining: after.map((r) => (r.text || "").replace(/\\s+/g, " ")),
    };
  `,

  /** 验收 5 回归：动态注入与多行 prompt 契约仍在（看 AI 面板状态行与 prompt 折叠块）。 */
  "regression": `
    await closeDialogs();
    const statusLines = all("div,p,span").map((d) => textOf(d))
      .filter((t) => t && (t.includes("环境注入") || t.includes("启动方式") || t.includes("未配置 API Key")));
    const promptToggle = all("button").map((b) => textOf(b)).find((t) => t && t.includes("prompt"));
    const chips = all("button").filter((b) => (b.getAttribute("aria-label") || "").includes("移除模型")).map((b) => b.getAttribute("aria-label"));
    return { statusLines: [...new Set(statusLines)].slice(0, 6), promptToggle: promptToggle || null, modelChipRemoveButtons: chips.length };
  `,
};

const body = STEPS[step];
if (!body) {
  console.error(`未知步骤：${step}\n可用：${Object.keys(STEPS).join(", ")}`);
  process.exit(4);
}

const expression = `(async () => { ${PRELUDE}\n${body}\n})()`;
const res = await send("Runtime.evaluate", {
  expression,
  awaitPromise: true,
  returnByValue: true,
});

const exception = res.result?.exceptionDetails;
if (exception) {
  console.error("JS 异常：", exception.text, exception.exception?.description ?? "");
  process.exit(3);
}
console.log(JSON.stringify(res.result?.result?.value ?? null, null, 2));
ws.close();
