import type { ReactNode } from "react";

import { cn } from "@/lib/cn";

/**
 * 轻量 Markdown 渲染（GFM 子集）：标题 / 表格 / 列表 / 引用 / 分隔线 / 行内粗体与代码。
 *
 * 为什么不用 react-markdown：只为 AI 输出流这一个场景引入一整条 remark 依赖链不值当，
 * 而且自写解析可以把「AI 吐表格不带分隔行」「行间夹空行」这类脏输出兜住。
 * 不支持的能力（图片、代码块、链接跳转）按原样显示，不吞内容。
 */

// ---------------------------------------------------------------- 行内

function inline(text: string, keyBase = "i"): ReactNode[] {
  const out: ReactNode[] = [];
  // 粗体 / 行内代码 / 斜体，按出现顺序切
  const re = /(\*\*[^*\n]+\*\*|__[^_\n]+__|`[^`\n]+`|\*[^*\n]+\*)/g;
  let last = 0;
  let k = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const tok = m[0];
    if (tok.startsWith("**") || tok.startsWith("__")) {
      out.push(
        <strong key={`${keyBase}-${k++}`} className="font-semibold text-ink-900">
          {tok.slice(2, -2)}
        </strong>,
      );
    } else if (tok.startsWith("`")) {
      out.push(
        <code
          key={`${keyBase}-${k++}`}
          className="rounded bg-ink-100 px-1 py-0.5 font-mono text-[0.9em] text-ink-800"
        >
          {tok.slice(1, -1)}
        </code>,
      );
    } else {
      out.push(<em key={`${keyBase}-${k++}`}>{tok.slice(1, -1)}</em>);
    }
    last = m.index + tok.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

// ---------------------------------------------------------------- 表格

