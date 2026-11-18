//! 引擎适配器：把「宿主 Agent CLI 子进程」抽象成统一的流式接口。
//!
//! 唯一实现：
//! - [`ClaudeCodeAdapter`]：`claude --output-format stream-json -p <prompt>`，逐行解析 NDJSON。
//!
//! 三个约束（阶段 1 调研得出）：
//! 1. prompt 里的槽位由 oh-story 的脚本契约决定（`skills/story-long-write/references/agent-calls.md`），
//!    应用层只做字符串填充，不重写；
//! 2. hook 是 `bash .claude/hooks/*.sh`，所以子进程 PATH 里必须有 Git Bash 的 bin；
//! 3. 阻断信号是 **exit code 2 + stderr**，不是 JSON，解析交给 [`crate::services::hook_parse`]。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::models::ai::AgentConfig;
use crate::models::dto::{AiChunk, EngineKind, RunDone, RunKind};
use crate::models::settings::ProviderConfig;
use crate::models::StoryError;
use crate::services::opencode_home;

pub const CHUNK_EVENT: &str = "ai://chunk";
pub const DONE_EVENT: &str = "ai://done";
pub const BLOCKED_EVENT: &str = "hook://blocked";
/// 写完正文之后的自动体检结果。
pub const POSTCHECK_EVENT: &str = "ai://postcheck";
/// 主 AI 总监：思考中（流式文字）
pub const MASTER_THINK_EVENT: &str = "master://think";
/// 主 AI 总监：拆解完成，给出计划
pub const MASTER_PLAN_EVENT: &str = "master://plan";
/// 主 AI 总监：把某一步派给某个子智能体
pub const MASTER_DISPATCH_EVENT: &str = "master://dispatch";
/// 主 AI 总监：整轮编排结束
pub const MASTER_DONE_EVENT: &str = "master://done";
/// 阶段 8：把上一步的产出交给下一步时，前端插一条「[系统] 正在传递 …」。
pub const MASTER_CONTEXT_EVENT: &str = "master://context";
/// 主 AI 总监：某一步的实时心跳（已跑多久、吐了多少条输出、还活着没有）。
pub const MASTER_PROGRESS_EVENT: &str = "master://progress";

/// 上下文清单最多列多少个文件（防止 prompt 被一整个目录的列表吃掉）。
pub const CONTEXT_LIMIT: usize = 40;

/// 组装子智能体的 prompt：角色 → 目录 → 定制提示词 → 全局必读清单 → 上下文清单，**再接**原有四行契约。
///
/// 契约本身一个字都不改：`contract` 原样追加在末尾，所以「项目目录 / 任务包 /
/// 先完整读取任务包…/ 交付后只回…」这四行仍然逐字存在（`ai.rs` 的单测会断言）。
///
/// 注意入参是 [`AgentConfig`]：用户在设置里改过的 `system_prompt` / `target_dir`
/// 下一次运行就直接生效，不需要重启，也没有第二份默认值来打架。
///
/// `tools` 是**已算好的**生效清单（见 [`crate::models::ai::effective_tools`]）：
/// 角色自己配了白名单就用角色的，否则用设置里「允许调用的工具」那一栏。
/// 阶段 9 之前这里是写死的 `agent.tools`，设置界面没有开关，改一次得手改 JSON。
///
/// `global_files` 是阶段 10 加的：`设定/`、`追踪/`、`参考资料/`、本章细纲这些**全书级**的
/// 必读文件。之前只给「目标目录 + 上一步产出」，于是细纲里写着 2400–2600 字，正文照样跑偏。
///
/// `read_only` 为真时，第一行就写死「不要调用 Write 工具，只输出报告」，并且调用方
/// 已经把 Write/Edit/MultiEdit 从 `tools` 里摘掉了（见 `ai::read_only_tools`）。
/// 每个内置角色的必守边界。
///
/// 为什么放在子智能体的 prompt 里而不是只靠主 AI：主 AI 是模型，会误派（真实发生过：
/// 「写一本古代的农民起事的小说」被派给 architect，architect 为了交差写了一份细纲）。
/// 边界写在执行者自己的 prompt 里，误派时它能自己拒绝并说明。
fn role_boundary(id: &str, dir: &str) -> Option<String> {
    let line = match id {
        "architect" => format!(
            "【职责边界】你只负责 {dir}/ 目录（大纲、卷纲、章细纲）。禁止写 正文/ 与 设定/。\
             也不要把作者的整句需求原样当成细纲写进文件——细纲必须写清本章目标、冲突、转折与章末落点（落在动作或半句话上，不要刻意留悬案），\
             只复述一句需求的「细纲」等于没做。若你判断这一步其实是写正文或只是做检查，先说明判断再停下，不要动笔。"
        ),
        "writer" => "【职责边界】你只负责 正文/：按细纲把本章写成正文，不改 大纲/、不改 设定/。\
             细纲缺关键信息时按现有信息写，并在结尾三行内说明你补了什么假设。"
            .to_string(),
        "tracker" => "【职责边界】你管伏笔、时间线、人物状态与前后矛盾：默认只读、只出报告；\
             除非任务明确要求更新 追踪/ 下的账本，否则不要写任何文件。"
            .to_string(),
        "settings" => "【职责边界】你只负责 设定/：一致性核对、人物小传、术语表；不写正文，不改大纲。"
            .to_string(),
        "benchmarker" => "【职责边界】你只负责 对标/：节奏、情绪与文风拆解；不写正文。\
             任务要求「对标采集」时，用 webfetch 抓平台**公开免费**章节存入 对标/<书名>/，\
             抓不到的章节如实报告并提示走「导入对标文本」，严禁编造原文。".to_string(),
        "researcher" => "【职责边界】你只负责 参考资料/：考据与素材整理；不改正文与大纲。".to_string(),
        _ => return None,
    };
    Some(line)
}

/// 拼子智能体的执行 prompt。
///
/// `snapshot` 是编排器在本轮开始时生成的「设定快照」绝对路径（见
/// [`crate::services::setting_snapshot`]）：给了就先读快照，全局文件清单降级为
/// 「细节核对用，按需读」；不给就照旧要求先通读全部全局文件。
#[allow(clippy::too_many_arguments)]
pub fn compose_agent_prompt(
    agent: &AgentConfig,
    tools: &[String],
    context_files: &[String],
    global_files: &[String],
    read_only: bool,
    topic: Option<&str>,
    contract: &str,
    prev_digest: Option<&str>,
    snapshot: Option<&str>,
) -> String {
    let dir = agent
        .target_dir
        .trim()
        .trim_end_matches('/')
        .trim_end_matches('\\');

    let mut prompt = String::new();
    prompt.push_str(&format!("你现在是「野菜工作台」的【{}】。\n", agent.name));
    if read_only {
        prompt.push_str("【只读模式】不要调用 Write 工具，只输出报告：这一步只做检查、核对与分析，任何文件都不要改动。\n");
    }
    prompt.push_str(&format!("你的核心职责是处理 {dir}/ 目录下的内容。\n"));
    // 第 037 章事故：写手用 PowerShell 的 Get-Content / Set-Content 改正文，PS 5.1 把无 BOM 的
    // UTF-8 按 ANSI(cp936) 读成「绗?37绔?」，写回去就成了不可逆的乱码 + BOM。
    // 提示词堵不住所有情况（模型仍可能这么干），所以 hook 与写后体检各有一层，这里是第三层。
    // 只在真的勾了 Write 时才提：没勾 Write 的子智能体本来就不该改文件。
    if tools.iter().any(|t| t.trim().eq_ignore_ascii_case("write")) {
        prompt.push_str(
            "【写盘方式硬规则】改文件只用 Write / Edit 工具。禁止用 PowerShell 的 Get-Content、\
             Set-Content、Out-File、Add-Content 读写书里的 .md：Windows PowerShell 5.1 会把无 BOM 的 \
             UTF-8 当成本地 ANSI 编码（简体中文机器是 cp936）解码，写回去的正文是乱码，而且不可逆。\
             必须用脚本时用 python，读写都显式 encoding='utf-8'，不要写 BOM。\n",
        );
    }
    prompt.push_str("请根据你的角色定位，严格遵守 oh-story 的写作规范进行输出或直接修改文件。\n");
    prompt.push_str(&format!("角色约束：{}\n", agent.system_prompt));

    // 阶段 11：职责边界写进**子智能体自己的 prompt**，不能只靠主 AI 派对人。
    // 主 AI 万一误派（历史上真把「写一本小说」派给了建筑师），建筑师自己得先拒绝。
    if let Some(boundary) = role_boundary(&agent.id, dir) {
        prompt.push_str(&boundary);
        prompt.push('\n');
    }

    // 阶段 14：把「章号 / 字数 / 对白格式」写成执行者自己的硬规则。
    // 用户改过角色提示词时也照样生效——这三件事不能靠某一份可编辑的 prompt 兜底。
    if dir == "正文" {
        prompt.push_str(
            "【正文硬规则 · 最高优先级】\n\
             - 只写任务点名的那一章：文件名必须是「第NNN章_标题.md」，绝不修改、续写或重写其它章节。\n\
             - 用户规定了字数时，以用户字数为唯一标准；细纲里的单场「字数建议」不是全章目标，\
               不得按更低的区间提前收尾。写完自查纯字数（不计标点、标题与空白），不足就继续写。\n\
             - 人物对白一律使用中文弯双引号“”，禁止使用直角引号「」与『』；引号内句末不加句号，\
               用逗号、问号或叹号收束。\n\
             【场景化硬规则 · 网文写法】\n\
             - 演出优先：关键情节（冲突、转折、爽点、情绪爆点）必须写成当场戏——具体的动作、对白、\
               感官细节推着画面走；禁止用说明性叙述总结剧情（“他想起往事，不禁感慨万千”不是场景）。\n\
             - 对白与当场动作要占本章大头（目标五成以上）；纯叙述（背景、心理、概括）只许做短桥段，\
               连续叙述不许超过三段就必须回到一场戏。\n\
             - 禁止总结式过场：时间跳跃要落进一场具体戏开场（环境一笔＋动作/对白一笔），\
               不许用“三天后，他已今非昔比”这类段落级交代跳过剧情。\n\
             【章与章之间】\n\
             - 正文不写小标题：不要出现「## 一」「## 场景一」这类分节标记或幕序；分段只靠空行，\
               场景转换用时间、地点、动作或一句话落地，结构不该被读者看见。\n\
             - 章首接着上一章最后那一刻往下写：不 recap、不写「上一章说到」「话说」「另一边」，不复述读者已经知道的信息。\n\
             - 章末落在一个具体的动作、一句没答完的话或一件物品的状态上；不写设问句，\
               不写「他不知道的是…」「而这一切只是开始」「真正的危险才刚刚到来」「直到很多年后」这类断言式悬念，\
               也不许为了留扣子把正在进行的动作硬切开。\n",
        );
    }
    // 收尾章：上面的「章末落在一句没答完的话上」是给连载章写的，最后一章照它写就永远收不住。
    // 任务文本带 ENDING_MARK 时，用收尾规则覆盖那一条。
    if dir == "正文" && contract.contains(crate::services::intent::ENDING_MARK) {
        prompt.push_str(
            "【全书收尾 · 覆盖上一条章末规则】本章是全书最后一章：\n\
             - 「章末落在一句没答完的话上」在本章不适用：本章必须收束，最后一个画面停住就结束。\n\
             - 不得再开新人物、新线索、新悬念；已开的事件线在本章内结清，或当场交代去向。\n\
             - 正文最后另起一行写「（全书完）」。\n\
             - 不再为了凑字数续开情节；写到目标体量就收。\n",
        );
    }
    if dir == "大纲" {
        prompt.push_str(
            "【细纲硬规则 · 最高优先级】\n\
             - 章级细纲只写任务点名的那一章：文件名必须是「细纲_第NNN章.md」，绝不改其它章节或全书大纲。\n\
             - 细纲里的「正文目标字数」必须是全章总字数；单场「字数建议」之和要达到这个总量。\
               用户点名了字数时，以用户字数为准，不得沿用更低的旧区间。\n",
        );
    }

    // 阶段 10：全局设定必须先读，否则「写第 2 章」时模型只看到细纲、不知道角色状态与伏笔账。
    // R3 项 6：设定/追踪这些文件每一步都被要求通读，实际每章重读一遍同一批全文。
    // 编排器每轮先压一份浓缩快照，这里改成「先读快照 → 细节再回原文」，
    // 注入量降下来，语义不变（长段落仍在原文里，快照自己写明了哪几段没收录）。
    let snapshot = snapshot.map(str::trim).filter(|s| !s.is_empty());
    if let Some(snap) = snapshot {
        prompt.push_str(&format!(
            "\n动笔前先读这份设定快照（全书设定/追踪/伏笔/大纲要点的浓缩版）：{snap}\n\
             快照只有标题、清单与短句；需要人名、原话、完整时间线这些细节时，再按下面的清单去读原始文件——\
             不要一上来就把每份都通读一遍。\n"
        ));
    }
    if !global_files.is_empty() {
        let listed = &global_files[..global_files.len().min(CONTEXT_LIMIT)];
        if snapshot.is_some() {
            prompt.push_str(
                "\n原始文件（快照只压了设定/追踪/大纲主干，本章细纲与细节都在这里，按需查阅）：\n",
            );
        } else {
            prompt.push_str("\n动笔前，必须先阅读以下全局设定与大纲要求（它们是全书的硬约束，不要凭印象写）：\n");
        }
        for file in listed {
            prompt.push_str("- ");
            prompt.push_str(file);
            prompt.push('\n');
        }
        if global_files.len() > listed.len() {
            prompt.push_str(&format!(
                "- …（还有 {} 个未列出）\n",
                global_files.len() - listed.len()
            ));
        }
    }

    // 阶段 13（记忆功能）：上一章的前情提要。模型没有跨章记忆，写到后面忘了前面；
    // 这里直接把「上一章细纲要点 + 上章开头/结尾」内联进 prompt（不靠模型自己去翻文件，
    // 翻不翻全凭自觉）。只有章级任务（写正文/细纲）才有上一章可言。
    if let Some(digest) = prev_digest.map(str::trim).filter(|d| !d.is_empty()) {
        // digest 里可能带事件进度（`【事件内约束】`）：那就先把「该消耗/该新增」对齐，
        // 再接上一章的字面落点。没有事件层的书走的还是原来那句话。
        if digest.contains("【事件内约束】") {
            prompt.push_str(
                "\n【前情提要 · 事件进度与上一章落点】本章必须自然衔接：先按【事件内约束】对齐本章该消耗/该新增的事，再接住上一章的落点（人物状态、时间、地点都要接得上）：\n",
            );
        } else {
            prompt.push_str(
                "\n【前情提要 · 上一章已发生的事】本章必须自然衔接，人物状态、时间、地点都要接得上：\n",
            );
        }
        prompt.push_str(digest);
        prompt.push('\n');
    }

    if context_files.is_empty() {
        prompt.push_str(&format!("目标目录 {dir}/ 现有文件：（还没有文件）\n"));
    } else {
        let listed = &context_files[..context_files.len().min(CONTEXT_LIMIT)];
        prompt.push_str(&format!(
            "目标目录 {dir}/ 现有文件（共 {} 个，先读需要的再动手）：\n",
            context_files.len()
        ));
        for file in listed {
            prompt.push_str("- ");
            prompt.push_str(file);
            prompt.push('\n');
        }
        if context_files.len() > listed.len() {
            prompt.push_str(&format!(
                "- …（还有 {} 个未列出）\n",
                context_files.len() - listed.len()
            ));
        }
    }

    prompt.push('\n');
    // 阶段 11.5：题材一致性。实测问题是「计划对了、产物还是旧的」，
    // 所以把这条写成最高优先级的一段，而不是藏在任务描述里。
    if let Some(topic) = topic.map(str::trim).filter(|t| !t.is_empty()) {
        prompt.push_str(&format!(
            "【题材一致性 · 最高优先级】本书当前题材：{topic}。\
             落笔前必须先完整阅读 设定/ 下的世界观、人物、势力与文风文件，正文里的人物、地名、时代细节都要能在设定里找到出处；\
             已有文件若属于别的题材（例如旧章名与题材无关），一律按本题材重写，不要沿用旧章名与旧情节。\n"
        ));
    }
    // 阶段 8 起的目录硬约束；阶段 9 把「允许的工具」改成从设置里取，
    // 两段仍然放在四行契约**之前**——契约必须保持「原样在末尾」（`ai.rs` 的单测就这么断言）。
    prompt.push_str(&format!(
        "【严格限制】你当前仅被允许修改 {dir}/ 目录下的文件。严禁直接修改其他目录的内容，否则将被系统拦截。\n"
    ));
    // 归档规则（阶段 13.1 修正：不能一刀切）。六个大目录分两类：
    // - 设定/追踪/参考资料/对标：自由归档——已有主题子目录时新文件进对应子目录，
    //   否则全堆在根目录，用户建的目录结构形同虚设（实测问题，即「图4」）。
    // - 大纲/正文：**文件必须在根目录且文件名带章号**。体检脚本（storyctl.py、
    //   写前检查）只在根目录按 `细纲_第NNN章.md` / `第NNN章_*.md` 的模式找文件；
    //   这里若放任子目录归档，AI 会把细纲写进子目录，体检立刻假报「没有细纲」。
    if matches!(dir, "大纲" | "正文") {
        prompt.push_str(&format!(
            "【归档】{dir}/ 下的文件**一律写在根目录**，禁止创建或使用任何子目录。文件名必须带章号：\
             细纲用 `细纲_第NNN章.md`，正文用 `第NNN章_标题.md`（NNN 为三位章号）。\
             体检脚本只认根目录下的这个命名，放错位置等于没写。\n"
        ));
    } else {
        prompt.push_str(&format!(
            "【归档】先查看 {dir}/ 下已有的子目录；若存在与文件主题对应的子目录（例如 世界观/、势力/、角色/、时间线/），\
             新文件必须写进对应子目录；根目录下已有的同名旧文件也要一并挪进对应子目录；\
             只有目录下没有任何匹配的子目录时才写在 {dir}/ 根下。\n"
        ));
    }
    if tools.is_empty() {
        prompt.push_str(
            "允许的工具：（设置里一个都没勾选）只输出文字说明，不要读写任何文件，也不要执行命令。\n\n",
        );
    } else {
        prompt.push_str(&format!(
            "允许的工具：{}。除此之外不要调用其它工具。\n\n",
            tools.join("、")
        ));
    }
    prompt.push_str(contract);
    prompt
}

