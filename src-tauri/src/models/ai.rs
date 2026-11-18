//! 智能体（Agent）档案：角色 → 负责目录 / 职责 / 强约束。
//!
//! 这里是**唯一权威**：`agent_specs` 命令把 `AgentInfo` 交给前端渲染卡片，
//! `services::engine::compose_agent_prompt` 用同一份 `AgentSpec` 拼 system prompt，
//! 两边不会各写一套文案而慢慢漂移。

use serde::{Deserialize, Serialize};

/// 六种角色，对应书根下六个核心目录。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum AgentRole {
    /// 资料收集员 → 参考资料/
    Researcher,
    /// 大纲架构师 → 大纲/
    Architect,
    /// 对标拆解员 → 对标/
    Benchmarker,
    /// 主笔写手 → 正文/
    Writer,
    /// 设定管理员 → 设定/
    Settings,
    /// 连贯性追踪员 → 追踪/
    Tracker,
}

impl AgentRole {
    pub const ALL: [AgentRole; 6] = [
        Self::Researcher,
        Self::Architect,
        Self::Benchmarker,
        Self::Writer,
        Self::Settings,
        Self::Tracker,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Researcher => "researcher",
            Self::Architect => "architect",
            Self::Benchmarker => "benchmarker",
            Self::Writer => "writer",
            Self::Settings => "settings",
            Self::Tracker => "tracker",
        }
    }

    /// 大小写不敏感、容忍前后空格；未知值返回 `None`（前端传错不该让整条流水线挂掉）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "researcher" => Some(Self::Researcher),
            "architect" => Some(Self::Architect),
            "benchmarker" => Some(Self::Benchmarker),
            "writer" => Some(Self::Writer),
            "settings" => Some(Self::Settings),
            "tracker" => Some(Self::Tracker),
            _ => None,
        }
    }
}

/// 静态档案：不随项目变化，所以全用 `&'static str`。
#[derive(Clone, Copy, Debug)]
pub struct AgentSpec {
    pub role: AgentRole,
    pub name: &'static str,
    pub emoji: &'static str,
    /// 负责目录（相对书根，不带结尾斜杠）
    pub dir: &'static str,
    /// 一句话职责，卡片上用
    pub duty: &'static str,
    /// 写进 prompt 的强约束
    pub guard: &'static str,
}

impl AgentSpec {
    pub fn info(&self) -> AgentInfo {
        AgentInfo {
            role: self.role,
            name: self.name.to_string(),
            emoji: self.emoji.to_string(),
            dir: self.dir.to_string(),
            duty: self.duty.to_string(),
            guard: self.guard.to_string(),
        }
    }
}

/// `agent_specs` 的载荷（camelCase，前端直接当 `AgentInfo` 用）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub role: AgentRole,
    pub name: String,
    pub emoji: String,
    pub dir: String,
    pub duty: String,
    pub guard: String,
}

/// 允许作为「聚焦目录」的书根子目录白名单。
pub const KNOWN_DIRS: [&str; 6] = ["设定", "大纲", "正文", "对标", "参考资料", "追踪"];

pub fn is_known_dir(dir: &str) -> bool {
    KNOWN_DIRS.contains(&dir)
}

pub fn spec(role: AgentRole) -> AgentSpec {
    match role {
        AgentRole::Researcher => AgentSpec {
            role,
            name: "资料收集员",
            emoji: "📚",
            dir: "参考资料",
            duty: "收集题材资料、考据与素材，整理成可引用的条目",
            guard: "只做资料整理与来源标注，不要改动正文；无法确认的说法必须标明「待考」。",
        },
        AgentRole::Architect => AgentSpec {
            role,
            name: "大纲架构师",
            emoji: "🏗️",
            dir: "大纲",
            duty: "全书大纲、卷纲与逐章细纲的拆解与维护",
            guard: "自上而下搭结构：先全书、再卷、最后逐章；每章结尾必须留钩子。\
                    细纲必须写清全章「正文目标字数」（不是单场字数建议）；\
                    用户点名了字数时，以用户字数为准，不得按更低的旧区间设计。",
        },
        AgentRole::Benchmarker => AgentSpec {
            role,
            name: "对标拆解员",
            emoji: "🔍",
            dir: "对标",
            duty: "拆解对标作品的节奏、情绪模块与文风",
            guard: "只拆解不抄袭：输出可复用的节奏与情绪模块，禁止直接搬运原文句子。",
        },
        AgentRole::Writer => AgentSpec {
            role,
            name: "主笔写手",
            emoji: "✍️",
            dir: "正文",
            duty: "按细纲写正文，控制节奏与文风",
            guard: "严格遵循大纲，扩展细节，拒绝 AI 味（不写套话、不堆排比、不用「仿佛」「仿佛在诉说」这类句式）。\
                    人物对白一律使用中文弯双引号“”，禁止使用直角引号「」与『』；引号内句末不加句号。\
                    用户规定的字数是最高优先级：细纲里的单场「字数建议」不是全章目标，\
                    不得按更低的区间提前收尾；只写任务点名的那一章，绝不改别的章节。",
        },
        AgentRole::Settings => AgentSpec {
            role,
            name: "设定管理员",
            emoji: "⚙️",
            dir: "设定",
            duty: "世界观、人物、势力的设定维护与一致性校验",
            guard: "提取人物、世界观的矛盾点并逐条列出；改动设定要说明影响到的章节。",
        },
        AgentRole::Tracker => AgentSpec {
            role,
            name: "连贯性追踪员",
            emoji: "🕵️",
            dir: "追踪",
            duty: "伏笔、时间线、角色状态的连续性与欠账检查",
            guard: "只写 追踪/ 下的派生文件，不要改正文；发现断线先报告，不要自行补写情节。",
        },
    }
}

