// 阶段 7 验收驱动：智能体团队面板 + 动态 prompt 编排 + 闭环落盘。
// 用法：node scripts/acc7.mjs <step>
//   team              读六张卡片与 Tabs
//   wake:architect    点「唤醒并操作」并读聚焦目录
//   send:<role>:<text> 设指令并点「发送」
//   shortcut:<label>  点某条快捷指令
//   prompt            读「送进引擎的 prompt」全文
//   status            读当前 run 的状态徽标与时间线末尾
//   regression        核对引擎下拉、四行契约、智能体档案与 UI 是否一致
// 前置：应用以 dev 模式启动并带 --remote-debugging-port=9222；
//       claude-shim 的 mode.txt 写 ok（这样智能体「写大纲」会真的落盘）。

const PORT = process.env.CDP_PORT || "9222";
const arg = process.argv[2] || "team";

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
const waitFor = async (fn, ms = 15000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = fn();
    if (v) return v;
    await sleep(100);
  }
  return null;
};
/** Radix 的 Tabs 不响应 .click()，必须四连派发。 */
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
const cards = () =>
  all('[data-agent-role]').map((el) => ({
    role: el.getAttribute('data-agent-role'),
    active: el.getAttribute('data-agent-active') === 'true',
    name: textOf(el.querySelector('[class*="font-semibold"]')) || null,
    text: textOf(el),
    buttons: [...el.querySelectorAll('button')].map((b) => textOf(b)),
  }));