/// 主 AI 总监的规划 prompt：把作者一句话拆成 `{"plan":[{"agent_id","task"}]}`。
///
/// 只让模型做一件事——**输出 JSON**。执行顺序、失败处理、事件回传都在 Rust 侧，
/// 这样「模型不听话时」还能用 [`crate::commands::master::rules_plan`] 兜底。
pub fn compose_master_prompt(agents: &[AgentConfig], request: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str("你现在是「野菜工作台」的【主 AI 总监】。\n");
    prompt.push_str(
        "作者会把一句话需求交给你，你要把它拆成若干可执行的子任务，派给下面这些专业智能体。\n\n",
    );
    prompt.push_str("可用的智能体（agent_id 只能用这些值）：\n");
    for agent in agents {
        prompt.push_str(&format!(
            "- agent_id: {} / {} {} / 目录：{}/ / 职责：{}\n",
            agent.id,
            agent.avatar,
            agent.name,
            agent.target_dir,
            first_line(&agent.system_prompt)
        ));
    }

    // 阶段 10：职责边界。之前只写「目录 + 一句职责」，模型照样把「检查伏笔」派给建筑师，
    // 建筑师为了交差就写了一份细纲盖掉旧的。这里把「谁能写、谁只能看」讲死。
    prompt.push_str("\n职责边界（越界即算失败）：\n");
    for (role, duty, forbid) in [
        (
            "architect",
            "只负责大纲、卷纲、章细纲的规划与调整",
            "不要拿它去写正文，也不要让它去做检查/核对类任务",
        ),
        ("writer", "只负责把细纲写成正文", "不要派它改大纲或设定"),
        (
            "tracker",
            "负责伏笔、时间线、人物状态、前后矛盾的检查与记账",
            "检查类需求优先派给它",
        ),
        (
            "settings",
            "负责设定一致性、人物小传、术语表",
            "不要派它写正文",
        ),
        (
            "benchmarker",
            "负责对标作品的节奏、情绪、文风拆解",
            "不要派它写正文",
        ),
        (
            "researcher",
            "负责考据、素材、参考资料整理",
            "不要派它改已有的正文或大纲",
        ),
    ] {
        if agents.iter().any(|a| a.id == role) {
            prompt.push_str(&format!("- {role}：{duty}；{forbid}。\n"));
        }
    }

    prompt.push_str("\n先判断需求类型，再决定派谁（**从上往下按顺序判，命中就停**）：\n");
    prompt.push_str("0. 只读类优先判：出现「检查 / 核对 / 盘点 / 梳理 / 评估 / 审查 / 对账 / 伏笔 / 时间线 / 欠账 / 前后矛盾」，\n");
    prompt.push_str("   且没有「写第…章 / 写正文 / 完成…正文 / 扩写 / 落盘 / 写一本」——**只读**：派 tracker（涉及伏笔、时间线、人物状态）\n");
    prompt.push_str("   或 settings（只涉及设定一致性），task 以 `【只读模式】` 开头，并写清「只输出报告，不要调用 Write 工具」。\n");
    prompt.push_str("1. 写正文类：出现「写第…章 / 写正文 / 写一章 / 本章 / 下一章 / 扩写 / 落盘 / 成稿 / 写一部小说 / 写一本…小说 /\n");
    prompt.push_str("   根据设定写小说 / 把设定…写成正文 / 完成…章的正文」——派 writer；**这一步是写正文，不是排大纲**。\n");
    prompt.push_str(
        "   用户写「002章」「2章的正文」这类不带「第」的章号也要认出来；task 里必须写清该章号。\n",
    );
    prompt.push_str(
        "   没有本章细纲时，先派 architect 补一步细纲（task 写「按设定生成第 N 章细纲」）。\n",
    );
    prompt.push_str("   **章号锁定**：用户点名了第 N 章，所有正文/细纲 task 只能围绕第 N 章；\n");
    prompt.push_str("   不许改第 1 章，也不许因为正文/ 里已有别的章就转去写那一章。\n");
    prompt.push_str(
        "   **字数锁定**：用户点名了字数（如 9000-9500 字）时，task 必须原样带上这个区间，\n",
    );
    prompt.push_str("   并写明「细纲里的单场字数建议不作数，以用户要求为准」；不许把细纲的 8000-9500 或 2200-2600 当成目标。\n");
    prompt.push_str("   **对白格式**：writer task 里必须要求人物对白使用中文弯双引号“”，禁止直角引号「」与『』。\n");
    prompt.push_str("2. 排大纲类——先分层级再派人：\n");
    prompt.push_str("   - 「写大纲 / 把大纲写了 / 整本故事的大纲 / 全书大纲 / 总纲 / 卷纲」是**全书大纲**：派 architect，\n");
    prompt.push_str("     task 必须写「生成/重写 大纲/大纲.md，写到卷级为止」，**不要**降级成「第 1 章细纲」；\n");
    prompt.push_str("   - 「生成第 N 章细纲 / 第 N 章章纲」才是**章级细纲**：派 architect，task 写清目标章号；\n");
    prompt.push_str("     细纲要写「正文目标字数」为全章总量，不能只写单场字数建议。\n");
    prompt.push_str("3. 补设定类：「设定 / 世界观 / 势力 / 术语 / 人物小传 / 把设定…写好 / 补全设定」——派 settings，\n");
    prompt.push_str("   task 写清「先读现有大纲与 设定/ 里已有文件，再补全空缺的世界观、势力、人物、文风文件」。\n");
    prompt.push_str("4. 对标类：「对标 / 文风 / 节奏拆解」——派 benchmarker。\n");
    prompt.push_str("5. 找资料类：「考据 / 资料 / 素材」——派 researcher。\n");
    prompt.push_str("6. 一个词都没命中：**只派一步**，task 写「先读 设定/ 与 大纲/，用三行说清你理解的需求与建议动作」，不要动笔。\n");

    prompt.push_str("\ntask 必须是可执行的指令，不许把作者原话直接塞进去：\n");
    prompt.push_str("- 要写清目标目录与交付物，例如把「把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说」\n");
    prompt.push_str(
        "  写成「根据 设定/ 下的全部设定与题材定位，写第 1 章正文（题材：古代的农民起事）」；\n",
    );
    // 阶段 11.6：多意图必须拆步。实测「写大纲 + 补设定」被折叠成一步章细纲，设定意图直接丢失。
    prompt.push_str(
        "- 一句话里有多个意图（「先…再… / 先…然后…」）时，必须按顺序拆成多步，不允许折叠成一步、\n",
    );
    prompt.push_str(
        "  也不许丢掉后面的意图。例如「把大纲写了，整本故事的大纲写好，再把设定里这几个都写好」\n",
    );
    prompt.push_str("  → 第 1 步 architect 写全书大纲（大纲/大纲.md），第 2 步 settings 补全 设定/ 下的空缺文件。\n");
    // 阶段 11.6：强生成词禁止静默沿用。
    prompt.push_str(
        "- 用户原话出现「写了 / 写好 / 生成 / 重写 / 重新」这类强生成词时，task 里**禁止**出现\n",
    );
    prompt.push_str("  「已有…就沿用 / 不要覆盖」——文件已存在也必须重新生成。只有用户明说「按现有细纲写 / 继续写正文」\n");
    prompt.push_str("  才允许沿用现有文件。\n");
    // 阶段 11.5：实测「已有细纲就沿用」会把上一本书的细纲当成新的，必须按题材判一次。
    prompt.push_str(
        "- 出现「写一本/写一部/写小说/整本/从头」这类整本从头写的说法，视为**新书启动**：\n",
    );
    prompt.push_str("  第 1 步必须派 architect **重新生成第 1 章细纲**（task 写「旧细纲作废，不要沿用，按题材重新生成」），\n");
    prompt.push_str("  第 2 步派 writer 按新细纲写正文，task 里写清题材，并要求「正文里的人物与事件要与设定一致」。\n");

    prompt.push_str("\n输出要求（务必遵守）：\n");
    prompt.push_str("1. 只输出一个 JSON 对象，不要 Markdown 代码块，不要任何解释文字。\n");
    prompt.push_str("2. 格式：{\"plan\":[{\"agent_id\":\"...\",\"task\":\"...\"}]}\n");
    prompt.push_str("3. 最多 4 步，按执行先后排列；task 用中文写清这一步要交付什么。\n");
    prompt.push_str("4. 写正文之前必须先有细纲：先派大纲架构师，再派主笔写手；细纲已存在且题材一致时不要重复生成，\n");
    prompt.push_str("   但「整本从头写」或细纲与题材不符时必须重新生成。\n");
    prompt.push_str("5. 需求用不到某个智能体时就不要派它，不要凑步骤。\n");
    // 【本次只做一件事】/【产出】/【不要做】是硬边界。真实事故：用户写「【本次只做一件事】设定，
    // 【不要做】不写正文、不写大纲」，模型只给了一步 settings，规则闸却补了一步「【增量补大纲】」。
    // 代码层另有 enforce_requested_scope 兜底，这里先把边界对模型说死。
    prompt.push_str("6. 需求里有【本次只做一件事】时，计划里只能有那一个交付物对应的步骤；\n");
    prompt.push_str("   【产出】点名了哪个文件就只写那个文件；【不要做】列出的产物一律不许出现在 task 里；\n");
    prompt.push_str("   禁止替作者顺手加大纲、细纲、设定、正文步骤，也不要为了凑满步骤而派生别的角色。\n");
    prompt
        .push_str("7. 检查类需求的 task 一定以 `【只读模式】` 开头；生成类需求不要加这个词。\n\n");
    prompt.push_str("作者的需求：");
    prompt.push_str(request.trim());
    prompt.push('\n');
    prompt
}