pub fn all_specs() -> Vec<AgentSpec> {
    AgentRole::ALL.iter().copied().map(spec).collect()
}

// ---------------------------------------------------------------- 阶段 7.5：可定制子智能体

/// 可定制的子智能体（持久化在 `settings.json` 的 `agents` 里）。
///
/// [`AgentSpec`] 是内置默认值，`AgentConfig` 是用户改过之后**真正生效**的那份：
/// `compose_agent_prompt` 只认这个结构，所以「改了 Prompt 下次就生效」不需要额外开关。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    /// 唯一标识，如 `writer` / `writer-2`（小写字母、数字、短横线、下划线）
    pub id: String,
    /// 显示名称，如「主笔写手」
    pub name: String,
    /// Emoji 头像
    pub avatar: String,
    /// 定制的系统提示词（核心）：会作为「角色约束」写进子 Agent 的 prompt
    pub system_prompt: String,
    /// 绑定的目录（书根下的白名单目录之一）
    pub target_dir: String,
    /// 预留：允许使用的工具名（当前只落库，不参与编排）
    #[serde(default)]
    pub tools: Vec<String>,
}

impl AgentConfig {
    /// 由内置档案造一份默认配置（id 就是角色串，保证阶段 7 的旧数据能对上）。
    pub fn from_spec(spec: AgentSpec) -> Self {
        Self {
            id: spec.role.as_str().to_string(),
            name: spec.name.to_string(),
            avatar: spec.emoji.to_string(),
            system_prompt: spec.guard.to_string(),
            target_dir: spec.dir.to_string(),
            tools: Vec::new(),
        }
    }

    /// id 会当「任务计划的 agent_id」用，只接受小写字母/数字/短横线/下划线。
    pub fn is_valid_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 32
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    }

    /// 表单/落库前的清洗：去空白、补默认、修掉非法目录。
    pub fn normalize(&mut self) {
        self.id = self.id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.avatar = self.avatar.trim().to_string();
        self.system_prompt = self.system_prompt.trim().to_string();
        self.target_dir = self
            .target_dir
            .trim()
            .trim_end_matches(['/', '\\'])
            .to_string();

        if self.name.is_empty() {
            self.name = self.id.clone();
        }
        if self.avatar.is_empty() {
            self.avatar = "🤖".to_string();
        }
        if self.system_prompt.is_empty() {
            self.system_prompt =
                "按 oh-story 的写作规范输出或直接修改文件；不确定的地方先说明再动手。".to_string();
        }
        if !is_known_dir(&self.target_dir) {
            self.target_dir = AgentRole::parse(&self.id)
                .map(|r| spec(r).dir.to_string())
                .unwrap_or_else(|| "设定".to_string());
        }
        self.tools = self
            .tools
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        self.tools.sort();
        self.tools.dedup();
    }
}

/// 内置的六个角色配置（首次启动、或用户点了「恢复默认」时用）。
pub fn default_agents() -> Vec<AgentConfig> {
    all_specs()
        .into_iter()
        .map(AgentConfig::from_spec)
        .collect()
}

/// 设置界面「允许调用的工具」那一栏可勾的名字（阶段 9）。
///
/// 顺序就是界面上勾选框的顺序；[`normalize_tools`] 也按这个顺序输出，
/// 这样同一份配置在不同机器上拼出来的 prompt 完全一致（便于对比 args.log）。
pub const TOOL_CHOICES: [&str; 9] = [
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "Bash",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
];

/// 没配过时的默认值：只读写，不许跑命令。
pub fn default_tools() -> Vec<String> {
    vec!["Read".to_string(), "Write".to_string()]
}

/// 这个工具名是否在候选清单里（大小写不敏感，返回规范写法）。
pub fn canonical_tool(name: &str) -> Option<&'static str> {
    let key = name.trim().to_ascii_lowercase();
    TOOL_CHOICES
        .iter()
        .copied()
        .find(|tool| tool.to_ascii_lowercase() == key)
}

