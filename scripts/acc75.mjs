// 阶段 7.5 验收驱动：主从智能体（Master-Worker）+ 角色定制。
// 用法：node scripts/acc75.mjs <step>
//   panel                  读右栏结构：是否只剩主 AI 聊天流、有没有输入框、下属几个
//   settingsRoles          打开设置 → 智能体角色，读角色列表
//   editPrompt:<text>      把 architect 的系统提示词改成 <text> 并保存（验收 1）
//   resetRoles             点「恢复默认 6 个角色」
//   addRole:<id>:<name>    新增一个自定义角色
//   send:<text>            对主 AI 下一句综合指令，等整轮跑完
//   trace                  读对话流里的事件序列（用户/思考/计划/派发/收尾）
//   regression             核对：没有 Tabs、master_agents 与列表一致、引擎下拉还在
// 前置：应用以 dev 模式启动并带 --remote-debugging-port=9222；
//       claude-shim 的 mode.txt 写 ok（子智能体会真的写盘）。

const PORT = process.env.CDP_PORT || "9222";
const arg = process.argv[2] || "panel";

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

const PRELUDE = `
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const all = (sel, root = document) => [...root.querySelectorAll(sel)];
const textOf = (el) => (el ? (el.textContent || "").trim() : null);
const byText = (text, sel = "button", root = document) =>
  all(sel, root).find((el) => (el.textContent || "").trim() === text) || null;
const waitFor = async (fn, ms = 20000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = fn();
    if (v) return v;
    await sleep(120);
  }
  return null;
};
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
/** 打开设置并切到「智能体角色」页 */
const openRoles = async () => {
  if (!document.querySelector('[data-testid="agent-roles"]')) {
    const btn = byText("设置") || all("button").find((b) => (textOf(b) || "").includes("设置"));
    if (!btn) throw new Error("找不到「设置」按钮");
    dispatchClick(btn);
    await sleep(260);
  }
  const tab = all("button").find((b) => textOf(b) === "智能体角色");
  if (!tab) throw new Error("设置里没有「智能体角色」");
  dispatchClick(tab);
  await sleep(220);
  return Boolean(document.querySelector('[data-testid="agent-roles"]'));
};
const closeDialog = async () => {
  const btn = all("button").find((b) => textOf(b) === "关闭");
  if (btn) dispatchClick(btn);
  await sleep(200);
};
const roleRows = () =>
  all('[data-agent-id]').map((el) => ({
    id: el.getAttribute("data-agent-id"),
    text: textOf(el),
    buttons: [...el.querySelectorAll("button")].map((b) => textOf(b)),
  }));
const chatEvents = () =>
  all('[data-testid="master-chat"] > *').map((el) => ({
    testid: el.getAttribute("data-testid") || "",
    text: (textOf(el) || "").slice(0, 240),
  }));
`;