/// 取多行提示词的第一行（主 AI 的清单里每行只放一句职责，否则 prompt 会失控）。
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// Windows 隐藏控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 给任意子进程关掉控制台窗口。
///
/// 本程序是 GUI 进程，没有控制台；这种情况下 spawn 一个「控制台子系统」的可执行文件
/// （opencode.exe、python.exe、cmd.exe …）时，Windows 会**为子进程新建一个控制台窗口**，
/// 屏幕上就是那一下黑框闪。带上 `CREATE_NO_WINDOW` 之后子进程照样跑，只是不建窗口。
pub fn hide_console(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// spawn 前要注入子进程的环境变量。
///
/// **Key 只走这里**（`Command::env`），绝不进命令行参数、绝不写日志、绝不回传前端。
/// 没配 Key 时必须返回空 `vars`——注入空变量会把 CLI 的登录态顶掉。
#[derive(Debug, Clone, Default)]
pub struct EnvPlan {
    pub vars: Vec<(String, String)>,
    /// 给界面看的一句话（只含变量名，不含值）
    pub note: Option<String>,
}

impl EnvPlan {
    /// 不注入任何变量。
    pub fn none() -> Self {
        Self {
            vars: Vec::new(),
            note: None,
        }
    }

    /// 按「激活提供商 + 它的 Key」造注入计划。
    pub fn for_provider(provider: &ProviderConfig, key: Option<String>) -> Self {
        let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
        let Some(key) = key else {
            return Self {
                vars: Vec::new(),
                note: Some(format!(
                    "未配置 API Key：不注入任何环境变量，沿用 CLI 自身登录态（provider: {}）",
                    provider.id
                )),
            };
        };

        let mut vars = vec![(provider.api_key_env().to_string(), key)];
        if let Some(env_name) = provider.base_url_env() {
            let base = provider.base_url.trim();
            if !base.is_empty() {
                vars.push((env_name.to_string(), base.to_string()));
            }
        }

        // 用户在模型目录里**显式选中**的模型才注入（claude CLI 认 ANTHROPIC_MODEL）；
        // 没选就维持 CLI 自己的默认模型——内置提供商目录里那串预置模型不该替用户做主。
        // opencode 走 `-m`（model_id 带回退），不从这里拿。
        if provider.protocol == "anthropic" {
            if let Some(model) = provider
                .active_model
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
            {
                vars.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
            }
        }

        let names: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        Self {
            note: Some(format!(
                "环境注入：{}（provider: {} / {}）",
                names.join(", "),
                provider.id,
                provider.name
            )),
            vars,
        }
    }
}

/// 一次运行的上下文。
///
/// `workspace_root` / `task_packet` / `kind` / `chapter` 是「这次到底在跑什么」的信封：
/// claude 适配器只吃 prompt，不读它们，但它们随 RunCtx 一路带着，出问题时是唯一的现场记录。
#[derive(Clone)]
#[allow(dead_code)]
pub struct RunCtx {
    pub workspace_root: PathBuf,
    pub book_root: PathBuf,
    pub task_packet: Option<PathBuf>,
    pub kind: RunKind,
    pub chapter: Option<u32>,
    /// `ai_cancel` 置位后，适配器负责收尾
    pub cancel: Arc<AtomicBool>,
    /// 子进程 pid（取消时用 taskkill /T /F 杀整棵树）
    pub pid: Arc<Mutex<Option<u32>>>,
    /// 需要「一边流式一边收集全文」时填（主 AI 规划用）；普通运行留 None。
    pub capture: Option<Arc<Mutex<String>>>,
}

/// 终局。
#[derive(Debug, Clone)]
pub enum RunOutcome {
    Done { exit: i32 },
    Blocked { exit: i32, stderr: String },
    Failed { exit: i32, stderr: String },
}

/// 这次失败是不是「引擎自己抽了一下」——与内容无关，重跑就能过。
///
/// 只认网络/连接层的抖动。`UnknownError: Unexpected server error` 看着像偶发，
/// 实际是确定性的（provider 没注册、模型不在 opencode.json 里、会话目录跑偏），
/// 重试只会把同一条错误再跑一遍、让作者多等一轮，所以不算在内。
pub fn is_transient_error(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    if text.contains(CANCEL_MARK) {
        // 用户主动取消不算偶发错误，重试会把它变成「取消不掉」。
        return false;
    }
    [
        "socket hang up",
        "connection reset",
        "econnreset",
        "connection refused",
        "econnrefused",
        "timed out",
        "etimedout",
        "temporarily unavailable",
    ]
    .iter()
    .any(|m| t.contains(m))
}

/// 从引擎 stderr 里挑一句最像人话的报错，供界面直接显示。
///
/// opencode 失败时吐的是 NDJSON 或一大段堆栈：优先含 error/fail/错误 的行，
/// 没有就退回最后一行非空内容，最多 300 字，避免把整段落进聊天气泡。
pub fn error_brief(stderr: &str) -> Option<String> {
    // 先抓「模型没注册」这类结构化错误：`--print-logs` 下它长这样——
    //   error="ProviderModelNotFoundError: Model not found: storydesk/glm-5.3-flash."
    // 直接把它摘要出来，比让用户看 300 字的整行堆栈有用得多。
    for line in stderr.lines() {
        if let Some(pos) = line.find("ProviderModelNotFoundError") {
            let rest = &line[pos..];
            // 模型 id 自己带点（glm-5.3-flash），不能按 '.' 切；按引号/反斜杠/换行收尾，
            // 再去掉句末的句点。
            let detail = rest
                .split("Model not found: ")
                .nth(1)
                .map(|tail| {
                    let end = tail.find(['"', '\\', '\n']).unwrap_or(tail.len());
                    tail[..end].trim().trim_end_matches('.').trim().to_string()
                })
                .filter(|text| !text.is_empty());
            let brief = match detail {
                Some(model) => format!("模型没在配置里注册：{model}（ProviderModelNotFoundError）"),
                None => "模型没在配置里注册（ProviderModelNotFoundError）".to_string(),
            };
            return Some(brief);
        }
    }
    let mut fallback: Option<&str> = None;
    for line in stderr.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let lower = line.to_ascii_lowercase();
        if lower.contains("error")
            || lower.contains("fail")
            || line.contains("错误")
            || line.contains("失败")
        {
            return Some(line.chars().take(300).collect());
        }
        fallback = Some(line);
    }
    let last = fallback?;
    let brief: String = last.chars().take(300).collect();
    (!brief.is_empty()).then_some(brief)
}

/// 把引擎失败落到文件里。
///
/// 以前只在 eprintln 打一行，打包成 GUI 之后没人看得到 eprintln，
/// 于是 opencode 说「Check server logs」时作者和开发者都无日志可查。
pub fn append_engine_log(app: &AppHandle, text: &str) {
    use std::io::Write;
    let Ok(dir) = app.path().app_log_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // 先按体量轮转，再追加：这样 engine.log 永远不会长成一个几十 MB 的单文件。
    if let Some(note) = crate::services::ops_cleanup::rotate_engine_log(&dir) {
        eprintln!("[storydesk] {note}");
    }
    let path = dir.join("engine.log");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "[{stamp}] {text}");
    }
}

/// 关键失败路径的统一落点：写引擎日志 + 开发期 stderr。
///
/// main.rs 里是 windows_subsystem = windows，双击启动时根本没有控制台，
/// eprintln 一律进黑洞——「运行状态没写进去」「hook 没装上」这类事后必须
/// 能查的失败，只打 eprintln 等于没记。凡是能拿到 AppHandle 的失败路径都走这里。
pub fn log_failure(app: &AppHandle, scope: &str, detail: impl std::fmt::Display) {
    let text = format!("{scope}：{detail}");
    eprintln!("[storydesk] {text}");
    append_engine_log(app, &text);
}

/// 引擎统一接口。
pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;
    fn run<'a>(
        &'a self,
        prompt: String,
        ctx: RunCtx,
        app: AppHandle,
        run_id: String,
    ) -> BoxFuture<'a, Result<RunOutcome, StoryError>>;
}

// ---------------------------------------------------------------- 事件发射

// ---------------------------------------------------------------- 活动登记
//
// 子智能体一跑就是几分钟，中途只有 stdout 在动。主 AI 的心跳要靠这份登记
// 判断「它是在写，还是根本没动静」——没有这个信号，界面只能显示一个死计时器，
// 作者分不出「在写长章」和「卡死了」。

struct Activity {
    chunks: u64,
    last: Instant,
}

fn activity_map() -> &'static Mutex<HashMap<String, Activity>> {
    static ACTIVITY: OnceLock<Mutex<HashMap<String, Activity>>> = OnceLock::new();
    ACTIVITY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn note_activity(run_id: &str) {
    let mut map = match activity_map().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let entry = map.entry(run_id.to_string()).or_insert(Activity {
        chunks: 0,
        last: Instant::now(),
    });
    entry.chunks += 1;
    entry.last = Instant::now();
}

