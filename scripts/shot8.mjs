// 阶段 8 截图：计划确认卡 / 确认后的派发与上下文传递 / 重启后历史恢复。
// 同一个 CDP 连接里先摆好界面再截（分两次连接会丢掉中间态）。
//
// 用法：
//   node scripts/shot8.mjs plan    docs/phase8-plan.png     # 前置：已经点过「下发任务」，计划卡在等确认
//   node scripts/shot8.mjs flow    docs/phase8-flow.png     # 点「确认执行」→ 等到收尾 → 截底部
//   node scripts/shot8.mjs history docs/phase8-history.png  # 前置：重启应用 + 重新打开工作区
//
// 前置：应用以 dev 模式启动并带 --remote-debugging-port=9222，工作区已打开。
import { writeFileSync } from "node:fs";

const PORT = process.env.CDP_PORT || "9222";
const mode = process.argv[2] ?? "plan";
const out = process.argv[3] ?? `docs/phase8-${mode}.png`;

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
const pendingCalls = new Map();
ws.addEventListener("message", (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pendingCalls.has(msg.id)) {
    pendingCalls.get(msg.id)(msg);
    pendingCalls.delete(msg.id);
  }
});
const send = (method, params) => {
  const id = ++nextId;
  return new Promise((resolve) => {
    pendingCalls.set(id, resolve);
    ws.send(JSON.stringify({ id, method, params }));
  });
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) console.error(String(JSON.stringify(r.result.exceptionDetails)).slice(0, 300));
  return r.result?.result?.value ?? null;
};

// Radix 的按钮只认完整指针序列（纯 .click() 不生效）。
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
  const scrollerOf = () => {
    const chat = document.querySelector('[data-testid="master-chat"]');
    return chat && chat.closest(".overflow-y-auto");
  };