/// 清洗工具清单：丢掉不认识的、去重、按 [`TOOL_CHOICES`] 的顺序重排。
///
/// 空列表是**合法**结果（用户一个都没勾），调用方据此拼「不许调用任何工具」的约束；
/// 只有 `None`（字段缺失）才由调用方补 [`default_tools`]。
pub fn normalize_tools(input: &[String]) -> Vec<String> {
    let mut picked: Vec<&'static str> = Vec::new();
    for raw in input {
        if let Some(tool) = canonical_tool(raw) {
            if !picked.contains(&tool) {
                picked.push(tool);
            }
        }
    }
    TOOL_CHOICES
        .iter()
        .copied()
        .filter(|tool| picked.contains(tool))
        .map(|tool| tool.to_string())
        .collect()
}

/// 某个角色真正生效的工具清单：角色自己配了 `tools` 就用它，否则用全局设置。
///
/// 阶段 9 加的两层：`AgentConfig.tools` 是「这个角色特殊一点」的覆盖项，
/// `AppSettings.allowed_tools` 是所有角色共用的默认值。两边都没有时退回
/// [`default_tools`]，保证历史配置不改也能跑。
pub fn effective_tools(agent: &AgentConfig, global: Option<&[String]>) -> Vec<String> {
    if !agent.tools.is_empty() {
        return normalize_tools(&agent.tools);
    }
    match global {
        Some(list) => normalize_tools(list),
        None => default_tools(),
    }
}

