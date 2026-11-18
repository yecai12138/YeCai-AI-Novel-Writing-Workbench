// 假的 claude CLI：只用于验证 StoryDesk 的 ClaudeCodeAdapter
// （stream-json 解析 / exit 2 阻断 / ai_cancel 的 taskkill / Key 只走 env / 主 AI 规划 JSON）。
//
// 用法：
//   1. 把本目录加进 PATH：$env:PATH = "<repo>\scripts\fixtures\claude-shim;" + $env:PATH
//   2. 用 mode.txt 切模式：block（默认，模拟 hook 拦截）/ slow（一直流式，用来试取消）/ ok（正常结束）
//   3. 启动 StoryDesk 后选「Claude Code CLI」，面板会显示
//      「启动方式：node + fake-claude.mjs（已解开 .cmd 包装）」
//   4. 每次调用都会往本目录 args.log 追加一行，可用来核对
//      「参数里有 --output-format stream-json -p，且 Key 从不出现在参数里」。
//
// 它扮演两种角色：
//   - 主 AI 总监：prompt 里含「主 AI 总监」，按 `作者的需求：` 回一个 JSON 计划；
//   - 子智能体：按「你的核心职责是处理 <dir>/ 目录」判断该写哪儿。
//
// 阶段 10 起它还要**认意图**（以前是无脑写文件，于是「检查伏笔」也产出细纲）：
//   - prompt 里有 `【只读模式】` 或任务是检查类 → 只吐一段检查报告，**绝不 Write**；
//   - 写正文时按 prompt 里的字数区间打标记（`[此处模拟生成了 2500 字]`），让字数断言有据可依；
//   - 目标是 `追踪/` 目录 → 写/追加 `追踪/时间线.md`。
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const argv = process.argv.slice(2);
const promptIdx = argv.indexOf("-p");
const prompt = promptIdx >= 0 ? argv[promptIdx + 1] ?? "" : "";

// 章号可能是阿拉伯数字（第3章）也可能是中文数字（生成第一章细纲），两种都要认。
const CN_DIGIT = { 零: 0, 一: 1, 二: 2, 两: 2, 三: 3, 四: 4, 五: 5, 六: 6, 七: 7, 八: 8, 九: 9 };
function cnNumber(raw) {
  if (/^\d+$/.test(raw)) return Number(raw);
  const ten = raw.match(/^([一二三四五六七八九])?十([一二三四五六七八九])?$/);
  if (ten) return (ten[1] ? CN_DIGIT[ten[1]] : 1) * 10 + (ten[2] ? CN_DIGIT[ten[2]] : 0);
  if (raw.length === 1 && raw in CN_DIGIT) return CN_DIGIT[raw];
  return null;
}
function chapterOf(text) {
  const m = text.match(/第\s*([0-9]{1,3}|[零一二三四五六七八九十两]{1,3})\s*章/);
  const n = m ? cnNumber(m[1]) : null;
  return n && n > 0 ? n : 1;
}
function chapterOrNull(text) {
  const m = text.match(/第\s*([0-9]{1,3}|[零一二三四五六七八九十两]{1,3})\s*章/);
  const n = m ? cnNumber(m[1]) : null;
  return n && n > 0 ? n : null;
}
// 章号必须取「这次要写的章」，不能取上下文文件清单里的旧章号
// （文件清单里可能躺着 大纲/细纲_第001章.md，先匹配到它就会一直重复写第 1 章）。
function chapterForPrompt(text) {
  const extra = text.split("\n").find((l) => l.startsWith("补充要求："));
  const fromExtra = extra ? chapterOrNull(extra) : null;
  return fromExtra ?? chapterOf(text);
}
function bookRootOf(text) {
  const line = text.split("\n").find((l) => l.startsWith("项目目录：")) ?? "";
  return line.slice("项目目录：".length).trim();
}
function targetDirOf(text) {
  const line = text.split("\n").find((l) => l.startsWith("你的核心职责是处理 ")) ?? "";
  if (!line) return "";
  return line.slice("你的核心职责是处理 ".length).split("/")[0].trim();
}
// 主 AI 的 prompt 里每个角色一行：「- agent_id: tracker / 🕵️ 连贯性追踪员 / 目录：追踪/ / 职责：…」
function agentIdForDir(text, dir) {
  const line = text
    .split("\n")
    .find((l) => l.startsWith("- agent_id:") && l.includes(`目录：${dir}/`));
  if (!line) return null;
  const m = line.match(/- agent_id:\s*([a-z0-9_-]+)/);
  return m ? m[1] : null;
}

