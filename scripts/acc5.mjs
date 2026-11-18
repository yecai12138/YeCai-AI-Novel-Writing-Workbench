// 阶段 5 验收驱动：通过 WebView2 的 CDP 端口在应用页面里跑脚本。
// 用法：node scripts/acc5.mjs <step>
// 每个 step 对应一段页面内的 async 函数，执行完打印 JSON 结果。

const PORT = process.env.CDP_PORT || "9222";
const step = process.argv[2] || "brand";

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

// 页面内辅助函数：受控输入、按文字找元素、等待条件。
const PRELUDE = `
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const setInput = (el, value) => {
  if (!el) throw new Error("setInput: 元素不存在");
  if (!(el instanceof HTMLInputElement) && !(el instanceof HTMLTextAreaElement))
    throw new Error("setInput: 不是输入框（" + el.tagName + "）");
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value").set.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
};
/** Radix 的 Tab/Select 触发器响应 mousedown，单纯 click() 不切页。 */
const press = (el) => {
  if (!el) return false;
  el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }));
  el.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
  el.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true }));
  el.click();
  return true;
};
const all = (sel, root = document) => [...root.querySelectorAll(sel)];
const byText = (text, sel = "button", root = document) =>
  all(sel, root).find((el) => (el.textContent || "").trim() === text) || null;
const hasText = (text, sel = "button", root = document) =>
  all(sel, root).some((el) => (el.textContent || "").trim() === text);
const waitFor = async (fn, ms = 6000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = fn();
    if (v) return v;
    await sleep(80);
  }
  return null;
};
const dialog = () => document.querySelector('[role="dialog"]');
/** 模型选择弹窗与设置弹窗同时存在（选择弹窗嵌在设置弹窗里），按内容取内层那个。 */
const pickerDialog = () =>
  all('[role="dialog"]')
    .filter((d) => (d.textContent || "").includes("确认添加"))
    .sort((a, b) => (a.textContent || "").length - (b.textContent || "").length)[0] || null;
const textOf = (el) => (el ? (el.textContent || "").trim() : null);
const openAiTab = async () => {
  const settingsBtn = byText("设置");
  if (settingsBtn) settingsBtn.click();
  await waitFor(() => dialog());
  byText("AI 引擎")?.click();
  await sleep(400);
};
const innerTab = async (label) => {
  const tab = all('[role="tab"]').find((t) => textOf(t) === label);
  press(tab);
  await sleep(350);
  return Boolean(tab);
};
/** 阶段 6 起统一用这个名字：Radix 组件要 pointerdown→mousedown→mouseup→click 四连。 */
const dispatchClick = press;
/** 「全部提供商」表格的每一行（阶段 6 把列表改成了 table）。 */
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
/** 找到某行的按钮（按文字）。 */
const rowButton = (row, label) =>
  row ? [...row.el.querySelectorAll("button")].find((b) => textOf(b) === label) || null : null;
/** 二次确认弹窗里的确认按钮（删除类）。 */
const confirmDanger = () => {
  const d = all('[role="dialog"]').find((x) => (x.textContent || "").includes("不可恢复") || (x.textContent || "").includes("永久清除"));
  if (!d) return null;
  return [...d.querySelectorAll("button")].find((b) => textOf(b) === "删除") ||
    [...d.querySelectorAll("button")].find((b) => textOf(b) === "确定") || null;
};
`;