/// 某个 run 已经吐出多少条输出、距上次输出过去了多少毫秒。
pub fn activity_of(run_id: &str) -> Option<(u64, u64)> {
    let map = match activity_map().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.get(run_id)
        .map(|a| (a.chunks, a.last.elapsed().as_millis() as u64))
}

/// 一步结束后清掉登记，免得长会话把每个 step 的记录都攒在内存里。
pub fn forget_activity(run_id: &str) {
    let mut map = match activity_map().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.remove(run_id);
}

fn emit_chunk(
    app: &AppHandle,
    run_id: &str,
    seq: u64,
    kind: &str,
    text: Option<String>,
    tool: Option<String>,
    target: Option<String>,
    capture: &Option<Arc<Mutex<String>>>,
) {
    note_activity(run_id);
    // 规划场景要在流式播出的同时拿到纯文本（主 AI 的 JSON 就混在正文里），
    // 所以这里顺手往 capture 里追加，界面看到的流不受影响。
    if kind == "text" {
        if let (Some(sink), Some(chunk)) = (capture, text.as_deref()) {
            if let Ok(mut buf) = sink.lock() {
                buf.push_str(chunk);
            }
        }
    }
    let payload = AiChunk {
        run_id: run_id.to_string(),
        seq,
        kind: kind.to_string(),
        text,
        tool,
        target,
    };
    let _ = app.emit(CHUNK_EVENT, payload);
}

/// 发一次 `ai://done`（终局由命令层统一发，适配器只负责跑）。
pub fn emit_done(
    app: &AppHandle,
    run_id: &str,
    outcome: &RunOutcome,
    duration_ms: u64,
    detail: Option<String>,
) {
    let (exit, status, stderr) = match outcome {
        RunOutcome::Done { exit } => (*exit, "done", None),
        RunOutcome::Blocked { exit, stderr } => (*exit, "blocked", Some(stderr.clone())),
        // 用户取消走 Failed 通道（exit -1），但界面上必须显示「已取消」而不是「出错」。
        RunOutcome::Failed { exit, stderr } if stderr.contains(CANCEL_MARK) => {
            (*exit, "cancelled", Some(stderr.clone()))
        }
        RunOutcome::Failed { exit, stderr } => (*exit, "error", Some(stderr.clone())),
    };
    let payload = RunDone {
        run_id: run_id.to_string(),
        exit,
        status: status.to_string(),
        duration_ms,
        stderr,
        detail,
    };
    let _ = app.emit(DONE_EVENT, payload);
}

/// 用户在流里看到「已取消」时，适配器把这句话放进 `Failed.stderr`。
pub const CANCEL_MARK: &str = "已取消";

// ---------------------------------------------------------------- Claude Code CLI

/// 真实引擎：`claude --output-format stream-json -p <prompt>`。
pub struct ClaudeCodeAdapter {
    pub launcher: Launcher,
    /// 只用于 `Command::env` 注入，绝不进参数。
    pub env: EnvPlan,
}

/// 实际要执行的程序：`program` + 前置参数。
///
/// npm 在 Windows 上把 CLI 装成 `claude.cmd`（内容是把参数转给
/// `node "…\node_modules\@anthropic-ai\claude-code\cli.js"`）。Rust 的
/// `Command` 无法把**含换行的参数**安全地转给 .cmd（会报
/// `batch file arguments are invalid`），而我们的 prompt 天生四行，
/// 所以这里把 .cmd 拆开，直接跑 `node cli.js`。
pub struct Launcher {
    pub program: PathBuf,
    pub prefix: Vec<String>,
    /// 拆解成功时的说明，用于在流里显示启动方式。
    pub note: Option<String>,
}

fn js_target_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)([A-Za-z0-9_%\\.:/\-~]+\.(?:js|mjs|cjs))"#)
            .expect("js target regex")
    })
}

/// 从 .cmd/.bat 的文本里找出它真正执行的 .js 入口，并解析 `%dp0%` 前缀。
fn unwrap_cmd(program: &Path) -> Option<(PathBuf, PathBuf)> {
    let ext = program.extension()?.to_str()?.to_ascii_lowercase();
    if ext != "cmd" && ext != "bat" {
        return None;
    }
    let text = std::fs::read_to_string(program).ok()?;
    let dir = program.parent()?;

    let mut script: Option<PathBuf> = None;
    for caps in js_target_re().captures_iter(&text) {
        let raw = caps.get(1)?.as_str();
        // %dp0% / %~dp0 都是「脚本自身所在目录」
        let lowered = raw.to_ascii_lowercase();
        let candidate = if lowered.starts_with("%dp0%") {
            dir.join(&raw[5..])
        } else if lowered.starts_with("%~dp0") {
            dir.join(&raw[5..])
        } else if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            dir.join(raw)
        };
        if candidate.is_file() {
            script = Some(candidate);
            break;
        }
    }
    let script = script?;

    // 同目录若自带 node.exe（nvm / 便携版），优先用它。
    let local_node = dir.join("node.exe");
    let node = if local_node.is_file() {
        local_node
    } else {
        crate::services::env_check::which_exe("node")?
    };

    Some((node, script))
}

impl Launcher {
    pub fn for_program(program: PathBuf) -> Self {
        if let Some((node, script)) = unwrap_cmd(&program) {
            let note = format!(
                "启动方式：{} + {}（已解开 .cmd 包装）",
                node.display(),
                script.display()
            );
            return Self {
                program: node,
                prefix: vec![script.to_string_lossy().into_owned()],
                note: Some(note),
            };
        }
        Self {
            program,
            prefix: Vec::new(),
            note: None,
        }
    }
}

fn bash_bin_dir() -> Option<PathBuf> {
    crate::services::env_check::which_exe("bash")
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|d| {
            !d.to_string_lossy()
                .to_ascii_lowercase()
                .contains("system32")
        })
        .or_else(|| {
            let git = crate::services::env_check::which_exe("git")?;
            let dir = git.parent()?.join("../bin");
            dir.canonicalize().ok()
        })
        // 应用自动下载的 PortableGit：bash 在 usr\bin\，旁边就是全套 coreutils（hook 要用）
        .or_else(|| {
            crate::services::runtime_setup::locate_gitbash()
                .and_then(|bash| bash.parent().map(Path::to_path_buf))
        })
}

impl Engine for ClaudeCodeAdapter {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn run<'a>(
        &'a self,
        prompt: String,
        ctx: RunCtx,
        app: AppHandle,
        run_id: String,
    ) -> BoxFuture<'a, Result<RunOutcome, StoryError>> {
        Box::pin(async move {
            // seq 0/1：先告诉界面这次到底启动了什么、注入了哪些变量名（不含值）。
            let mut seq = 0u64;
            if let Some(note) = self.launcher.note.as_ref() {
                seq += 1;
                emit_chunk(
                    &app,
                    &run_id,
                    seq,
                    "status",
                    Some(note.clone()),
                    None,
                    None,
                    &ctx.capture,
                );
                // 同时落到宿主控制台，便于用日志核对环境变量名（值永不打印）。
                eprintln!("[storydesk] {note}");
            }
            if let Some(note) = self.env.note.as_ref() {
                seq += 1;
                emit_chunk(
                    &app,
                    &run_id,
                    seq,
                    "status",
                    Some(note.clone()),
                    None,
                    None,
                    &ctx.capture,
                );
                eprintln!("[storydesk] {note}");
            }

            let mut cmd = tokio::process::Command::new(&self.launcher.program);
            cmd.args(&self.launcher.prefix)
                .arg("--output-format")
                .arg("stream-json")
                .arg("-p")
                .arg(&prompt)
                .current_dir(&ctx.book_root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            #[cfg(windows)]
            {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }

            // 动态环境注入：提供商协议决定变量名，没 Key 时 vars 为空（不注入空变量）。
            for (name, value) in &self.env.vars {
                cmd.env(name, value);
            }
            // hook 是 .sh，子进程必须能找到 bash 与 node/python。
            if let Some(bin) = bash_bin_dir() {
                let mut paths = vec![bin];
                if let Some(existing) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("PATH", joined);
                }
            }

            let mut child = cmd.spawn().map_err(|e| {
                StoryError::env_missing(
                    "claude",
                    "启动 claude CLI 失败，请确认已安装并在 PATH 中。",
                    &format!("安装：npm i -g @anthropic-ai/claude-code；错误：{e}"),
                )
            })?;

            if let Some(pid) = child.id() {
                if let Ok(mut slot) = ctx.pid.lock() {
                    *slot = Some(pid);
                }
            }

            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| StoryError::internal("无法读取 claude stdout".to_string()))?;
            let stderr_pipe = child.stderr.take();

            // stderr 必须并发读走，否则管道写满会卡死子进程。
            let stderr_task = tokio::spawn(async move {
                let mut buf = String::new();
                if let Some(pipe) = stderr_pipe {
                    let mut lines = BufReader::new(pipe).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
                buf
            });

            let mut lines = BufReader::new(stdout).lines();
            let mut cancelled = false;

            loop {
                if ctx.cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    let _ = child.kill().await;
                    break;
                }
                match tokio::time::timeout(Duration::from_millis(200), lines.next_line()).await {
                    Ok(Ok(Some(line))) => {
                        for part in normalize_line(&line) {
                            seq += 1;
                            emit_chunk(
                                &app,
                                &run_id,
                                seq,
                                part.kind,
                                part.text,
                                part.tool,
                                part.target,
                                &ctx.capture,
                            );
                        }
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        let msg = format!("读取 claude 输出失败: {e}");
                        // 收尾顺序不能再依赖「先 await stderr 任务」：
                        // stderr 的写端还握在子进程手里，管道不会 EOF，await 永不返回，
                        // 结果是 RunHandle 不释放、前端永远显示 running（整个 run 挂死）。
                        // 先 kill + wait 把管道关掉，再给 stderr 收集线程一个有限等待。
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        let stderr = tokio::time::timeout(
                            Duration::from_millis(200),
                            stderr_task,
                        )
                        .await
                        .ok()
                        .and_then(|joined| joined.ok())
                        .unwrap_or_default();
                        let stderr = if stderr.trim().is_empty() {
                            msg
                        } else {
                            format!("{msg}\nstderr: {stderr}")
                        };
                        return Ok(RunOutcome::Failed { exit: -1, stderr });
                    }
                    Err(_) => continue,
                }
            }

            let status = child.wait().await;
            let stderr = stderr_task.await.unwrap_or_default();
            let exit = status.ok().and_then(|s| s.code()).unwrap_or(-1);

            if cancelled {
                return Ok(RunOutcome::Failed {
                    exit,
                    stderr: "已取消".to_string(),
                });
            }

            let outcome = match exit {
                0 => RunOutcome::Done { exit },
                2 => RunOutcome::Blocked { exit, stderr },
                _ => RunOutcome::Failed { exit, stderr },
            };
            Ok(outcome)
        })
    }
}

// ---------------------------------------------------------------- opencode CLI

/// opencode 引擎：`opencode run --format json --auto [-m storydesk/<model>] <prompt>`。
///
/// 与 claude 的关键差异：
/// 1. 自定义 API（OpenAI 协议）**没有 `OPENAI_BASE_URL` 环境变量**，baseURL 只能写进
///    `opencode.json`；所以每次运行前按激活提供商生成配置（写到工作区根与书根，两处都写，
///    opencode 无论把哪层当项目根都能读到）。Key 仍然只走 `Command::env`，
///    配置文件里只放 `{env:OPENAI_API_KEY}` 占位符，明文永不落盘。
/// 2. webfetch / websearch 内置且默认允许——「AI 联网」在 opencode 引擎下天然可用。
pub struct OpencodeAdapter {
    pub program: PathBuf,
    pub env: EnvPlan,
    /// 激活提供商：决定 opencode.json 怎么写。None = 沿用 opencode 自身登录态。
    pub provider: Option<ProviderConfig>,
    /// 兜底提示（例如「claude 未检测到，改用 opencode」），流的第一条 status。
    pub note: Option<String>,
}

/// opencode.json 里自定义提供商的固定 id（`-m` 参数要引用它）。
const OPENCODE_PROVIDER_ID: &str = "storydesk";