// 阶段 10：意图识别。检查类 = 有检查词、且没有「写 / 生成 / 正文」这类动笔词。
const CHECK_WORDS = /检查|核对|分析|盘点|梳理|评估|审查|对账|伏笔|时间线|连贯|前后矛盾|一致性/;
const WRITE_WORDS = /写第|写一章|写一本|写一部|写小说|写故事|正文|生成第|扩写|润色|改写|补一份|补一章|落盘|成稿/;
function wantsCheck(text) {
  return CHECK_WORDS.test(text) && !WRITE_WORDS.test(text);
}
// 阶段 11：把口语外壳剥掉，派给智能体的任务不能再是一整句用户原话。
const FILLER = [
  "把设定里的几个文件内容给我写了",
  "把设定里的文件内容给我写了",
  "内容给我写了",
  "给我写了",
  "帮我写",
  "帮忙写",
  "请你",
  "麻烦你",
  "麻烦",
  "帮我",
  "帮我把",
  "给我",
  "我想要",
  "我要",
  "要写",
  "写一下",
  "写了",
  "吧",
  "一下",
];
function cleanSubject(text) {
  let out = String(text ?? "").trim();
  for (const filler of FILLER) out = out.split(filler).join("");
  out = out.replace(/[，,。！!？?；;]/g, "").replace(/\s+/g, " ").trim();
  return out.length > 40 ? `${out.slice(0, 40)}…` : out || String(text ?? "").trim();
}
// 「写一本古代的农民起事的小说」→ 「古代的农民起事」
function genreOf(text) {
  const m = String(text ?? "").match(
    /写(?:一本|一部|个)?\s*([^，。,.！!？?\s]{2,20}?)\s*的?\s*(?:小说|故事|长篇|网文)/,
  );
  return m ? m[1].trim() : null;
}
// 细纲里抽出来的硬约束（阶段 10 会把它接到「补充要求：」末尾）
function constraintOf(text) {
  const range = text.match(/(\d{3,4})\s*[-–—~～至到]\s*(\d{3,4})\s*字/);
  return range ? { lo: Number(range[1]), hi: Number(range[2]), raw: range[0] } : null;
}
// 阶段 11.5：题材现在会以两种形式出现在 prompt 里——
// 任务里的「（题材：古代的农民起事）」和最高优先级那段的「本书当前题材：古代的农民起事」。
function topicOfPrompt(text) {
  const body = String(text ?? "");
  const m =
    body.match(/本书当前题材：([^。\n]{2,30})/) || body.match(/（题材：([^）]{2,30})）/) || body.match(/题材「([^」]{2,30})」/);
  return m ? m[1].trim() : null;
}
// 章名：六到八字的小标题，从题材里取；实在取不到就给一个通用的，绝不沿用上一本的章名。
function titleFor(topic) {
  if (!topic) return "开篇";
  const cleaned = topic.replace(/的|之|类|型|题材/g, "");
  return cleaned.length > 8 ? cleaned.slice(0, 8) : cleaned || "开篇";
}
// 模拟正文：围绕题材写，长度凑到字数区间的中值（联调要看「字数是否接近硬约束」）。
function proseBody({ padded, title, topic, mid, constraints, globalLines }) {
  const theme = topic || "这一章";
  const paragraphs = [
    `# 第${padded}章 ${title}`,
    "",
    `> [此处模拟生成了 ${mid} 字]（字数区间 ${constraints ? constraints.raw : "未在细纲里找到"}；对白约四成，环境描写约两成）`,
    "",
    `> 设定引用：${(globalLines || []).slice(0, 4).join("；") || "设定/ 下的世界观与人物"}`,
    "",
    `天还没亮，${theme}这件事已经压在每个人心上。村口的老槐树下挂着一口破钟，钟绳断了一截，`,
    "谁也没去接，像是大家都默认：这口钟再响，也不会有人来。",
    "",
    "他蹲在田埂上，把手里的谷穗一颗一颗剥开，数到第十七颗才停手。",
    "身后有人在等他开口，等了很久，久到露水从鞋面渗进袜子里。",
    "",
    "“官仓的账，昨晚上换过了。”来人说。",
    "“换成了什么样。”",
    "“一样厚。”来人顿了顿，“纸是新的。”",
    "",
    "他笑了一下，那种笑法不像高兴。",
    "他把剥好的谷粒倒进掌心，又倒回田里，说：“那就从纸开始问。”",
    "",
    "天光一点点亮起来。远处的堤坝上有衙役在走动，脚步声顺着水面传过来，很轻，却让人心里发紧。",
    "他知道，从今天起，他们再没有退路了。",
    "",
    "“真要走这一步？”来人问。",
    "“不是我走。”他说，“是这条路已经铺到脚底下了。”",
    "",
    "风把稻浪压下去一片，又抬起来。有人开始磨刀，声音是从村子另一头传过来的。",
    "",
    `——这一章的余下部分，围绕「${theme}」继续展开：第一次冲突、第一次让步、第一次流血。`,
    "",
  ];
  // 凑字数：联调时要看「字数是否落在细纲要的区间里」，所以补到中值附近为止。
  let body = paragraphs.join("\n");
  let filler = 1;
  while (body.length < mid && filler <= 200) {
    body += [
      `第${filler}段。他们把话说完，谁也没先站起来。门外的雨停了，路还没干，泥地里印着一串新的脚印。`,
      `他数着那串脚印，一共七步，最后一步停在他家门口。`,
      `“他们在等我们出错。”来人说。`,
      `“那就让他们等。”他把门闩上，转身去点灯，火苗舔了两下才稳住。`,
      "",
    ].join("\n");
    filler += 1;
  }
  body += "（本章为假 CLI 生成的联调样本，只用于验证流程与字数，不作为正文质量参考。）\n";
  return body;
}