const STEPS = {
  /** R4：真实规划链路里的结构化字段。
   *  master_plan 会走 llm_plan（模型）或 rules_plan（本地兜底），两条路都要经过
   *  normalize_step 的标准化层，所以即使模型不可用，返回的步骤也必须带字段。 */
  planFields: `
    const ws = localStorage.getItem("storydesk.lastWorkspace") || "D:\\\\桌面\\\\大制作\\\\xs\\\\我的作品集";
    const agents = await window.__TAURI_INTERNALS__.invoke("master_agents");
    const info = await window.__TAURI_INTERNALS__.invoke("open_project", { workspaceDir: ws });
    const plan = await window.__TAURI_INTERNALS__.invoke("master_plan", {
      req: {
        workspaceRoot: ws,
        bookRoot: info.bookRoot,
        request: "按细纲写第003章到第012章的正文，每章三千字",
        engine: "claude",
        agents,
      },
    });
    const steps = plan.steps.map((s) => ({
      agentId: s.agentId,
      task: (s.task || "").slice(0, 60),
      role: s.role ?? null,
      kind: s.kind ?? null,
      range: s.range ?? null,
      lock: s.lock ?? null,
      words: s.words ?? null,
      readOnly: s.readOnly ?? null,
    }));
    return {
      source: plan.source,
      count: steps.length,
      steps,
      allHaveKind: steps.every((s) => s.kind !== null),
      allHaveRole: steps.every((s) => s.role !== null),
      allHaveReadOnly: steps.every((s) => s.readOnly !== null),
    };
  `,

  /** R4：拆步 + plan-final 在真实界面上是否生效。
   *  直接喂一份「task 里没有章号、只有 range=span(3,12)」的计划——这正是修 bug 前
   *  会被当成一步执行十万字的输入。跑完看计划卡是不是原位变成 10 步。 */
  planFinal: `
    const ws = localStorage.getItem("storydesk.lastWorkspace") || "D:\\\\桌面\\\\大制作\\\\xs\\\\我的作品集";
    const agents = await window.__TAURI_INTERNALS__.invoke("master_agents");
    const info = await window.__TAURI_INTERNALS__.invoke("open_project", { workspaceDir: ws });
    const cardsBefore = all('[data-testid="master-plan"]').length;
    const steps = [{
      agentId: "writer",
      task: "按细纲写这三万字",
      role: "writer",
      kind: "prose",
      range: { type: "span", value: [3, 12] },
      words: [30000, 30000],
      readOnly: false,
    }];
    // 不 await 到底：没有 claude CLI 时逐步派发会立刻失败，但 plan-final 在派发之前就发了。
    const running = window.__TAURI_INTERNALS__.invoke("master_execute", {
      req: {
        workspaceRoot: ws,
        bookRoot: info.bookRoot,
        steps,
        engine: "claude",
        agents,
        request: "按细纲写这三万字",
        chapter: null,
        runId: "acc75-planfinal-" + Date.now(),
        source: "acc75",
      },
    }).catch((e) => String(e));
    const expandedText = await waitFor(() => {
      const el = all('[data-testid="plan-expanded"]').pop();
      return el ? textOf(el) : null;
    }, 20000);
    await sleep(400);
    const card = all('[data-testid="master-plan"]').pop();
    const result = await running;
    const cardSteps = card
      ? [...card.querySelectorAll("ol > li")].map((li) => textOf(li))
      : [];
    return {
      cardsBefore,
      cardsAfter: all('[data-testid="master-plan"]').length,
      expandedText,
      expandedCount: expandedText ? Number((expandedText.match(/(\\d+)/) || [])[1]) : null,
      cardStepCount: cardSteps.length,
      firstSteps: cardSteps.slice(0, 3),
      lastStep: cardSteps[cardSteps.length - 1] || null,
      hasChapter3: cardSteps.some((t) => /第\\s*0*3\\s*章/.test(t || "")),
      hasChapter12: cardSteps.some((t) => /第\\s*0*12\\s*章/.test(t || "")),
      engineResult: typeof result === "string" ? result.slice(0, 200) : "ok",
    };
  `,

  /** 回到工作台：HMR 整页刷新后应用会落在欢迎页，这里用上次的工作区直接重开。 */
  open: `
    if (document.querySelector('[data-testid="master-note"]')) return { already: true };
    const ws = localStorage.getItem("storydesk.lastWorkspace");
    if (!ws) throw new Error("localStorage 里没有 storydesk.lastWorkspace");
    const info = await window.__TAURI_INTERNALS__.invoke("open_project", { workspaceDir: ws });
    await sleep(1600);
    return {
      ws,
      bookRoot: info && info.bookRoot,
      hasInput: Boolean(document.querySelector('[data-testid="master-note"]')),
      hasChat: Boolean(document.querySelector('[data-testid="master-chat"]')),
    };
  `,

  panel: `
    const tabs = all('[role="tab"]').map((t) => textOf(t));
    return {
      hasChat: Boolean(document.querySelector('[data-testid="master-chat"]')) ||
                Boolean(document.body.innerText.includes("对主 AI 总监说一句话")),
      hasInput: Boolean(document.querySelector('[data-testid="master-note"]')),
      hasSend: Boolean(document.querySelector('[data-testid="master-send"]')),
      tabs,
      title: (textOf(document.querySelector('[data-testid="ai-panel"]')) || "").slice(0, 60),
      suggestions: all('[data-testid="master-suggestion"]').map((b) => textOf(b)),
      agentBadge: all("span").map((s) => textOf(s)).filter((t) => t && t.startsWith("下属 ")),
    };
  `,

  settingsRoles: `
    await openRoles();
    const panel = document.querySelector('[data-testid="agent-roles"]');
    const rows = roleRows();
    await closeDialog();
    return {
      rows,
      count: rows.length,
      header: (textOf(panel) || "").slice(0, 40),
      hasReset: Boolean(all("button").find((b) => (textOf(b) || "").includes("恢复默认"))),
    };
  `,

  resetRoles: `
    await openRoles();
    const btn = all("button").find((b) => (textOf(b) || "").includes("恢复默认"));
    dispatchClick(btn);
    await sleep(900);
    const rows = roleRows();
    await closeDialog();
    return { clicked: Boolean(btn), rows: rows.map((r) => r.id), count: rows.length };
  `,

  addRole: `
    const [, id, name] = ${JSON.stringify(arg)}.split(":");
    await openRoles();
    dispatchClick(document.querySelector('[data-testid="agent-new"]'));
    await sleep(220);
    return { opened: Boolean(document.querySelector('[data-testid="agent-name"]')), id, name };
  `,

  trace: `
    const events = chatEvents();
    return {
      events,
      kinds: events.map((e) => e.testid || "user"),
      hasDispatch: events.some((e) => e.testid === "master-dispatch"),
      hasDone: events.some((e) => e.testid === "master-done"),
      text: (textOf(document.querySelector('[data-testid="master-chat"]')) || "").slice(0, 1200),
    };
  `,

  regression: `
    const agents = await window.__TAURI_INTERNALS__.invoke("master_agents");
    const defaults = await window.__TAURI_INTERNALS__.invoke("master_default_agents");
    const engines = await window.__TAURI_INTERNALS__.invoke("ai_engines");
    const engineBadge = document.querySelector('[data-testid="ai-engine"]');
    return {
      noTabs: all('[role="tab"]').length === 0,
      noTeamCards: all('[data-agent-role]').length === 0,
      hasInput: Boolean(document.querySelector('[data-testid="master-note"]')),
      agents: agents.map((a) => ({ id: a.id, name: a.name, dir: a.targetDir, promptHead: (a.systemPrompt || "").slice(0, 30) })),
      defaultCount: defaults.length,
      // 引擎已收成唯一一种：列表只有 claude，UI 只显示只读文本
      engineOptions: engines.map((e) => e.kind),
      engineText: engineBadge ? textOf(engineBadge) : null,
    };
  `,
};