`;

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

if (mode === "intent11") {
  // 阶段 11：一句口语需求 → 计划卡上要能看见「我理解为…」+ 每步派给谁 + 清洗过的任务
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const dispatchClick = press;
    const waitFor = async (fn, ms = 20000) => {
      const t0 = Date.now();
      while (Date.now() - t0 < ms) {
        const v = fn();
        if (v) return v;
        await sleep(120);
      }
      throw new Error("waitFor 超时");
    };
    const setInput = (el, value) => {
      const proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const desc = Object.getOwnPropertyDescriptor(proto, "value");
      desc.set.call(el, value);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    };
    const clearBtn = document.querySelector('[data-testid="chat-clear"]');
    if (clearBtn) {
      dispatchClick(clearBtn);
      await sleep(700);
    }
    const area = document.querySelector('[data-testid="master-note"]');
    if (!area) return { ok: false, reason: "no-input" };
    setInput(area, "把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说");
    await sleep(150);
    dispatchClick(document.querySelector('[data-testid="master-send"]'));
    await waitFor(() => all('[data-testid="master-plan"]').length > 0, 40000);
    await sleep(700);
    const cards = all('[data-testid="master-plan"]');
    const card = cards[cards.length - 1];
    const scroller = scrollerOf();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    await sleep(300);
    card.scrollIntoView({ block: "center" });
    await sleep(300);
    const intentEl = card.querySelector('[data-testid="plan-intent"]');
    return {
      ok: true,
      intent: intentEl ? (textOf(intentEl) || "").trim() : null,
      steps: Array.from(card.querySelectorAll("ol li")).map((li) => (textOf(li) || "").trim()),
      pending: card.getAttribute("data-plan-pending"),
      hasConfirm: !!card.querySelector('[data-testid="plan-confirm"]'),
    };
  })()`);
  console.log("intent11:", JSON.stringify(info));
} else if (mode === "plan") {
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const cards = all('[data-testid="master-plan"]');
    if (!cards.length) return { ok: false, reason: "no-plan-card" };
    const card = cards[cards.length - 1];
    // 计划卡在对话流末尾，滚到底它就完整露出来
    const scroller = scrollerOf();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    await sleep(400);
    card.scrollIntoView({ block: "center" });
    await sleep(300);
    return {
      ok: true,
      pending: card.getAttribute("data-plan-pending"),
      hasConfirm: !!document.querySelector('[data-testid="plan-confirm"]'),
      hasEdit: !!document.querySelector('[data-testid="plan-edit"]'),
      dispatches: all('[data-testid="master-dispatch"]').length,
      text: textOf(card).slice(0, 120),
    };
  })()`);
  console.log("plan:", JSON.stringify(info));
} else if (mode === "flow") {
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const cards = all('[data-testid="master-plan"]');
    const card = cards[cards.length - 1];
    if (!card) return { ok: false, reason: "no-plan-card" };
    const confirm = card.querySelector('[data-testid="plan-confirm"]');
    if (confirm) {
      press(confirm);
      const deadline = Date.now() + 180000;
      while (Date.now() < deadline) {
        await sleep(400);
        const done = all('[data-testid="master-done"]').length;
        const busy = (textOf(document.querySelector('[data-testid="master-send"]')) || "").includes("派活");
        if (done > 0 && !busy) break;
      }
    }
    await sleep(900);
    const scroller = scrollerOf();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    await sleep(400);
    return {
      ok: true,
      dispatches: all('[data-testid="master-dispatch"]').length,
      doneCards: all('[data-testid="master-done"]').length,
      contextRows: all('[data-testid="master-system"]').map(textOf).filter((t) => t.includes("上下文")),
      tail: textOf(document.querySelector('[data-testid="master-chat"]')).slice(-140),
    };
  })()`);
  console.log("flow:", JSON.stringify(info));
} else if (mode === "context") {
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const rows = all('[data-testid="master-system"]').filter((el) => (textOf(el) || "").includes("上下文"));
    if (!rows.length) return { ok: false, reason: "no-context-row" };
    const row = rows[rows.length - 1];
    row.scrollIntoView({ block: "center" });
    await sleep(400);
    return {
      ok: true,
      rows: rows.length,
      text: textOf(row),
      dispatches: all('[data-testid="master-dispatch"]').length,
    };
  })()`);
  console.log("context:", JSON.stringify(info));
} else if (mode === "historytop") {
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const scroller = scrollerOf();
    if (scroller) scroller.scrollTop = 0;
    await sleep(500);
    return {
      ok: true,
      user: all('[data-testid="master-user"]').length,
      plans: all('[data-testid="master-plan"]').length,
      firstUser: textOf(all('[data-testid="master-user"]')[0]),
      planText: textOf(all('[data-testid="master-plan"]')[0]).slice(0, 120),
    };
  })()`);
  console.log("historytop:", JSON.stringify(info));
} else if (mode === "history") {
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const scroller = scrollerOf();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    await sleep(500);
    return {
      ok: !!document.querySelector('[data-testid="master-chat"]'),
      user: all('[data-testid="master-user"]').length,
      plans: all('[data-testid="master-plan"]').length,
      dispatches: all('[data-testid="master-dispatch"]').length,
      doneCards: all('[data-testid="master-done"]').length,
      session: textOf(document.querySelector('[data-testid="ai-panel"]')).slice(-40),
    };
  })()`);
  console.log("history:", JSON.stringify(info));
} else if (mode === "engine") {
  // 阶段 8.1：顶部引擎区只剩「Claude Code CLI」，没有下拉
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    await sleep(300);
    return {
      ok: !!document.querySelector('[data-testid="ai-panel"]'),
      badge: textOf(document.querySelector('[data-testid="ai-engine"]')),
      hasSelect: !!document.querySelector('select[data-testid="ai-engine"]'),
      bodyHasMock: document.body.innerText.includes("Mock"),
    };
  })()`);
  console.log("engine:", JSON.stringify(info));
} else if (mode === "check") {
  // 阶段 10：检查类任务派给 tracker 走只读，面板上是报告而不是写文件
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const setValue = (el, value) => {
      const setter = Object.getOwnPropertyDescriptor(el.constructor.prototype, "value").set;
      setter.call(el, value);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    };
    const clear = all("button").find((b) => b.getAttribute("data-testid") === "chat-clear");
    if (clear) { press(clear); await sleep(600); }
    const note = document.querySelector('[data-testid="master-note"]');
    if (!note) return { ok: false, reason: "no-note-input" };
    setValue(note, "检查伏笔与时间线");
    await sleep(300);
    press(document.querySelector('[data-testid="master-send"]'));
    for (let i = 0; i < 120 && !document.querySelector('[data-testid="plan-confirm"]'); i += 1) await sleep(250);
    const cards = all('[data-testid="master-plan"]');
    const card = cards[cards.length - 1];
    const planText = textOf(card);
    const confirm = document.querySelector('[data-testid="plan-confirm"]');
    if (!confirm) return { ok: false, reason: "no-confirm", planText };
    press(confirm);
    const deadline = Date.now() + 180000;
    while (Date.now() < deadline) {
      await sleep(400);
      const done = all('[data-testid="master-done"]').length;
      const busy = (textOf(document.querySelector('[data-testid="master-send"]')) || "").match(/停止|派活/);
      if (done > 0 && !busy) break;
    }
    await sleep(900);
    const scroller = scrollerOf();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    await sleep(500);
    return {
      ok: true,
      planText,
      dispatches: all('[data-testid="master-dispatch"]').length,
      doneCards: all('[data-testid="master-done"]').length,
      tail: textOf(document.querySelector('[data-testid="master-chat"]')).slice(-200),
    };
  })()`);
  console.log("check:", JSON.stringify(info));
} else if (mode === "prompt10") {
  // 阶段 10：子智能体 prompt 里的「全局设定」清单与「补充要求」硬约束
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const pres = all('[data-testid="ai-prompt"]');
    if (!pres.length) return { ok: false, reason: "no-prompt" };
    const pre = pres[pres.length - 1];
    const box = pre.closest("details");
    if (box) box.open = true;
    await sleep(300);
    pre.scrollIntoView({ block: "center" });
    await sleep(400);
    const text = pre.textContent || "";
    return {
      ok: true,
      lines: text.split("\\n").length,
      global: (text.match(/^\\s*- (设定|追踪|参考资料|大纲)\\//gm) || []).length,
      hasReadFirst: text.includes("动笔前，必须先阅读"),
      constraint: (text.match(/^补充要求：.*$/m) || [""])[0].slice(0, 120),
    };
  })()`);
  console.log("prompt10:", JSON.stringify(info));
} else if (mode === "tools") {
  // 阶段 9：设置 → 智能体角色 → 「允许的工具」勾选区
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const dialogs = () => all('[role="dialog"]');
    if (!dialogs().length) {
      const open = all('button').find((b) => textOf(b) === "设置");
      if (open) press(open);
      await sleep(700);
    }
    const dlg = () => dialogs().sort((a, b) => (a.textContent || "").length - (b.textContent || "").length)[0];
    const tab = [...(dlg() ? dlg().querySelectorAll('button,[role="tab"]') : [])].find((b) =>
      textOf(b).includes("智能体角色"),
    );
    if (tab) press(tab);
    for (let i = 0; i < 40 && !document.querySelector('[data-testid="tool-whitelist"]'); i += 1) await sleep(150);
    const panel = document.querySelector('[data-testid="tool-whitelist"]');
    if (panel) {
      let box = panel.parentElement;
      while (box && box.scrollHeight <= box.clientHeight) box = box.parentElement;
      if (box) box.scrollTop = 0;
      panel.scrollIntoView({ block: "center" });
    }
    await sleep(500);
    return {
      ok: !!panel,
      title: panel ? textOf(panel).split("  ")[0].slice(0, 20) : null,
      checked: panel ? [...panel.querySelectorAll('input[type="checkbox"]')].filter((el) => el.checked).map((el) => el.getAttribute("data-testid")) : [],
    };
  })()`);
  console.log("tools:", JSON.stringify(info));
} else if (mode === "planedit") {
  // 阶段 9：计划卡的编辑态（可改文本、可删步骤）
  const info = await evaluate(`(async () => {
    ${PRELUDE}
    const cards = all('[data-testid="master-plan"]');
    const card = cards[cards.length - 1];
    if (!card) return { ok: false, reason: "no-plan-card" };
    const edit = card.querySelector('[data-testid="plan-edit"]');
    if (edit) press(edit);
    for (let i = 0; i < 40 && !document.querySelector('[data-testid="plan-editor"]'); i += 1) await sleep(150);
    const scroller = scrollerOf();
    if (scroller) scroller.scrollTop = scroller.scrollHeight;
    await sleep(300);
    card.scrollIntoView({ block: "center" });
    await sleep(400);
    return {
      ok: !!document.querySelector('[data-testid="plan-editor"]'),
      editing: card.getAttribute("data-plan-editing"),
      steps: all('[data-testid^="plan-step-"]').filter((el) => el.tagName === "TEXTAREA").length,
      hasSave: !!document.querySelector('[data-testid="plan-save"]'),
    };
  })()`);
  console.log("planedit:", JSON.stringify(info));
} else {
  console.error(`未知模式：${mode}`);
  process.exit(2);
}

await sleep(400);
await capture(out);
ws.close();