const readOnlyPrompt = prompt.includes("【只读模式】");
const targetDir = targetDirOf(prompt);
const extraLine = prompt.split("\n").find((l) => l.startsWith("补充要求：")) ?? "";
// 这一步的任务正文（`补充要求：` 之后那一段）。阶段 11 起用它判断「这一步到底要我干什么」，
// 不能再用整段 prompt 里是否出现「细纲」——角色边界那段里本来就写着「章细纲」。
const taskLine = extraLine.replace(/^补充要求：/, "").trim();
const constraints = constraintOf(extraLine || prompt);

// 只记录 Key 是否存在，绝不记录它的值。
const record = {
  at: new Date().toISOString(),
  argv,
  promptLines: prompt.split("\n").length,
  promptFirstLine: prompt.split("\n")[0] ?? "",
  promptTail: prompt.split("\n").slice(-1)[0] ?? "",
  hasStreamJson: argv.includes("stream-json"),
  hasPrintFlag: argv.includes("-p"),
  apiKeyPresent: Boolean(process.env.ANTHROPIC_API_KEY),
  apiKeyInArgs: argv.some((a) => a.includes("sk-ant")),
  master: prompt.includes("主 AI 总监"),
  // 阶段 9：把 prompt 里「允许的工具」那一行原样记下来，用来核对设置勾选是否真的生效
  allowedLine: (prompt.match(/^允许的工具：.*$/m) || [""])[0],
  // 阶段 10：意图判定与全局上下文注入的证据
  // 主 AI 自己的 prompt 里也有「【只读模式】」这个词（那是它的调度规则），所以 master 行不算只读。
  readOnly: !prompt.includes("主 AI 总监") && readOnlyPrompt,
  targetDir,
  globalLines: prompt
    .split("\n")
    .filter((l) => /^- (设定|追踪|参考资料|大纲)\//.test(l.trim())),
  constraintLine: constraints ? constraints.raw : "",
  extraLine,
  // 阶段 11：这一步的任务正文（补充要求之后的指令），用来核对「任务有没有被清洗」
  taskLine,
  checkIntent: wantsCheck(prompt),
};
appendFileSync(join(here, "args.log"), JSON.stringify(record) + "\n");

// 阶段 11.5：除了每次调用的主记录，还记一件「这次到底落盘了什么」，
// 用来核对题材切换后写到的是不是新文件、新细纲。
const note = (obj) =>
  appendFileSync(
    join(here, "args.log"),
    JSON.stringify({ at: new Date().toISOString(), ...obj }) + "\n",
  );

const modeFile = join(here, "mode.txt");
const mode = existsSync(modeFile) ? readFileSync(modeFile, "utf8").trim() : "block";

const out = (obj) => process.stdout.write(JSON.stringify(obj) + "\n");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Windows 管道是异步写：紧接着 process.exit() 会把最后几行丢掉，
// 上层适配器就看不到 result 事件，只能判成「没跑完」。退出前留一拍让它冲干净。
const finish = async (code) => {
  await sleep(150);
  process.exit(code);
};

const textOf = (text) => out({ type: "assistant", message: { content: [{ type: "text", text }] } });
const toolOf = (name, input, id) =>
  out({ type: "assistant", message: { content: [{ type: "tool_use", id, name, input }] } });

out({ type: "system", subtype: "init", session_id: "shim-session", model: "shim-model" });
await sleep(100);

// ---------------------------------------------------------------- 主 AI 规划
if (prompt.includes("主 AI 总监") && prompt.includes("agent_id")) {
  const want = (prompt.split("作者的需求：")[1] ?? "").trim();
  const chapter = chapterOrNull(want) ?? chapterForPrompt(prompt);
  // 阶段 11：动笔信号很硬（写一本/写第/写小说…），跟「检查」同时出现时也按动笔算。
  const strongWrite = /写本章正文|写第|写一章|落盘|扩写|开写|写一本|写小说/.test(want);
  const prose =
    /正文|落盘|成稿|扩写|写一章|写一版|写一本|写一部|写小说|写故事|开写|写第|写出来|动笔|本章|这一章|下一章|写了|写一下|写成/.test(
      want,
    ) && !(wantsCheck(want) && !strongWrite);
  // 派给智能体的任务必须是清洗过的指令：不能把「把设定里的几个文件内容给我写了」原样塞进去。
  const subject = cleanSubject(want);
  const genre = genreOf(want);

  const tracker = agentIdForDir(prompt, "追踪") ?? "tracker";
  const settingsAgent = agentIdForDir(prompt, "设定") ?? "settings";

  let plan;
  if (!prose && wantsCheck(want)) {
    // 检查类：只读、只派一步、派给贴题的那个角色（伏笔/时间线 → 追踪者）
    const settingsCheck = /设定|人物|世界观|术语|势力/.test(want) && !/伏笔|时间线|连贯/.test(want);
    plan = [
      {
        agent_id: settingsCheck ? settingsAgent : tracker,
        task: `【只读模式】${subject}：只输出报告，不要调用 Write 工具，不要修改任何文件`,
      },
    ];
  } else if (/细纲|大纲|卷纲|章纲/.test(want)) {
    // 排大纲：只派架构师，任务是明确可执行的细纲任务（不是原话）
    plan = [
      {
        agent_id: "architect",
        task: `生成第 ${chapter} 章细纲：目标、冲突、转折、结尾钩子（已有细纲就沿用，不要覆盖）`,
      },
    ];
  } else if (prose) {
    // 阶段 11.5：说了「写一本…小说」就是新书启动——旧细纲属于上一本书，必须重写。
    const fresh = Boolean(genre) || /写一本|写一部|写小说|写故事|整本|从头/.test(want);
    plan = [
      {
        agent_id: "architect",
        task: fresh
          ? `重新生成第 ${chapter} 章细纲（题材：${genre ?? "按 设定/题材定位.md"}）：旧细纲作废，不要沿用；写清时代、人物、冲突、三幕场景与结尾钩子，并给出字数区间与对白比例`
          : `生成第 ${chapter} 章细纲：目标、冲突、转折、结尾钩子（已有细纲就沿用，不要覆盖）`,
      },
      {
        agent_id: "writer",
        task: `按第 ${chapter} 章细纲写本章正文${genre ? `（题材：${genre}）` : ""}；先读 设定/ 下的人物与世界观，正文要与之一致，文件名不得沿用与题材无关的旧章名`,
      },
    ];
  } else {
    // 拿不准就只派一步、只读，并把「我理解成什么」说清楚——绝不把原话当任务。
    plan = [
      {
        agent_id: tracker,
        task: `【只读模式】先读 设定/ 与 大纲/，用三行说清你理解的需求与建议动作，不要动笔（原话：${subject}）`,
      },
    ];
  }

  appendFileSync(
    join(here, "args.log"),
    JSON.stringify({ event: "master-plan", want, subject, genre, chapter, prose, plan }) + "\n",
  );
  textOf(`收到。作者的诉求是「${subject}」，我拆成 ${plan.length} 步：`);
  await sleep(120);
  textOf("```json\n" + JSON.stringify({ plan }, null, 2) + "\n```");
  await sleep(120);
  out({ type: "result", subtype: "success", result: `已拆成 ${plan.length} 步`, is_error: false });
  await finish(0);
}

// ---------------------------------------------------------------- 子智能体
textOf("先读任务包与细纲…");
await sleep(120);
toolOf("Read", { file_path: "大纲/细纲_第001章.md" }, "t1");
await sleep(120);

if (mode === "block") {
  toolOf("Write", { file_path: "正文/第001章_雨夜来客.md" }, "t2");
  // 与 guard-outline-before-prose.sh 的 exit 2 + stderr 完全同形。
  await sleep(150);
  process.stderr.write("⛔ 写正文被拦截：第 1 章缺少细纲（大纲/细纲_第001章.md）。\n");
  await finish(2);
}

if (mode === "slow") {
  for (let i = 0; i < 600; i += 1) {
    await sleep(200);
    textOf(`慢速输出 ${i}`);
  }
  await finish(0);
}

// ok 模式：模拟真实智能体「真的把文件写进磁盘」。
// 目录由 prompt 里的「你的核心职责是处理 <dir>/ 目录」决定，章号取「补充要求」里这次要写的章。
if (mode === "ok") {
  const bookRoot = bookRootOf(prompt);
  const chapterNo = chapterForPrompt(prompt);
  const padded = String(chapterNo).padStart(3, "0");

  // ---- 只读任务：只出报告，一个文件都不写（阶段 10 的核心约束）
  if (readOnlyPrompt || (targetDir === "追踪" && wantsCheck(extraLine))) {
    await sleep(150);
    const report = [
      "## 检查报告（只读）",
      "",
      "- 伏笔盘点：第 1 章埋的「袖口的泥 / 干净的鞋底」尚未回收，按细纲应在本卷第 3 章回收；",
      "- 时间线：卷纲里的三段时间顺序与正文一致，暂未发现前后矛盾；",
      "- 人物状态：主角在第 1 章末已经知道「后门被人动过」，第 2 章开头要接着这个认知写。",
      "",
      "结论：伏笔已核对，暂无冲突。以上为报告，未修改任何文件。",
    ].join("\n");
    textOf(report);
    await sleep(150);
    out({
      type: "result",
      subtype: "success",
      result: "已生成检查报告（只读，未修改任何文件）",
      is_error: false,
    });
    await finish(0);
  }

  if (bookRoot && targetDir === "追踪") {
    const rel = "追踪/时间线.md";
    const abs = join(bookRoot, "追踪", "时间线.md");
    const existed = existsSync(abs);
    const block = [
      `## 第${padded}章 之前的时间线`,
      "",
      `- 第 1 章：雨夜，旧书店，来客从后门进来；主角知道后门被人动过。`,
      `- 第${padded}章：紧接在同一个夜晚之后，天亮前必须决定稿子要不要继续写。`,
      "",
    ].join("\n");
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, (existed ? readFileSync(abs, "utf8") + "\n" : "# 时间线\n\n") + block, "utf8");
    toolOf("Write", { file_path: rel }, "t3");
    await sleep(150);
    textOf(`已${existed ? "追加" : "新建"} ${rel}`);
    await sleep(150);
    out({ type: "result", subtype: "success", result: `已写入 ${rel}`, is_error: false });
    await finish(0);
  }

  if (bookRoot && targetDir === "正文") {
    // 阶段 11.5：章名与内容都跟着题材走。之前写死「雨夜来客」，换了题材也照样产出旧章名，
    // 用户看到的计划是对的、落盘还是上一本书。
    const topic = topicOfPrompt(prompt);
    const title = titleFor(topic);
    const name = `第${padded}章_${title}.md`;
    const rel = `正文/${name}`;
    const abs = join(bookRoot, "正文", name);
    const mid = constraints ? Math.round((constraints.lo + constraints.hi) / 2) : 2500;
    const globalLines = (prompt.match(/^- (设定|追踪|参考资料|大纲)\/.*$/gm) || []).map((l) =>
      l.replace(/^- /, ""),
    );
    const body = proseBody({ padded, title, topic, mid, constraints, globalLines });

    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, body, "utf8");
    note({ event: "prose-written", topic, title, rel, chars: body.length, target: mid });
    toolOf("Write", { file_path: rel }, "t3");
    await sleep(150);
    textOf(`已写入 ${rel}（${body.length} 字，目标 ${mid} 字）`);
    await sleep(150);
    out({ type: "result", subtype: "success", result: `已写入 ${rel}`, is_error: false });
    await finish(0);
  }

  // 阶段 11.5：设定太薄时先补一步。真实模型会自己读设定写文件，这里只写两份最小的。
  if (bookRoot && /【补设定】/.test(taskLine)) {
    const topic = topicOfPrompt(prompt) || "本书题材";
    const overview = [
      "# 世界概览",
      "",
      `- 时代与地理：${topic} 的背景落在一条常年泛滥的河与两座官仓之间，堤北是佃户村，堤南是县城。`,
      "- 经济与权力：官仓、义仓、漕运三层盘剥；县衙与粮商互相担保，佃户手里只有租契。",
      "- 矛盾起点：春荒时官仓封仓，义仓的粮账被换过纸。",
      "",
    ].join("\n");
    const people = [
      "# 人物",
      "",
      "- 李敢（主角，佃户，三十岁）：识字，会算账，替村里记工。不信官，也不信天上掉粮。",
      "- 阿禾（主角之妻）：管着家里最后半袋粟，最先看出村里要出事的人。",
      "- 赵簿（县衙书吏）：换账的人之一，嘴紧，怕事，但知道全部数目。",
      "- 老钟头（里正）：两面做人，最后被两边都当成叛徒。",
      "",
    ].join("\n");
    const files = [
      ["设定/世界概览.md", overview],
      ["设定/人物.md", people],
    ];
    for (const [rel, text] of files) {
      const abs = join(bookRoot, rel);
      mkdirSync(dirname(abs), { recursive: true });
      writeFileSync(abs, text, "utf8");
      toolOf("Write", { file_path: rel }, `s-${rel}`);
      await sleep(100);
    }
    note({ event: "settings-bootstrap", topic, files: files.map(([rel]) => rel) });
    textOf("已补齐 设定/世界概览.md 与 设定/人物.md（各约 200 字）。");
    await sleep(120);
    out({ type: "result", subtype: "success", result: "已补齐基础设定：世界概览、人物", is_error: false });
    await finish(0);
  }

  // 阶段 11：大纲架构师只认「细纲/大纲/卷纲」这类任务。整段 prompt 里出现「细纲」不算数
  // （角色边界里本来就写着「章细纲」），要看这一步的任务正文。不是细纲任务就明确拒绝，
  // 而不是为了交差写一份假细纲把上一版覆盖掉。
  const isOutlineTask = /细纲|大纲|卷纲|章纲/.test(taskLine);
  if (bookRoot && targetDir === "大纲" && !isOutlineTask) {
    textOf("【职责边界】这一步不是细纲任务，我只能读写 大纲/，不会动笔，也不会把一句话包装成细纲。");
    await sleep(80);
    textOf(`收到的是：「${taskLine || "（空任务）"}」。请把它拆成明确的细纲任务再派给我。`);
    await sleep(80);
    out({ type: "result", subtype: "success", result: "已拒绝写入：这一步不是细纲任务", is_error: false });
    await finish(0);
  }

  if (bookRoot && isOutlineTask) {
    const rel = `大纲/细纲_第${padded}章.md`;
    const abs = join(bookRoot, "大纲", `细纲_第${padded}章.md`);
    // 阶段 11.5：任务里要求「重新生成 / 旧细纲作废」时必须覆盖，不能又沿用上一本的细纲。
    const regenerate = /重新生成|作废|不要沿用/.test(taskLine);
    if (existsSync(abs) && !regenerate) {
      textOf(`${rel} 已经在了，沿用现成的，不覆盖。`);
      await sleep(80);
      out({ type: "result", subtype: "success", result: `沿用了 ${rel}（未覆盖）`, is_error: false });
      await finish(0);
    }
    const topic = topicOfPrompt(prompt);
    const title = titleFor(topic);
    const theme = topic || "这一章";
    const body = [
      `# 第${padded}章 ${title}`,
      "",
      `> 生成方式：StoryDesk + claude-shim（假 CLI，仅用于联调）`,
      `> 题材：${theme}${regenerate ? "（旧细纲已作废，本版按题材重写）" : ""}`,
      "",
      "## 一句话梗概",
      `春荒将至，${theme}的引线被一次换账点燃：主角发现官仓的数目被人动过，`,
      "而唯一能作证的人，是那个替县衙抄了十年账的书吏。",
      "",
      "## 人物出场",
      "- 李敢（佃户，三十岁，会算账）：沉默、记性好，习惯先把数记在心里。",
      "- 阿禾（李敢之妻）：最先看出村里要出事的人，也是拦住他冲动的人。",
      "- 赵簿（县衙书吏）：换账者之一，怕事，知道全部数目。",
      "",
      "## 场景与情绪",
      "1. 田埂与破钟（清晨、露水）——压抑。剥谷穗数到十七颗，说明他心里已经在算总账。",
      "2. 换账对峙（村口、纸是新的）——试探与压住火气。对白短，句句落在数目上。",
      "3. 磨刀声起（村子另一头）——决断。第一次有人先动手，主角被迫表态。",
      "",
      "## 冲突与反转",
      "表面冲突：官仓封仓，村里断粮。",
      "里层冲突：账被换过纸，数目对不上，而作证等于把全村推到刀口上。",
      `反转落点：主角以为自己在问账，其实是在问「谁还记得旧数目」——${theme}从来不是一天起的。`,
      "",
      "## 伏笔与回收",
      "- 埋：断了一截的钟绳。第 3 章回收成「钟再响就没人来」的明证。",
      "- 埋：赵簿袖口的墨迹。第 4 章回收成他私抄的旧账。",
      "",
      "## 结尾钩子",
      "磨刀声越来越近，主角把门闩上，说了句「让他们等」；下一章开门时，门外站的不是衙役。",
      "",
      "## 节奏与字数",
      "字数目标：2400–2600 字。三段场景按 3:4:3 分配；对白占四成，环境描写不超过两成。",
      "",
      "## 待确认",
      "- 赵簿会不会交出旧账（留给第 3 章）",
      "- 老钟头站哪边（留给第 2 章）",
    ].join("\n");
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, body, "utf8");
    note({ event: "outline-written", topic, title, rel, regenerate, chars: body.length });
    toolOf("Write", { file_path: rel }, "t3");
    await sleep(150);
    textOf(`已${regenerate ? "重新生成" : "写入"} ${rel}（${body.length} 字节）`);
    await sleep(150);
    out({ type: "result", subtype: "success", result: `已写入 ${rel}`, is_error: false });
    await finish(0);
  }
}

out({ type: "result", subtype: "success", result: "已完成（未写文件）", is_error: false });
await finish(0);