/// 按激活提供商生成 `opencode.json` 内容。
///
/// 返回 `None` 表示不该写配置（没 Key / bedrock 协议 / 没配模型），
/// 此时交给 opencode 自身的登录态与默认模型。
fn opencode_config(provider: &ProviderConfig, model_id: &str) -> serde_json::Value {
    // openai 协议（含 DeepSeek / Moonshot / 网关）走 openai-compatible；
    // anthropic 协议走官方适配器。bedrock 不支持，调用方已提前排除。
    let (npm, api_env) = if provider.protocol == "anthropic" {
        ("@ai-sdk/anthropic", provider.api_key_env())
    } else {
        ("@ai-sdk/openai-compatible", provider.api_key_env())
    };
    serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "provider": {
            OPENCODE_PROVIDER_ID: {
                "npm": npm,
                "options": {
                    "baseURL": provider.base_url,
                    "apiKey": format!("{{env:{}}}", api_env),
                },
                "models": {
                    model_id: {},
                },
            },
        },
    })
}

/// 从 `opencode --version` 的输出里取主/次版本号。
///
/// 两种形态都要认：v1 打印裸的 `1.19.5`，v2 打印 `opencode v2.2.0`。
pub fn parse_version(text: &str) -> Option<(u32, u32)> {
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let parts: Vec<&str> = token.split('.').filter(|part| !part.is_empty()).collect();
        if parts.len() >= 2 {
            if let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                return Some((major, minor));
            }
        }
    }
    None
}

/// 探测二进制自身报告的版本，按路径缓存（一个进程只问一次，每次 spawn 都问太浪费）。
pub fn probe_version(program: &std::path::Path) -> Option<(u32, u32)> {
    static CACHE: Mutex<Option<(std::path::PathBuf, Option<(u32, u32)>)>> = Mutex::new(None);
    if let Ok(guard) = CACHE.lock() {
        if let Some((cached_path, cached)) = guard.as_ref() {
            if cached_path == program {
                return *cached;
            }
        }
    }

    // 探测本身也会让 opencode 写自己的日志与库（v2 启动即 bootstrap 数据库），
    // 所以这一枪必须打在一个一次性目录里，不能落进用户真实的 ~/.local/share/opencode。
    let probe_root = std::env::temp_dir().join("storydesk-opencode-probe");
    for leaf in ["data", "state", "config"] {
        let _ = std::fs::create_dir_all(probe_root.join(leaf));
    }

    let mut probe_cmd = std::process::Command::new(program);
    probe_cmd
        .arg("--version")
        .env("XDG_DATA_HOME", probe_root.join("data"))
        .env("XDG_STATE_HOME", probe_root.join("state"))
        .env("XDG_CONFIG_HOME", probe_root.join("config"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    hide_console(&mut probe_cmd);

    let probed = probe_cmd
        .output()
        .ok()
        .and_then(|out| {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            if text.trim().is_empty() {
                text = String::from_utf8_lossy(&out.stderr).into_owned();
            }
            parse_version(&text)
        });

    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((program.to_path_buf(), probed));
    }
    probed
}

/// 版本相关的命令行开关：v2 起默认连「托管后台服务」（固定端口），
/// 必须用 `--standalone` 换成本次运行自己的私有服务，否则会和桌面版抢端口、
/// 也会把刚做好的数据目录隔离一起作废。
pub fn opencode_flags(version: Option<(u32, u32)>) -> Vec<String> {
    match version {
        Some((major, _)) if major >= 2 => vec!["--standalone".to_string()],
        _ => Vec::new(),
    }
}

/// `--log-level` 的取值两版不兼容：v1 只认大写 `ERROR`，v2 只认小写 `error`
/// （v2 传大写直接 `InvalidValue` 退出，实测）。所以取值也得跟着版本走。
pub fn opencode_log_level(version: Option<(u32, u32)>) -> &'static str {
    match version {
        Some((major, _)) if major >= 2 => "error",
        _ => "ERROR",
    }
}

/// 跑一轮 `opencode run` 的固定前缀参数（不含 `-m` 与 prompt），按版本拼。
pub fn run_args_head(version: Option<(u32, u32)>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".to_string(),
        // 让 opencode 把自己的 ERROR 日志打到 stderr。否则 500 只回一句
        // 「Unexpected server error. Check server logs for details.」，
        // 真正的原因（模型没注册、配置没加载）根本进不了 error_brief。
        "--print-logs".to_string(),
        "--log-level".to_string(),
        opencode_log_level(version).to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--auto".to_string(),
    ];
    args.extend(opencode_flags(version));
    args
}

/// 一份 opencode 配置里，指定 provider 声明了哪些模型 id（排序后返回）。
///
/// 派发前的模型对账靠它：`-m <provider>/<model>` 必须是这里的一个键，
/// 否则 opencode 会抛 `ProviderModelNotFoundError`，而它在界面上只会被渲染成
/// 一句「Unexpected server error. Check server logs for details.」。
pub fn declared_models(cfg: &serde_json::Value, provider_id: &str) -> Vec<String> {
    let mut out: Vec<String> = cfg
        .get("provider")
        .and_then(|providers| providers.get(provider_id))
        .and_then(|provider| provider.get("models"))
        .and_then(|models| models.as_object())
        .map(|models| models.keys().cloned().collect())
        .unwrap_or_default();
    out.sort();
    out
}

/// 模型不一致时的可操作文案：两个模型名都要出现，并说清下一步该点哪里。
pub fn model_mismatch_text(declared: &[String], provider_id: &str, model_id: &str) -> String {
    let declared_text = if declared.is_empty() {
        "（一个都没声明）".to_string()
    } else {
        declared.join(" / ")
    };
    format!(
        "配置声明的模型是 {declared_text}，本次请求的是 {provider_id}/{model_id}，二者不一致：\
         已拦下，没有调用模型。请到 设置 → 模型 API 把模型 ID 改成上面声明的那一个，\
         或重新选择一个已声明的模型，再重新派发这一步。"
    )
}

/// 按「将要生效的那份配置文件」做模型对账（与运行期读到的是同一份文件）。
///
/// 返回 `None` 表示放行：模型在配置里，或者读不到/解析不了配置（读不到时不拦，
/// 因为运行期也读不到，这种失败会由引擎自己报出来，不该由对账层误判）。
pub fn check_model_declared(
    config_path: &std::path::Path,
    provider_id: &str,
    model_id: &str,
) -> Option<String> {
    let text = std::fs::read_to_string(config_path).ok()?;
    let cfg: serde_json::Value = serde_json::from_str(&text).ok()?;
    let declared = declared_models(&cfg, provider_id);
    if declared.iter().any(|name| name == model_id) {
        return None;
    }
    Some(model_mismatch_text(&declared, provider_id, model_id))
}

