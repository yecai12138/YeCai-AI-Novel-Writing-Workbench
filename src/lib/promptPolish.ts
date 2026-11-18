/**
 * 发送区「优化提示词」：把作者的口语指令补成一份可执行的规格。
 *
 * 只做确定性的补全——不调用模型，也不改用户原话的意思：
 * 识别「交付什么 / 哪几章 / 多少字 / 要不要收尾 / 不许动什么」，
 * 搞得清的原样搬进结构，搞不清的写进 notes 说明缺口，绝不替用户补事实。
 *
 * 为什么需要它：「词不达意」的现场大多是同一个形状——一句话里同时有
 * 「结尾处理」「第300至320章」「先写细纲」和「重新生成全书大纲」，
 * 主 AI 只能挑一个理解，计划就偏了。先把规格摊开，偏差在派发前就能看见。
 */
export interface PolishResult {
  text: string;
  notes: string[];
}

const RANGE_RES = [
  /第\s*(\d{1,4})\s*章?\s*(?:到|至|-|—|~|～)\s*第?\s*(\d{1,4})\s*章/,
  /(\d{1,4})\s*(?:-|—|~|～|至|到)\s*(\d{1,4})\s*章/,
];
const SINGLE_RE = /第\s*(\d{1,4})\s*章/;
const WORDS_RANGE_RE = /(\d{3,5})\s*(?:-|—|~|～|至|到)\s*(\d{3,5})\s*字/;
const WORDS_SINGLE_RE = /(?:每章|单章|一章)\s*(\d{3,5})\s*字/;
const ENDING_RE = /结尾|收尾|大结局|完结|终章|完本|结束全书/;

function chapterSpan(text: string): [number, number] | null {
  for (const re of RANGE_RES) {
    const m = text.match(re);
    if (!m) continue;
    const start = Number(m[1]);
    const end = Number(m[2]);
    if (start > 0 && end >= start) return [start, end];
  }
  return null;
}

function singleChapter(text: string): number | null {
  const m = text.match(SINGLE_RE);
  if (!m) return null;
  const n = Number(m[1]);
  return n > 0 ? n : null;
}

function wordRange(text: string): [number, number] | null {
  const m = text.match(WORDS_RANGE_RE);
  if (m) {
    const lo = Number(m[1]);
    const hi = Number(m[2]);
    if (lo > 0 && hi >= lo) return [lo, hi];
  }
  const one = text.match(WORDS_SINGLE_RE);
  if (one) {
    const n = Number(one[1]);
    if (n > 0) return [n, n];
  }
  return null;
}

interface Target {
  label: string;
  file: string;
  kind: "prose" | "outline" | "settings" | "benchmark" | "research" | "check";
}

function targetOf(text: string): Target | null {
  // 「把大纲重新生成一遍」这种省略了「全书」的说法也是大纲：大纲/ 下只有 大纲.md 与 细纲_*.md 两类，
  // 没提细纲就只能是 大纲.md。
  // 「根据大纲…写第30至40章的正文」里的大纲是参考资料，不是交付物——只认大纲**直接**当动词宾语
  // 的说法（写大纲 / 把大纲重新生成 / 大纲重排…），中间隔着「根据/按/参考」都不算。
  const outlineIsDeliverable =
    /全书大纲|整本.{0,4}大纲|卷纲/.test(text) ||
    /大纲s*(重新生成|重生成|重写|生成|重做|重排|改写|更新|扩写|细化|补全|补齐|补)/.test(text) ||
    /(写|生成|重新生成|重生成|重做|重排|扩写|细化|补全|补齐|改|更新)s*(全书)?大纲/.test(text);
  if (outlineIsDeliverable)
    return { label: "全书大纲", file: "大纲/大纲.md（写到卷级，不展开成单章细纲）", kind: "outline" };
  if (/细纲/.test(text))
    return { label: "章级细纲", file: "大纲/细纲_第NNN章.md", kind: "outline" };
  if (/正文|续写|写.{0,4}章/.test(text))
    return { label: "正文", file: "正文/第NNN章_<小标题>.md（每章一份，不许跳章）", kind: "prose" };
  if (/设定/.test(text)) return { label: "设定", file: "设定/ 下对应文件（只增补，不整文件重写）", kind: "settings" };
  if (/对标/.test(text)) return { label: "拆对标", file: "对标/ 下的拆解笔记", kind: "benchmark" };
  if (/参考资料|找资料/.test(text)) return { label: "找资料", file: "参考资料/ 下的材料", kind: "research" };
  if (/检查|核对|审一遍|审一下|盘点/.test(text)) return { label: "只读检查", file: "不改文件，只输出结论与问题清单", kind: "check" };
  return null;
}

