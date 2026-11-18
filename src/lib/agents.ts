import type { AgentRole, RunKind } from "@/types";

/** 智能体卡片数据（UI 侧）：与 Rust `models::ai::AgentSpec` 同名同目录，另外补了「快捷指令」与流水线归属。 */
export interface AgentCardInfo {
  role: AgentRole;
  emoji: string;
  name: string;
  /** 负责目录（相对书根，不带结尾斜杠） */
  dir: string;
  duty: string;
  guard: string;
  /** 这个智能体的动作落到哪条流水线（决定前置门禁与后置体检） */
  kind: RunKind;
  /** 快捷指令：点一下就把这句填进输入框 */
  shortcuts: string[];
}

export const AGENTS: AgentCardInfo[] = [
  {
    role: "researcher",
    emoji: "📚",
    name: "资料收集员",
    dir: "参考资料",
    duty: "收集题材资料、考据与素材，整理成可引用的条目",
    guard: "只做资料整理与来源标注，不要改动正文；无法确认的说法必须标明「待考」。",
    kind: "outline",
    shortcuts: ["整理这个题材的资料清单", "为世界观补一段考据", "把参考资料整理成条目"],
  },
  {
    role: "architect",
    emoji: "🏗️",
    name: "大纲架构师",
    dir: "大纲",
    duty: "全书大纲、卷纲与逐章细纲的拆解与维护",
    guard: "自上而下搭结构：先全书、再卷、最后逐章；每章结尾必须留钩子。",
    kind: "outline",
    shortcuts: ["根据设定生成全书大纲", "拆第一卷卷纲", "生成第一章细纲"],
  },
  {
    role: "benchmarker",
    emoji: "🔍",
    name: "对标拆解员",
    dir: "对标",
    duty: "拆解对标作品的节奏、情绪模块与文风",
    guard: "只拆解不抄袭：输出可复用的节奏与情绪模块，禁止直接搬运原文句子。",
    kind: "outline",
    shortcuts: ["拆解对标作品的节奏", "提取对标作品的情绪模块", "总结对标作品的文风"],
  },
  {
    role: "writer",
    emoji: "✍️",
    name: "主笔写手",
    dir: "正文",
    duty: "按细纲写正文，控制节奏与文风",
    guard: "严格遵循大纲，扩展细节，拒绝 AI 味。",
    kind: "prose",
    shortcuts: ["写本章正文", "把这一章改写得更紧凑", "去 AI 味"],
  },
  {
    role: "settings",
    emoji: "⚙️",
    name: "设定管理员",
    dir: "设定",
    duty: "世界观、人物、势力的设定维护与一致性校验",
    guard: "提取人物、世界观的矛盾点并逐条列出；改动设定要说明影响到的章节。",
    kind: "outline",
    shortcuts: ["检查人物矛盾", "补一份角色小传", "统一全书术语表"],
  },
  {
    role: "tracker",
    emoji: "🕵️",
    name: "连贯性追踪员",
    dir: "追踪",
    duty: "伏笔、时间线、角色状态的连续性与欠账检查",
    guard: "只写 追踪/ 下的派生文件，不要改正文；发现断线先报告，不要自行补写情节。",
    kind: "outline",
    shortcuts: ["更新伏笔登记", "检查时间线断裂", "列出上一章遗留的钩子"],
  },
];

export const AGENT_ROLE_LABEL: Record<AgentRole, string> = {
  researcher: "资料收集员",
  architect: "大纲架构师",
  benchmarker: "对标拆解员",
  writer: "主笔写手",
  settings: "设定管理员",
  tracker: "连贯性追踪员",
};

export function agentByRole(role: AgentRole | null): AgentCardInfo | null {
  if (!role) return null;
  return AGENTS.find((a) => a.role === role) ?? null;
}

/** 聚焦目录提示文案，例如 `当前聚焦目录：设定/`。 */
export function focusLabel(dir: string): string {
  return `当前聚焦目录：${dir}/`;
}