impl Engine for OpencodeAdapter {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn run<'a>(
        &'a self,
        prompt: String,
        ctx: RunCtx,
        app: AppHandle,
        run_id: String,
    ) -> BoxFuture<'a, Result<RunOutcome, StoryError>> {
        Box::pin(async move {
            let mut seq = 0u64;
            for note in [&self.note, &self.env.note] {
                if let Some(text) = note {
                    seq += 1;
                    emit_chunk(
                        &app,
                        &run_id,
                        seq,
                        "status",
                        Some(text.clone()),
                        None,
                        None,
                        &ctx.capture,
                    );
                    eprintln!("[storydesk] {text}");
                }
            }

            // 配置文件与模型：有 Key、协议受支持、且用户配了模型 ID 才写 opencode.json。
            let engine_version = probe_version(&self.program);
            let mut args: Vec<String> = run_args_head(engine_version);
            // 我们生成的那份 opencode.json 的绝对路径，稍后用 OPENCODE_CONFIG 兜底。
            let mut config_file: Option<std::path::PathBuf> = None;
            if let Some(provider) = &self.provider {
                let has_key = self
                    .env
                    .vars
                    .iter()
                    .any(|(k, _)| k == provider.api_key_env());
                let supported = provider.protocol == "openai" || provider.protocol == "anthropic";
                if !supported {
                    seq += 1;
                    let text = format!(
                        "提供商 {} 走 {} 协议，opencode 不支持；已改用 opencode 自身登录态运行",
                        provider.id, provider.protocol
                    );
                    emit_chunk(
                        &app,
                        &run_id,
                        seq,
                        "status",
                        Some(text.clone()),
                        None,
                        None,
                        &ctx.capture,
                    );
                } else if has_key {
                    // 用户在模型目录里选中的模型优先（model_id），没选回退第一个；
                    // 以前硬取 models.first()，目录里 6 个模型也只能用头一个。
                    if let Some(model_id) = provider.model_id() {
                        let cfg = opencode_config(provider, model_id);
                        // 两处都写：opencode 解析项目根的位置不受我们控制（cwd 或 git 根）。
                        for dir in [&ctx.workspace_root, &ctx.book_root] {
                            let path = dir.join("opencode.json");
                            if let Err(e) = std::fs::write(
                                &path,
                                serde_json::to_vec_pretty(&cfg).unwrap_or_default(),
                            ) {
                                seq += 1;
                                let text =
                                    format!("写 opencode.json 失败（{}）：{e}", path.display());
                                emit_chunk(
                                    &app,
                                    &run_id,
                                    seq,
                                    "status",
                                    Some(text),
                                    None,
                                    None,
                                    &ctx.capture,
                                );
                            } else if *dir == ctx.book_root {
                                // 书目录那份是 cwd 的配置，最该被 opencode 读到；
                                // 记下来给 OPENCODE_CONFIG 用。
                                config_file = Some(path);
                            }
                        }
                        // 派发前对账：`-m storydesk/<model_id>` 必须是刚写下去的那份配置里
                        // 声明过的模型。不查就派发的话，opencode 抛 ProviderModelNotFoundError，
                        // 用户在界面上只看到「Unexpected server error」，既不知道是模型名写错，
                        // 也不知道该改哪里——这条路径实测能稳定复现，必须在下发前拦住。
                        if let Some(path) = &config_file {
                            if let Some(text) =
                                check_model_declared(path, OPENCODE_PROVIDER_ID, &model_id)
                            {
                                seq += 1;
                                emit_chunk(
                                    &app,
                                    &run_id,
                                    seq,
                                    "status",
                                    Some(text.clone()),
                                    None,
                                    None,
                                    &ctx.capture,
                                );
                                eprintln!("[storydesk] {text}");
                                return Err(StoryError::invalid("model", text));
                            }
                        }
                        args.push("-m".into());
                        args.push(format!("{OPENCODE_PROVIDER_ID}/{model_id}"));
                        seq += 1;
                        let text = format!(
                            "opencode 模型：{OPENCODE_PROVIDER_ID}/{}（provider: {}）",
                            model_id, provider.id
                        );
                        emit_chunk(
                            &app,
                            &run_id,
                            seq,
                            "status",
                            Some(text.clone()),
                            None,
                            None,
                            &ctx.capture,
                        );
                        eprintln!("[storydesk] {text}");
                    } else {
                        seq += 1;
                        let text = "激活提供商没有配置模型 ID：跳过 opencode.json，沿用 opencode 自身默认模型（设置 → 模型 API → 添加模型）".to_string();
                        emit_chunk(
                            &app,
                            &run_id,
                            seq,
                            "status",
                            Some(text),
                            None,
                            None,
                            &ctx.capture,
                        );
                    }
                } else {
                    seq += 1;
                    let text =
                        "未配置 API Key：不写 opencode.json，沿用 opencode 自身登录态".to_string();
                    emit_chunk(
                        &app,
                        &run_id,
                        seq,
                        "status",
                        Some(text),
                        None,
                        None,
                        &ctx.capture,
                    );
                }
            }
            args.push(prompt);

            // 把真实命令行写进 engine.log：版本相关的开关（`--standalone`、`--log-level`
            // 大小写）出错时，靠这一行就能判断到底用了哪套参数，不用再翻源码。
            let shown: Vec<String> = args
                .iter()
                .map(|arg| {
                    if arg.chars().count() > 80 {
                        format!("<prompt {} 字>", arg.chars().count())
                    } else {
                        arg.clone()
                    }
                })
                .collect();
            append_engine_log(
                &app,
                &format!(
                    "opencode 命令行（v{:?}）：{} {}",
                    engine_version,
                    self.program.display(),
                    shown.join(" ")
                ),
            );

            let mut cmd = tokio::process::Command::new(&self.program);
            cmd.args(&args)
                .current_dir(&ctx.book_root)
                // opencode 的「会话目录」认 PWD：父进程若带着别处的 PWD（从终端、宿主或
                // 别的启动器继承来的），它会把实例开到那个目录，读不到本项目的
                // opencode.json，于是 provider 不存在 → ProviderModelNotFoundError →
                // 界面上只剩一句「Unexpected server error」。这里钉死成真正的 cwd。
                .env("PWD", &ctx.book_root)
                .env_remove("OLDPWD")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(windows)]
            {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            for (name, value) in &self.env.vars {
                cmd.env(name, value);
            }
            // 兜底：不管 opencode 的项目根落到哪个目录，都让它看到我们生成的提供商配置。
            // （OPENCODE_CONFIG 是 opencode 自己的全局配置入口，实测能把 provider 带进去。）
            if let Some(cfg_path) = &config_file {
                cmd.env("OPENCODE_CONFIG", cfg_path);
            }
            // 数据目录隔离：本程序用自己的 opencode 地盘，不和桌面版共用
            // ~/.local/share/opencode（同一个 1.7 GB 的库、同一个 log、同一个全局
            // opencode.json——任何一处被别的程序动过，这里就只剩一句 500）。
            // 旧目录全程只读：不读、不改、不迁移。
            let mut probe_envs: Vec<(String, String)> = self.env.vars.clone();
            if let Some(cfg_path) = &config_file {
                probe_envs.push((
                    "OPENCODE_CONFIG".to_string(),
                    cfg_path.display().to_string(),
                ));
            }
            match app.path().app_local_data_dir() {
                Ok(base) => {
                    let dirs = opencode_home::dirs_under(&base.join("opencode"));
                    if let Err(e) = opencode_home::ensure_dirs(&dirs) {
                        seq += 1;
                        let text = format!(
                            "opencode 数据目录没建成（{}）：{e}；本次继续沿用全局目录",
                            dirs.root.display()
                        );
                        emit_chunk(
                            &app,
                            &run_id,
                            seq,
                            "status",
                            Some(text.clone()),
                            None,
                            None,
                            &ctx.capture,
                        );
                        eprintln!("[storydesk] {text}");
                    } else {
                        for (name, value) in opencode_home::env_pairs(&dirs) {
                            probe_envs.push((name.clone(), value.clone()));
                            cmd.env(name, value);
                        }
                        if let Some(note) = opencode_home::rotate_log(
                            &dirs.opencode_log(),
                            opencode_home::ROTATE_AT,
                            opencode_home::KEEP_COPIES,
                        ) {
                            seq += 1;
                            emit_chunk(
                                &app,
                                &run_id,
                                seq,
                                "status",
                                Some(note.clone()),
                                None,
                                None,
                                &ctx.capture,
                            );
                            eprintln!("[storydesk] {note}");
                        }
                        if let Some(warn) = opencode_home::db_warning(
                            &dirs.db_path(),
                            opencode_home::DB_WARN_BYTES,
                        ) {
                            seq += 1;
                            emit_chunk(
                                &app,
                                &run_id,
                                seq,
                                "status",
                                Some(warn.clone()),
                                None,
                                None,
                                &ctx.capture,
                            );
                            eprintln!("[storydesk] {warn}");
                        }
                    }
                }
                Err(e) => {
                    seq += 1;
                    let text =
                        format!("取不到本程序的数据目录（{e}）：opencode 继续沿用全局目录");
                    emit_chunk(
                        &app,
                        &run_id,
                        seq,
                        "status",
                        Some(text.clone()),
                        None,
                        None,
                        &ctx.capture,
                    );
                    eprintln!("[storydesk] {text}");
                }
            }

            // 健康自检（零 Token：只跑 `opencode models`，不调模型）。
            // 二进制起不来、目录不可写、配置解析不了，都在这里变成人话，
            // 而不是等界面抛一句「Unexpected server error」。
            if let Err(mut text) = opencode_home::health_check(&self.program, &probe_envs) {
                if let Some(hint) = crate::services::opencode_setup::fallback_hint() {
                    text.push('\n');
                    text.push_str(&hint);
                }
                seq += 1;
                emit_chunk(
                    &app,
                    &run_id,
                    seq,
                    "status",
                    Some(text.clone()),
                    None,
                    None,
                    &ctx.capture,
                );
                log_failure(&app, "opencode 引擎自检没通过", &text);
                return Err(StoryError::env_missing("opencode", "引擎自检没通过。", &text));
            }
            // 子进程还得能找到 bash / node / python（hook 与工具脚本要用）。
            if let Some(bin) = bash_bin_dir() {
                let mut paths = vec![bin];
                if let Some(existing) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("PATH", joined);
                }
            }

            let mut child = cmd.spawn().map_err(|e| {
                StoryError::env_missing(
                    "opencode",
                    "启动 opencode CLI 失败。",
                    &format!(
                        "确认 opencode 可执行：{}（错误：{e}）",
                        self.program.display()
                    ),
                )
            })?;
            if let Some(pid) = child.id() {
                if let Ok(mut slot) = ctx.pid.lock() {
                    *slot = Some(pid);
                }
            }

            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| StoryError::internal("无法读取 opencode stdout".to_string()))?;
            let stderr_pipe = child.stderr.take();
            let stderr_task = tokio::spawn(async move {
                let mut buf = String::new();
                if let Some(pipe) = stderr_pipe {
                    let mut lines = BufReader::new(pipe).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
                buf
            });

            let mut lines = BufReader::new(stdout).lines();
            let mut cancelled = false;
            let mut error_text = String::new();
            loop {
                if ctx.cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    let _ = child.kill().await;
                    break;
                }
                match tokio::time::timeout(Duration::from_millis(200), lines.next_line()).await {
                    Ok(Ok(Some(line))) => {
                        for part in normalize_opencode_line(&line, &mut error_text) {
                            seq += 1;
                            emit_chunk(
                                &app,
                                &run_id,
                                seq,
                                part.kind,
                                part.text,
                                part.tool,
                                part.target,
                                &ctx.capture,
                            );
                        }
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        let msg = format!("读取 opencode 输出失败: {e}");
                        // 收尾顺序不能再依赖「先 await stderr 任务」：
                        // stderr 的写端还握在子进程手里，管道不会 EOF，await 永不返回，
                        // 结果是 RunHandle 不释放、前端永远显示 running（整个 run 挂死）。
                        // 先 kill + wait 把管道关掉，再给 stderr 收集线程一个有限等待。
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        let stderr = tokio::time::timeout(
                            Duration::from_millis(200),
                            stderr_task,
                        )
                        .await
                        .ok()
                        .and_then(|joined| joined.ok())
                        .unwrap_or_default();
                        let stderr = if stderr.trim().is_empty() {
                            msg
                        } else {
                            format!("{msg}\nstderr: {stderr}")
                        };
                        return Ok(RunOutcome::Failed { exit: -1, stderr });
                    }
                    Err(_) => continue,
                }
            }

            let status = child.wait().await;
            let stderr = stderr_task.await.unwrap_or_default();
            let exit = status.ok().and_then(|s| s.code()).unwrap_or(-1);

            if cancelled {
                return Ok(RunOutcome::Failed {
                    exit,
                    stderr: "已取消".to_string(),
                });
            }
            let outcome = match exit {
                0 => RunOutcome::Done { exit },
                // opencode 的错误信息在 NDJSON 的 error 事件里，比 stderr 有用得多；
                // stderr 也一并带上——信封里没有 message 时（网络层直接崩）只有 stderr 可看。
                _ if !error_text.is_empty() => RunOutcome::Failed {
                    exit,
                    stderr: if stderr.trim().is_empty() {
                        error_text
                    } else {
                        format!("{error_text}\nstderr: {stderr}")
                    },
                },
                _ => RunOutcome::Failed { exit, stderr },
            };
            Ok(outcome)
        })
    }
}

/// opencode `--format json` 的 NDJSON 归一化。
///
/// 每行信封 `{type, timestamp, sessionID, part, …}`；`error_text` 收集 error 事件的
/// 原文（终局比 stderr 优先用它）。认不出的一律当纯文本，绝不吞内容。
pub fn normalize_opencode_line(line: &str, error_text: &mut String) -> Vec<Part> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return vec![text_part(line.to_string())];
    };

    let ty = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let part = value.get("part");

    match ty {
        "text" => part
            .and_then(|p| p.get("text"))
            .and_then(|v| v.as_str())
            .map(|t| vec![text_part(t)])
            .unwrap_or_default(),
        // 思考流：界面当状态行展示，截一刀防止刷屏；主 AI 的规划捕获只收 text。
        "reasoning" => part
            .and_then(|p| p.get("text"))
            .and_then(|v| v.as_str())
            .map(|t| {
                let head: String = t.chars().take(120).collect();
                vec![status_part(format!("思考：{head}"))]
            })
            .unwrap_or_default(),
        "tool_use" => {
            let name = part
                .and_then(|p| p.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .to_string();
            let input = part
                .and_then(|p| p.get("input"))
                .or_else(|| {
                    part.and_then(|p| p.get("state"))
                        .and_then(|s| s.get("input"))
                })
                .or_else(|| part.and_then(|p| p.get("state")));
            let target = input
                .and_then(target_of)
                .or_else(|| opencode_title_of(part));
            vec![tool_part(name, target)]
        }
        "error" => {
            // opencode 的错误信封不止一种形状：AI SDK 风格是 {name, data:{message}}，
            // 老版本是 {message}，还有部分只有顶层文本。以前只认 error.message，
            // 认不出就报「未给出原因」——用户永远看不到真实原因（实测事故）。
            // 现在逐级深挖，实在认不出就把原文留着。
            let err = value.get("error");
            let name = err.and_then(|e| e.get("name")).and_then(|v| v.as_str());
            let message = err
                .and_then(|e| e.get("message"))
                .or_else(|| {
                    err.and_then(|e| e.get("data"))
                        .and_then(|d| d.get("message"))
                })
                .or_else(|| err.and_then(|e| e.get("data")).and_then(|d| d.get("error")))
                .or_else(|| part.and_then(|p| p.get("text")))
                .and_then(|v| v.as_str());
            let rendered = match (name, message) {
                (Some(n), Some(m)) => format!("{n}: {m}"),
                (Some(n), None) => n.to_string(),
                (None, Some(m)) => m.to_string(),
                (None, None) => {
                    let head: String = trimmed.chars().take(300).collect();
                    format!("未识别的错误信封：{head}")
                }
            };
            if !error_text.is_empty() {
                error_text.push('\n');
            }
            error_text.push_str(&rendered);
            vec![status_part(format!("opencode 错误：{rendered}"))]
        }
        _ => part
            .and_then(|p| p.get("text"))
            .and_then(|v| v.as_str())
            .map(|t| vec![text_part(t)])
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------- NDJSON 归一化

/// 归一化后的片段。
pub struct Part {
    pub kind: &'static str,
    pub text: Option<String>,
    pub tool: Option<String>,
    pub target: Option<String>,
}

fn text_part(s: impl Into<String>) -> Part {
    Part {
        kind: "text",
        text: Some(s.into()),
        tool: None,
        target: None,
    }
}

fn status_part(s: impl Into<String>) -> Part {
    Part {
        kind: "status",
        text: Some(s.into()),
        tool: None,
        target: None,
    }
}

fn tool_part(name: String, target: Option<String>) -> Part {
    Part {
        kind: "tool_use",
        text: None,
        tool: Some(name),
        target,
    }
}

/// 单行 NDJSON → 若干片段。认不出的一律当纯文本，绝不吞内容。
pub fn normalize_line(line: &str) -> Vec<Part> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return vec![text_part(line.to_string())];
    };

    let ty = value.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match ty {
        "text" => match value.get("text").and_then(|v| v.as_str()) {
            Some(t) => vec![text_part(t)],
            None => Vec::new(),
        },
        "status" => match value.get("text").and_then(|v| v.as_str()) {
            Some(t) => vec![status_part(t)],
            None => Vec::new(),
        },
        "tool_use" => {
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .to_string();
            vec![tool_part(name, value.get("input").and_then(target_of))]
        }
        "assistant" | "user" => {
            let content = value
                .get("message")
                .and_then(|m| m.get("content"))
                .or_else(|| value.get("content"));
            content_parts(content)
        }
        "result" => {
            let text = value
                .get("result")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("subtype").and_then(|v| v.as_str()))
                .unwrap_or("运行结束");
            vec![status_part(text)]
        }
        "system" => {
            let text = value
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("system");
            vec![status_part(format!("system: {text}"))]
        }
        // 未知 type：把能看见的文本捞出来，别丢
        _ => value
            .get("text")
            .and_then(|v| v.as_str())
            .map(|t| vec![text_part(t)])
            .unwrap_or_default(),
    }
}

