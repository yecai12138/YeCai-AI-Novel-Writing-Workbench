// 阶段 7.5 截图：主 AI 总监聊天流 / 智能体角色设置页。
// 同一个 CDP 连接里先摆好界面再截（分两次连接会丢掉中间态）。
//
// 用法：
//   node scripts/shot75.mjs master docs/phase7.5-master.png
//   node scripts/shot75.mjs roles  docs/phase7.5-roles.png
//
// 前置：应用以 dev 模式启动并带 --remote-debugging-port=9222，工作区已打开。
import { writeFileSync } from "node:fs";

const PORT = process.env.CDP_PORT || "9222";
const mode = process.argv[2] ?? "master";
const out = process.argv[3] ?? `docs/phase7.5-${mode}.png`;

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
  if (r.result?.exceptionDetails) console.error(String(JSON.stringify(r.result.exceptionDetails)).slice(0, 300));
  return r.result?.result?.value ?? null;
};

// Radix 的 Tabs / Select 只认完整指针序列（纯 .click() 不生效）。
const PRELUDE = `
  const all = (sel) => [...document.querySelectorAll(sel)];
  const textOf = (el) => (el && el.textContent ? el.textContent.replace(/\\s+/g, " ").trim() : "");
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const press = (el) => {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    const base = { bubbles: true, cancelable: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2, button: 0 };
    el.dispatchEvent(new PointerEvent("pointerdown", { ...base, buttons: 1, pointerId: 1, pointerType: "mouse", isPrimary: true }));
    el.dispatchEvent(new MouseEvent("mousedown", { ...base, buttons: 1 }));
    el.dispatchEvent(new PointerEvent("pointerup", { ...base, buttons: 0, pointerId: 1, pointerType: "mouse", isPrimary: true }));
    el.dispatchEvent(new MouseEvent("mouseup", { ...base, buttons: 0 }));
    el.dispatchEvent(new MouseEvent("click", { ...base, buttons: 0 }));
    return true;
  };
  const setInput = (el, value) => {
    if (!el) return false;
    const proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, "value").set.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  };
  const byText = (sel, needle) => all(sel).find((el) => (textOf(el) || "").includes(needle)) ?? null;
`;

if (mode === "master") {
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    if (!document.querySelector('[data-testid="master-chat"]')) return { ok: false, reason: "no-chat" };

    // 先把上一轮的对话清掉，截图里只留这一次的拆解与派发
    const clear = all("button").find((b) => (textOf(b) || "").includes("清空对话"));
    if (clear) {
      press(clear);
      await sleep(500);
    }

    // 引擎切成 Claude Code CLI：Mock 引擎只演流式、不落盘，截图里看不出真干活
    const trigger = document.querySelector('[data-testid="ai-engine"]');
    if (trigger && !(textOf(trigger) || "").includes("Claude")) {
      press(trigger);
      await sleep(400);
      press(byText('[role="option"]', "Claude"));
      await sleep(600);
    }

    setInput(document.querySelector('[data-testid="master-note"]'), "帮我写第一章正文");
    await sleep(150);
    press(document.querySelector('[data-testid="master-send"]'));

    const deadline = Date.now() + 120000;
    let doneCount = 0;
    while (Date.now() < deadline) {
      await sleep(400);
      doneCount = all('[data-testid="master-done"]').length;
      const busy = (textOf(document.querySelector('[data-testid="master-send"]')) || "").includes("派活");
      if (doneCount > 0 && !busy) break;
    }
    await sleep(900);

    // 滚到对话流底部，让最后一张派发卡和收尾卡露出来
    const chat = document.querySelector('[data-testid="master-chat"]');
    const scroller = chat && chat.closest(".overflow-y-auto");
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    await sleep(400);

    return {
      ok: true,
      engine: textOf(trigger),
      dispatches: all('[data-testid="master-dispatch"]').length,
      doneCards: all('[data-testid="master-done"]').length,
      tail: textOf(document.querySelector('[data-testid="master-chat"]')).slice(-160),
    };
  })()`);
  console.log("master:", JSON.stringify(info));
} else if (mode === "roles") {
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const gear = all("button").find((b) => (textOf(b) || "").includes("设置"));
    press(gear);
    await sleep(800);
    press(byText('[role="tab"], button', "智能体角色"));
    await sleep(700);
    const rows = all("[data-agent-id]").length;
    if (rows < 1) return { ok: false, reason: "no-rows" };
    // 选中架构师，让右侧表单里有真内容
    press(document.querySelector('[data-agent-id="architect"] button'));
    await sleep(600);
    return { ok: true, rows, head: textOf(all('[role="dialog"]')[0]).slice(0, 60) };
  })()`);
  console.log("roles:", JSON.stringify(info));
} else {
  console.error(`未知模式：${mode}`);
  process.exit(2);
}

await sleep(400);

const capture = async (path) => {
  const shot = await send("Page.captureScreenshot", { format: "png" });
  const data = shot.result?.data;
  if (!data) {
    console.error("截图失败");
    process.exit(5);
  }
  writeFileSync(path, Buffer.from(data, "base64"));
  console.log(`saved ${path}`);
};

if (mode === "master") {
  // 上半段：用户指令 → 主 AI 思考 → 拆解出的计划 → 第一张派发卡
  await evaluate(`(() => {
    const chat = document.querySelector('[data-testid="master-chat"]');
    const scroller = chat && chat.closest(".overflow-y-auto");
    if (scroller) scroller.scrollTop = 0;
    return true;
  })()`);
  await sleep(400);
  await capture(out.replace(/\.png$/, "-plan.png"));
  // 下半段：第二张派发卡 + 子智能体流式日志 + 收尾
  await evaluate(`(() => {
    const chat = document.querySelector('[data-testid="master-chat"]');
    const scroller = chat && chat.closest(".overflow-y-auto");
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    return true;
  })()`);
  await sleep(400);
}

await capture(out);
ws.close();