/// 这一步是不是「只看不动」的任务（阶段 10）。
///
/// 主 AI 把「检查伏笔与时间线」派给建筑师，建筑师就拿 Write 把大纲覆盖一遍——
/// 这是阶段 10 修的第一号 bug。判定规则：
/// - task 以 `【只读模式】` 开头 → 一定只读（主 AI 自己标的，最可信）；
/// - 含「写 / 生成 / 补 / 改写 / 润色 / 保存 / 新建」→ 一定不是只读（哪怕句子里还有「检查」，
///   比如「写本章正文并检查错别字」，落盘的仍然是正文）；
/// - 其余含「检查 / 核对 / 分析 / 盘点 / 审查 / 梳理 / 列出 / 报告 / 评估 / 只读」→ 只读。
pub fn is_read_only_task(task: &str) -> bool {
    let text = task.trim();
    if text.starts_with("【只读模式】") || text.contains("【只读模式】") {
        return true;
    }
    let writes = [
        "写", "生成", "补", "改写", "润色", "保存", "新建", "落盘", "扩写",
    ];
    if writes.iter().any(|k| text.contains(k)) {
        return false;
    }
    [
        "只读", "检查", "核对", "分析", "盘点", "审查", "梳理", "列出", "报告", "评估", "对账",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// 只读任务的工具清单：把会改文件的三个工具摘掉，其余（Read/Glob/Grep/Bash…）保留。
///
/// 这一步是「提示词之外的第二道锁」：就算模型无视 `【只读模式】`，
/// 它也拿不到 Write，落不了盘。
pub fn read_only_tools(tools: &[String]) -> Vec<String> {
    tools
        .iter()
        .filter(|t| {
            let name = t.as_str();
            name != "Write" && name != "Edit" && name != "MultiEdit"
        })
        .cloned()
        .collect()
}

/// 兜底清洗：空列表补默认、去重 id、丢掉非法 id、逐项 normalize。///
/// 清洗后仍然为空时退回默认——宁可让用户看到六个内置角色，
/// 也不要让主 AI 因为「一个可派的智能体都没有」而永远规划失败。
pub fn normalized_agents(input: Vec<AgentConfig>) -> Vec<AgentConfig> {
    let mut out: Vec<AgentConfig> = Vec::new();
    for mut agent in input {
        agent.normalize();
        if !AgentConfig::is_valid_id(&agent.id) {
            continue;
        }
        if out.iter().any(|a| a.id == agent.id) {
            continue;
        }
        out.push(agent);
    }
    if out.is_empty() {
        return default_agents();
    }
    out
}

/// 按 id 找一份配置；找不到时按角色名回落，再找不到就给一份通用写手配置。
pub fn resolve_agent(agents: &[AgentConfig], id: &str) -> AgentConfig {
    let key = id.trim();
    if let Some(found) = agents.iter().find(|a| a.id == key) {
        return found.clone();
    }
    if let Some(role) = AgentRole::parse(key) {
        if let Some(found) = agents
            .iter()
            .find(|a| a.target_dir == spec(role).dir && a.id == role.as_str())
        {
            return found.clone();
        }
        return AgentConfig::from_spec(spec(role));
    }
    agents
        .first()
        .cloned()
        .unwrap_or_else(|| AgentConfig::from_spec(spec(AgentRole::Writer)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_round_trip_through_strings() {
        for role in AgentRole::ALL {
            assert_eq!(AgentRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(AgentRole::parse("  WRITER "), Some(AgentRole::Writer));
        assert_eq!(AgentRole::parse("nope"), None);
    }

    #[test]
    fn every_agent_owns_a_known_dir() {
        for spec in all_specs() {
            assert!(is_known_dir(spec.dir), "{} 的目录不在白名单里", spec.name);
            assert!(!spec.guard.trim().is_empty());
            assert!(!spec.emoji.trim().is_empty());
        }
    }

    #[test]
    fn default_agents_cover_all_six_roles() {
        let agents = default_agents();
        assert_eq!(agents.len(), 6);
        for role in AgentRole::ALL {
            let found = agents.iter().find(|a| a.id == role.as_str());
            assert!(found.is_some(), "缺少角色 {}", role.as_str());
            assert_eq!(found.unwrap().target_dir, spec(role).dir);
            assert!(!found.unwrap().system_prompt.trim().is_empty());
        }
    }

    #[test]
    fn normalize_repairs_junk_fields() {
        let mut agent = AgentConfig {
            id: " writer ".to_string(),
            name: "   ".to_string(),
            avatar: String::new(),
            system_prompt: "  ".to_string(),
            target_dir: "../../etc/".to_string(),
            tools: vec![" Read ".to_string(), "Read".to_string(), "".to_string()],
        };
        agent.normalize();
        assert_eq!(agent.id, "writer");
        assert_eq!(agent.name, "writer");
        assert_eq!(agent.avatar, "🤖");
        assert!(!agent.system_prompt.is_empty());
        // 非法目录回落到该 id 的默认目录
        assert_eq!(agent.target_dir, "正文");
        assert_eq!(agent.tools, vec!["Read".to_string()]);
    }

    #[test]
    fn normalized_agents_drop_dupes_and_refill_when_empty() {
        let junk = vec![
            AgentConfig {
                id: "BAD ID".to_string(),
                name: "x".to_string(),
                avatar: "🚫".to_string(),
                system_prompt: "p".to_string(),
                target_dir: "设定".to_string(),
                tools: Vec::new(),
            },
            AgentConfig {
                id: "writer".to_string(),
                name: "主笔".to_string(),
                avatar: "✍️".to_string(),
                system_prompt: "p".to_string(),
                target_dir: "正文".to_string(),
                tools: Vec::new(),
            },
            AgentConfig {
                id: "writer".to_string(),
                name: "重复".to_string(),
                avatar: "✍️".to_string(),
                system_prompt: "p".to_string(),
                target_dir: "正文".to_string(),
                tools: Vec::new(),
            },
        ];
        let cleaned = normalized_agents(junk);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0].name, "主笔");
        assert_eq!(normalized_agents(Vec::new()).len(), 6);
    }

    #[test]
    fn resolve_agent_falls_back_to_the_role_default() {
        let agents = default_agents();
        assert_eq!(resolve_agent(&agents, "architect").name, "大纲架构师");
        // 不在列表里但能被角色解析出来 → 用内置默认
        assert_eq!(resolve_agent(&[], "writer").target_dir, "正文");
        // 完全不认识 → 给第一个可用项（这里空列表回落写手）
        assert_eq!(resolve_agent(&[], "nope").target_dir, "正文");
    }

    #[test]
    fn tools_are_normalized_against_the_whitelist() {
        // 大小写、别名、重复、不认识的都清掉，并按 TOOL_CHOICES 顺序排
        let cleaned = normalize_tools(&[
            "write".to_string(),
            "READ".to_string(),
            "Read".to_string(),
            "rm -rf".to_string(),
            "Bash".to_string(),
        ]);
        assert_eq!(cleaned, vec!["Read", "Write", "Bash"]);
        // 空清单合法（用户一个都没勾）
        assert!(normalize_tools(&[]).is_empty());
        assert_eq!(default_tools(), vec!["Read", "Write"]);
        assert_eq!(canonical_tool(" multiedit "), Some("MultiEdit"));
        assert_eq!(canonical_tool("nope"), None);
    }

    #[test]
    fn per_agent_tools_override_the_global_setting() {
        let mut agent = AgentConfig::from_spec(spec(AgentRole::Writer));
        let global = vec!["Read".to_string()];

        // 角色没配 → 用全局
        assert_eq!(effective_tools(&agent, Some(&global)), vec!["Read"]);
        // 全局也没配 → 默认 Read / Write
        assert_eq!(effective_tools(&agent, None), default_tools());
        // 角色自己配了 → 优先（哪怕全局把它关了）
        agent.tools = vec!["Bash".to_string()];
        assert_eq!(effective_tools(&agent, Some(&global)), vec!["Bash"]);
    }
}