const focusText = () => textOf(document.querySelector('[data-testid="ai-focus"]'));
const openTab = async (tab) => {
  dispatchClick(document.querySelector('[data-testid="tab-' + tab + '"]'));
  await sleep(160);
};
const openChat = async () => {
  await openTab('chat');
  return Boolean(document.querySelector('[data-testid="ai-note"]'));
};
`;

const STEPS = {
  team: `
    await openTab('team');
    const tabs = all('[role="tab"]').map((t) => textOf(t));
    return {
      tabs,
      cards: cards(),
      cardCount: cards().length,
      activeRole: (cards().find((c) => c.active) || {}).role || null,
      visible: Boolean(document.querySelector('[data-testid="agent-team"]')),
    };
  `,

  wakeArchitect: `
    await openTab('team');
    const card = document.querySelector('[data-agent-role="architect"]');
    const btn = [...card.querySelectorAll('button')].find((b) =>
      ['唤醒并操作', '继续指挥'].includes(textOf(b)));
    dispatchClick(btn);
    await sleep(220);
    return {
      focus: focusText(),
      onChat: Boolean(document.querySelector('[data-testid="ai-note"]')),
      placeholder: (document.querySelector('[data-testid="ai-note"]') || {}).placeholder || null,
      shortcuts: all('[data-agent-shortcut]').map((b) => b.getAttribute('data-agent-shortcut')),
      cardActive: (cards().find((c) => c.role === 'architect') || {}).active || false,
    };
  `,

  wakeSettings: `
    await openTab('team');
    const card = document.querySelector('[data-agent-role="settings"]');
    const btn = [...card.querySelectorAll('button')].find((b) =>
      ['唤醒并操作', '继续指挥'].includes(textOf(b)));
    dispatchClick(btn);
    await sleep(220);
    return {
      focus: focusText(),
      shortcuts: all('[data-agent-shortcut]').map((b) => b.getAttribute('data-agent-shortcut')),
    };
  `,

  defaultTab: `
    // 不点任何东西：读当前哪个 Tab 被选中、团队面板是否可见
    const tabs = all('[role="tab"]').map((t) => ({
      text: textOf(t),
      state: t.getAttribute('data-state'),
      selected: t.getAttribute('aria-selected'),
      testid: t.getAttribute('data-testid'),
    }));
    const selected = tabs.filter((t) => t.state === 'active' || t.selected === 'true').map((t) => t.testid);
    return {
      tabs,
      selected,
      teamVisible: Boolean(document.querySelector('[data-testid="agent-team"]')),
      chatVisible: Boolean(document.querySelector('[data-testid="ai-note"]')),
      cardCount: cards().length,
    };
  `,

  chatHeader: `
    await openChat();
    const head = document.querySelector('[data-testid="chat-header"]');
    const exit = document.querySelector('[data-testid="chat-exit"]');
    const details = document.querySelector('[data-testid="run-details"]');
    return {
      headerPresent: Boolean(head),
      headerText: textOf(head),
      focus: focusText(),
      hasExit: Boolean(exit),
      shortcuts: all('[data-agent-shortcut]').map((b) => b.getAttribute('data-agent-shortcut')),
      detailsPresent: Boolean(details),
      detailsText: textOf(details),
      detailsOpen: details ? details.getAttribute('data-state') : null,
      traceRowsVisible: all('[data-testid="run-details"] ~ * [title]').length,
    };
  `,

  exitAgent: `
    await openChat();
    dispatchClick(document.querySelector('[data-testid="chat-exit"]'));
    await sleep(220);
    const tabs = all('[role="tab"]').map((t) => ({ testid: t.getAttribute('data-testid'), state: t.getAttribute('data-state') }));
    return {
      selected: tabs.filter((t) => t.state === 'active').map((t) => t.testid),
      teamVisible: Boolean(document.querySelector('[data-testid="agent-team"]')),
      activeRole: (cards().find((c) => c.active) || {}).role || null,
    };
  `,

  prompt: `
    await openChat();
    const pre = document.querySelector('[data-testid="ai-prompt"]');
    const text = textOf(pre);
    return {
      present: Boolean(pre),
      lines: text ? text.split('\\n').length : 0,
      head: text ? text.split('\\n').slice(0, 8) : [],
      tail: text ? text.split('\\n').slice(-4) : [],
      text,
    };
  `,

  status: `
    await openChat();
    const badges = all('span,div')
      .map((el) => textOf(el))
      .filter((t) => t && ['生成中','已完成','被拦截','已取消','出错'].includes(t));
    const alerts = all('[role="alert"]').map((el) => textOf(el));
    const timeline = textOf(document.querySelector('[data-testid="run-timeline"]'))
      || (document.querySelector('.overflow-auto') ? '' : '');
    return {
      badges,
      alerts,
      focus: focusText(),
      chapter: (document.querySelector('[data-testid="ai-chapter"]') || {}).value || null,
    };
  `,

  regression: `
    // 引擎下拉还在，且智能体档案与 UI 卡片一致（都来自 Rust agent_specs）
    const select = document.querySelector('select');
    const options = select ? [...select.options].map((o) => ({ v: o.value, t: textOf(o), disabled: o.disabled })) : [];
    const specs = await window.__TAURI_INTERNALS__.invoke('agent_specs');
    const cardsNow = await (async () => { await openTab('team'); return cards(); })();
    const mismatch = specs
      .map((s) => {
        const c = cardsNow.find((x) => x.role === s.role);
        if (!c) return s.role + ':缺少卡片';
        if (!(c.text || '').includes(s.name)) return s.role + ':名称不一致(' + c.name + ')';
        if (!(c.text || '').includes(s.dir + '/')) return s.role + ':目录不一致';
        return null;
      })
      .filter(Boolean);
    const manual = await (async () => { await openChat(); return all('button').map((b) => textOf(b)); })();
    return {
      engineOptions: options,
      specs: specs.map((s) => ({ role: s.role, name: s.name, dir: s.dir })),
      mismatch,
      hasSend: manual.includes('发送'),
      hasShortcuts: all('[data-agent-shortcut]').length,
    };
  `,
};

// send:<role>:<text> 与 shortcut:<label> 是带参步骤，单独拼装。
function bodyFor(step) {
  if (STEPS[step]) return STEPS[step];

  if (step.startsWith("send:")) {
    const rest = step.slice("send:".length);
    const idx = rest.indexOf(":");
    const role = idx >= 0 ? rest.slice(0, idx) : "architect";
    const text = idx >= 0 ? rest.slice(idx + 1) : rest;
    return `
      await openTab('team');
      const card = document.querySelector('[data-agent-role=${JSON.stringify(role)}]');
      if (card) {
        const btn = [...card.querySelectorAll('button')].find((b) =>
          ['唤醒并操作', '继续指挥'].includes(textOf(b)));
        if (btn) dispatchClick(btn);
        await sleep(200);
      }
      await openChat();
      setInput(document.querySelector('[data-testid="ai-note"]'), ${JSON.stringify(text)});
      await sleep(120);
      const sendBtn = document.querySelector('[data-testid="ai-send"]');
      const disabled = sendBtn ? sendBtn.disabled : null;
      dispatchClick(sendBtn);
      await sleep(200);
      return { role: ${JSON.stringify(role)}, text: ${JSON.stringify(text)}, focus: focusText(), disabled, started: true };
    `;
  }

  if (step.startsWith("shortcut:")) {
    const label = step.slice("shortcut:".length);
    return `
      await openChat();
      const btn = all('[data-agent-shortcut]').find((b) => b.getAttribute('data-agent-shortcut') === ${JSON.stringify(label)});
      const exists = Boolean(btn);
      dispatchClick(btn);
      await sleep(200);
      return { label: ${JSON.stringify(label)}, exists, focus: focusText() };
    `;
  }

  if (step.startsWith("engine:")) {
    const kind = step.slice("engine:".length);
    return `
      const sel = document.querySelector('select');
      if (!sel) throw new Error('页面上没有引擎下拉');
      Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set.call(sel, ${JSON.stringify(kind)});
      sel.dispatchEvent(new Event('change', { bubbles: true }));
      await sleep(200);
      return { engine: sel.value, options: [...sel.options].map((o) => o.value) };
    `;
  }

  if (step.startsWith("wait:")) {
    const ms = Number(step.slice("wait:".length)) || 3000;
    return `await sleep(${ms}); await openChat(); return { waited: ${ms}, done: true };`;
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