function bodyFor(step) {
  if (STEPS[step]) return STEPS[step];

  if (step.startsWith("editPrompt:")) {
    const text = step.slice("editPrompt:".length);
    return `
      await openRoles();
      const target = ${JSON.stringify(text)};
      const btn = document.querySelector('[data-testid="agent-edit-architect"]');
      dispatchClick(btn);
      await sleep(220);
      const area = document.querySelector('[data-testid="agent-prompt"]');
      const before = area ? area.value : null;
      setInput(area, target);
      await sleep(120);
      dispatchClick(document.querySelector('[data-testid="agent-save"]'));
      await sleep(900);
      await closeDialog();
      const saved = await window.__TAURI_INTERNALS__.invoke("master_agents");
      const arch = saved.find((a) => a.id === "architect");
      return {
        before,
        after: arch ? arch.systemPrompt : null,
        matched: arch ? arch.systemPrompt === target : false,
      };
    `;
  }

  if (step.startsWith("send:")) {
    const text = step.slice("send:".length);
    return `
      const before = chatEvents().length;
      if (!document.querySelector('[data-testid="master-note"]')) {
        throw new Error("右栏没有主 AI 输入框");
      }
      setInput(document.querySelector('[data-testid="master-note"]'), ${JSON.stringify(text)});
      await sleep(150);
      dispatchClick(document.querySelector('[data-testid="master-send"]'));
      const done = await waitFor(
        () => chatEvents().some((e) => e.testid === "master-done"),
        40000,
      );
      await sleep(400);
      const events = chatEvents();
      return {
        text: ${JSON.stringify(text)},
        newEvents: events.length - before,
        kinds: events.map((e) => e.testid || "user"),
        done: Boolean(done),
        dispatchCount: events.filter((e) => e.testid === "master-dispatch").length,
        tail: (textOf(document.querySelector('[data-testid="master-chat"]')) || "").slice(-600),
      };
    `;
  }

  if (step.startsWith("plan:")) {
    const text = step.slice("plan:".length);
    return `
      const ws = localStorage.getItem("storydesk.lastWorkspace");
      const agents = await window.__TAURI_INTERNALS__.invoke("master_agents");
      const info = await window.__TAURI_INTERNALS__.invoke("open_project", { workspaceDir: ws });
      const plan = await window.__TAURI_INTERNALS__.invoke("master_plan", {
        req: { workspaceRoot: ws, bookRoot: info.bookRoot, request: ${JSON.stringify(text)}, engine: "claude", agents },
      });
      return { source: plan.source, steps: plan.steps, raw: (plan.raw || "").slice(0, 900) };
    `;
  }

  if (step.startsWith("remember:")) {
    const dir = step.slice("remember:".length);
    return `
      localStorage.setItem("storydesk.lastWorkspace", ${JSON.stringify(dir)});
      location.reload();
      await sleep(300);
      return { remembered: ${JSON.stringify(dir)}, origin: location.origin };
    `;
  }

  if (step.startsWith("open:")) {
    const dir = step.slice("open:".length);
    return `
      const info = await window.__TAURI_INTERNALS__.invoke("open_project", { workspaceDir: ${JSON.stringify(dir)} });
      await sleep(1600);
      return {
        ws: ${JSON.stringify(dir)},
        bookRoot: info && info.bookRoot,
        hasInput: Boolean(document.querySelector('[data-testid="master-note"]')),
        hasChat: Boolean(document.querySelector('[data-testid="master-chat"]')),
      };
    `;
  }

  if (step.startsWith("engine:")) {
    const kind = step.slice("engine:".length);
    return `
      // 引擎已固定为 claude，界面上没有下拉可点；这一步退化成「确认当前引擎」
      const sel = document.querySelector('select[data-testid="ai-engine"]');
      if (sel) {
        Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value").set.call(sel, ${JSON.stringify(kind)});
        sel.dispatchEvent(new Event("change", { bubbles: true }));
        await sleep(250);
        return { engine: sel.value, options: [...sel.options].map((o) => o.value) };
      }
      const engines = await window.__TAURI_INTERNALS__.invoke("ai_engines");
      return {
        engine: engines[0] ? engines[0].kind : null,
        options: engines.map((e) => e.kind),
        fixed: true,
        note: "引擎已固定为 claude，没有下拉可切（请求的是 ${kind}）",
      };
    `;
  }

  if (step.startsWith("wait:")) {
    const ms = Number(step.slice("wait:".length)) || 3000;
    return `await sleep(${ms}); return { waited: ${ms}, done: true };`;
  }

  throw new Error("未知步骤：" + step);
}

const expression = `(async () => { ${PRELUDE}\n${bodyFor(arg)} })()`;
const res = await send("Runtime.evaluate", {
  expression,
  awaitPromise: true,
  returnByValue: true,
  userGesture: true,
});

ws.close();

if (res.result?.exceptionDetails) {
  console.error("页面内异常：", JSON.stringify(res.result.exceptionDetails, null, 2));
  process.exit(3);
}
console.log(JSON.stringify(res.result?.result?.value ?? null, null, 2));
