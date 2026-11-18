// 阶段 8 验收驱动：人机协同（HITL）、会话持久化、Tools 约束、上下文传递。
// 用法：node scripts/acc8.mjs <step>
//   panel8                 读右栏：历史/清空按钮、会话 id、聊天流是否在
//   planOnly:<text>        只用 UI 发一句，等计划卡；断言「没确认 → 没有派发」
//   confirm                点计划卡上的「✅ 确认执行」，等整轮跑完
//   edit                   点「✏️ 修改需求」，读回填到输入框的草稿
//   trace8                 读对话流里的事件序列（含 [系统] 上下文提示）
//   session                直接读后端：最近会话 id + 落盘消息条数 + 首尾消息
//   reload8                整页刷新（模拟重启），随后应能从磁盘恢复历史
//   clear8                 点「清空当前对话」
//   regression8            引擎清单（只有 claude）、角色名单、master_execute 空步骤守卫
// 前置：应用以 dev 模式启动并带 --remote-debugging-port=9222；
//       claude-shim 的 mode.txt 写 ok（子智能体会真的写盘）。

const PORT = process.env.CDP_PORT || "9222";
const arg = process.argv[2] || "panel8";

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
const waitFor = async (fn, ms = 45000) => {
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
const chatEvents = () =>
  all('[data-testid="master-chat"] > *').map((el) => ({
    testid: el.getAttribute("data-testid") || "user",
    text: (textOf(el) || "").slice(0, 260),
  }));
const ws0 = () => localStorage.getItem("storydesk.lastWorkspace");
`;

const OPEN_TOOLS = `
  const dialogs = () => [...document.querySelectorAll('[role="dialog"]')];
  if (dialogs().length === 0) {
    const openBtn = byText("设置");
    if (openBtn) dispatchClick(openBtn);
    await waitFor(() => dialogs().length > 0, 8000);
    await sleep(400);
  }
  const dlg0 = () => dialogs().sort((a, b) => (a.textContent || "").length - (b.textContent || "").length)[0];
  const tabEl = [...(dlg0() ? dlg0().querySelectorAll('button,[role="tab"]') : [])].find((b) =>
    (b.textContent || "").includes("智能体角色"),
  );
  if (tabEl) dispatchClick(tabEl);
  await waitFor(() => document.querySelector('[data-testid="tool-whitelist"]'), 8000);
  await sleep(400);
`;

const STEPS = {
  // 阶段 11：清空对话（点「清空当前对话」）——probe 之前用，免得旧计划卡混进断言
  clearChatUI: `
    await waitFor(() => document.querySelector('[data-testid="chat-clear"]'), 12000);
    dispatchClick(document.querySelector('[data-testid="chat-clear"]'));
    await sleep(700);
    return { events: chatEvents().length };
  `,
  // 阶段 8.1：引擎只剩 claude —— 打开设置里的 AI 引擎页，确认界面上没有「Mock 引擎（内置）」
  ui8: `
    const dialogs = () => [...document.querySelectorAll('[role="dialog"]')];
    const top = dialogs().sort((a, b) => (a.textContent || "").length - (b.textContent || "").length)[0];
    const openBtn = byText("设置");
    if (openBtn) dispatchClick(openBtn);
    await waitFor(() => dialogs().length > 0);
    await sleep(300);
    const dlg0 = dialogs().sort((a, b) => (a.textContent || "").length - (b.textContent || "").length)[0];
    const nav =
      [...(dlg0 ? dlg0.querySelectorAll("button,[role=tab]") : [])].find((b) =>
        (b.textContent || "").includes("AI 引擎"),
      ) || null;
    if (nav) dispatchClick(nav);
    await sleep(500);
    const body = document.body.innerText;
    const badge = document.querySelector('[data-testid="ai-engine"]');
    const at = body.indexOf("引擎");
    return {
      engineBadge: badge ? textOf(badge) : null,
      settingsSlice: at >= 0 ? body.slice(at, at + 220) : null,
      hasMockWord: body.includes("Mock"),
      hasMockOption: body.includes("Mock 引擎"),
      topLevelDialogs: dialogs().length,
    };
  `,
  // 阶段 9：「允许的工具」勾选区（设置 → 智能体角色）
  tools: `
    ${OPEN_TOOLS}
    const panel = document.querySelector('[data-testid="tool-whitelist"]');
    const boxes = [...(panel ? panel.querySelectorAll('input[type="checkbox"]') : [])].map((el) => ({
      tool: (el.getAttribute("data-testid") || "").replace("tool-", ""),
      checked: el.checked,
    }));
    const settings = await window.__TAURI_INTERNALS__.invoke("get_settings");
    return {
      hasPanel: Boolean(panel),
      title: panel ? textOf(panel).split("\\n")[0] : null,
      tools: boxes,
      allowedInSettings: settings.settings.allowedTools,
      panelText: panel ? textOf(panel).slice(0, 320) : null,
    };
  `,

  // 阶段 9：计划卡「编辑计划」——改第一步的任务文本后保存并执行
  planEdit: `
    const cards = all('[data-testid="master-plan"]');
    const last = cards[cards.length - 1];
    const editBtn = last ? last.querySelector('[data-testid="plan-edit"]') : null;
    if (!editBtn) throw new Error("最后一张计划卡上没有「编辑计划」按钮");
    const before = chatEvents().length;
    dispatchClick(editBtn);
    const editor = await waitFor(() => document.querySelector('[data-testid="plan-editor"]'), 8000);
    if (!editor) throw new Error("点了编辑但没有出现编辑器");
    const areas = all('[data-testid^="plan-step-"]');
    const original = areas.map((el) => el.value);
    const delBtn = last.querySelector('[data-testid="plan-step-del-1"]');
    let deleted = false;
    if (areas.length > 1 && delBtn) { dispatchClick(delBtn); deleted = true; await sleep(200); }
    const target = document.querySelector('[data-testid="plan-step-0"]');
    if (!target) throw new Error("编辑器里没有 plan-step-0");
    setInput(target, "手工改写过的第一步：先立三条主线");
    await sleep(200);
    const after = all('[data-testid^="plan-step-"]').map((el) => el.value);
    dispatchClick(document.querySelector('[data-testid="plan-save"]'));
    const done = await waitFor(() => chatEvents().some((e) => e.testid === "master-done"), 60000);
    await sleep(500);
    const events = chatEvents();
    const editedCard = all('[data-testid="master-plan"]').pop();
    return {
      original,
      after,
      deleted,
      editedCardText: editedCard ? textOf(editedCard).slice(0, 300) : null,
      newEvents: events.length - before,
      done: Boolean(done),
      dispatchCount: events.filter((e) => e.testid === "master-dispatch").length,
      tail: (textOf(document.querySelector('[data-testid="master-chat"]')) || "").slice(-600),
    };
  `,

  // 阶段 9（R4 回归）：改章号——把第一步里的「第 N 章」改成「第 N+2 章」。
  // 修 bug 前 `patchStep` 只换文本、保留旧 range=Single(N)，执行闸层会拿第 N 章的边界
  // 去卡第 N+2 章的稿子，判越界并把稿子扔进回收站。这里断言终版计划用的是新章号、
  // 而且聊天流里没有出现越界/回收站字样。
  planEditChapter: `
    const cards = all('[data-testid="master-plan"]');
    const last = cards[cards.length - 1];
    const editBtn = last ? last.querySelector('[data-testid="plan-edit"]') : null;
    if (!editBtn) throw new Error("最后一张计划卡上没有「编辑计划」按钮");
    dispatchClick(editBtn);
    const editor = await waitFor(() => document.querySelector('[data-testid="plan-editor"]'), 8000);
    if (!editor) throw new Error("点了编辑但没有出现编辑器");
    const area = document.querySelector('[data-testid="plan-step-0"]');
    if (!area) throw new Error("编辑器里没有 plan-step-0");
    const before = area.value;
    const found = before.match(/第\\s*(\\d{1,4})\\s*章/);
    if (!found) throw new Error("第一步里没有章号，无法做改章号验证：" + before);
    const oldChapter = Number(found[1]);
    const newChapter = oldChapter + 2;
    const patched = before.replace(found[0], "第" + String(newChapter).padStart(3, "0") + "章");
    setInput(area, patched);
    await sleep(250);
    const after = document.querySelector('[data-testid="plan-step-0"]').value;
    const beforeEvents = chatEvents().length;
    dispatchClick(document.querySelector('[data-testid="plan-save"]'));
    await waitFor(
      () => document.querySelector('[data-testid="plan-expanded"]') ||
            chatEvents().some((e) => e.testid === "master-done"),
      60000,
    );
    await sleep(600);
    const card = all('[data-testid="master-plan"]').pop();
    const cardText = card ? textOf(card) : "";
    const chat = textOf(document.querySelector('[data-testid="master-chat"]')) || "";
    const padded = (n) => String(n).padStart(3, "0");
    return {
      oldChapter,
      newChapter,
      before: before.slice(0, 120),
      after: after.slice(0, 120),
      fieldCleared: after !== before,
      cardHasNewChapter: new RegExp("第\\\\s*0*" + newChapter + "\\\\s*章").test(cardText),
      cardHasOldChapterOnly: new RegExp("第\\\\s*0*" + oldChapter + "\\\\s*章").test(cardText) &&
                             !new RegExp("第\\\\s*0*" + newChapter + "\\\\s*章").test(cardText),
      mentionedOldPadded: cardText.includes(padded(oldChapter)),
      outOfRangeWording: /越界|回收站|超出.*章节范围/.test(chat),
      newEvents: chatEvents().length - beforeEvents,
      tail: chat.slice(-500),
    };
  `,

  // 阶段 9：计划卡「取消计划」——卡片消失、原话回填输入框
  planCancel: `
    const cards = all('[data-testid="master-plan"]');
    const last = cards[cards.length - 1];
    const cancelBtn = last ? last.querySelector('[data-testid="plan-cancel"]') : null;
    if (!cancelBtn) throw new Error("最后一张计划卡上没有「取消计划」按钮");
    const before = all('[data-testid="master-plan"]').length;
    dispatchClick(cancelBtn);
    await sleep(600);
    const area = document.querySelector('[data-testid="master-note"]');
    return {
      cardsBefore: before,
      cardsAfter: all('[data-testid="master-plan"]').length,
      stillPending: Boolean(document.querySelector('[data-testid="plan-confirm"]')),
      draft: area ? area.value : null,
      lastSystem: (chatEvents().filter((e) => e.testid === "master-system").pop() || {}).text || null,
    };
  `,

  // 阶段 9：关掉所有弹层（设置弹窗盖住右栏时，点不到主 AI 输入框）
  esc8: `
    for (let i = 0; i < 3; i += 1) {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", code: "Escape", keyCode: 27, bubbles: true }));
      document.activeElement?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", code: "Escape", keyCode: 27, bubbles: true }));
      await sleep(250);
    }
    return {
      dialogs: document.querySelectorAll('[role="dialog"]').length,
      hasNote: Boolean(document.querySelector('[data-testid="master-note"]')),
    };
  `,

  panel8: `
    return {
      hasChat: Boolean(document.querySelector('[data-testid="master-chat"]')) ||
                Boolean(document.body.innerText.includes("对主 AI 总监说一句话")),
      hasInput: Boolean(document.querySelector('[data-testid="master-note"]')),
      hasSend: Boolean(document.querySelector('[data-testid="master-send"]')),
      hasHistory: Boolean(document.querySelector('[data-testid="chat-history"]')),
      hasClear: Boolean(document.querySelector('[data-testid="chat-clear"]')),
      session: (textOf(all('[data-testid="ai-panel"] span').pop()) || "").slice(0, 40),
      events: chatEvents(),
    };
  `,

  trace8: `
    const events = chatEvents();
    return {
      events,
      kinds: events.map((e) => e.testid || "user"),
      dispatchCount: events.filter((e) => e.testid === "master-dispatch").length,
      contextRows: events.filter((e) => e.testid === "master-system").map((e) => e.text),
      hasDone: events.some((e) => e.testid === "master-done"),
      text: (textOf(document.querySelector('[data-testid="master-chat"]')) || "").slice(-900),
    };
  `,

  session: `
    const ws = ws0();
    const info = await window.__TAURI_INTERNALS__.invoke("open_project", { workspaceDir: ws });
    const latest = await window.__TAURI_INTERNALS__.invoke("latest_chat_session", { bookRoot: info.bookRoot });
    const mine = (all('[data-testid="ai-panel"] span') || []).map(textOf).filter((t) => t && t.startsWith("s-"));
    const sid = latest || (mine[0] || null);
    const history = sid
      ? await window.__TAURI_INTERNALS__.invoke("load_chat_history", { bookRoot: info.bookRoot, sessionId: sid })
      : [];
    return {
      bookRoot: info.bookRoot,
      latestSession: latest,
      panelSession: mine[0] || null,
      count: history.length,
      roles: history.map((m) => m.role),
      first: history.length ? (history[0].content || "").slice(0, 80) : null,
      last: history.length ? (history[history.length - 1].content || "").slice(0, 120) : null,
      hasPlan: history.some((m) => Array.isArray(m.plan) && m.plan.length > 0),
      hasDispatch: history.some((m) => m.role === "agent"),
    };
  `,

  reload8: `
    location.reload();
    await sleep(300);
    return { reloading: true, origin: location.origin };
  `,

  clear8: `
    await waitFor(() => document.querySelector('[data-testid="chat-clear"]'), 15000);
    dispatchClick(document.querySelector('[data-testid="chat-clear"]'));
    await sleep(800);
    return {
      events: chatEvents().length,
      empty: Boolean(document.body.innerText.includes("对主 AI 总监说一句话")),
    };
  `,

  regression8: `
    const engines = await window.__TAURI_INTERNALS__.invoke("ai_engines");
    const badge = document.querySelector('[data-testid="ai-engine"]');
    const agents = await window.__TAURI_INTERNALS__.invoke("master_agents");
    let guard = null;
    try {
      await window.__TAURI_INTERNALS__.invoke("master_execute", {
        req: { workspaceRoot: ws0(), bookRoot: "", steps: [], agents: [], source: "confirmed" },
      });
      guard = "没有报错（不应该）";
    } catch (err) {
      guard = String(err && (err.message || err));
    }
    return {
      engineOptions: engines.map((e) => e.kind),
      engineNames: engines.map((e) => e.name),
      engineText: badge ? textOf(badge) : null,
      hasSelect: Boolean(document.querySelector('select[data-testid="ai-engine"]')),
      agentCount: agents.length,
      agentIds: agents.map((a) => a.id),
      hasHistoryButton: Boolean(document.querySelector('[data-testid="chat-history"]')),
      emptyStepsGuard: guard,
    };
  `,
};

function bodyFor(step) {
  if (STEPS[step]) return STEPS[step];

  if (step.startsWith("planOnly:")) {
    const text = step.slice("planOnly:".length);
    return `
      const before = chatEvents().length;
      if (!document.querySelector('[data-testid="master-note"]')) throw new Error("右栏没有主 AI 输入框");
      setInput(document.querySelector('[data-testid="master-note"]'), ${JSON.stringify(text)});
      await sleep(150);
      dispatchClick(document.querySelector('[data-testid="master-send"]'));
      const card = await waitFor(() => {
        const cards = all('[data-testid="master-plan"]');
        return cards.length > 0 ? cards[cards.length - 1] : null;
      }, 40000);
      await sleep(900);   // 多等一会儿：确认「没人点确认就不会有派发」
      const events = chatEvents();
      const cards = all('[data-testid="master-plan"]');
      const cardEl = cards.length > 0 ? cards[cards.length - 1] : null;
      return {
        text: ${JSON.stringify(text)},
        newEvents: events.length - before,
        kinds: events.map((e) => e.testid || "user"),
        hasCard: Boolean(card),
        pending: cardEl ? cardEl.getAttribute("data-plan-pending") : null,
        hasConfirm: Boolean(document.querySelector('[data-testid="plan-confirm"]')),
        hasEdit: Boolean(document.querySelector('[data-testid="plan-edit"]')),
        dispatchCount: events.filter((e) => e.testid === "master-dispatch").length,
        done: events.some((e) => e.testid === "master-done"),
        cardText: (textOf(card) || "").slice(0, 300),
      };
    `;
  }

  // 阶段 11：一句话 → 看主 AI「理解成了什么」+ 每一步派给谁、任务写的是什么。
  // 先清空对话（免得旧计划卡混进来），再发一句，只读不走流程。
  if (step.startsWith("probe:")) {
    const text = step.slice("probe:".length);
    return `
      const clearBtn = document.querySelector('[data-testid="chat-clear"]');
      if (clearBtn) {
        dispatchClick(clearBtn);
        await sleep(700);
      }
      if (!document.querySelector('[data-testid="master-note"]')) throw new Error("右栏没有主 AI 输入框");
      setInput(document.querySelector('[data-testid="master-note"]'), ${JSON.stringify(text)});
      await sleep(150);
      dispatchClick(document.querySelector('[data-testid="master-send"]'));
      await waitFor(() => all('[data-testid="master-plan"]').length > 0, 40000);
      const cards = all('[data-testid="master-plan"]');
      const card = cards[cards.length - 1];
      const intentEl = card.querySelector('[data-testid="plan-intent"]');
      const rows = Array.from(card.querySelectorAll("ol li")).map((li) => (textOf(li) || "").trim());
      return {
        text: ${JSON.stringify(text)},
        intent: intentEl ? (textOf(intentEl) || "").trim() : null,
        steps: rows,
        stepCount: rows.length,
        pending: card.getAttribute("data-plan-pending"),
        agents: rows.map((r) => {
          const parts = r.replace(/^\\d+\\.\\s*/, "").split(/\\s+/);
          return parts[0] || "";
        }),
        dispatchCount: chatEvents().filter((e) => e.testid === "master-dispatch").length,
        done: chatEvents().some((e) => e.testid === "master-done"),
      };
    `;
  }

  if (step === "confirm") {
    return `
      const before = chatEvents().length;
      const cards = all('[data-testid="master-plan"]');
      const last = cards[cards.length - 1];
      const btn = last ? last.querySelector('[data-testid="plan-confirm"]') : null;
      if (!btn) throw new Error("最后一张计划卡上没有「确认执行」按钮");
      dispatchClick(btn);
      const done = await waitFor(() => chatEvents().some((e) => e.testid === "master-done"), 60000);
      await sleep(500);
      const events = chatEvents();
      return {
        newEvents: events.length - before,
        kinds: events.map((e) => e.testid || "user"),
        dispatchCount: events.filter((e) => e.testid === "master-dispatch").length,
        contextRows: events.filter((e) => e.testid === "master-system").map((e) => e.text),
        done: Boolean(done),
        tail: (textOf(document.querySelector('[data-testid="master-chat"]')) || "").slice(-700),
      };
    `;
  }

  // 阶段 8 的「修改需求」按钮在阶段 9 变成「🚫 取消计划」（行为一致：丢计划 + 回填原话）
  if (step === "edit") {
    return `
      await waitFor(
        () => document.querySelector('[data-testid="plan-cancel"]') || document.querySelector('[data-testid="plan-edit"]'),
        10000,
      );
      const btn =
        document.querySelector('[data-testid="plan-cancel"]') || document.querySelector('[data-testid="plan-edit"]');
      if (!btn) throw new Error("没有「取消计划」按钮");
      dispatchClick(btn);
      await sleep(400);
      const area = document.querySelector('[data-testid="master-note"]');
      return {
        draft: area ? area.value : null,
        stillPending: Boolean(document.querySelector('[data-testid="plan-confirm"]')),
        lastSystem: (chatEvents().filter((e) => e.testid === "master-system").pop() || {}).text || null,
      };
    `;
  }

  if (step.startsWith("toolsOff:") || step.startsWith("toolsOn:")) {
    const on = step.startsWith("toolsOn:");
    const prefix = on ? "toolsOn:" : "toolsOff:";
    const tool = step.slice(prefix.length);
    return `
      ${OPEN_TOOLS}
      const box = document.querySelector('[data-testid="tool-${tool}"]');
      if (!box) throw new Error("设置里找不到工具勾选框：${tool}");
      dispatchClick(box);
      await sleep(700);
      const settings = await window.__TAURI_INTERNALS__.invoke("get_settings");
      const panel = document.querySelector('[data-testid="tool-whitelist"]');
      return {
        tool: ${JSON.stringify(tool)},
        want: ${on},
        checked: box.checked,
        allowedInSettings: settings.settings.allowedTools,
        title: panel ? textOf(panel).split("\\n")[0] : null,
      };
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