/** 「| a | b |」→ ["a","b"]；容忍首尾竖线缺失与转义 \|。 */
function splitRow(line: string): string[] {
  const t = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < t.length; i++) {
    if (t[i] === "\\" && t[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (t[i] === "|") {
      cells.push(cur.trim());
      cur = "";
    } else {
      cur += t[i];
    }
  }
  cells.push(cur.trim());
  return cells;
}

/** 分隔行：只含 | - : 和空白。 */
function isSeparatorRow(line: string): boolean {
  const t = line.trim();
  return t.length > 0 && /^[\s|:-]+$/.test(t) && t.includes("-");
}

const isPipeLine = (line: string) => line.trim().startsWith("|");

function PipeTable({ lines }: { lines: string[] }) {
  // 有分隔行：分隔行之前是表头；没有：全部当数据行（AI 常忘写分隔行）
  const sepIdx = lines.findIndex((l, i) => i > 0 && isSeparatorRow(l));
  const header = sepIdx > 0 ? splitRow(lines[sepIdx - 1]) : null;
  // 没有分隔行（AI 常忘写）：全部当数据行
  const bodyLines = lines.filter((_, i) => i !== sepIdx && !(sepIdx > 0 && i === sepIdx - 1));
  const rows = bodyLines.map(splitRow);
  const cols = Math.max(header?.length ?? 0, ...rows.map((r) => r.length));

  return (
    <div className="max-w-full overflow-x-auto">
      <table className="mt-1 w-full border-collapse text-[11px] leading-5">
        {header && (
          <thead>
            <tr>
              {Array.from({ length: cols }, (_, i) => (
                <th
                  key={i}
                  className="border border-ink-200 bg-ink-50 px-1.5 py-0.5 text-left font-medium text-ink-700"
                >
                  {inline(header[i] ?? "", `h${i}`)}
                </th>
              ))}
            </tr>
          </thead>
        )}
        <tbody>
          {rows.map((row, r) => (
            <tr key={r} className="even:bg-ink-50/40">
              {Array.from({ length: cols }, (_, c) => (
                <td key={c} className="border border-ink-150 px-1.5 py-0.5 align-top text-ink-700">
                  {inline(row[c] ?? "", `c${r}-${c}`)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ---------------------------------------------------------------- 块级

const HEADING_CLS: Record<number, string> = {
  1: "mt-2 text-[15px] font-semibold text-ink-900",
  2: "mt-2 text-sm font-semibold text-ink-900",
  3: "mt-1.5 text-[13px] font-semibold text-ink-800",
  4: "mt-1 text-xs font-semibold text-ink-800",
  5: "mt-1 text-xs font-medium text-ink-700",
  6: "mt-1 text-xs font-medium text-ink-600",
};

/** 把连续的文本行解析成块级元素列表。 */
function parseBlocks(src: string): ReactNode[] {
  const lines = src.split("\n");
  const out: ReactNode[] = [];
  let key = 0;
  let para: string[] = [];

  const flushPara = () => {
    if (para.length === 0) return;
    out.push(
      <p key={`p-${key++}`} className="whitespace-pre-wrap break-words leading-6 text-ink-800">
        {inline(para.join("\n"), `p${key}`)}
      </p>,
    );
    para = [];
  };

  let i = 0;
  while (i < lines.length) {
    const t = lines[i].trim();

    // 表格：连续的竖线行（允许中间夹一个空行——AI 吐流时常把行拆散）
    if (isPipeLine(lines[i])) {
      flushPara();
      const group: string[] = [];
      let gap = 0;
      let j = i;
      while (j < lines.length) {
        if (isPipeLine(lines[j])) {
          group.push(lines[j].trim());
          gap = 0;
          j++;
        } else if (lines[j].trim() === "" && group.length > 0 && gap === 0) {
          gap++;
          j++;
        } else {
          break;
        }
      }
      if (group.length >= 2) {
        out.push(<PipeTable key={`tb-${key++}`} lines={group} />);
        i = j;
        continue;
      }
      // 孤零零一根竖线行：当普通段落处理
      para.push(lines[i]);
      i++;
      continue;
    }

    if (t === "") {
      flushPara();
      i++;
      continue;
    }

    const h = /^(#{1,6})\s+(.*)$/.exec(t);
    if (h) {
      flushPara();
      const level = h[1].length;
      out.push(
        <div key={`h-${key++}`} className={cn("leading-6", HEADING_CLS[level])}>
          {inline(h[2], `h${key}`)}
        </div>,
      );
      i++;
      continue;
    }

    if (/^(-{3,}|\*{3,}|_{3,})$/.test(t)) {
      flushPara();
      out.push(<hr key={`hr-${key++}`} className="my-2 border-t border-ink-150" />);
      i++;
      continue;
    }

    const ul = /^[-*+]\s+(.*)$/.exec(t);
    const ol = /^\d+[.、)]\s+(.*)$/.exec(t);
    if (ul || ol) {
      flushPara();
      const ordered = !ul;
      const items: string[] = [];
      let j = i;
      while (j < lines.length) {
        const tj = lines[j].trim();
        const mu = /^[-*+]\s+(.*)$/.exec(tj);
        const mo = /^\d+[.、)]\s+(.*)$/.exec(tj);
        if (mu && !ordered) items.push(mu[1]);
        else if (mo && ordered) items.push(mo[1]);
        else if (tj !== "" && items.length > 0 && /^\s{2,}/.test(lines[j])) items[items.length - 1] += `\n${tj}`;
        else break;
        j++;
      }
      const ListTag = ordered ? "ol" : "ul";
      out.push(
        <ListTag
          key={`l-${key++}`}
          className={cn("mt-1 space-y-0.5 pl-4 leading-6 text-ink-800", ordered ? "list-decimal" : "list-disc")}
        >
          {items.map((it, idx) => (
            <li key={idx} className="whitespace-pre-wrap break-words">
              {inline(it, `li${idx}`)}
            </li>
          ))}
        </ListTag>,
      );
      i = j;
      continue;
    }

    if (t.startsWith(">")) {
      flushPara();
      const quote: string[] = [];
      let j = i;
      while (j < lines.length && lines[j].trim().startsWith(">")) {
        quote.push(lines[j].trim().replace(/^>\s?/, ""));
        j++;
      }
      out.push(
        <blockquote
          key={`q-${key++}`}
          className="mt-1 border-l-2 border-ink-200 pl-2 leading-6 text-ink-500"
        >
          {inline(quote.join("\n"), `q${key}`)}
        </blockquote>,
      );
      i = j;
      continue;
    }

    para.push(lines[i]);
    i++;
  }
  flushPara();
  return out;
}

export function Markdown({ source, className }: { source: string; className?: string }) {
  return <div className={cn("min-w-0", className)}>{parseBlocks(source)}</div>;
}