fn content_parts(content: Option<&serde_json::Value>) -> Vec<Part> {
    let Some(items) = content.and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        match item.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "text" => {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    out.push(text_part(t));
                }
            }
            "tool_use" => {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                out.push(tool_part(name, item.get("input").and_then(target_of)));
            }
            // 思考块（扩展思维）：当状态行展示，截一刀防刷屏；主 AI 的规划捕获只收 text，
            // 所以它不会污染 JSON 解析（用户要求「主 AI 正在思考」能看到思考内容）。
            "thinking" => {
                if let Some(t) = item.get("thinking").and_then(|v| v.as_str()) {
                    let head: String = t.chars().take(160).collect();
                    out.push(status_part(format!("思考：{head}")));
                }
            }
            "tool_result" => {
                let tool = item
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("工具")
                    .to_string();
                out.push(status_part(format!("工具返回（{tool}）")));
            }
            _ => {}
        }
    }
    out
}

fn target_of(input: &serde_json::Value) -> Option<String> {
    // claude 系工具用 snake_case（file_path），opencode 系工具用 camelCase（filePath），
    // 两套都得认，否则「读取文件/写入文件」在执行详情里只剩一个光秃秃的标签。
    for key in [
        "file_path",
        "path",
        "filePath",
        "filepath",
        "notebook_path",
        "notebookPath",
        "pattern",
        "url",
        "command",
        "cmd",
        "description",
    ] {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                return Some(v.to_string());
            }
        }
    }
    // 规划类工具（TodoWrite/todowrite）没有路径可抽：给一条「共 N 项」摘要，
    // 让执行详情里看得到规划动作本身，而不是空行。
    if let Some(todos) = input.get("todos").and_then(|v| v.as_array()) {
        let n = todos.len();
        let first = todos
            .first()
            .and_then(|t| t.get("content").or_else(|| t.get("text")))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let head: String = first.chars().take(40).collect();
        return Some(format!("共 {n} 项：{head}"));
    }
    None
}

/// opencode 工具事件的 target 兜底：入参抽不出时用 `state.title`
/// （opencode 会生成「Read 设定/世界观.md」这类人话摘要）。
fn opencode_title_of(part: Option<&serde_json::Value>) -> Option<String> {
    part.and_then(|p| p.get("state"))
        .and_then(|s| s.get("title"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------- 运行注册表

/// 一个在跑的 run 的句柄。
#[derive(Clone)]
pub struct RunHandle {
    pub cancel: Arc<AtomicBool>,
    pub pid: Arc<Mutex<Option<u32>>>,
}

/// 全局运行表：`ai_start` 写入，`ai_cancel` 取出并强杀。
#[derive(Default)]
pub struct EngineState {
    runs: Mutex<HashMap<String, RunHandle>>,
}

impl EngineState {
    pub fn new() -> Self {
        Self {
            runs: Mutex::new(HashMap::new()),
        }
    }

    pub fn insert(&self, run_id: &str) -> RunHandle {
        let handle = RunHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            pid: Arc::new(Mutex::new(None)),
        };
        if let Ok(mut map) = self.runs.lock() {
            map.insert(run_id.to_string(), handle.clone());
        }
        handle
    }

    pub fn take(&self, run_id: &str) -> Option<RunHandle> {
        self.runs.lock().ok()?.remove(run_id)
    }

    pub fn cancel(&self, run_id: &str) -> Option<RunHandle> {
        let handle = self.runs.lock().ok()?.get(run_id).cloned()?;
        handle.cancel.store(true, Ordering::SeqCst);
        Some(handle)
    }

    pub fn running(&self) -> Vec<String> {
        self.runs
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }
}

/// 运行登记的 RAII 把手：作用域结束时自动把条目从 EngineState 里摘掉。
///
/// 为什么要它：表里的条目只有 take 会清，而以前每个收尾点都靠手写 state.take(&id)。
/// 中途任何提前 return / panic / future 被丢弃都会漏掉那一句，条目就永久留在表里——
/// running() 永远报它在跑，ai_cancel 还能打到僵尸句柄，界面上的「执行中」再也下不去。
pub struct RunSlot<'a> {
    state: &'a EngineState,
    run_id: String,
}

impl<'a> RunSlot<'a> {
    /// 登记一个新 run（等价于以前的 state.insert，但会自己收尾）。
    pub fn open(state: &'a EngineState, run_id: &str) -> (Self, RunHandle) {
        let handle = state.insert(run_id);
        (
            Self {
                state,
                run_id: run_id.to_string(),
            },
            handle,
        )
    }

    /// 接管一个已经登记过的 run（登记的句柄在别人手上时用，这里只负责收尾）。
    pub fn adopt(state: &'a EngineState, run_id: &str) -> Self {
        Self {
            state,
            run_id: run_id.to_string(),
        }
    }
}

impl Drop for RunSlot<'_> {
    fn drop(&mut self) {
        self.state.take(&self.run_id);
    }
}

/// 造一个引擎实例。`env` 是本次的注入计划（由激活提供商算出来），
/// `provider` 是激活提供商本体（opencode 要按它写 `opencode.json`）。
///
/// 选择规则：
/// - 指定 `Opencode` → 只找 opencode；
/// - 指定 `Claude` → 优先 claude；claude 不在但 opencode 在时**自动兜底**到 opencode
///   （用户只配了自定义 API、没装 claude 的机器照样能跑），并在流里说明。
pub fn build(
    kind: EngineKind,
    env: EnvPlan,
    provider: Option<&ProviderConfig>,
    opencode_path: Option<&str>,
) -> Result<Box<dyn Engine>, StoryError> {
    match kind {
        EngineKind::Opencode => {
            let program = crate::services::opencode_setup::locate_preferring(opencode_path)
                .ok_or_else(|| {
                StoryError::env_missing(
                    "opencode",
                    "未检测到 opencode CLI，无法执行任何任务。",
                    "在 设置 → AI 引擎 点「下载 opencode」自动安装，或手动安装后重试。",
                )
            })?;
            Ok(Box::new(OpencodeAdapter {
                program,
                env,
                provider: provider.cloned(),
                note: None,
            }))
        }
        EngineKind::Claude => {
            if let Some(program) = crate::services::env_check::which_exe("claude") {
                return Ok(Box::new(ClaudeCodeAdapter {
                    launcher: Launcher::for_program(program),
                    env,
                }));
            }
            // claude 不在：只要 opencode 在就兜底，别让任务直接死在「没装 claude」上。
            if let Some(program) = crate::services::opencode_setup::locate() {
                return Ok(Box::new(OpencodeAdapter {
                    program,
                    env,
                    provider: provider.cloned(),
                    note: Some(
                        "未检测到 claude CLI，本次自动改用 opencode 引擎（自定义模型 API 会通过 opencode.json 映射给引擎）"
                            .to_string(),
                    ),
                }));
            }
            Err(StoryError::env_missing(
                "claude",
                "未检测到 claude / opencode 任何一个宿主 CLI，无法执行任何任务。",
                "安装 Claude Code：npm i -g @anthropic-ai/claude-code；或在 设置 → AI 引擎 点「下载 opencode」自动安装（自定义 API 推荐这条路）。",
            ))
        }
    }
}

#[cfg(test)]
mod console_guard_tests {
    use std::path::Path;

    /// 扫源码：任何 spawn 子进程的文件都必须带上隐藏控制台的三种写法之一。
    ///
    /// 这是给「GUI 进程 spawn 控制台程序会闪黑框」立的护栏——新增一处
    /// `Command::new(...)` 却忘了加 `hide_console` / `hide_window` / `creation_flags`，
    /// 这条测试就会红，不用等用户看见黑框才发现。
    fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_spawner_hides_the_console() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(!files.is_empty(), "没扫到任何源文件，路径可能不对");