/**
 * 改写一条口语指令。
 *
 * 规则：
 * - 交付物、章号区间、字数、收尾四件事各自独立识别，识别到就写进规格；
 * - 没识别到的写进 notes（缺哪一项、会影响什么），不猜、不填默认事实；
 * - 原话永远附在最后，主 AI 仍能读到用户的原始措辞。
 */
export function polishPrompt(draft: string): PolishResult {
  const text = draft.trim();
  const notes: string[] = [];
  if (!text) return { text: "", notes: ["输入是空的——先写一句要它做什么"] };

  const target = targetOf(text);
  // 「重新生成全书大纲」和「写第N章细纲」互斥：硬塞进一份规格，主 AI 只能挑一个理解，
  // 用户看到的偏差就是这么来的。这种时候不合并——按原话里先出现的那个定型，另一个提出来让你拆。
  const bookOutlineAt = text.search(/全书大纲|整本.{0,4}大纲|卷纲/);
  const chapterOutlineAt = text.search(/细纲/);
  const conflict = bookOutlineAt >= 0 && chapterOutlineAt >= 0;
  const span = chapterSpan(text);
  const single = span ? null : singleChapter(text);
  const words = wordRange(text);
  const ending = ENDING_RE.test(text);
  // 「结尾钩子」是在说章末写法，不是在要求这一轮收尾。
  const hookTalk = /结尾钩子/.test(text) && !/结尾处理|写结尾|写个结尾|全书收尾|大结局|完结|终章|完本/.test(text);

  const lines: string[] = [];
  lines.push(
    target
      ? `【本次只做一件事】${target.label}`
      : "【本次只做一件事】按原话判断（未识别出明确交付物：正文 / 细纲 / 大纲 / 设定）",
  );
  if (target) lines.push(`【产出】${target.file}`);

  if (conflict) {
    lines.push(
      `【冲突】原话同时要「全书大纲」和「第 N 章细纲」：这两件事互相覆盖，本轮只按先出现的那个（${bookOutlineAt < chapterOutlineAt ? "全书大纲" : "章级细纲"}）执行，另一个请单独下发一次`,
    );
    lines.push("【范围】只做上面那一件；另一件不要顺手一起做");
  } else if (span) {
    lines.push(
      `【范围】第 ${span[0]}—${span[1]} 章（共 ${span[1] - span[0] + 1} 章）；一步只写一章，不许跳章，也不许把区间扩成整本`,
    );
  } else if (single) {
    lines.push(`【范围】只写第 ${single} 章；不要顺手改其它章`);
  } else {
    lines.push("【范围】原话没给章号区间：如果这是批量任务，补一句「第X至第Y章」能让派发不缩水成单章");
  }

  // 字数这条要看交付物：正文是「写完自查」，细纲是「把目标字数写进细纲」，
  // 只读检查/找资料/拆对标不吃每章字数，全书大纲按卷级而不是按章派字。
  // 「全书40万字」是整本目标，不是每章下限；两者分开写，免得下游把它当单章下限去凑。
  const wholeWords = (() => {
    const m = text.match(/(?:全书|整本|一共|总共)[^，。；、]{0,8}?(\d+(?:\.\d+)?)\s*万\s*字/);
    return m ? Number(m[1]) : null;
  })();
  const noWordScope =
    !!target &&
    (target.kind === "check" || target.kind === "research" || target.kind === "benchmark" || target.label === "全书大纲");
  const forProse = !target || target.kind === "prose";
  if (noWordScope) {
    if (words && target?.label === "全书大纲") {
      lines.push(
        `【字数】全书大纲写到 ${words[0] === words[1] ? `${words[0]}` : `${words[0]}—${words[1]}`} 字（卷级条目，不展开成每章细纲）`,
      );
    }
  } else if (words) {
    lines.push(
      forProse
        ? `【字数】每章纯正文 ${words[0] === words[1] ? `${words[0]}` : `${words[0]}—${words[1]}`} 字（不计标点、标题与空白），写完自查`
        : `【字数】每章正文目标 ${words[0] === words[1] ? `${words[0]}` : `${words[0]}—${words[1]}`} 字：写进细纲的「正文目标字数」，各场景字数建议之和要达到 ${words[0]} 字以上`,
    );
  } else {
    lines.push("【字数】原话没给字数：按本章细纲里的「正文目标字数」执行，不额外承诺");
  }

  if (wholeWords) {
    lines.push(`【总字数】全书目标 ${wholeWords} 万字：按区间均分到各章去把握，不要为了让整本达标把单章写炸`);
  }

  if (ending && !hookTalk) {
    const last = span ? span[1] : single;
    const outlineTarget = target?.kind === "outline";
    if (last && outlineTarget) {
      lines.push(
        `【收尾】第 ${last} 章是全书最后一章的细纲：这一章把主线与各条事件线收干净，章末闭合；细纲里不许写「后续如续写」「可以为下一卷开场」这类续写邀请，末尾写明「本章为全书最后一章，正文收在（全书完）」`,
      );
    } else if (last) {
      lines.push(
        `【收尾】第 ${last} 章是全书最后一章：写完即收束，正文最后另起一行写「（全书完）」；不开新人物新线索、不留钩子，本章不按字数下限追加续写`,
      );
    } else {
      lines.push(
        "【收尾】这轮要把书写完：最后一章写完即收束，标「（全书完）」；不开新线、不留钩子，不按字数下限追加续写",
      );
    }
  }

  if (/参考资料|对标/.test(text)) lines.push("【依据】先读 参考资料/ 与 对标/ 下的材料，再动笔；没读到的部分不要凭印象写");

  if (target?.kind === "check" || target?.kind === "research") {
    lines.push("【不要做】不改任何文件；只输出结论与问题清单；不反问确认，直接按本指令执行");
  } else if (target?.label === "全书大纲") {
    lines.push("【不要做】不展开成单章细纲、不碰 正文/ 与 大纲/细纲_*.md；不重写已有内容；不反问确认，直接按本指令执行");
  } else {
    lines.push("【不要做】不改区间外的章节文件；不重写已有内容；不反问确认，直接按本指令执行");
  }
  lines.push(`【原话】${text}`);

  // 缺口与风险：只报事实，不替用户决定。
  if (!span && !single && !conflict) {
    notes.push("没识别到章号区间——范围这条只写了占位建议，需要你自己补「第X至第Y章」");
  }
  if (!words && !noWordScope) notes.push("没识别到字数要求——字数这条按细纲目标走，没有额外下限");
  if (!target) notes.push("没识别到交付物（正文/细纲/大纲/设定），计划可能派错角色");
  if (conflict) {
    notes.push("全书大纲与章级细纲互斥：规格里只保留了先出现的那个，另一个需要你配套改一句再发一次");
  }
  if (hookTalk) notes.push("「结尾钩子」是章末写法要求，不算这一轮的收尾请求——没有替你加收尾规则");

  return { text: lines.join("\n"), notes };
}
