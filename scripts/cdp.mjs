// 通过 WebView2 的 CDP 端口在应用页面里执行一段 JS，用于自动化验收。
//
// 前置：应用带 --remote-debugging-port=9222 启动，即
//   $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9222"
// 用法：
//   node scripts/cdp.mjs "document.title"
//   node scripts/cdp.mjs "document.querySelectorAll('button').length"

const PORT = process.env.CDP_PORT || "9222";

const targets = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
const page = targets.find((t) => t.type === "page");
if (!page) {
  console.error("没有找到页面目标：", JSON.stringify(targets));
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

const expression = process.argv.slice(2).join(" ");
const res = await send("Runtime.evaluate", {
  expression,
  awaitPromise: true,
  returnByValue: true,
});

const value = res.result?.result?.value;
const exception = res.result?.exceptionDetails;
if (exception) {
  console.error("JS 异常：", exception.text, JSON.stringify(exception.exception?.description ?? ""));
  process.exit(3);
}

console.log(typeof value === "string" ? value : JSON.stringify(value, null, 2));
ws.close();