        let mut offenders = Vec::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            if !text.contains("Command::new(") {
                continue;
            }
            let guarded = text.contains("hide_console")
                || text.contains("hide_window")
                || text.contains("creation_flags");
            if !guarded {
                offenders.push(file.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "这些文件 spawn 子进程却没有隐藏控制台：{offenders:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 第 8 项 A：版本解析要同时认 v1 的裸版本号与 v2 的 `opencode vX.Y.Z`。
    #[test]
    fn parse_version_reads_both_shapes() {
        assert_eq!(parse_version("1.19.5"), Some((1, 19)));
        assert_eq!(parse_version("opencode v2.2.0"), Some((2, 2)));
        assert_eq!(parse_version("opencode v2.2.0\n"), Some((2, 2)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("no digits here"), None);
    }

    /// 第 8 项 A 验收：v2 必须带 `--standalone`，v1 一个多余参数都不许加。
    #[test]
    fn opencode_flags_are_version_aware() {
        assert_eq!(opencode_flags(Some((2, 2))), vec!["--standalone".to_string()]);
        assert_eq!(opencode_flags(Some((3, 0))), vec!["--standalone".to_string()]);
        assert!(opencode_flags(Some((1, 19))).is_empty());
        // 版本探测失败时按老行为走：不加 v2 专属开关
        assert!(opencode_flags(None).is_empty());
    }

    /// `--log-level` 两版取值不兼容：v2 传大写 `ERROR` 会直接 InvalidValue 退出。
    #[test]
    fn log_level_matches_the_cli_choices() {
        assert_eq!(opencode_log_level(Some((2, 2))), "error");
        assert_eq!(opencode_log_level(Some((1, 19))), "ERROR");
        assert_eq!(opencode_log_level(None), "ERROR");
    }

    /// 第 8 项 A 验收：拼出来的命令行顺序固定，`--standalone` 在 `--auto` 之后，
    /// 而 `-m` 与 prompt 由调用方接在末尾。
    #[test]
    fn run_args_head_keeps_the_contract_order() {
        let v2 = run_args_head(Some((2, 2)));
        assert_eq!(v2[0], "run");
        assert!(v2.windows(2).any(|w| w == ["--format", "json"]));
        assert!(v2.windows(2).any(|w| w == ["--log-level", "error"]));
        assert_eq!(v2.last().map(String::as_str), Some("--standalone"));
        assert_eq!(v2.iter().position(|a| a == "--auto"), Some(6));

        let v1 = run_args_head(Some((1, 19)));
        assert_eq!(v1.last().map(String::as_str), Some("--auto"));
        assert!(v1.windows(2).any(|w| w == ["--log-level", "ERROR"]));
        assert!(!v1.iter().any(|a| a == "--standalone"));
    }

    /// 第 0 项验收夹具：配置只声明 A 模型、却请求 B 模型 → 必须拦下，
    /// 且文案里两个模型名都要出现（用户照着文案就能改设置）。
    #[test]
    fn config_declaring_another_model_blocks_the_dispatch() {
        let dir = std::env::temp_dir().join("storydesk-model-check");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("opencode.json");
        let cfg = serde_json::json!({
            "provider": {
                OPENCODE_PROVIDER_ID: {
                    "npm": "@ai-sdk/openai-compatible",
                    "models": { "nvidia/nemotron-3-ultra-550b-a55b": {} }
                }
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();

        let blocked = check_model_declared(&path, OPENCODE_PROVIDER_ID, "glm-5.3-flash")
            .expect("模型不在配置里时必须拦下");
        assert!(blocked.contains("nvidia/nemotron-3-ultra-550b-a55b"), "{blocked}");
        assert!(blocked.contains("storydesk/glm-5.3-flash"), "{blocked}");
        assert!(blocked.contains("二者不一致"), "{blocked}");
        assert!(blocked.contains("没有调用模型"), "{blocked}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 反面：配置声明的就是请求的那个模型 → 放行。
    #[test]
    fn matching_model_is_allowed_through() {
        let dir = std::env::temp_dir().join("storydesk-model-check-ok");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("opencode.json");
        let cfg = serde_json::json!({
            "provider": {
                OPENCODE_PROVIDER_ID: {
                    "npm": "@ai-sdk/openai-compatible",
                    "models": { "glm-5.3-flash": {} }
                }
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
        assert_eq!(check_model_declared(&path, OPENCODE_PROVIDER_ID, "glm-5.3-flash"), None);
        // 读不到配置时不拦（运行期同样读不到，这种失败交给引擎自己报）。
        assert_eq!(
            check_model_declared(&dir.join("missing.json"), OPENCODE_PROVIDER_ID, "glm-5.3-flash"),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--print-logs` 下的原始结构化错误要能被摘要出来，别只落 engine.log。
    #[test]
    fn error_brief_extracts_the_unregistered_model() {
        let stderr = concat!(
            "timestamp=2026-11-17T07:42:25.917Z level=ERROR run=f14719c7 message=failed \n",
            "error=\"ProviderModelNotFoundError: Model not found: storydesk/glm-5.3-flash.\" \n",
            "    at SessionPrompt.getModel (B:/~BUN/root/chunk-4mj21eew.js:50:831)\n"
        );
        let brief = error_brief(stderr).expect("必须摘出摘要");
        assert!(brief.contains("storydesk/glm-5.3-flash"), "{brief}");
        assert!(brief.contains("模型没在配置里注册"), "{brief}");
        assert!(brief.len() < 120, "摘要不能是整行堆栈：{brief}");
    }

    /// 现场原话：连接层抖动要认成「偶发、可重试」；用户主动取消绝不能被重试
    /// （否则取消不掉）。opencode 的「Unexpected server error」是确定性的
    /// （模型没注册 / 会话目录跑偏），重试只是把同一条错误再跑一遍，故不算偶发。
    #[test]
    fn transient_error_detection() {
        assert!(is_transient_error("socket hang up"));
        assert!(is_transient_error("connect ECONNREFUSED 127.0.0.1:8788"));
        assert!(!is_transient_error(
            r#"{"type":"error","error":{"name":"UnknownError","data":{"message":"Unexpected server error. Check server logs for details."}}}"#
        ));
        assert!(!is_transient_error(&format!("{CANCEL_MARK} 用户已取消")));
        assert!(!is_transient_error("第 331 章细纲缺失，先补细纲"));
    }

    /// 界面只显示一句有用的报错，而不是整段 NDJSON。
    #[test]
    fn error_brief_picks_the_error_line() {
        let stderr = "noise line\n{\"name\":\"UnknownError\",\"message\":\"Unexpected server error. Check server logs for details.\"}\n";
        let brief = error_brief(stderr).expect("要挑出一行");
        assert!(brief.contains("Unexpected server error"), "{brief}");
        assert!(brief.chars().count() <= 300);
        assert_eq!(error_brief("  \n\n "), None);
    }

    #[test]
    fn plain_line_becomes_text() {
        let parts = normalize_line("不是 JSON 的一行");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, "text");
    }

    #[test]
    fn text_json_shape() {
        let parts = normalize_line(r#"{"type":"text","text":"正在生成细纲..."}"#);
        assert_eq!(parts[0].kind, "text");
        assert_eq!(parts[0].text.as_deref(), Some("正在生成细纲..."));
    }

    #[test]
    fn env_plan_skips_empty_key() {
        let provider = crate::models::settings::builtin_providers()
            .into_iter()
            .find(|p| p.id == "anthropic")
            .expect("builtin anthropic");
        let plan = EnvPlan::for_provider(&provider, None);
        assert!(plan.vars.is_empty(), "没 key 时不能注入任何变量");
        assert!(plan.note.unwrap().contains("未配置 API Key"));

        let plan = EnvPlan::for_provider(&provider, Some("  ".to_string()));
        assert!(plan.vars.is_empty(), "空白 key 等同于没配");

        let plan = EnvPlan::for_provider(&provider, Some("sk-test".to_string()));
        let names: Vec<&str> = plan.vars.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, vec!["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL"]);
        assert!(plan.vars.iter().all(|(_, v)| v != ""), "变量值不能为空");
        // note 只报变量名，绝不带值。
        assert!(!plan.note.unwrap().contains("sk-test"));
    }

    /// 模型选择：用户在目录里**显式选中**才注入 ANTHROPIC_MODEL；
    /// 没选维持 CLI 默认——内置提供商预置的模型列表不能替用户做主。
    #[test]
    fn anthropic_model_injected_only_when_selected() {
        let mut provider = crate::models::settings::builtin_providers()
            .into_iter()
            .find(|p| p.id == "anthropic")
            .expect("builtin anthropic");
        provider.active_model = None;
        let plan = EnvPlan::for_provider(&provider, Some("sk-test".to_string()));
        assert!(
            !plan.vars.iter().any(|(k, _)| k == "ANTHROPIC_MODEL"),
            "没选模型时不能注入"
        );
        provider.active_model = Some("claude-3-5-haiku".to_string());
        let plan = EnvPlan::for_provider(&provider, Some("sk-test".to_string()));
        assert!(plan
            .vars
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_MODEL" && v == "claude-3-5-haiku"));
    }

    #[test]
    fn assistant_tool_use_has_target() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"大纲/细纲_第001章.md"}},{"type":"text","text":"读完了"}]}}"#;
        let parts = normalize_line(line);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].kind, "tool_use");
        assert_eq!(parts[0].tool.as_deref(), Some("Read"));
        assert_eq!(parts[0].target.as_deref(), Some("大纲/细纲_第001章.md"));
        assert_eq!(parts[1].text.as_deref(), Some("读完了"));
    }

    // ---------------------------------------------------------------- opencode

    #[test]
    fn opencode_text_part() {
        let line = r#"{"type":"text","sessionID":"s1","part":{"type":"text","text":"第 1 章细纲已生成。"}}"#;
        let mut err = String::new();
        let parts = normalize_opencode_line(line, &mut err);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, "text");
        assert_eq!(parts[0].text.as_deref(), Some("第 1 章细纲已生成。"));
        assert!(err.is_empty());
    }

    #[test]
    fn opencode_tool_use_has_target() {
        let line = r#"{"type":"tool_use","part":{"type":"tool_use","tool":"read","state":{"input":{"path":"设定/世界观.md"}}}}"#;
        let mut err = String::new();
        let parts = normalize_opencode_line(line, &mut err);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, "tool_use");
        assert_eq!(parts[0].tool.as_deref(), Some("read"));
        assert_eq!(parts[0].target.as_deref(), Some("设定/世界观.md"));
    }

    #[test]
    fn opencode_camel_case_file_path_has_target() {
        // opencode 的 read/write 工具入参是 filePath（驼峰），不认的话执行详情只剩光标签。
        let line = r#"{"type":"tool_use","part":{"type":"tool","tool":"read","state":{"input":{"filePath":"设定/题材定位.md"}}}}"#;
        let mut err = String::new();
        let parts = normalize_opencode_line(line, &mut err);
        assert_eq!(parts[0].target.as_deref(), Some("设定/题材定位.md"));
    }

    #[test]
    fn run_slot_removes_the_entry_on_drop() {
        // 运行表里的条目只有 take 会清：RunSlot 保证任何作用域退出（含 panic）都会摘掉，
        // 否则 running() 会永久报它在跑，界面上的「执行中」再也下不去。
        let state = EngineState::new();
        {
            let (slot, handle) = RunSlot::open(&state, "run-1");
            assert_eq!(state.running(), vec!["run-1".to_string()]);
            assert!(!handle.cancel.load(std::sync::atomic::Ordering::SeqCst));
            drop(slot);
        }
        assert!(state.running().is_empty(), "作用域结束必须摘掉条目");

        // adopt：登记是别人做的，这里只负责收尾。
        let _ = state.insert("run-2");
        {
            let _slot = RunSlot::adopt(&state, "run-2");
            assert_eq!(state.running(), vec!["run-2".to_string()]);
        }
        assert!(state.running().is_empty());
    }

    #[test]
    fn opencode_state_title_fallback() {
        // 入参里没有任何可识别键时，退回 state.title 的人话摘要，别让详情空着。
        let line = r#"{"type":"tool_use","part":{"type":"tool","tool":"read","state":{"title":"Read 设定/文风.md","input":{}}}}"#;
        let mut err = String::new();
        let parts = normalize_opencode_line(line, &mut err);
        assert_eq!(parts[0].target.as_deref(), Some("Read 设定/文风.md"));
    }

    #[test]
    fn claude_todowrite_yields_summary_target() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"写全书大纲","status":"pending"},{"content":"补设定","status":"pending"}]}}]}}"#;
        let parts = normalize_line(line);
        assert_eq!(parts[0].kind, "tool_use");
        let target = parts[0].target.as_deref().unwrap_or("");
        assert!(
            target.starts_with("共 2 项"),
            "todos 要给摘要，实际：{target}"
        );
        assert!(target.contains("写全书大纲"));
    }

    #[test]
    fn opencode_error_collects_message() {
        let line = r#"{"type":"error","error":{"type":"api","message":"401 Unauthorized"}}"#;
        let mut err = String::new();
        let parts = normalize_opencode_line(line, &mut err);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, "status");
        assert!(
            err.contains("401 Unauthorized"),
            "终局要用 error 事件里的原文"
        );
    }

    #[test]
    fn opencode_error_envelope_keeps_the_reason() {
        // AI SDK 风格信封：{name, data:{message}}——以前只认 error.message，
        // 这种形状一律显示「未给出原因」，用户永远看不到真实原因。
        let mut err = String::new();
        let parts = normalize_opencode_line(
            r#"{"type":"error","error":{"name":"ProviderAuthError","data":{"message":"Invalid API key"}}}"#,
            &mut err,
        );
        assert!(err.contains("Invalid API key"), "{err}");
        assert!(err.contains("ProviderAuthError"), "{err}");
        assert!(parts[0]
            .text
            .as_deref()
            .unwrap_or_default()
            .contains("Invalid API key"));

        // 认不出的信封：保留原文，不许再出现「未给出原因」。
        err.clear();
        normalize_opencode_line(r#"{"type":"error","weird":{"foo":"bar"}}"#, &mut err);
        assert!(!err.contains("未给出原因"), "{err}");
        assert!(err.contains("weird"), "未知信封要把原文留着：{err}");
    }

    #[test]
    fn opencode_reasoning_is_status_not_text() {
        // 思考流只能进 status：主 AI 的规划捕获只收 text，混进去会污染 JSON 解析。
        let line = r#"{"type":"reasoning","part":{"type":"reasoning","text":"先想想结构……"}}"#;
        let mut err = String::new();
        let parts = normalize_opencode_line(line, &mut err);
        assert_eq!(parts[0].kind, "status");
        assert!(parts[0].text.as_deref().unwrap().contains("先想想结构"));
    }

    #[test]
    fn opencode_plain_line_stays_text() {
        let mut err = String::new();
        let parts = normalize_opencode_line("不是 JSON 的一行", &mut err);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].kind, "text");
    }

    #[test]
    fn opencode_config_maps_openai_provider() {
        let provider = crate::models::settings::builtin_providers()
            .into_iter()
            .find(|p| p.id == "openai")
            .expect("builtin openai");
        let cfg = opencode_config(&provider, "gpt-4.1");
        let p = &cfg["provider"]["storydesk"];
        assert_eq!(p["npm"].as_str().unwrap(), "@ai-sdk/openai-compatible");
        assert_eq!(
            p["options"]["baseURL"].as_str().unwrap(),
            "https://api.openai.com/v1"
        );
        // Key 只能是占位符，明文永不落盘。
        assert_eq!(
            p["options"]["apiKey"].as_str().unwrap(),
            "{env:OPENAI_API_KEY}"
        );
        assert!(p["models"].get("gpt-4.1").is_some());
    }

    #[test]
    fn opencode_config_maps_anthropic_provider() {
        let provider = crate::models::settings::builtin_providers()
            .into_iter()
            .find(|p| p.id == "anthropic")
            .expect("builtin anthropic");
        let cfg = opencode_config(&provider, "claude-sonnet-4-5");
        let p = &cfg["provider"]["storydesk"];
        assert_eq!(p["npm"].as_str().unwrap(), "@ai-sdk/anthropic");
        assert_eq!(
            p["options"]["apiKey"].as_str().unwrap(),
            "{env:ANTHROPIC_API_KEY}"
        );
    }
}