const STEPS = {
  /** 页面结构探针。 */
  dump: `
    const settingsBtn = byText("设置");
    if (settingsBtn) settingsBtn.click();
    await waitFor(() => dialog());
    await sleep(300);
    return {
      title: document.title,
      h1: textOf(document.querySelector("h1")),
      buttons: all("button").map((b) => textOf(b)).filter(Boolean),
      tabs: all('[role="tab"]').map((t) => textOf(t)),
      dialogHead: (dialog()?.textContent || "").slice(0, 200),
    };
  `,

  /** 验收 1 + 2：品牌与两个 Tab 是否存在。 */
  brand: `
    const out = { title: document.title, welcome: textOf(document.querySelector("h1")) };
    const settingsBtn = byText("设置");
    if (settingsBtn) settingsBtn.click();
    await waitFor(() => dialog());
    const tabs = all('[role="tab"]').map((t) => textOf(t));
    out.dialogTabs = tabs;
    out.hasPresetTab = tabs.includes("第三方模型提供商");
    out.hasCustomTab = tabs.includes("自定义模型 API");
    const aboutBtn = byText("关于") || byText("关于本应用");
    if (aboutBtn) aboutBtn.click();
    await sleep(400);
    out.aboutBrand = (dialog()?.textContent || "").includes("野菜 · AI 小说工作台");
    out.aboutVersion = [...(dialog()?.querySelectorAll("div") || [])].map((d) => textOf(d)).find((t) => t && t.startsWith("v0.1.0")) || null;
    return out;
  `,

  /** 验收 3 前半：自定义 Tab 填表并真实调 /models。 */
  "custom-fetch": `
    await openAiTab();
    const switched = await innerTab("自定义模型 API");
    const ids = ["custom-id", "custom-name", "custom-base", "custom-protocol", "custom-key"]
      .map((id) => [id, Boolean(document.getElementById(id))]);
    if (!switched || !document.getElementById("custom-id")) {
      return { switched, ids, error: "自定义 Tab 没切过去或表单没挂载" };
    }

    setInput(document.getElementById("custom-id"), "mock-gateway");
    setInput(document.getElementById("custom-name"), "本地 Mock 网关");
    setInput(document.getElementById("custom-base"), "http://127.0.0.1:8791/v1");
    setInput(document.getElementById("custom-key"), "sk-mock-acc5-key");
    await sleep(250);

    const fetchBtn = byText("获取可用模型");
    const disabled = fetchBtn ? fetchBtn.disabled : null;
    if (fetchBtn) fetchBtn.click();
    const picker = await waitFor(() => pickerDialog(), 20000);
    return {
      switched,
      fetchDisabled: disabled,
      pickerOpened: Boolean(picker),
      options: picker ? all('input[type="checkbox"]', picker).map((c) => c.disabled) : [],
      labels: picker ? all("label", picker).map((l) => textOf(l)) : [],
      pickerText: (picker?.textContent || "").slice(0, 240),
    };
  `,

  /** 验收 3 全流程：填表 → 拉列表 → 勾选 → 创建（必须一口气做完，弹窗中间会关）。 */
  "custom-all": `
    await openAiTab();
    const switched = await innerTab("自定义模型 API");
    if (!switched || !document.getElementById("custom-id")) return { error: "自定义 Tab 没挂载", switched };

    setInput(document.getElementById("custom-id"), "mock-gateway");
    setInput(document.getElementById("custom-name"), "本地 Mock 网关");
    setInput(document.getElementById("custom-base"), "http://127.0.0.1:8791/v1");
    setInput(document.getElementById("custom-key"), "sk-mock-acc5-key");
    await sleep(250);

    byText("获取可用模型")?.click();
    const picker = await waitFor(() => pickerDialog(), 20000);
    if (!picker) return { error: "模型选择弹窗没打开" };
    const items = [...picker.querySelectorAll("button")]
      .map((b) => ({ b, t: textOf(b) }))
      .filter((x) => x.t && x.t.startsWith("mock-"));
    const selectable = items.filter((x) => !x.t.includes("已在目录"));
    selectable.slice(0, 2).forEach((x) => x.b.click());
    await sleep(250);
    [...picker.querySelectorAll("button")].find((b) => textOf(b) === "确认添加")?.click();
    await sleep(800);

    const chips = all('[role="dialog"] span')
      .map((s) => textOf(s))
      .filter((t) => t && t.startsWith("mock-"));

    byText("创建提供商")?.click();
    await sleep(2200);

    const created = [...document.querySelectorAll('[role="dialog"] div')]
      .map((d) => textOf(d))
      .find((t) => t && t.startsWith("已创建提供商")) || null;
    const rows = providerRows().map((r) => r.text);
    return { items: items.map((x) => x.t), picked: selectable.slice(0, 2).map((x) => x.t), chips, created, rows };
  `,

  /** 验收 3 辅助：把 mock-gateway 设为当前提供商。 */
  "set-active": `
    const row = providerRows().find((r) => (r.id || "") === "mock-gateway" || (r.text || "").includes("mock-gateway"));
    const btn = rowButton(row, "设为当前");
    if (btn) dispatchClick(btn);
    await sleep(1200);
    const rows = providerRows().map((r) => r.text);
    return { clicked: Boolean(btn), rows };
  `,

  /** 打开验收项目：走自绘文件夹选择器。 */
  "open-project": `
    if (!byText("选择此文件夹")) byText("打开项目")?.click();
    const pickerReady = await waitFor(() => byText("选择此文件夹"), 8000);
    if (!pickerReady) return { error: "文件夹选择器没打开" };
    const goto = [...document.querySelectorAll("input")].find(
      (i) => (i.placeholder || "").includes("/") || (i.placeholder || "").includes("路径"),
    );
    if (!goto) return { error: "找不到「转到」输入框", inputs: [...document.querySelectorAll("input")].map((i) => i.placeholder) };
    setInput(goto, "D:/桌面/大制作/小说/验收示例");
    await sleep(200);
    byText("转到")?.click();
    await sleep(1200);
    const btn = byText("选择此文件夹");
    if (!btn) return { error: "进目录后没有「选择此文件夹」" };
    if (btn.disabled) return { error: "「选择此文件夹」是禁用的", hint: document.body.innerText.slice(0, 200) };
    btn.click();
    const opened = await waitFor(() => document.querySelectorAll("select").length > 0, 12000);
    return {
      opened: Boolean(opened),
      buttons: [...document.querySelectorAll("button")].map((b) => textOf(b)).filter(Boolean).slice(0, 12),
      selects: document.querySelectorAll("select").length,
    };
  `,

  /** 把 AI 面板的引擎切到 Claude Code CLI。 */
  "engine-claude": `
    const sel = document.querySelector("select");
    if (!sel) return { error: "找不到引擎下拉" };
    const opt = [...sel.options].find((o) => o.textContent.includes("Claude"));
    if (!opt) return { error: "下拉里没有 Claude 选项", options: [...sel.options].map((o) => o.textContent) };
    sel.value = opt.value;
    sel.dispatchEvent(new Event("change", { bubbles: true }));
    await sleep(500);
    return { value: sel.value, options: [...sel.options].map((o) => o.textContent) };
  `,

  /** 验收 5：跑一次正文，等后置体检卡片出现。 */
  "run-prose": `
    const btn = byText("写本章正文");
    if (!btn) return { error: "找不到「写本章正文」" };
    btn.click();
    const card = await waitFor(() => {
      const el = document.querySelector('[role="alert"]');
      return el && (el.textContent || "").includes("体检") ? el : null;
    }, 60000);
    const alerts = all('[role="alert"]').map((a) => textOf(a).replace(/\\s+/g, " ").slice(0, 160));
    const badge = all("span").map((s) => textOf(s)).filter((t) => t && (t === "已完成" || t === "被拦截" || t === "已取消" || t === "生成中"));
    const chunks = all("pre, p, div")
      .map((d) => textOf(d))
      .filter((t) => t && (t.includes("环境注入") || t.includes("启动方式") || t.includes("工具") || t.includes("读取文件")))
      .slice(-6);
    const postCard = card ? textOf(card).replace(/\\s+/g, " ").slice(0, 400) : null;
    return { status: badge, alerts, postCard, chunks };
  `,

  /** 前置门禁仍生效（回归）：第 1 章缺细纲时点写正文应出现阻断卡片。 */
  "block-check": `
    const before = all('[role="alert"]').length;
    byText("写本章正文")?.click();
    await sleep(3000);
    const alerts = all('[role="alert"]').map((a) => textOf(a).replace(/\\s+/g, " ").slice(0, 200));
    const buttons = all('button').map((b) => textOf(b)).filter((t) => t && t.includes("一键生成"));
    return { before, alerts, buttons };
  `,
  /** 回归：第 2 章没有细纲 → 前置门禁拦截卡片。 */
  "chapter2-block": `
    const chapInput = all('input')[0];
    setInput(chapInput, "2");
    await sleep(250);
    byText("写本章正文")?.click();
    await sleep(2800);
    return {
      chapterValue: chapInput ? chapInput.value : null,
      alerts: all('[role="alert"]').map((a) => textOf(a).replace(/\\s+/g, " ").slice(0, 280)),
    };
  `,
  /** 回归：运行中点「停止」→ 状态应为「已取消」。 */
  cancel: `
    byText("生成细纲")?.click();
    await sleep(900);
    const stop = all('button').find((b) => textOf(b) === "停止");
    if (!stop) return { error: "没找到「停止」按钮", buttons: all('button').map(textOf).filter(Boolean) };
    stop.click();
    await sleep(1500);
    const texts = all('span,div').map(textOf);
    return {
      cancelled: texts.filter((t) => t === "已取消"),
      running: texts.filter((t) => t === "运行中"),
      done: texts.filter((t) => t === "已完成"),
    };
  `,
  /** 重启后验收：提供商是否还在。 */
  persisted: `
    const settingsBtn = byText("设置");
    if (settingsBtn) settingsBtn.click();
    await waitFor(() => dialog());
    byText("AI 引擎")?.click();
    await sleep(700);
    const rows = providerRows().map((r) => r.text);
    const current = providerRows().filter((r) => (r.actions || []).length === 0 || (r.text || "").includes("当前")).map((r) => r.id);
    const table = document.querySelector('[role="dialog"] table');
    const gateBlock = table ? textOf(table) : null;
    return { rows, current, engineBlock: gateBlock };
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
